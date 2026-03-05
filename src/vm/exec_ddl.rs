use super::execute::{ExecResult, VM};
use crate::error::{KkdbError, Result};
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
        self.schema.create_table(
            &mut self.pager,
            &create.table_name,
            &create.columns,
            create.if_not_exists,
            original_sql,
        )?;
        self.clear_index_caches();
        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!("Table '{}' created", create.table_name),
        })
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

        self.schema.create_index(
            &mut self.pager,
            &create_idx.index_name,
            &create_idx.table_name,
            &create_idx.columns,
            create_idx.unique,
            create_idx.if_not_exists,
            &sql,
        )?;
        self.clear_index_caches();
        self.auto_flush()?;

        Ok(ExecResult::Ok {
            message: format!("Index created: {}", create_idx.index_name),
        })
    }

    /// Update the root page number in the schema table for a table or index object.
    pub(crate) fn update_schema_object_root_page(
        &mut self,
        object_name: &str,
        new_root: u32,
    ) -> Result<()> {
        let mut btree = BTree::new(&mut self.pager);
        let schema_rows = btree.scan_all(1)?;

        for (rowid, row) in schema_rows {
            if row.len() >= 5 {
                if let Value::Text(ref name) = row[1] {
                    if name.eq_ignore_ascii_case(object_name) {
                        let mut new_row = row.clone();
                        new_row[3] = Value::Integer(new_root as i64);
                        let mut btree = BTree::new(&mut self.pager);
                        let new_schema_root = btree.update_row(1, rowid, &new_row)?;
                        if new_schema_root != 1 {
                            return Err(KkdbError::Internal(
                                "schema table overflow during root page update".into(),
                            ));
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

    pub(crate) fn from_name(&self, from: &FromClause) -> String {
        match from {
            FromClause::Table { name, .. } => name.clone(),
            FromClause::Join { left, right, .. } => {
                format!("{} JOIN {}", self.from_name(left), self.from_name(right))
            }
            FromClause::Subquery { alias, .. } => format!("(subquery) AS {}", alias),
        }
    }
}
