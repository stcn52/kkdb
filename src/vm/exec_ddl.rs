use super::execute::{ExecResult, VM};
use crate::error::Result;
use crate::sql::ast::*;
use crate::storage::btree::BTree;
use crate::types::Value;

impl VM {
    // ---- CREATE TABLE ----
    pub(crate) fn exec_create_table(
        &mut self,
        create: &CreateTableStmt,
        original_sql: &str,
    ) -> Result<ExecResult> {
        // CREATE TABLE AS SELECT
        if let Some(ref query) = create.source {
            return self.exec_create_table_as_select(create, query.as_ref().clone());
        }

        // In multi-file mode: open/create a separate pager file for this table.
        if let Some(ref db_dir) = self.db_dir.clone() {
            if VM::is_safe_table_name(&create.table_name) {
                VM::open_or_create_table_pager(
                    &mut self.table_pagers,
                    db_dir,
                    &create.table_name,
                    self.pager.in_transaction(),
                )?;
            }
        }

        // Get the two pagers: catalog (schema B-tree) and table (data B-tree).
        let table_name = create.table_name.clone();
        let tbl_key = table_name.to_ascii_lowercase();
        if self.table_pagers.contains_key(&tbl_key) {
            // Multi-file: temporarily split borrow by extracting table_pager.
            let table_pager = self.table_pagers.get_mut(&tbl_key).unwrap() as *mut _;
            // SAFETY: table_pager and self.pager are disjoint fields.
            let table_pager: &mut crate::storage::pager::Pager = unsafe { &mut *table_pager };
            self.schema.create_table(
                &mut self.pager,
                table_pager,
                &create.table_name,
                &create.columns,
                create.if_not_exists,
                original_sql,
                &create.checks,
                create.is_fts,
            )?;
        } else {
            // Single-file / memory mode: catalog pager is also table pager.
            // We need two &mut borrows of self.pager 鈥?split via raw ptr.
            let p = &mut self.pager as *mut _;
            let p2: &mut crate::storage::pager::Pager = unsafe { &mut *p };
            self.schema.create_table(
                &mut self.pager,
                p2,
                &create.table_name,
                &create.columns,
                create.if_not_exists,
                original_sql,
                &create.checks,
                create.is_fts,
            )?;
        }

        // L4: Auto-create FTS internal index table and indices
        if create.is_fts {
            let fts_tbl = format!("{}_fts_idx", create.table_name);
            let ddl = format!("CREATE TABLE {} (id INTEGER PRIMARY KEY, term TEXT, doc_id INTEGER)", fts_tbl);
            let internal_create = CreateTableStmt {
                table_name: fts_tbl.clone(),
                columns: vec![
                    ColumnDef { name: "id".to_string(), data_type: crate::types::DataType::Integer, primary_key: true, autoincrement: false, not_null: false, unique: false, default: None, references: None, check_expr: None },
                    ColumnDef { name: "term".to_string(), data_type: crate::types::DataType::Text, primary_key: false, autoincrement: false, not_null: false, unique: false, default: None, references: None, check_expr: None },
                    ColumnDef { name: "doc_id".to_string(), data_type: crate::types::DataType::Integer, primary_key: false, autoincrement: false, not_null: false, unique: false, default: None, references: None, check_expr: None },
                ],
                if_not_exists: true,
                is_fts: false,
                source: None,
                checks: vec![],
            };
            self.exec_create_table(&internal_create, &ddl)?;
            
            let idx_term = format!("idx_{}_fts_term", create.table_name);
            let internal_idx1 = CreateIndexStmt {
                index_name: idx_term,
                table_name: fts_tbl.clone(),
                columns: vec!["term".to_string()],
                unique: false,
                if_not_exists: true,
            };
            self.exec_create_index(&internal_idx1)?;

            let idx_doc = format!("idx_{}_fts_doc", create.table_name);
            let internal_idx2 = CreateIndexStmt {
                index_name: idx_doc,
                table_name: fts_tbl.clone(),
                columns: vec!["doc_id".to_string()],
                unique: false,
                if_not_exists: true,
            };
            self.exec_create_index(&internal_idx2)?;
        }

        self.clear_index_caches();
        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!("Table '{}' created", create.table_name),
        })
    }

    fn exec_create_table_as_select(
        &mut self,
        create: &CreateTableStmt,
        query: crate::sql::ast::SelectStmt,
    ) -> Result<ExecResult> {
        use crate::sql::ast::ColumnDef;
        use crate::types::{DataType, Value};

        // Phase 1: Execute SELECT and materialise all rows + column names.
        let (col_names, rows) = match self.exec_select(&query)? {
            crate::vm::execute::ExecResult::QueryResult { columns, rows } => (columns, rows),
            _ => return Err(crate::error::KkdbError::Internal("CTAS: exec_select did not return rows".into())),
        };

        // Infer column types from the first non-NULL value in each column.
        // Falls back to Text if column is all-NULL or result set is empty.
        let mut col_types: Vec<DataType> = vec![DataType::Text; col_names.len()];
        'outer: for row in &rows {
            let mut all_inferred = true;
            for (i, val) in row.iter().enumerate() {
                if matches!(col_types[i], DataType::Text) {
                    if !matches!(val, Value::Null) {
                        col_types[i] = match val {
                            Value::Integer(_) => DataType::Integer,
                            Value::Real(_) => DataType::Real,
                            Value::Blob(_) => DataType::Blob,
                            _ => DataType::Text,
                        };
                    } else {
                        all_inferred = false;
                    }
                }
            }
            if all_inferred {
                break 'outer;
            }
        }

        // Build column definitions with auto-names for expressions without alias.
        let columns: Vec<ColumnDef> = col_names
            .iter()
            .zip(col_types)
            .enumerate()
            .map(|(i, (name, dt))| {
                let col_name = if name.is_empty() || name == "?" {
                    format!("col_{}", i + 1)
                } else {
                    name.to_ascii_lowercase()
                };
                ColumnDef {
                    name: col_name,
                    data_type: dt,
                    primary_key: false,
                    autoincrement: false,
                    not_null: false,
                    unique: false,
                    default: None,
                    references: None,
                    check_expr: None,
                }
            })
            .collect();

        // Generate DDL SQL string to store in schema.
        let cols_sql: String = columns
            .iter()
            .map(|c| format!("{} {}", c.name, c.data_type))
            .collect::<Vec<_>>()
            .join(", ");
        let ddl_sql = format!("CREATE TABLE {} ({})", create.table_name, cols_sql);

        if create.if_not_exists
            && self.schema.tables.contains_key(&create.table_name.to_lowercase())
        {
            return Ok(ExecResult::Ok {
                message: format!("Table '{}' already exists", create.table_name),
            });
        }

        // Phase 2: Create table + bulk-insert with implicit transaction (atomicity).
        let need_auto_txn = !self.pager.in_transaction();
        if need_auto_txn {
            self.pager.begin_transaction()?;
            self.schema_snapshot = Some(self.schema.clone());
        }

        let result = (|| -> Result<ExecResult> {
            // In multi-file mode: open/create a separate pager file for this table.
            if let Some(ref db_dir) = self.db_dir.clone() {
                if VM::is_safe_table_name(&create.table_name) {
                    VM::open_or_create_table_pager(
                        &mut self.table_pagers,
                        db_dir,
                        &create.table_name,
                        self.pager.in_transaction(),
                    )?;
                }
            }
            let tbl_key = create.table_name.to_ascii_lowercase();
            if self.table_pagers.contains_key(&tbl_key) {
                let table_pager = self.table_pagers.get_mut(&tbl_key).unwrap() as *mut _;
                let table_pager: &mut crate::storage::pager::Pager = unsafe { &mut *table_pager };
                self.schema.create_table(
                    &mut self.pager,
                    table_pager,
                    &create.table_name,
                    &columns,
                    create.if_not_exists,
                    &ddl_sql,
                    &[],
                    create.is_fts,
                )?;
            } else {
                let p = &mut self.pager as *mut _;
                let p2: &mut crate::storage::pager::Pager = unsafe { &mut *p };
                self.schema.create_table(
                    &mut self.pager,
                    p2,
                    &create.table_name,
                    &columns,
                    create.if_not_exists,
                    &ddl_sql,
                    &[],
                    create.is_fts,
                )?;
            }
            self.clear_index_caches();

            // Insert every row from the SELECT result.
            let insert_stmt = crate::sql::ast::InsertStmt {
                table_name: create.table_name.clone(),
                columns: None,
                source: crate::sql::ast::InsertSource::Values(
                    rows.iter()
                        .map(|r| r.iter().map(|v| crate::sql::ast::Expr::from_value(v.clone())).collect())
                        .collect(),
                ),
                conflict: crate::sql::ast::ConflictPolicy::Error,
            };
            self.exec_insert(&insert_stmt)?;

            Ok(ExecResult::Ok {
                message: format!("Table '{}' created with {} row(s)", create.table_name, rows.len()),
            })
        })();

        if need_auto_txn {
            match result {
                Ok(r) => {
                    self.pager.commit_transaction()?;
                    self.schema_snapshot = None;
                    Ok(r)
                }
                Err(e) => {
                    let _ = self.pager.rollback_transaction();
                    if let Some(snap) = self.schema_snapshot.take() {
                        self.schema = snap;
                    }
                    self.clear_index_caches();
                    Err(e)
                }
            }
        } else {
            result
        }
    }

    // ---- DROP TABLE ----
    pub(crate) fn exec_drop_table(&mut self, drop: &DropTableStmt) -> Result<ExecResult> {
        let is_fts = self.schema.tables.get(&drop.table_name.to_lowercase()).map(|t| t.is_fts).unwrap_or(false);
        self.schema
            .drop_table(&mut self.pager, &drop.table_name, drop.if_exists)?;
        
        // L4: Cascade drop for FTS internal table
        if is_fts {
            let fts_tbl = format!("{}_fts_idx", drop.table_name);
            let _ = self.exec_drop_table(&DropTableStmt {
                table_name: fts_tbl,
                if_exists: true,
            });
        }

        self.clear_index_caches();
        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!("Table '{}' dropped", drop.table_name),
        })
    }

    // ---- ALTER TABLE ----
    pub(crate) fn exec_alter_table(&mut self, alter: &AlterTableStmt) -> Result<ExecResult> {
        let msg = match &alter.action {
            AlterTableAction::AddColumn(col_def) => {
                self.schema
                    .alter_add_column(&mut self.pager, &alter.table_name, col_def)?;
                format!(
                    "Column '{}' added to table '{}'",
                    col_def.name, alter.table_name
                )
            }
            AlterTableAction::DropColumn(col_name) => {
                self.schema
                    .alter_drop_column(&mut self.pager, &alter.table_name, col_name)?;
                format!(
                    "Column '{}' dropped from table '{}'",
                    col_name, alter.table_name
                )
            }
            AlterTableAction::RenameTable(new_name) => {
                self.schema
                    .alter_rename_table(&mut self.pager, &alter.table_name, new_name)?;
                format!("Table '{}' renamed to '{}'", alter.table_name, new_name)
            }
            AlterTableAction::RenameColumn { old_name, new_name } => {
                self.schema.alter_rename_column(
                    &mut self.pager,
                    &alter.table_name,
                    old_name,
                    new_name,
                )?;
                format!(
                    "Column '{}' renamed to '{}' in table '{}'",
                    old_name, new_name, alter.table_name
                )
            }
            AlterTableAction::EnableRowLevelSecurity => {
                let tbl_key = alter.table_name.to_ascii_lowercase();
                if let Some(tbl) = self.schema.tables.get_mut(&tbl_key) {
                    tbl.rls_enabled = true;
                } else {
                    return Err(crate::error::KkdbError::RuntimeError(
                        format!("table '{}' not found", alter.table_name),
                    ));
                }
                format!("RLS enabled on table '{}'", alter.table_name)
            }
        };

        self.clear_index_caches();
        self.auto_flush()?;
        Ok(ExecResult::Ok { message: msg })
    }

    // ---- CREATE INDEX ----
    pub(crate) fn exec_create_index(&mut self, create_idx: &CreateIndexStmt) -> Result<ExecResult> {
        // Reconstruct the original SQL for schema storage
        let unique_str = if create_idx.unique { "UNIQUE " } else { "" };
        let cols_str = create_idx.columns.join(", ");
        let sql = format!(
            "CREATE {}INDEX {} ON {} ({})",
            unique_str, create_idx.index_name, create_idx.table_name, cols_str
        );

        let tbl_key = create_idx.table_name.to_ascii_lowercase();
        if self.table_pagers.contains_key(&tbl_key) {
            let table_pager = self.table_pagers.get_mut(&tbl_key).unwrap() as *mut _;
            let table_pager: &mut crate::storage::pager::Pager = unsafe { &mut *table_pager };
            self.schema.create_index(
                &mut self.pager,
                table_pager,
                &create_idx.index_name,
                &create_idx.table_name,
                &create_idx.columns,
                create_idx.unique,
                create_idx.if_not_exists,
                &sql,
            )?;
        } else {
            let p = &mut self.pager as *mut _;
            let p2: &mut crate::storage::pager::Pager = unsafe { &mut *p };
            self.schema.create_index(
                &mut self.pager,
                p2,
                &create_idx.index_name,
                &create_idx.table_name,
                &create_idx.columns,
                create_idx.unique,
                create_idx.if_not_exists,
                &sql,
            )?;
        }
        self.clear_index_caches();
        self.auto_flush()?;

        Ok(ExecResult::Ok {
            message: format!("Index created: {}", create_idx.index_name),
        })
    }

    // ---- DROP INDEX ----
    pub(crate) fn exec_drop_index(&mut self, drop: &DropIndexStmt) -> Result<ExecResult> {
        self.schema
            .drop_index(&mut self.pager, &drop.index_name, drop.if_exists)?;
        self.clear_index_caches();
        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!("Index '{}' dropped", drop.index_name),
        })
    }

    /// Update the root page number in the schema table for a table or index object.
    pub(crate) fn update_schema_object_root_page(
        &mut self,
        object_name: &str,
        new_root: u32,
    ) -> Result<()> {
        let schema_root = self.pager.schema_root_page();
        let mut btree = BTree::new(&mut self.pager);
        let schema_rows = btree.scan_all(schema_root)?;

        for (rowid, row) in schema_rows {
            if row.len() >= 5 {
                if let Value::Text(ref name) = row[1] {
                    if name.eq_ignore_ascii_case(object_name) {
                        let mut new_row = row.clone();
                        new_row[3] = Value::Integer(new_root as i64);
                        let mut btree = BTree::new(&mut self.pager);
                        let new_schema_root = btree.update_row(schema_root, rowid, &new_row)?;
                        if new_schema_root != schema_root {
                            btree.pager.set_schema_root_page(new_schema_root)?;
                        }
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Update the root page number for a table.
    pub(crate) fn update_schema_root_page(
        &mut self,
        table_name: &str,
        new_root: u32,
    ) -> Result<()> {
        self.update_schema_object_root_page(table_name, new_root)
    }

    // ---- EXPLAIN ----
    pub(crate) fn exec_explain(&mut self, stmt: &Statement) -> Result<ExecResult> {
        let plan = match stmt {
            Statement::Select(select) => {
                let mut plan = String::new();
                plan.push_str("QUERY PLAN\n");
                if let Some(ref from) = select.from {
                    plan.push_str(&format!("  SCAN {}\n", self.from_name(from)));
                }
                if select.where_clause.is_some() {
                    plan.push_str("  FILTER (WHERE clause)\n");
                }
                if !select.order_by.is_empty() {
                    plan.push_str("  SORT (ORDER BY)\n");
                }
                if select.limit.is_some() {
                    plan.push_str("  LIMIT\n");
                }
                plan
            }
            Statement::Insert(insert) => {
                format!("INSERT INTO {}\n", insert.table_name)
            }
            Statement::Update(update) => {
                let mut plan = format!("UPDATE {}\n", update.table_name);
                plan.push_str("  SCAN table\n");
                if update.where_clause.is_some() {
                    plan.push_str("  FILTER (WHERE clause)\n");
                }
                plan
            }
            Statement::Delete(delete) => {
                let mut plan = format!("DELETE FROM {}\n", delete.table_name);
                plan.push_str("  SCAN table\n");
                if delete.where_clause.is_some() {
                    plan.push_str("  FILTER (WHERE clause)\n");
                }
                plan
            }
            _ => "No plan available for this statement type\n".to_string(),
        };
        Ok(ExecResult::Explain { plan })
    }

    // ---- CREATE VIEW (Batch E) ----
    pub(crate) fn exec_create_view(&mut self, create: &CreateViewStmt) -> Result<ExecResult> {
        use crate::schema::TableSchema;
        let exists = self.schema.get_table(&create.name).is_ok();
        if exists {
            if create.or_replace {
                self.schema.remove_table(&create.name);
            } else if create.if_not_exists {
                return Ok(ExecResult::Ok {
                    message: format!("VIEW {} already exists", create.name),
                });
            } else {
                return Err(crate::error::KkdbError::RuntimeError(
                    format!("view '{}' already exists", create.name),
                ));
            }
        }
        // Views are stored as TableSchema in-memory with root_page=0 and view_select set
        let view_schema = TableSchema {
            name: create.name.clone(),
            columns: Vec::new(),
            col_names: create.columns.clone(),
            root_page: 0,
            next_rowid: 0,
            view_select: Some(create.query.as_ref().clone()),
            is_fts: false,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            rls_enabled: false,
            policies: Vec::new(),
        };
        self.schema.add_view(view_schema);
        Ok(ExecResult::Ok {
            message: format!("VIEW {} created", create.name),
        })
    }

    pub(crate) fn from_name(&self, from: &FromClause) -> String {
        match from {
            FromClause::Table { name, .. } => name.clone(),
            FromClause::Join { left, right, .. } => {
                format!("{} JOIN {}", self.from_name(left), self.from_name(right))
            }
            FromClause::Subquery { alias, .. } => format!("(subquery) AS {}", alias),
            FromClause::SetOp { alias, .. } => format!("(setop) AS {}", alias),
            FromClause::TableFunction { name, alias, .. } => {
                alias.as_deref().unwrap_or(name.as_str()).to_string()
            }
        }
    }

    // ---- VACUUM ----
    /// VACUUM: merge pending-free pages into the active freelist, then flush.
    /// Reclaims storage space by making deleted / overflow pages available for reuse.
    /// In multi-file mode, applies to every open pager.
    pub(crate) fn exec_vacuum(&mut self) -> Result<ExecResult> {
        // Flush the catalog / legacy pager
        self.pager.flush()?;

        // Flush all per-table pagers
        for pager in self.table_pagers.values_mut() {
            pager.flush()?;
        }

        Ok(ExecResult::Ok {
            message: "VACUUM completed".into(),
        })
    }

    /// O1: ANALYZE TABLE — scan table data and compute per-column statistics.
    pub(crate) fn exec_analyze_table(&mut self, table_name: String) -> Result<ExecResult> {
        use crate::schema::ColumnStats;
        use crate::types::Value;
        use std::collections::HashSet;

        let root_page = self.schema.get_table(&table_name)?.root_page;
        let col_count = self.schema.get_table(&table_name)?.columns.len();

        // Scan all rows
        let tbl_pager = self.get_table_pager_mut(&table_name);
        let mut btree = crate::storage::btree::BTree::new(tbl_pager);
        let all_rows = btree.scan_all(root_page)?;

        // Per-column accumulators
        let total_count = all_rows.len() as i64;
        let mut null_counts = vec![0i64; col_count];
        let mut mins: Vec<Option<Value>> = vec![None; col_count];
        let mut maxs: Vec<Option<Value>> = vec![None; col_count];
        let mut ndvs: Vec<HashSet<String>> = (0..col_count).map(|_| HashSet::new()).collect();

        for (_rowid, row) in &all_rows {
            for (ci, val) in row.iter().enumerate() {
                if ci >= col_count { break; }
                match val {
                    Value::Null => { null_counts[ci] += 1; }
                    v => {
                        // Track NDV via string repr (fast enough for ANALYZE)
                        ndvs[ci].insert(format!("{:?}", v));

                        // Min tracking
                        if mins[ci].is_none() {
                            mins[ci] = Some(v.clone());
                        } else if let Some(ref cur_min) = mins[ci].clone() {
                            if val_lt(v, cur_min) {
                                mins[ci] = Some(v.clone());
                            }
                        }

                        // Max tracking
                        if maxs[ci].is_none() {
                            maxs[ci] = Some(v.clone());
                        } else if let Some(ref cur_max) = maxs[ci].clone() {
                            if val_lt(cur_max, v) {
                                maxs[ci] = Some(v.clone());
                            }
                        }
                    }
                }
            }
        }

        // Write stats back to schema
        let tbl = self.schema.get_table_mut(&table_name)?;
        for ci in 0..col_count {
            if let Some(col) = tbl.columns.get_mut(ci) {
                col.stats = Some(ColumnStats {
                    total_count,
                    null_count: null_counts[ci],
                    ndv: ndvs[ci].len() as i64,
                    min: mins[ci].clone(),
                    max: maxs[ci].clone(),
                });
            }
        }

        Ok(ExecResult::Ok {
            message: format!(
                "ANALYZE TABLE {} complete: {} rows, {} columns",
                table_name, total_count, col_count
            ),
        })
    }

    /// L3: CREATE TRIGGER — persist trigger definition and register in memory schema
    pub(crate) fn exec_create_trigger(&mut self, trig: &CreateTriggerStmt) -> Result<ExecResult> {
        use crate::schema::TriggerSchema;
        let trig_name_lower = trig.name.to_lowercase();
        // Check if trigger already exists
        let exists = self.schema.triggers.values()
            .any(|v| v.iter().any(|t| t.name.to_lowercase() == trig_name_lower));
        if exists {
            if trig.or_replace {
                // drop existing first
                self.schema.drop_trigger_by_name(&mut self.pager, &trig.name, true)?;
            } else {
                return Err(crate::error::KkdbError::RuntimeError(
                    format!("trigger '{}' already exists", trig.name),
                ));
            }
        }
        // Verify the table exists
        let _ = self.schema.get_table(&trig.table_name)?;
        let schema_trigger = TriggerSchema {
            name: trig.name.clone(),
            timing: trig.timing.clone(),
            event: trig.event.clone(),
            table_name: trig.table_name.clone(),
            body_sql: trig.body_sql.clone(),
            rowid: 0, // will be assigned in save_trigger
        };
        self.schema.save_trigger(&mut self.pager, schema_trigger)?;
        Ok(ExecResult::Ok {
            message: format!("TRIGGER {} created", trig.name),
        })
    }

    /// L3: DROP TRIGGER [IF EXISTS] name
    pub(crate) fn exec_drop_trigger(&mut self, name: &str, if_exists: bool) -> Result<ExecResult> {
        self.schema.drop_trigger_by_name(&mut self.pager, name, if_exists)?;
        Ok(ExecResult::Ok {
            message: format!("TRIGGER {} dropped", name),
        })
    }

    // ---- USER MANAGEMENT ----
    pub(crate) fn exec_create_user(&mut self, stmt: &CreateUserStmt) -> Result<ExecResult> {
        let sql = format!(
            "INSERT INTO kkdb_users (username, password_hash) VALUES ('{}', '{}')",
            stmt.username,
            stmt.password.as_deref().unwrap_or("")
        );
        self.execute_sql(&sql)?;
        Ok(ExecResult::Ok {
            message: format!("User '{}' created", stmt.username),
        })
    }

    pub(crate) fn exec_alter_user(&mut self, stmt: &AlterUserStmt) -> Result<ExecResult> {
        if let Some(ref pw) = stmt.password {
            let sql = format!(
                "UPDATE kkdb_users SET password_hash = '{}' WHERE username = '{}'",
                pw, stmt.username
            );
            self.execute_sql(&sql)?;
        }
        Ok(ExecResult::Ok {
            message: format!("User '{}' altered", stmt.username),
        })
    }

    pub(crate) fn exec_drop_user(&mut self, stmt: &DropUserStmt) -> Result<ExecResult> {
        for username in &stmt.usernames {
            let sql = format!("DELETE FROM kkdb_users WHERE username = '{}'", username);
            let _ = self.execute_sql(&sql); // ignore if not exists
            let sql_privs = format!("DELETE FROM kkdb_privileges WHERE username = '{}'", username);
            let _ = self.execute_sql(&sql_privs); // cascade privs
        }
        Ok(ExecResult::Ok {
            message: format!("Dropped user(s)"),
        })
    }

    pub(crate) fn exec_grant(&mut self, stmt: &GrantStmt) -> Result<ExecResult> {
        let privs = match &stmt.privileges {
            PrivilegeList::All => vec!["ALL".to_string()],
            PrivilegeList::Specific(list) => list.clone(),
        };
        let obj_str = match &stmt.object {
            GrantObject::Table(t) => t.clone(),
            GrantObject::Database(d) => d.clone(),
            GrantObject::Global => "GLOBAL".to_string(),
        };
        
        for grantee in &stmt.grantees {
            for priv_type in &privs {
                let sql = format!(
                    "INSERT INTO kkdb_privileges (username, obj_name, priv_type) VALUES ('{}', '{}', '{}')",
                    grantee, obj_str, priv_type
                );
                self.execute_sql(&sql)?;
            }
        }
        Ok(ExecResult::Ok {
            message: "Privileges granted".into(),
        })
    }

    pub(crate) fn exec_revoke(&mut self, stmt: &RevokeStmt) -> Result<ExecResult> {
        let privs = match &stmt.privileges {
            PrivilegeList::All => vec!["ALL".to_string()],
            PrivilegeList::Specific(list) => list.clone(),
        };
        let obj_str = match &stmt.object {
            GrantObject::Table(t) => t.clone(),
            GrantObject::Database(d) => d.clone(),
            GrantObject::Global => "GLOBAL".to_string(),
        };

        for grantee in &stmt.grantees {
            for priv_type in &privs {
                let sql = format!(
                    "DELETE FROM kkdb_privileges WHERE username = '{}' AND obj_name = '{}' AND priv_type = '{}'",
                    grantee, obj_str, priv_type
                );
                self.execute_sql(&sql)?;
            }
        }
        Ok(ExecResult::Ok {
            message: "Privileges revoked".into(),
        })
    }

    // ---- RLS POLICIES ----
    pub(crate) fn exec_create_policy(&mut self, stmt: &CreatePolicyStmt) -> Result<ExecResult> {
        let tbl_key = stmt.table_name.to_ascii_lowercase();
        let tbl = self.schema.tables.get_mut(&tbl_key)
            .ok_or_else(|| crate::error::KkdbError::RuntimeError(
                format!("table '{}' not found", stmt.table_name)
            ))?;
        
        // Remove existing policy with same name if any
        tbl.policies.retain(|p| p.name != stmt.name);
        tbl.policies.push(crate::schema::PolicySchema {
            name: stmt.name.clone(),
            role: stmt.role.clone(),
            using_expr: stmt.using_expr.clone(),
            check_expr: stmt.check_expr.clone(),
        });
        
        Ok(ExecResult::Ok {
            message: format!("POLICY '{}' on '{}' created", stmt.name, stmt.table_name),
        })
    }

    pub(crate) fn exec_drop_policy(&mut self, stmt: &DropPolicyStmt) -> Result<ExecResult> {
        let tbl_key = stmt.table_name.to_ascii_lowercase();
        let tbl = self.schema.tables.get_mut(&tbl_key)
            .ok_or_else(|| crate::error::KkdbError::RuntimeError(
                format!("table '{}' not found", stmt.table_name)
            ))?;
        
        let before = tbl.policies.len();
        tbl.policies.retain(|p| p.name != stmt.name);
        if tbl.policies.len() == before && !stmt.if_exists {
            return Err(crate::error::KkdbError::RuntimeError(
                format!("policy '{}' not found on '{}'", stmt.name, stmt.table_name)
            ));
        }
        
        Ok(ExecResult::Ok {
            message: format!("POLICY '{}' on '{}' dropped", stmt.name, stmt.table_name),
        })
    }
}

/// Helper for O1 statistics: value less-than comparison
fn val_lt(a: &crate::types::Value, b: &crate::types::Value) -> bool {
    use crate::types::Value;
    match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => x < y,
        (Value::Real(x),    Value::Real(y))    => x < y,
        (Value::Integer(x), Value::Real(y))    => (*x as f64) < *y,
        (Value::Real(x),    Value::Integer(y)) => *x < (*y as f64),
        (Value::Text(x),    Value::Text(y))    => x < y,
        _ => false,
    }
}
