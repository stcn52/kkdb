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
                returning: None,
            };
            self.exec_insert(&insert_stmt)?;

            Ok(ExecResult::Ok {
                message: format!("Table '{}' created with {} row(s)", create.table_name, rows.len()),
            })
        })();

        if need_auto_txn {
            match result {
                Ok(r) => {
                    // B13-1 fix: if commit fails, rollback and return error (same as B12-6)
                    match self.pager.commit_transaction() {
                        Ok(()) => {
                            self.schema_snapshot = None;
                            Ok(r)
                        }
                        Err(e) => {
                            let _ = self.pager.rollback_transaction();
                            for tbl_pager in self.table_pagers.values_mut() {
                                if tbl_pager.in_transaction() {
                                    let _ = tbl_pager.rollback_transaction();
                                }
                            }
                            if let Some(snap) = self.schema_snapshot.take() {
                                self.schema = snap;
                            }
                            self.clear_index_caches();
                            Err(e)
                        }
                    }
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
                // M5 fix: persist the RLS flag to the catalog so it survives a restart
                let schema_root = self.pager.schema_root_page();
                let next_rowid = {
                    let mut bt = BTree::new(&mut self.pager);
                    bt.max_rowid(schema_root).unwrap_or(0) + 1
                };
                let rls_row = vec![
                    Value::Text("rls_enabled".into()),                   // type
                    Value::Text(tbl_key.clone().into()),                  // name
                    Value::Text(tbl_key.clone().into()),                  // tbl_name
                    Value::Integer(0),                                    // root_page (unused)
                    Value::Text("ALTER TABLE ... ENABLE ROW LEVEL SECURITY".into()), // sql
                ];
                let mut btree = BTree::new(&mut self.pager);
                let new_schema_root = btree.insert(schema_root, next_rowid, &rls_row)?
;
                if new_schema_root != schema_root {
                    self.pager.set_schema_root_page(new_schema_root)?;
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

    // ---- CREATE FULLTEXT INDEX (BM25) ----
    /// Phase 4: Register a full-text index in the schema and backfill existing rows.
    pub(crate) fn exec_create_fulltext_index(
        &mut self,
        stmt: &CreateFulltextIndexStmt,
    ) -> Result<ExecResult> {
        use crate::storage::btree::BTree;
        use std::collections::HashMap;

        // 1. Validate table exists
        let tbl = self.schema.get_table(&stmt.table_name)?.clone();

        // 2. Validate columns exist in table
        let col_indices: Vec<usize> = stmt.columns.iter().map(|col_name| {
            tbl.columns.iter()
                .find(|c| c.name.eq_ignore_ascii_case(col_name))
                .map(|c| c.col_index)
                .ok_or_else(|| crate::error::KkdbError::ColumnNotFound(
                    format!("{}.{}", stmt.table_name, col_name)
                ))
        }).collect::<Result<Vec<_>>>()?;

        // 3. Check IF NOT EXISTS
        let idx_lower = stmt.index_name.to_lowercase();
        if self.schema.indexes.contains_key(&idx_lower) {
            if stmt.if_not_exists {
                return Ok(ExecResult::Ok {
                    message: format!("FULLTEXT INDEX '{}' already exists", stmt.index_name),
                });
            }
            return Err(crate::error::KkdbError::Internal(format!(
                "FULLTEXT INDEX '{}' already exists", stmt.index_name
            )));
        }

        // 4. Allocate a real BTree root page for FTS postings storage.
        // This avoids execute_sql (schema.create_table unsafe double-borrow) entirely.
        let fts_root = {
            let mut btree = BTree::new(&mut self.pager);
            btree.create_table()?
        };

        // 5. Persist FTS index metadata to schema catalog.
        // Store fts_root (a small valid page number) as the root_page field.
        let original_sql = format!(
            "CREATE FULLTEXT INDEX {} ON {} ({})",
            stmt.index_name, stmt.table_name, stmt.columns.join(", ")
        );
        let schema_row: crate::types::Row = vec![
            crate::types::Value::Text("fulltext_index".into()),
            crate::types::Value::Text(stmt.index_name.clone().into()),
            crate::types::Value::Text(stmt.table_name.clone().into()),
            crate::types::Value::Integer(fts_root as i64),  // valid page number, no UB
            crate::types::Value::Text(original_sql.into()),
        ];
        let schema_root = self.pager.schema_root_page();
        {
            let mut btree = BTree::new(&mut self.pager);
            let max_id = btree.max_rowid(schema_root).unwrap_or(0);
            let new_root = btree.insert(schema_root, max_id + 1, &schema_row)?;
            if new_root != schema_root {
                self.pager.set_schema_root_page(new_root)?;
            }
        }

        // 6. Register in in-memory schema (root_page = fts_root, the real BTree page)
        self.schema.register_fts_index(
            &stmt.index_name,
            &stmt.table_name,
            stmt.columns.clone(),
            fts_root,  // root_page IS the FTS BTree root page
        );

        // 7. Backfill: scan existing rows and build inverted index directly via BTree
        let rows = {
            let pager = self.get_table_pager_mut(&stmt.table_name);
            let mut btree = BTree::new(pager);
            btree.scan_all(tbl.root_page)?
        };

        let mut total_docs: u64 = 0;
        let mut total_field_len: u64 = 0;
        let mut postings: HashMap<String, HashMap<u64, (u32, u32)>> = HashMap::new();
        let mut doc_freq: HashMap<String, u64> = HashMap::new();

        for (rowid, row) in &rows {
            total_docs += 1;
            let mut field_tokens: Vec<String> = Vec::new();
            for &ci in &col_indices {
                if let Some(crate::types::Value::Text(s)) = row.get(ci) {
                    let tok = crate::fulltext::tokenizer::simple_tokenize(s);
                    field_tokens.extend(tok);
                }
            }
            let field_len = field_tokens.len() as u32;
            total_field_len += field_len as u64;
            let mut tf_map: HashMap<String, u32> = HashMap::new();
            for token in field_tokens {
                *tf_map.entry(token).or_insert(0) += 1;
            }
            let rid = *rowid as u64;
            for (token, tf) in tf_map {
                *doc_freq.entry(token.clone()).or_insert(0) += 1;
                postings.entry(token).or_default().insert(rid, (tf, field_len));
            }
        }

        // Write postings directly to the FTS BTree root page (no execute_sql)
        self.write_fts_postings_raw(fts_root, &postings, &doc_freq, total_docs, total_field_len)?;

        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!(
                "FULLTEXT INDEX '{}' created on {}.({}) with {} tokens across {} rows",
                stmt.index_name, stmt.table_name, stmt.columns.join(", "),
                doc_freq.len(), total_docs
            ),
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
        use crate::storage::btree::BTree;
        let exists = self.schema.get_table(&create.name).is_ok();
        if exists {
            if create.or_replace {
                // B-NEW-1 fix: drop from catalog before replacing
                self.schema.drop_table(&mut self.pager, &create.name, true)?;
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

        // B-NEW-1 fix: persist view definition to catalog BTree so it survives restarts
        let view_sql = format!("CREATE VIEW {} AS (view query)", create.name);
        let schema_root = self.pager.schema_root_page();
        let schema_row = vec![
            crate::types::Value::Text("view".into()),
            crate::types::Value::Text(create.name.clone().into()),
            crate::types::Value::Text(create.name.clone().into()),
            crate::types::Value::Integer(0),
            crate::types::Value::Text(view_sql.into()),
        ];
        let mut btree = BTree::new(&mut self.pager);
        let max_id = btree.max_rowid(schema_root).unwrap_or(0);
        let new_root = btree.insert(schema_root, max_id + 1, &schema_row)?;
        if new_root != schema_root {
            self.pager.set_schema_root_page(new_root)?;
        }

        self.auto_flush()?;
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
        // S-NEW-1 fix: use parameterized insert to avoid SQL injection
        use crate::sql::ast::{InsertStmt, InsertSource, ConflictPolicy, Expr};
        let pw_hash = stmt.password.as_deref().unwrap_or("").to_string();
        let insert = InsertStmt {
            table_name: "kkdb_users".to_string(),
            columns: Some(vec!["username".to_string(), "password_hash".to_string()]),
            source: InsertSource::Values(vec![vec![
                Expr::StringLiteral(stmt.username.clone().into()),
                Expr::StringLiteral(pw_hash.into()),
            ]]),
            conflict: ConflictPolicy::Error,
            returning: None,
        };
        self.exec_insert(&insert)?;
        Ok(ExecResult::Ok {
            message: format!("User '{}' created", stmt.username),
        })
    }

    pub(crate) fn exec_alter_user(&mut self, stmt: &AlterUserStmt) -> Result<ExecResult> {
        if let Some(ref pw) = stmt.password {
            // S-NEW-1 fix: use parameterized update to avoid SQL injection
            use crate::sql::ast::{UpdateStmt, Expr};
            use crate::sql::ast::BinaryOperator;
            let update = UpdateStmt {
                table_name: "kkdb_users".to_string(),
                assignments: vec![
                    ("password_hash".to_string(), Expr::StringLiteral(pw.clone().into())),
                ],
                where_clause: Some(Expr::BinaryOp {
                    left: Box::new(Expr::ColumnRef { table: None, column: "username".to_string() }),
                    op: BinaryOperator::Equal,
                    right: Box::new(Expr::StringLiteral(stmt.username.clone().into())),
                }),
                returning: None,
            };
            self.exec_update(&update)?;
        }
        Ok(ExecResult::Ok {
            message: format!("User '{}' altered", stmt.username),
        })
    }

    pub(crate) fn exec_drop_user(&mut self, stmt: &DropUserStmt) -> Result<ExecResult> {
        // S-NEW-1 fix: use parameterized delete for each username
        use crate::sql::ast::{DeleteStmt, Expr, BinaryOperator};
        for username in &stmt.usernames {
            let where_clause = Expr::BinaryOp {
                left: Box::new(Expr::ColumnRef { table: None, column: "username".to_string() }),
                op: BinaryOperator::Equal,
                right: Box::new(Expr::StringLiteral(username.clone().into())),
            };
            let del_user = DeleteStmt {
                table_name: "kkdb_users".to_string(),
                where_clause: Some(where_clause.clone()),
                returning: None,
            };
            let _ = self.exec_delete(&del_user);
            let del_privs = DeleteStmt {
                table_name: "kkdb_privileges".to_string(),
                where_clause: Some(Expr::BinaryOp {
                    left: Box::new(Expr::ColumnRef { table: None, column: "username".to_string() }),
                    op: BinaryOperator::Equal,
                    right: Box::new(Expr::StringLiteral(username.clone().into())),
                }),
                returning: None,
            };
            let _ = self.exec_delete(&del_privs);
        }
        Ok(ExecResult::Ok {
            message: "Dropped user(s)".to_string(),
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

    /// Write FTS postings directly to the given BTree root page (no execute_sql, no schema table).
    /// Row format: (token TEXT, doc_id INTEGER, tf INTEGER, field_len INTEGER, meta_key TEXT)
    ///  - posting: meta_key = Null
    ///  - doc-freq: meta_key = "DF"
    ///  - global stats: meta_key = "GLOBAL"
    pub(crate) fn write_fts_postings_raw(
        &mut self,
        fts_root: u32,
        postings: &std::collections::HashMap<String, std::collections::HashMap<u64, (u32, u32)>>,
        doc_freq: &std::collections::HashMap<String, u64>,
        total_docs: u64,
        total_field_len: u64,
    ) -> Result<()> {
        use crate::types::Value;
        // Get next rowid from in-memory sequence (atomic, race-free even across pagers).
        // Re-seed from btree if our cached value would collide with existing rows
        // (e.g., after CREATE FULLTEXT INDEX wrote rows before any DML).
        let actual_max = {
            let mut btree = BTree::new(&mut self.pager);
            btree.max_rowid(fts_root).unwrap_or(0)
        };
        let mut next_rowid = {
            let seq = self.fts_rowid_sequences.entry(fts_root).or_insert(actual_max + 1);
            // If the cached value is stale (behind actual max), re-sync it.
            if *seq <= actual_max {
                *seq = actual_max + 1;
            }
            *seq
        };
        let mut current_root = fts_root;

        // Write posting entries
        for (token, row_map) in postings {
            for (&doc_id, &(tf, field_len)) in row_map {
                let row = vec![
                    Value::Text(token.as_str().into()),
                    Value::Integer(doc_id as i64),
                    Value::Integer(tf as i64),
                    Value::Integer(field_len as i64),
                    Value::Null,
                ];
                let mut btree = BTree::new(&mut self.pager);
                current_root = btree.insert(current_root, next_rowid, &row)?;
                next_rowid += 1;
            }
        }

        // Write DF entries
        for (token, &df) in doc_freq {
            let row = vec![
                Value::Text(token.as_str().into()),
                Value::Integer(df as i64),
                Value::Integer(0),
                Value::Integer(0),
                Value::Text("DF".into()),
            ];
            let mut btree = BTree::new(&mut self.pager);
            current_root = btree.insert(current_root, next_rowid, &row)?;
            next_rowid += 1;
        }

        // Write GLOBAL stats row (total_docs in col1, total_field_len in col2)
        let global_row = vec![
            Value::Text("_GLOBAL".into()),
            Value::Integer(total_docs as i64),
            Value::Integer(total_field_len as i64),
            Value::Integer(0),
            Value::Text("GLOBAL".into()),
        ];
        let mut btree = BTree::new(&mut self.pager);
        let final_root = btree.insert(current_root, next_rowid, &global_row)?;

        // If root changed (tree split), update schema
        if final_root != fts_root {
            // Update the root_page in IndexSchema for this FTS index
            for idx in self.schema.indexes.values_mut() {
                if idx.is_fts && idx.root_page == fts_root {
                    idx.root_page = final_root;
                    break;
                }
            }
            // Reset sequence so it will be reseeded on next open (avoids stale key)
            self.fts_rowid_sequences.remove(&fts_root);
            self.fts_rowid_sequences.insert(final_root, next_rowid + 1);
        } else {
            // Persist the incremented counter for this fts_root
            self.fts_rowid_sequences.insert(fts_root, next_rowid + 1);
        }
        Ok(())
    }

    /// Convenience wrapper that finds the fts_root from the schema for a given index_id.
    /// Here index_id IS the fts_root (stored in IndexSchema.root_page).
    pub(crate) fn write_fts_postings(
        &mut self,
        fts_root: u32,
        postings: &std::collections::HashMap<String, std::collections::HashMap<u64, (u32, u32)>>,
        doc_freq: &std::collections::HashMap<String, u64>,
        total_docs: u64,
        total_field_len: u64,
    ) -> Result<()> {
        self.write_fts_postings_raw(fts_root, postings, doc_freq, total_docs, total_field_len)
    }

    /// Drain the pending FTS inserts collected during the last statement execution.
    /// Called at the end of execute_sql to avoid reentrant execute_sql during DML.
    pub(crate) fn drain_pending_fts_inserts(&mut self) {
        if self.pending_fts_inserts.is_empty() { return; }
        let pending = std::mem::take(&mut self.pending_fts_inserts);

        for (stale_fts_root, doc_id, tfs, field_len) in pending {
            if stale_fts_root == 0 { continue; }

            // Re-read the CURRENT root_page from schema in case a previous write split
            // the B-Tree and updated the root (the queued value may now be stale).
            let fts_root = self.schema.indexes.values()
                .find(|idx| idx.is_fts && (idx.root_page == stale_fts_root || {
                    // After a split the old root may no longer match; fall back to
                    // locating the FTS index whose columns cover the same pages.
                    // For now we trust that root_page==stale means no split yet;
                    // if not found, keep using stale_fts_root (best effort).
                    false
                }))
                .map(|idx| idx.root_page)
                .unwrap_or(stale_fts_root);

            let (cur_docs, cur_field_len) = self.read_fts_global_stats(fts_root);
            let new_total_docs = cur_docs + 1;
            let new_total_field_len = cur_field_len + field_len as u64;

            let mut postings: std::collections::HashMap<String, std::collections::HashMap<u64, (u32, u32)>> =
                std::collections::HashMap::new();
            let mut doc_freq: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();

            for (token, tf) in tfs {
                postings.entry(token.clone())
                    .or_default()
                    .insert(doc_id as u64, (tf, field_len));
                let existing_df = self.get_fts_doc_freq(fts_root, &token);
                doc_freq.insert(token, existing_df + 1);
            }

            let _ = self.write_fts_postings_raw(
                fts_root, &postings, &doc_freq, new_total_docs, new_total_field_len
            );
        }
    }

    /// Read global FTS stats (total_docs, total_field_len) directly from BTree.
    /// fts_root is the IndexSchema.root_page for the FTS index.
    /// Returns the stats from the LAST GLOBAL row found (most recent).
    pub(crate) fn read_fts_global_stats(&mut self, fts_root: u32) -> (u64, u64) {
        use crate::types::Value;
        if fts_root == 0 { return (0, 0); }
        let mut btree = BTree::new(&mut self.pager);
        let rows = btree.scan_rows(fts_root).unwrap_or_default();
        let mut result = (0u64, 0u64);
        for row in &rows {
            if row.get(4) == Some(&Value::Text("GLOBAL".into())) {
                let total_docs = if let Some(Value::Integer(v)) = row.get(1) { *v as u64 } else { 0 };
                let total_field_len = if let Some(Value::Integer(v)) = row.get(2) { *v as u64 } else { 0 };
                result = (total_docs, total_field_len);  // take latest (last) GLOBAL row
            }
        }
        result
    }

    /// Scan all posting entries for a given token in the FTS index (direct BTree).
    pub(crate) fn scan_fts_postings(&mut self, fts_root: u32, token: &str) -> Vec<(u64, u32, u32)> {
        use crate::types::Value;
        if fts_root == 0 { return Vec::new(); }
        let mut btree = BTree::new(&mut self.pager);
        let rows = btree.scan_rows(fts_root).unwrap_or_default();
        rows.into_iter().filter_map(|row| {
            if row.get(4) != Some(&Value::Null) { return None; }
            let row_token = if let Some(Value::Text(s)) = row.get(0) { s.to_string() } else { return None; };
            if row_token != token { return None; }
            let doc_id = if let Some(Value::Integer(v)) = row.get(1) { *v as u64 } else { return None; };
            let tf = if let Some(Value::Integer(v)) = row.get(2) { *v as u32 } else { 0 };
            let fl = if let Some(Value::Integer(v)) = row.get(3) { *v as u32 } else { 0 };
            Some((doc_id, tf, fl))
        }).collect()
    }

    /// Get doc_freq for a token directly from BTree.
    pub(crate) fn get_fts_doc_freq(&mut self, fts_root: u32, token: &str) -> u64 {
        use crate::types::Value;
        if fts_root == 0 { return 0; }
        let mut btree = BTree::new(&mut self.pager);
        let rows = btree.scan_rows(fts_root).unwrap_or_default();
        for row in &rows {
            if row.get(4) == Some(&Value::Text("DF".into())) {
                if let Some(Value::Text(t)) = row.get(0) {
                    if t.as_ref() == token {
                        return if let Some(Value::Integer(v)) = row.get(1) { *v as u64 } else { 0 };
                    }
                }
            }
        }
        0
    }

}

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
