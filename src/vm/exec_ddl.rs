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
            )?;
        } else {
            // Single-file / memory mode: catalog pager is also table pager.
            // We need two &mut borrows of self.pager — split via raw ptr.
            let p = &mut self.pager as *mut _;
            let p2: &mut crate::storage::pager::Pager = unsafe { &mut *p };
            self.schema.create_table(
                &mut self.pager,
                p2,
                &create.table_name,
                &create.columns,
                create.if_not_exists,
                original_sql,
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
        self.schema
            .drop_table(&mut self.pager, &drop.table_name, drop.if_exists)?;
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
        self.auto_flush();

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
            foreign_keys: Vec::new(),
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
}
