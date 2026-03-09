//! DML statement execution for KKDB.
//!
//! This module implements `INSERT`, `UPDATE`, and `DELETE` operations, including:
//!
//! - **INSERT** — column alignment, rowid assignment, `AUTOINCREMENT`,
//!   `ON CONFLICT DO NOTHING / REPLACE / UPDATE SET`, `RETURNING` clause,
//!   trigger firing (BEFORE / AFTER), constraint checking (NOT NULL, CHECK, FK,
//!   UNIQUE), FTS index maintenance, and binlog appending.
//! - **UPDATE** — index-accelerated lookup for simple `WHERE col = value` predicates,
//!   full-scan fallback, constraint re-validation, FTS and secondary index delta,
//!   MVCC undo entry recording, FK cascade (ON UPDATE), and `RETURNING`.
//! - **DELETE** — index-accelerated or full-scan row collection, FK restrict check
//!   (ON DELETE RESTRICT), FTS and secondary index cleanup, and `RETURNING`.
//!
//! ## Constraint checking order (INSERT)
//!
//! ```text
//! BEFORE trigger → NOT NULL → FK child-side → CHECK → UNIQUE → insert row
//! → update FTS / secondary indexes → add to binlog → add undo entry → AFTER trigger
//! ```
//!
//! ## MVCC / rollback
//!
//! Within an explicit `BEGIN` transaction each DML operation appends an entry to
//! [`VM::mvcc_undo_log`].  On `ROLLBACK` the COW pager physically reverts pages;
//! the undo log is preserved for potential non-COW future backends.

use super::execute::{ExecResult, VM};
use crate::error::{KkdbError, Result};
use crate::sql::ast::*;
use crate::storage::btree::BTree;
use crate::types::{Row, Value};
use std::collections::{HashMap, HashSet};

impl VM {
    // ---- INSERT ----

    /// Fast programmatic bulk insert bypassing SQL parsing/AST
    pub fn insert_batch_raw(
        &mut self,
        table_name: &str,
        value_rows: Vec<Vec<Value>>,
    ) -> Result<ExecResult> {
        let insert = InsertStmt {
            table_name: table_name.to_string(),
            columns: None,
            source: InsertSource::Values(vec![]),
            conflict: ConflictPolicy::Error,
            returning: None,
        };

        let need_auto_txn = !self.pager.in_transaction();
        if need_auto_txn {
            self.pager.begin_transaction()?;
            self.schema_snapshot = Some(self.schema.clone());
        }

        let result = self.insert_value_rows(&insert, value_rows);

        if need_auto_txn {
            match result {
                Ok(r) => {
                    // B12-6 fix: if commit fails, rollback and return error
                    match self.pager.commit_transaction() {
                        Ok(()) => {
                            self.schema_snapshot = None;
                            return Ok(r);
                        }
                        Err(e) => {
                            let _ = self.pager.rollback_transaction();
                            if let Some(snap) = self.schema_snapshot.take() {
                                self.schema = snap;
                            }
                            self.clear_index_caches();
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    let _ = self.pager.rollback_transaction();
                    if let Some(snap) = self.schema_snapshot.take() {
                        self.schema = snap;
                    }
                    self.clear_index_caches();
                    return Err(e);
                }
            }
        }
        result
    }

    #[inline]
    pub(crate) fn exec_insert(&mut self, insert: &InsertStmt) -> Result<ExecResult> {
        // Resolve the rows to insert. For InsertSource::Select we materialize
        // all result rows first to avoid borrow-checker conflicts with &mut self.
        let value_rows: Vec<Vec<Value>> = match &insert.source {
            InsertSource::Values(expr_rows) => {
                let empty_row: Vec<Value> = Vec::new();
                let empty_col_map: HashMap<String, usize> = HashMap::new();
                let mut out = Vec::with_capacity(expr_rows.len());
                for expr_row in expr_rows {
                    let mut row = Vec::with_capacity(expr_row.len());
                    for expr in expr_row {
                        row.push(self.eval_expr(expr, &empty_row, &empty_col_map)?);
                    }
                    out.push(row);
                }
                out
            }
            InsertSource::Select(query) => {
                // Clone the query to release borrow on `insert` before calling exec_select.
                let query = query.as_ref().clone();
                match self.exec_select(&query)? {
                    ExecResult::QueryResult { rows, .. } => rows,
                    _ => {
                        return Err(crate::error::KkdbError::Internal(
                            "INSERT SELECT: exec_select did not return rows".into(),
                        ))
                    }
                }
            }
        };

        // Wrap in implicit transaction if not already in one (atomicity guarantee).
        let need_auto_txn = !self.pager.in_transaction();
        if need_auto_txn {
            self.pager.begin_transaction()?;
            self.schema_snapshot = Some(self.schema.clone());
        }

        let result = self.insert_value_rows(insert, value_rows);

        if need_auto_txn {
            match result {
                Ok(r) => {
                    // B12-6 fix: if commit fails, rollback and return error
                    match self.pager.commit_transaction() {
                        Ok(()) => {
                            self.schema_snapshot = None;
                            return Ok(r);
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
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    let _ = self.pager.rollback_transaction();
                    if let Some(snap) = self.schema_snapshot.take() {
                        self.schema = snap;
                    }
                    self.clear_index_caches();
                    return Err(e);
                }
            }
        }
        result
    }

    /// Inner loop: inserts already-evaluated rows into the target table.
    fn insert_value_rows(
        &mut self,
        insert: &InsertStmt,
        value_rows: Vec<Vec<Value>>,
    ) -> Result<ExecResult> {
        let (col_count, col_indices, pk_col_idx, not_null_cols, table_name_owned, original_root);
        let mut root_page;
        let mut next_rowid;
        {
            let table = self.schema.get_table(&insert.table_name)?;
            col_count = table.columns.len();
            table_name_owned = table.name.clone();
            root_page = table.root_page;
            original_root = table.root_page;
            next_rowid = table.next_rowid;

            col_indices = if let Some(ref cols) = insert.columns {
                cols.iter()
                    .map(|c| {
                        table
                            .columns
                            .iter()
                            .position(|tc| tc.name.eq_ignore_ascii_case(c))
                            .ok_or_else(|| KkdbError::ColumnNotFound(c.clone()))
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                (0..col_count).collect()
            };

            pk_col_idx = table
                .columns
                .iter()
                .find(|c| c.primary_key)
                .map(|c| c.col_index);

            not_null_cols = table
                .columns
                .iter()
                .filter(|c| c.not_null && !c.primary_key)
                .map(|c| (c.col_index, c.name.clone()))
                .collect::<Vec<_>>();
        }

        let mut rows_inserted = 0;
        // Tracks rows that were successfully inserted, used for RETURNING evaluation
        let mut inserted_rows_for_returning: Vec<Row> = Vec::new();
        let mut serialize_buf: Vec<u8> = Vec::new();
        let mut row = vec![Value::Null; col_count];

        for value_row in &value_rows {
            if value_row.len() != col_indices.len() {
                return Err(KkdbError::ColumnCountMismatch {
                    expected: col_indices.len(),
                    got: value_row.len(),
                });
            }

            for v in row.iter_mut() {
                *v = Value::Null;
            }
            for (val_idx, &col_idx) in col_indices.iter().enumerate() {
                row[col_idx] = value_row[val_idx].clone();
            }

            let rowid = if let Some(pk_idx) = pk_col_idx {
                match &row[pk_idx] {
                    Value::Integer(v) => {
                        let rid = *v;
                        if rid >= next_rowid {
                            next_rowid = rid + 1;
                        }
                        rid
                    }
                    Value::Null => {
                        let rid = next_rowid;
                        row[pk_idx] = Value::Integer(rid);
                        next_rowid = rid + 1;
                        rid
                    }
                    _ => {
                        let rid = next_rowid;
                        next_rowid += 1;
                        rid
                    }
                }
            } else {
                let rid = next_rowid;
                next_rowid += 1;
                rid
            };

            // --- L3: BEFORE INSERT triggers ---
            self.fire_triggers(
                &insert.table_name,
                &crate::sql::ast::TriggerTiming::Before,
                &crate::sql::ast::TriggerEvent::Insert,
            )?;

            for (col_idx, col_name) in &not_null_cols {
                if matches!(row[*col_idx], Value::Null) {
                    return Err(KkdbError::ConstraintViolation(format!(
                        "NOT NULL constraint failed: {}.{}",
                        table_name_owned, col_name
                    )));
                }
            }

            // --- L1: Foreign Key constraint check (RESTRICT on insert) ---
            self.check_fk_on_insert(&insert.table_name, &row)?;

            // --- L2: CHECK constraint validation ---
            self.check_constraints_for_row(&insert.table_name, &row, &[])?;

            // --- Conflict resolution ---
            match &insert.conflict {
                ConflictPolicy::Error => {
                    self.validate_unique_indexes_for_row(&insert.table_name, rowid, &row, None)?;
                    let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                    let mut btree = BTree::new(tbl_pager);
                    let new_root =
                        btree.insert_with_buf(root_page, rowid, &row, &mut serialize_buf)?;
                    root_page = new_root;
                    self.insert_index_entries(&insert.table_name, rowid, &row)?;
                    // L4: Maintain FTS + vector indexes
                    self.maintain_fts_insert(&insert.table_name, rowid, &row)?;
                    self.maintain_vec_insert(&insert.table_name, rowid, &row)?;

                    // Add to binlog
                    let txid = self.pager.active_txid().unwrap_or(0);
                    let _ = self.binlog.append(&crate::binlog::LogRecord::Insert {
                        txid,
                        table_name: insert.table_name.clone(),
                        rowid,
                        row: row.clone(),
                    });

                    // C1: Record undo entry for ROLLBACK
                    if self.pager.in_transaction() && self.current_txn_id != 0 {
                        self.mvcc_undo_log.push(crate::vm::mvcc::UndoEntry::Insert {
                            table: insert.table_name.clone(),
                            rowid,
                        });
                    }
                    // --- L3: AFTER INSERT triggers ---
                    self.fire_triggers(
                        &insert.table_name,
                        &crate::sql::ast::TriggerTiming::After,
                        &crate::sql::ast::TriggerEvent::Insert,
                    )?;
                    rows_inserted += 1;
                    inserted_rows_for_returning.push(row.clone());
                }
                ConflictPolicy::Ignore => {
                    // Check for conflicts; skip row on any constraint violation
                    let conflict = self
                        .validate_unique_indexes_for_row(&insert.table_name, rowid, &row, None)
                        .err()
                        .filter(|e| matches!(e, KkdbError::ConstraintViolation(_)));
                    // Also check if rowid already exists (PK conflict)
                    let pk_exists = if pk_col_idx.is_some() {
                        let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                        let mut btree = BTree::new(tbl_pager);
                        btree.find_by_rowid(root_page, rowid)?.is_some()
                    } else {
                        false
                    };
                    if conflict.is_none() && !pk_exists {
                        let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                        let mut btree = BTree::new(tbl_pager);
                        let new_root =
                            btree.insert_with_buf(root_page, rowid, &row, &mut serialize_buf)?;
                        root_page = new_root;
                        self.insert_index_entries(&insert.table_name, rowid, &row)?;

                        // Add to binlog
                        let txid = self.pager.active_txid().unwrap_or(0);
                        let _ = self.binlog.append(&crate::binlog::LogRecord::Insert {
                            txid,
                            table_name: insert.table_name.clone(),
                            rowid,
                            row: row.clone(),
                        });

                        // --- L3: AFTER INSERT triggers ---
                        self.fire_triggers(
                            &insert.table_name,
                            &crate::sql::ast::TriggerTiming::After,
                            &crate::sql::ast::TriggerEvent::Insert,
                        )?;
                        rows_inserted += 1;
                    }
                    // else: silently skip this row
                }
                ConflictPolicy::Replace => {
                    // Delete existing row with the same rowid (PK) if it exists
                    let pk_exists = if pk_col_idx.is_some() {
                        let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                        let mut btree = BTree::new(tbl_pager);
                        btree.find_by_rowid(root_page, rowid)?.is_some()
                    } else {
                        false
                    };
                    if pk_exists {
                        // B-NEW-5 fix: fetch old row before deletion so we can record undo entry
                        let old_row_for_undo = {
                            let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                            let mut btree = BTree::new(tbl_pager);
                            btree.find_by_rowid(root_page, rowid)?.map(|(_, r)| r)
                        };
                        self.maintain_fts_delete(&insert.table_name, rowid)?;
                        self.delete_index_entries(&insert.table_name, rowid)?;
                        let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                        let mut btree = BTree::new(tbl_pager);
                        let (_, new_root) = btree.delete_by_rowid(root_page, rowid)?;
                        root_page = new_root;
                        // Record undo entry for this deletion
                        if self.pager.in_transaction() && self.current_txn_id != 0 {
                            if let Some(old_row) = old_row_for_undo {
                                self.mvcc_undo_log.push(crate::vm::mvcc::UndoEntry::Delete {
                                    table: insert.table_name.clone(),
                                    rowid,
                                    old_row,
                                });
                            }
                        }
                    }
                    // Also remove any rows that would conflict on UNIQUE indexes
                    let conflict_rowids: Vec<i64> = {
                        // Clone indexes to release the schema borrow before calling get_table_pager_mut.
                        let indexes: Vec<_> = self
                            .schema
                            .indexes_for_table(&insert.table_name)
                            .into_iter()
                            .cloned()
                            .collect();
                        let mut cids: Vec<i64> = Vec::new();
                        for idx in indexes {
                            if !idx.unique {
                                continue;
                            }
                            // Pre-compute key_vals from schema (drops schema borrow) before touching pager.
                            let key_vals: Vec<Value> = {
                                let tbl_schema = self.schema.get_table(&insert.table_name)?;
                                idx.columns
                                    .iter()
                                    .filter_map(|c| {
                                        tbl_schema
                                            .columns
                                            .iter()
                                            .find(|col| col.name.eq_ignore_ascii_case(c))
                                            .map(|col| row[col.col_index].clone())
                                    })
                                    .collect()
                            };
                            // Skip if any key part is NULL (unique index doesn't cover NULLs)
                            if key_vals.iter().any(|v| matches!(v, Value::Null)) {
                                continue;
                            }
                            let key = crate::schema::Schema::index_key(&key_vals);
                            // Scan index BTree via table pager (schema borrow dropped above).
                            let idx_rows = {
                                let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                                let mut btree = BTree::new(tbl_pager);
                                btree.scan_all(idx.root_page)?
                            };
                            for (_, idx_row) in idx_rows {
                                if let Some(Value::Integer(tbl_rowid)) = idx_row.last() {
                                    if *tbl_rowid != rowid {
                                        let entry_key = crate::schema::Schema::index_key(
                                            &idx_row[..idx_row.len() - 1],
                                        );
                                        if entry_key == key {
                                            cids.push(*tbl_rowid);
                                        }
                                    }
                                }
                            }
                        }
                        cids
                    };
                    for conflict_rid in conflict_rowids {
                        self.maintain_fts_delete(&insert.table_name, conflict_rid)?;
                        self.delete_index_entries(&insert.table_name, conflict_rid)?;
                        let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                        let mut btree = BTree::new(tbl_pager);
                        let (_, new_root) = btree.delete_by_rowid(root_page, conflict_rid)?;
                        root_page = new_root;
                    }
                    let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                    let mut btree = BTree::new(tbl_pager);
                    let new_root =
                        btree.insert_with_buf(root_page, rowid, &row, &mut serialize_buf)?;
                    root_page = new_root;
                    self.insert_index_entries(&insert.table_name, rowid, &row)?;
                    // L4: Maintain FTS + vector indexes
                    self.maintain_fts_insert(&insert.table_name, rowid, &row)?;
                    self.maintain_vec_insert(&insert.table_name, rowid, &row)?;

                    // Add to binlog
                    let txid = self.pager.active_txid().unwrap_or(0);
                    let _ = self.binlog.append(&crate::binlog::LogRecord::Insert {
                        txid,
                        table_name: insert.table_name.clone(),
                        rowid,
                        row: row.clone(),
                    });

                    // --- L3: AFTER INSERT triggers ---
                    self.fire_triggers(
                        &insert.table_name,
                        &crate::sql::ast::TriggerTiming::After,
                        &crate::sql::ast::TriggerEvent::Insert,
                    )?;
                    rows_inserted += 1;
                }
                // Batch G: ON CONFLICT DO UPDATE SET col = expr ...
                ConflictPolicy::Update(assignments) => {
                    // Find conflicting row by PK using scan_all which returns (rowid, row) pairs
                    let pk_idx = pk_col_idx.unwrap_or(0);
                    let pk_val = row.get(pk_idx).cloned().unwrap_or(Value::Null);
                    let existing = {
                        let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                        let mut btree = BTree::new(tbl_pager);
                        btree.scan_all(root_page)?
                    };
                    let existing_rowid = existing.iter().find_map(|(id, r)| {
                        if r.get(pk_idx) == Some(&pk_val) {
                            Some(*id)
                        } else {
                            None
                        }
                    });
                    if let Some(conflict_rowid) = existing_rowid {
                        // B-NEW-2 fix: fetch old row in a block so borrow ends before &mut self calls
                        let old_row = {
                            let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                            let mut bt = BTree::new(tbl_pager);
                            bt.find_by_rowid(root_page, conflict_rowid)?
                                .map(|(_, r)| r)
                                .unwrap_or_default()
                        };
                        // Build updated row by applying assignments
                        let col_map_tmp: std::collections::HashMap<String, usize> = {
                            let table_schema = self.schema.get_table(&insert.table_name)?;
                            table_schema
                                .col_names
                                .iter()
                                .enumerate()
                                .map(|(i, n)| (n.to_ascii_lowercase(), i))
                                .collect()
                        };
                        let mut new_row = old_row.clone();
                        for (col_name, expr) in assignments.iter() {
                            if let Ok(idx) = self.schema.find_column(&insert.table_name, col_name) {
                                let new_val = self.eval_expr(expr, &old_row, &col_map_tmp)?;
                                if idx < new_row.len() {
                                    new_row[idx] = new_val;
                                }
                            }
                        }

                        // B-NEW-2 fix #1: CHECK constraint validation
                        self.check_constraints_for_row(&insert.table_name, &new_row, &[])?;
                        // B-NEW-2 fix #2: FK child-side constraint check
                        self.check_fk_on_insert(&insert.table_name, &new_row)?;
                        // B-NEW-2 fix #3 & #4: Remove old FTS/index entries
                        self.maintain_fts_delete(&insert.table_name, conflict_rowid)?;
                        self.delete_index_entries(&insert.table_name, conflict_rowid)?;
                        // B-NEW-2 fix #5: UNIQUE index re-validation (exclude current row)
                        self.validate_unique_indexes_for_row(
                            &insert.table_name,
                            conflict_rowid,
                            &new_row,
                            Some(conflict_rowid),
                        )?;

                        // Perform the actual row update
                        let new_root = {
                            let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                            let mut btree = BTree::new(tbl_pager);
                            btree.update_row(root_page, conflict_rowid, &new_row)?
                        };
                        root_page = new_root;

                        // B-NEW-2 fix #6, #7: Insert new FTS/index + vec entries
                        self.insert_index_entries(&insert.table_name, conflict_rowid, &new_row)?;
                        self.maintain_fts_insert(&insert.table_name, conflict_rowid, &new_row)?;
                        self.maintain_vec_delete(&insert.table_name, conflict_rowid);
                        self.maintain_vec_insert(&insert.table_name, conflict_rowid, &new_row)?;

                        // B-NEW-2 fix #8: Write Update to binlog
                        let txid = self.pager.active_txid().unwrap_or(0);
                        let _ = self.binlog.append(&crate::binlog::LogRecord::Update {
                            txid,
                            table_name: insert.table_name.clone(),
                            rowid: conflict_rowid,
                            old_row: old_row.clone(),
                            new_row: new_row.clone(),
                        });
                        // B-NEW-2 fix #9: MVCC undo log for rollback
                        if self.pager.in_transaction() && self.current_txn_id != 0 {
                            self.mvcc_undo_log.push(crate::vm::mvcc::UndoEntry::Update {
                                table: insert.table_name.clone(),
                                rowid: conflict_rowid,
                                old_row,
                            });
                        }
                        // B-NEW-2 fix #10: count the upsert
                        rows_inserted += 1;
                    } else {
                        // No conflict: plain insert
                        let new_root = {
                            let tbl_pager = self.get_table_pager_mut(&table_name_owned);
                            let mut btree = BTree::new(tbl_pager);
                            btree.insert_with_buf(root_page, rowid, &row, &mut serialize_buf)?
                        };
                        root_page = new_root;
                        let _ = self.insert_index_entries(&insert.table_name, rowid, &row);
                        // L4: Maintain FTS + vector indexes
                        self.maintain_fts_insert(&insert.table_name, rowid, &row)?;
                        self.maintain_vec_insert(&insert.table_name, rowid, &row)?;
                        // --- L3: AFTER INSERT triggers ---
                        self.fire_triggers(
                            &insert.table_name,
                            &crate::sql::ast::TriggerTiming::After,
                            &crate::sql::ast::TriggerEvent::Insert,
                        )?;
                        rows_inserted += 1;
                    }
                }
            }
        }

        {
            let table_schema = self.schema.get_table_mut(&insert.table_name)?;
            table_schema.root_page = root_page;
            table_schema.next_rowid = next_rowid;
        }

        if root_page != original_root {
            self.update_schema_root_page(&insert.table_name, root_page)?;
        }

        self.auto_flush()?;

        // --- RETURNING clause ---
        if let Some(ref returning_exprs) = insert.returning {
            let col_map = self.build_table_col_map(&insert.table_name)?;
            // We need to re-read inserted rows; for simplicity, use the captured rows_inserted_data
            let mut result_rows: Vec<Row> = Vec::new();
            for inserted_row in &inserted_rows_for_returning {
                let mut result_row = Vec::new();
                for expr in returning_exprs {
                    if let Expr::ColumnRef { table: _, column } = expr {
                        if column == "*" {
                            result_row.extend_from_slice(inserted_row);
                            continue;
                        }
                    }
                    let v = self.eval_expr(expr, inserted_row, &col_map)?;
                    result_row.push(v);
                }
                result_rows.push(result_row);
            }
            let col_names: Vec<String> = returning_exprs
                .iter()
                .enumerate()
                .map(|(i, e)| match e {
                    Expr::ColumnRef { table: _, column } => column.clone(),
                    _ => format!("col{}", i),
                })
                .collect();
            return Ok(ExecResult::QueryResult {
                columns: col_names,
                rows: result_rows,
            });
        }

        Ok(ExecResult::RowsAffected {
            count: rows_inserted,
            message: format!("{} row(s) inserted", rows_inserted),
        })
    }

    // ---- UPDATE ----
    pub(crate) fn exec_update(&mut self, update: &UpdateStmt) -> Result<ExecResult> {
        let (col_map, original_root) = {
            let table = self.schema.get_table(&update.table_name)?;
            let mut cm: HashMap<String, usize> = HashMap::with_capacity(table.columns.len());
            for (i, c) in table.columns.iter().enumerate() {
                if c.name.bytes().any(|b| b.is_ascii_uppercase()) {
                    let mut lower = String::with_capacity(c.name.len());
                    for b in c.name.bytes() {
                        lower.push(b.to_ascii_lowercase() as char);
                    }
                    cm.insert(lower, i);
                } else {
                    cm.insert(c.name.clone(), i);
                }
            }
            (cm, table.root_page)
        };

        // Pre-resolve assignment column indices (avoids per-row to_lowercase allocation)
        let assignment_indices: Vec<(usize, &Expr)> = update
            .assignments
            .iter()
            .map(|(col_name, expr)| {
                let lower = col_name.to_ascii_lowercase();
                let idx = *col_map
                    .get(lower.as_str())
                    .ok_or_else(|| KkdbError::ColumnNotFound(col_name.clone()))?;
                Ok((idx, expr))
            })
            .collect::<Result<Vec<_>>>()?;

        // Try index-accelerated path for simple WHERE col = value
        let index_rowids = if let Some(ref where_expr) = update.where_clause {
            self.try_dml_index_rowids(&update.table_name, where_expr)?
        } else {
            None
        };

        let mut rows_to_update: Vec<(i64, Row, Row)> = Vec::new();

        if let Some(rowids) = index_rowids {
            // Index path: fetch only matching rows by rowid (bulk-capable helper).
            let fetched_rows =
                self.fetch_rows_by_rowids(&update.table_name, original_root, &rowids)?;
            for (rid, row) in fetched_rows {
                let mut new_row = row.clone();
                for &(col_idx, expr) in &assignment_indices {
                    let val = self.eval_expr(expr, &row, &col_map)?;
                    new_row[col_idx] = val;
                }
                rows_to_update.push((rid, row, new_row));
            }
        } else {
            // Full scan path
            let tbl_pager = self.get_table_pager_mut(&update.table_name);
            let mut btree = BTree::new(tbl_pager);
            let all_rows = btree.scan_all(original_root)?;

            for (rowid, row) in all_rows {
                let should_update = if let Some(ref where_expr) = update.where_clause {
                    self.eval_expr(where_expr, &row, &col_map)?.is_truthy()
                } else {
                    true
                };

                if should_update {
                    let mut new_row = row.clone();
                    for &(col_idx, expr) in &assignment_indices {
                        let val = self.eval_expr(expr, &row, &col_map)?;
                        new_row[col_idx] = val;
                    }
                    rows_to_update.push((rowid, row, new_row));
                }
            }
        }

        let count = rows_to_update.len();
        let mut root_page = original_root;
        let mut serialize_buf: Vec<u8> = Vec::new();
        // I1: collect updated rows when a RETURNING clause is present
        let mut returning_rows: Vec<crate::types::Row> = if update.returning.is_some() {
            Vec::with_capacity(count)
        } else {
            Vec::new()
        };

        for (rowid, old_row_pre, new_row) in rows_to_update {
            // --- L3: BEFORE UPDATE triggers ---
            self.fire_triggers(
                &update.table_name,
                &crate::sql::ast::TriggerTiming::Before,
                &crate::sql::ast::TriggerEvent::Update,
            )?;

            // --- L2: CHECK constraint validation ---
            self.check_constraints_for_row(&update.table_name, &new_row, &[])?;

            // Update indexes: delete old entry, insert new entry
            self.maintain_fts_delete(&update.table_name, rowid)?;
            self.maintain_vec_delete(&update.table_name, rowid);
            self.delete_index_entries(&update.table_name, rowid)?;
            self.validate_unique_indexes_for_row(&update.table_name, rowid, &new_row, Some(rowid))?;

            let tbl_pager = self.get_table_pager_mut(&update.table_name);
            let mut btree = BTree::new(tbl_pager);
            let new_root =
                btree.update_row_with_buf(root_page, rowid, &new_row, &mut serialize_buf)?;
            root_page = new_root;

            self.insert_index_entries(&update.table_name, rowid, &new_row)?;
            // L4 + Vec: Maintain FTS and vector indexes
            self.maintain_fts_insert(&update.table_name, rowid, &new_row)?;
            self.maintain_vec_insert(&update.table_name, rowid, &new_row)?;

            // Log Update to Binlog (use old_row_pre captured before the row was modified)
            let txid = self.pager.active_txid().unwrap_or(0);
            let _ = self.binlog.append(&crate::binlog::LogRecord::Update {
                txid,
                table_name: update.table_name.clone(),
                rowid,
                old_row: old_row_pre.clone(),
                new_row: new_row.clone(),
            });
            // C1: Record undo entry (use old_row_pre captured before update)
            if self.pager.in_transaction() && self.current_txn_id != 0 {
                self.mvcc_undo_log.push(crate::vm::mvcc::UndoEntry::Update {
                    table: update.table_name.clone(),
                    rowid,
                    old_row: old_row_pre.clone(),
                });
            }

            // --- I1: Collect for RETURNING ---
            if update.returning.is_some() {
                returning_rows.push(new_row.clone());
            }

            // --- I2: FK ON UPDATE enforcement ---
            self.enforce_fk_parent_update(&update.table_name, &old_row_pre, &new_row)?;

            // --- L3: AFTER UPDATE triggers ---
            self.fire_triggers(
                &update.table_name,
                &crate::sql::ast::TriggerTiming::After,
                &crate::sql::ast::TriggerEvent::Update,
            )?;
        }

        // Update root page if changed
        if root_page != original_root {
            let table_schema = self.schema.get_table_mut(&update.table_name)?;
            table_schema.root_page = root_page;
            self.update_schema_root_page(&update.table_name, root_page)?;
        }

        self.auto_flush()?;

        // --- I1: RETURNING clause for UPDATE ---
        if let Some(ref returning_exprs) = update.returning {
            let col_map2 = self.build_table_col_map(&update.table_name)?;
            let mut result_rows = Vec::new();
            for new_row in &returning_rows {
                let mut result_row = Vec::new();
                for expr in returning_exprs {
                    if let crate::sql::ast::Expr::ColumnRef { table: _, column } = expr {
                        if column == "*" {
                            result_row.extend_from_slice(new_row);
                            continue;
                        }
                    }
                    let v = self.eval_expr(expr, new_row, &col_map2)?;
                    result_row.push(v);
                }
                result_rows.push(result_row);
            }
            let col_names: Vec<String> = returning_exprs
                .iter()
                .enumerate()
                .map(|(i, e)| match e {
                    crate::sql::ast::Expr::ColumnRef { column, .. } => column.clone(),
                    _ => format!("col{}", i),
                })
                .collect();
            return Ok(ExecResult::QueryResult {
                columns: col_names,
                rows: result_rows,
            });
        }

        Ok(ExecResult::RowsAffected {
            count,
            message: format!("{} row(s) updated", count),
        })
    }

    // ---- DELETE ----
    pub(crate) fn exec_delete(&mut self, delete: &DeleteStmt) -> Result<ExecResult> {
        let (col_map, original_root) = {
            let table = self.schema.get_table(&delete.table_name)?;
            let mut cm: HashMap<String, usize> = HashMap::with_capacity(table.columns.len());
            for (i, c) in table.columns.iter().enumerate() {
                if c.name.bytes().any(|b| b.is_ascii_uppercase()) {
                    let mut lower = String::with_capacity(c.name.len());
                    for b in c.name.bytes() {
                        lower.push(b.to_ascii_lowercase() as char);
                    }
                    cm.insert(lower, i);
                } else {
                    cm.insert(c.name.clone(), i);
                }
            }
            (cm, table.root_page)
        };

        // Try index-accelerated path for simple WHERE col = value
        let index_rowids = if let Some(ref where_expr) = delete.where_clause {
            self.try_dml_index_rowids(&delete.table_name, where_expr)?
        } else {
            None
        };

        let rowids_to_delete: Vec<i64> = if let Some(rowids) = index_rowids {
            rowids
        } else {
            // Scan all rows first (releases pager borrow), then filter.
            let all_rows = {
                let tbl_pager = self.get_table_pager_mut(&delete.table_name);
                let mut btree = BTree::new(tbl_pager);
                btree.scan_all(original_root)?
            };
            let mut ids = Vec::new();
            for (rowid, row) in &all_rows {
                let should_delete = if let Some(ref where_expr) = delete.where_clause {
                    self.eval_expr(where_expr, row, &col_map)?.is_truthy()
                } else {
                    true
                };
                if should_delete {
                    ids.push(*rowid);
                }
            }
            ids
        };

        let count = rowids_to_delete.len();
        let mut root_page = original_root;
        // I1: collect deleted rows for RETURNING clause
        let mut deleted_rows_for_returning: Vec<crate::types::Row> = if delete.returning.is_some() {
            Vec::with_capacity(count)
        } else {
            Vec::new()
        };

        for rowid in rowids_to_delete {
            // --- L3: BEFORE DELETE triggers ---
            self.fire_triggers(
                &delete.table_name,
                &crate::sql::ast::TriggerTiming::Before,
                &crate::sql::ast::TriggerEvent::Delete,
            )?;

            // --- L1: FK CASCADE / SET NULL enforcement (parent delete) ---
            {
                // B12-5 fix: use root_page (updated per loop iteration), not original_root
                // which becomes stale after the first B-Tree rebalance.
                let parent_row_opt = {
                    let tbl_pager = self.get_table_pager_mut(&delete.table_name);
                    let mut btree = BTree::new(tbl_pager);
                    btree.find_by_rowid(root_page, rowid)?.map(|(_, r)| r)
                };
                if let Some(ref parent_row) = parent_row_opt {
                    self.enforce_fk_parent_delete(&delete.table_name, parent_row)?;
                }
            }

            // Delete index entries before deleting the row
            self.maintain_fts_delete(&delete.table_name, rowid)?;
            self.maintain_vec_delete(&delete.table_name, rowid);
            self.delete_index_entries(&delete.table_name, rowid)?;

            let tbl_pager = self.get_table_pager_mut(&delete.table_name);
            let mut btree = BTree::new(tbl_pager);

            // Fetch old row for binlog + RETURNING before deleting
            let old_row = btree.find_by_rowid(root_page, rowid)?.map(|(_, r)| r);
            // I1: collect old row for RETURNING clause
            if delete.returning.is_some() {
                if let Some(ref r) = old_row {
                    deleted_rows_for_returning.push(r.clone());
                }
            }

            let (_, new_root) = btree.delete_by_rowid(root_page, rowid)?;
            root_page = new_root;

            // Log Delete to Binlog
            let txid = self.pager.active_txid().unwrap_or(0);
            let _ = self.binlog.append(&crate::binlog::LogRecord::Delete {
                txid,
                table_name: delete.table_name.clone(),
                rowid,
                row: old_row,
            });
            // C1: DELETE undo: pager COW handles physical rollback; no logical undo needed here.

            // --- L3: AFTER DELETE triggers ---
            self.fire_triggers(
                &delete.table_name,
                &crate::sql::ast::TriggerTiming::After,
                &crate::sql::ast::TriggerEvent::Delete,
            )?;
        }

        // Update root page if changed (future: rebalancing may change root)
        if root_page != original_root {
            let table_schema = self.schema.get_table_mut(&delete.table_name)?;
            table_schema.root_page = root_page;
            self.update_schema_root_page(&delete.table_name, root_page)?;
        }

        self.auto_flush()?;

        // --- I1: RETURNING clause for DELETE ---
        if let Some(ref returning_exprs) = delete.returning {
            let col_map2 = self.build_table_col_map(&delete.table_name)?;
            let mut result_rows = Vec::new();
            for old_row in &deleted_rows_for_returning {
                let mut result_row = Vec::new();
                for expr in returning_exprs {
                    if let crate::sql::ast::Expr::ColumnRef { table: _, column } = expr {
                        if column == "*" {
                            result_row.extend_from_slice(old_row);
                            continue;
                        }
                    }
                    let v = self.eval_expr(expr, old_row, &col_map2)?;
                    result_row.push(v);
                }
                result_rows.push(result_row);
            }
            let col_names: Vec<String> = returning_exprs
                .iter()
                .enumerate()
                .map(|(i, e)| match e {
                    crate::sql::ast::Expr::ColumnRef { column, .. } => column.clone(),
                    _ => format!("col{}", i),
                })
                .collect();
            // I19 fix: write binlog Commit record before returning so binlog followers
            // can correctly apply the transaction boundary for DELETE RETURNING.
            {
                let txid = self.pager.active_txid().unwrap_or(0);
                let _ = self.binlog.append(&crate::binlog::LogRecord::Commit(txid));
            }
            return Ok(ExecResult::QueryResult {
                columns: col_names,
                rows: result_rows,
            });
        }

        Ok(ExecResult::RowsAffected {
            count,
            message: format!("{} row(s) deleted", count),
        })
    }

    /// Try to use an index to find matching rowids for a simple WHERE col = value.
    /// Used by UPDATE/DELETE to avoid full table scan.
    /// Returns Some(vec of rowids) if index was used, None otherwise.
    fn try_dml_index_rowids(
        &mut self,
        table_name: &str,
        where_expr: &Expr,
    ) -> Result<Option<Vec<i64>>> {
        enum Lookup {
            Eq(Value),
            In(Vec<Value>),
            Comparison(BinaryOperator, Value),
            Between(Value, Value),
        }

        let (col_name, lookup) = match where_expr {
            Expr::BinaryOp { left, op, right } => {
                let Some(op_normalized) = (match (left.as_ref(), right.as_ref()) {
                    (Expr::ColumnRef { .. }, _) => Some(op.clone()),
                    (_, Expr::ColumnRef { .. }) => VM::flip_comparison_operator(op),
                    _ => None,
                }) else {
                    return Ok(None);
                };

                let Some(val) = (match (left.as_ref(), right.as_ref()) {
                    (Expr::ColumnRef { .. }, rhs) => self.eval_constant_expr(rhs),
                    (lhs, Expr::ColumnRef { .. }) => self.eval_constant_expr(lhs),
                    _ => None,
                }) else {
                    return Ok(None);
                };

                let column = match (left.as_ref(), right.as_ref()) {
                    (Expr::ColumnRef { column, .. }, _) => column.clone(),
                    (_, Expr::ColumnRef { column, .. }) => column.clone(),
                    _ => return Ok(None),
                };

                let lookup = if op_normalized == BinaryOperator::Equal {
                    Lookup::Eq(val)
                } else if matches!(
                    op_normalized,
                    BinaryOperator::LessThan
                        | BinaryOperator::LessThanOrEqual
                        | BinaryOperator::GreaterThan
                        | BinaryOperator::GreaterThanOrEqual
                ) {
                    Lookup::Comparison(op_normalized, val)
                } else {
                    return Ok(None);
                };
                (column, lookup)
            }
            Expr::InList {
                expr,
                list,
                negated,
            } if !negated => {
                let Expr::ColumnRef { column, .. } = expr.as_ref() else {
                    return Ok(None);
                };
                let mut values = Vec::with_capacity(list.len());
                for item in list {
                    let Some(v) = self.eval_constant_expr(item) else {
                        return Ok(None);
                    };
                    // NULL in IN-list can never make predicate TRUE in WHERE context.
                    if !matches!(v, Value::Null) {
                        values.push(v);
                    }
                }
                (column.clone(), Lookup::In(values))
            }
            Expr::Between {
                expr,
                low,
                high,
                negated,
            } if !negated => {
                let Expr::ColumnRef { column, .. } = expr.as_ref() else {
                    return Ok(None);
                };
                let Some(low_v) = self.eval_constant_expr(low) else {
                    return Ok(None);
                };
                let Some(high_v) = self.eval_constant_expr(high) else {
                    return Ok(None);
                };
                (column.clone(), Lookup::Between(low_v, high_v))
            }
            _ => return Ok(None),
        };

        // Find an index that starts with this column
        let indexes: Vec<_> = self
            .schema
            .indexes_for_table(table_name)
            .into_iter()
            .cloned()
            .collect();
        let idx = match indexes
            .iter()
            .find(|idx| !idx.columns.is_empty() && idx.columns[0].eq_ignore_ascii_case(&col_name))
        {
            Some(i) => i.clone(),
            None => return Ok(None),
        };

        match lookup {
            Lookup::Eq(search_val) => {
                // SQL '=' with NULL is unknown, so WHERE never matches.
                if matches!(search_val, Value::Null) {
                    return Ok(Some(Vec::new()));
                }
                Ok(Some(self.index_rowids_for_value(&idx, &search_val)?))
            }
            Lookup::In(search_vals) => {
                if search_vals.is_empty() {
                    return Ok(Some(Vec::new()));
                }
                let mut seen = HashSet::new();
                let mut out = Vec::new();
                for v in search_vals {
                    for rid in self.index_rowids_for_value(&idx, &v)? {
                        if seen.insert(rid) {
                            out.push(rid);
                        }
                    }
                }
                Ok(Some(out))
            }
            Lookup::Comparison(op, search_val) => Ok(Some(self.index_rowids_for_comparison(
                &idx,
                &op,
                &search_val,
            )?)),
            Lookup::Between(low, high) => {
                Ok(Some(self.index_rowids_for_between(&idx, &low, &high)?))
            }
        }
    }

    // ---- INDEX MAINTENANCE ----

    /// Insert index entries for all indexes on a table after a row is inserted
    pub(crate) fn insert_index_entries(
        &mut self,
        table_name: &str,
        rowid: i64,
        row: &Row,
    ) -> Result<()> {
        if !self.schema.has_indexes_for_table(table_name) {
            return Ok(());
        }
        let table = self.schema.get_table(table_name)?.clone();
        let indexes: Vec<_> = self
            .schema
            .indexes_for_table(table_name)
            .into_iter()
            .cloned()
            .collect();

        for idx in &indexes {
            // Build column indices for this index
            let col_indices: Vec<usize> = idx
                .columns
                .iter()
                .filter_map(|col_name| {
                    table
                        .columns
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(col_name))
                        .map(|c| c.col_index)
                })
                .collect();

            // Build index entry: [col_val_1, ..., col_val_n, table_rowid]
            let mut index_row: Row = Vec::with_capacity(col_indices.len() + 1);
            for &ci in &col_indices {
                index_row.push(if ci < row.len() {
                    row[ci].clone()
                } else {
                    Value::Null
                });
            }

            if idx.unique
                && !index_row.iter().any(|v| matches!(v, Value::Null))
                && self.unique_index_conflict_exists(idx, &index_row, Some(rowid))?
            {
                return Err(KkdbError::ConstraintViolation(format!(
                    "UNIQUE constraint failed: {}.{}",
                    table_name,
                    idx.columns.join(", ")
                )));
            }

            let first_col_value = index_row.first().cloned();
            index_row.push(Value::Integer(rowid));

            let current_root = idx.root_page;
            let tbl_pager = self.get_table_pager_mut(table_name);
            let mut btree = BTree::new(tbl_pager);
            let idx_next = btree.max_rowid(current_root).unwrap_or(0) + 1;
            let new_root = btree.insert(current_root, idx_next, &index_row)?;

            if let Some(ref first_val) = first_col_value {
                self.index_cache_insert(idx, first_val, rowid, idx_next);
            }

            if new_root != current_root {
                if let Some(schema_idx) = self.schema.indexes.get_mut(&idx.name.to_lowercase()) {
                    schema_idx.root_page = new_root;
                }
                let _ = self.update_schema_object_root_page(&idx.name, new_root);
            }
        }
        Ok(())
    }

    /// Delete index entries for all indexes on a table when a row is deleted
    pub(crate) fn delete_index_entries(&mut self, table_name: &str, rowid: i64) -> Result<()> {
        if !self.schema.has_indexes_for_table(table_name) {
            return Ok(());
        }
        let indexes: Vec<_> = self
            .schema
            .indexes_for_table(table_name)
            .into_iter()
            .cloned()
            .collect();

        for idx in &indexes {
            let mut first_col_value: Option<Value> = None;
            if let Some(idx_rowid) = self.index_entry_rowid_for_table_rowid(idx, rowid)? {
                let tbl_pager = self.get_table_pager_mut(table_name);
                let mut btree = BTree::new(tbl_pager);
                if let Some((_rid, idx_row)) = btree.find_by_rowid(idx.root_page, idx_rowid)? {
                    first_col_value = idx_row.first().cloned();
                }
                let (_deleted, new_root) = btree.delete_by_rowid(idx.root_page, idx_rowid)?;

                if new_root != idx.root_page {
                    if let Some(schema_idx) = self.schema.indexes.get_mut(&idx.name.to_lowercase())
                    {
                        schema_idx.root_page = new_root;
                    }
                    self.update_schema_object_root_page(&idx.name, new_root)?;
                }

                if let Some(ref first_val) = first_col_value {
                    self.index_cache_delete(idx, first_val, rowid);
                }
            }
        }
        Ok(())
    }

    /// Validate UNIQUE indexes for an incoming table row.
    /// `ignore_rowid` is used when checking updates to avoid self-collision.
    fn validate_unique_indexes_for_row(
        &mut self,
        table_name: &str,
        rowid: i64,
        row: &Row,
        ignore_rowid: Option<i64>,
    ) -> Result<()> {
        if !self.schema.has_indexes_for_table(table_name) {
            return Ok(());
        }

        let table = self.schema.get_table(table_name)?.clone();
        let indexes: Vec<_> = self
            .schema
            .indexes_for_table(table_name)
            .into_iter()
            .cloned()
            .collect();

        for idx in &indexes {
            if !idx.unique {
                continue;
            }

            let mut key_values: Row = Vec::with_capacity(idx.columns.len());
            for col_name in &idx.columns {
                let col_idx = table
                    .columns
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(col_name))
                    .map(|c| c.col_index)
                    .ok_or_else(|| {
                        KkdbError::ColumnNotFound(format!("{}.{}", table_name, col_name))
                    })?;
                key_values.push(if col_idx < row.len() {
                    row[col_idx].clone()
                } else {
                    Value::Null
                });
            }

            // SQLite UNIQUE allows multiple NULL keys.
            if key_values.iter().any(|v| matches!(v, Value::Null)) {
                continue;
            }

            if self.unique_index_conflict_exists(idx, &key_values, ignore_rowid.or(Some(rowid)))? {
                return Err(KkdbError::ConstraintViolation(format!(
                    "UNIQUE constraint failed: {}.{}",
                    table_name,
                    idx.columns.join(", ")
                )));
            }
        }

        Ok(())
    }

    /// Returns true if any index entry with the same key already exists.
    fn unique_index_conflict_exists(
        &mut self,
        index: &crate::schema::IndexSchema,
        key_values: &[Value],
        ignore_rowid: Option<i64>,
    ) -> Result<bool> {
        if key_values.is_empty() {
            return Ok(false);
        }

        // Use index cache by first key column to narrow candidate table rowids,
        // then verify full composite key by reading corresponding index entries.
        let candidate_table_rowids = self.index_rowids_for_value(index, &key_values[0])?;
        if candidate_table_rowids.is_empty() {
            return Ok(false);
        }

        let mut candidate_index_rowids = Vec::with_capacity(candidate_table_rowids.len());
        for table_rowid in candidate_table_rowids {
            if Some(table_rowid) == ignore_rowid {
                continue;
            }
            if let Some(idx_rowid) = self.index_entry_rowid_for_table_rowid(index, table_rowid)? {
                candidate_index_rowids.push(idx_rowid);
            }
        }

        if candidate_index_rowids.is_empty() {
            return Ok(false);
        }

        // B12-1 fix: use the table's own pager (not catalog pager) to read index entries.
        // In multi-file mode, index data lives in the table's .kkdb file, not catalog.kkdb.
        let idx_table = index.table_name.clone();
        let mut btree = BTree::new(self.get_table_pager_mut(&idx_table));
        for idx_rowid in candidate_index_rowids {
            let Some((_rid, idx_row)) = btree.find_by_rowid(index.root_page, idx_rowid)? else {
                continue;
            };
            if idx_row.len() < key_values.len() + 1 {
                continue;
            }

            let mut all_equal = true;
            for (i, key_val) in key_values.iter().enumerate() {
                if idx_row[i] != *key_val {
                    all_equal = false;
                    break;
                }
            }
            if !all_equal {
                continue;
            }

            if let Some(Value::Integer(existing_rowid)) = idx_row.last() {
                if Some(*existing_rowid) == ignore_rowid {
                    continue;
                }
            }
            return Ok(true);
        }

        Ok(false)
    }
}

// Extension trait for FK validation
impl VM {
    // 鈹€鈹€ L1: FOREIGN KEY Validation 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    /// On INSERT: verify that each FK column value exists in the referenced parent table.
    pub(crate) fn check_fk_on_insert(
        &mut self,
        table_name: &str,
        row: &[crate::types::Value],
    ) -> crate::error::Result<()> {
        use crate::error::KkdbError;
        let fks: Vec<crate::schema::ForeignKey> =
            self.schema.get_table(table_name)?.foreign_keys.clone();
        for fk in fks {
            let fk_value = row
                .get(fk.col_index)
                .cloned()
                .unwrap_or(crate::types::Value::Null);
            if matches!(fk_value, crate::types::Value::Null) {
                // NULL FK values always pass (SQL standard)
                continue;
            }
            // Resolve referenced column: default to first PK column
            let parent_col = fk.ref_col.as_deref();
            let found = self.fk_value_exists_in_parent(&fk.ref_table, parent_col, &fk_value)?;
            if !found {
                return Err(KkdbError::ConstraintViolation(format!(
                    "FOREIGN KEY constraint failed: {}.{} references {}",
                    table_name, fk.col_name, fk.ref_table
                )));
            }
        }
        Ok(())
    }

    /// On DELETE from parent table: verify no child rows reference the deleted row.
    #[allow(dead_code)]
    pub(crate) fn check_fk_on_delete(
        &mut self,
        parent_table: &str,
        deleted_row: &[crate::types::Value],
    ) -> crate::error::Result<()> {
        use crate::error::KkdbError;
        // Find all tables that have FK references to this parent
        let all_tables: Vec<String> = self.schema.list_tables();
        for child_table in all_tables {
            let fks: Vec<crate::schema::ForeignKey> = {
                let ts = match self.schema.get_table(&child_table) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                ts.foreign_keys.clone()
            };
            for fk in fks {
                if !fk.ref_table.eq_ignore_ascii_case(parent_table) {
                    continue;
                }
                // Find the PK column idx of the parent
                let parent_pk_idx = {
                    let ts = self.schema.get_table(parent_table)?;
                    ts.columns
                        .iter()
                        .find(|c| c.primary_key)
                        .map(|c| c.col_index)
                };
                let referenced_val = if let Some(col_name) = &fk.ref_col {
                    let ts = self.schema.get_table(parent_table)?;
                    let col_idx = ts
                        .columns
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(col_name))
                        .map(|c| c.col_index);
                    col_idx.and_then(|i| deleted_row.get(i)).cloned()
                } else {
                    parent_pk_idx.and_then(|i| deleted_row.get(i)).cloned()
                };
                let Some(ref_val) = referenced_val else {
                    continue;
                };
                if matches!(ref_val, crate::types::Value::Null) {
                    continue;
                }
                // Check if any child row has this FK value
                let fk_col_name = fk.col_name.clone();
                let has_child = self.fk_value_exists_in_parent(
                    &child_table,
                    Some(fk_col_name.as_str()),
                    &ref_val,
                )?;
                if has_child {
                    return Err(KkdbError::ConstraintViolation(format!(
                        "FOREIGN KEY constraint failed: deleting from {} has dependent rows in {}",
                        parent_table, child_table
                    )));
                }
            }
        }
        Ok(())
    }

    /// Returns true if `value` exists in `col_name` (or PK) of `table`.
    fn fk_value_exists_in_parent(
        &mut self,
        table: &str,
        col_name: Option<&str>,
        value: &crate::types::Value,
    ) -> crate::error::Result<bool> {
        use crate::storage::btree::BTree;
        let (root_page, col_idx) = {
            let ts = self.schema.get_table(table)?;
            let idx = if let Some(col) = col_name {
                ts.columns
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(col))
                    .map(|c| c.col_index)
                    .unwrap_or(0)
            } else {
                // Default: PK column
                ts.columns
                    .iter()
                    .find(|c| c.primary_key)
                    .map(|c| c.col_index)
                    .unwrap_or(0)
            };
            (ts.root_page, idx)
        };
        let pager = self.get_table_pager_mut(table);
        let mut btree = BTree::new(pager);
        let rows = btree.scan_rows(root_page)?;
        for row in &rows {
            if row.get(col_idx) == Some(value) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

// ── Helpers used by RETURNING and FK cascade ───────────────────────────────
impl VM {
    /// Build a {column_name -> index} map for a table (lowercase keys).
    pub(crate) fn build_table_col_map(
        &mut self,
        table_name: &str,
    ) -> crate::error::Result<std::collections::HashMap<String, usize>> {
        let cols = self.schema.get_table(table_name)?.columns.clone();
        let mut map = std::collections::HashMap::new();
        for col in &cols {
            map.insert(col.name.to_ascii_lowercase(), col.col_index);
        }
        Ok(map)
    }

    /// Enforce FK referential actions for a parent table DELETE.
    ///
    /// For each child table that references `parent_table`, look up the FK action:
    /// - `Cascade`  → DELETE matching child rows recursively.
    /// - `SetNull`  → UPDATE matching child rows SET fk_col = NULL.
    /// - `Restrict` → Return ConstraintViolation if any child row exists.
    pub(crate) fn enforce_fk_parent_delete(
        &mut self,
        parent_table: &str,
        deleted_row: &[crate::types::Value],
    ) -> crate::error::Result<()> {
        use crate::sql::ast::FkAction;
        let all_tables: Vec<String> = self.schema.list_tables();
        for child_table in all_tables {
            let fks: Vec<crate::schema::ForeignKey> = {
                let ts = match self.schema.get_table(&child_table) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                ts.foreign_keys.clone()
            };
            for fk in fks {
                if !fk.ref_table.eq_ignore_ascii_case(parent_table) {
                    continue;
                }
                // Find the referenced value in the parent row
                let parent_pk_idx = {
                    let ts = self.schema.get_table(parent_table)?;
                    ts.columns
                        .iter()
                        .find(|c| c.primary_key)
                        .map(|c| c.col_index)
                };
                let ref_val = if let Some(col_name) = &fk.ref_col {
                    let ts = self.schema.get_table(parent_table)?;
                    let col_idx = ts
                        .columns
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(col_name))
                        .map(|c| c.col_index);
                    col_idx.and_then(|i| deleted_row.get(i)).cloned()
                } else {
                    parent_pk_idx.and_then(|i| deleted_row.get(i)).cloned()
                };
                let Some(ref_val) = ref_val else {
                    continue;
                };
                if matches!(ref_val, crate::types::Value::Null) {
                    continue;
                }

                match fk.on_delete {
                    FkAction::Cascade => {
                        // DELETE all child rows that have this FK value
                        let sql = format!(
                            "DELETE FROM {} WHERE {} = {}",
                            child_table,
                            fk.col_name,
                            match &ref_val {
                                crate::types::Value::Integer(i) => i.to_string(),
                                crate::types::Value::Real(f) => f.to_string(),
                                crate::types::Value::Text(t) =>
                                    format!("'{}'", t.replace('\'', "''")),
                                _ => continue,
                            }
                        );
                        self.execute_sql(&sql)?;
                    }
                    FkAction::SetNull => {
                        // UPDATE child SET fk_col = NULL WHERE fk_col = ref_val
                        let sql = format!(
                            "UPDATE {} SET {} = NULL WHERE {} = {}",
                            child_table,
                            fk.col_name,
                            fk.col_name,
                            match &ref_val {
                                crate::types::Value::Integer(i) => i.to_string(),
                                crate::types::Value::Real(f) => f.to_string(),
                                crate::types::Value::Text(t) =>
                                    format!("'{}'", t.replace('\'', "''")),
                                _ => continue,
                            }
                        );
                        self.execute_sql(&sql)?;
                    }
                    FkAction::Restrict => {
                        let has_child = self.fk_value_exists_in_parent(
                            &child_table,
                            Some(fk.col_name.as_str()),
                            &ref_val,
                        )?;
                        if has_child {
                            return Err(KkdbError::ConstraintViolation(format!(
                                "FOREIGN KEY constraint failed: deleting from {} has dependent rows in {}",
                                parent_table, child_table
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// I2: On UPDATE to a parent table: enforce FK ON UPDATE actions in child tables.
    ///
    /// `parent_table`  — the table being updated.  
    /// `old_row` / `new_row` — before and after the UPDATE for a single row.
    pub(crate) fn enforce_fk_parent_update(
        &mut self,
        parent_table: &str,
        old_row: &[crate::types::Value],
        new_row: &[crate::types::Value],
    ) -> crate::error::Result<()> {
        use crate::sql::ast::FkAction;
        let all_tables: Vec<String> = self.schema.list_tables();
        for child_table in all_tables {
            let fks: Vec<crate::schema::ForeignKey> = {
                let ts = match self.schema.get_table(&child_table) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                ts.foreign_keys.clone()
            };
            for fk in fks {
                if !fk.ref_table.eq_ignore_ascii_case(parent_table) {
                    continue;
                }
                // Find old and new value of the referenced column in the parent row
                let parent_col_idx = if let Some(col_name) = &fk.ref_col {
                    let ts = self.schema.get_table(parent_table)?;
                    ts.columns
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(col_name))
                        .map(|c| c.col_index)
                        .unwrap_or(0)
                } else {
                    let ts = self.schema.get_table(parent_table)?;
                    ts.columns
                        .iter()
                        .find(|c| c.primary_key)
                        .map(|c| c.col_index)
                        .unwrap_or(0)
                };
                let old_val = old_row
                    .get(parent_col_idx)
                    .cloned()
                    .unwrap_or(crate::types::Value::Null);
                let new_val = new_row
                    .get(parent_col_idx)
                    .cloned()
                    .unwrap_or(crate::types::Value::Null);
                // Only act if the referenced value actually changed
                if old_val == new_val {
                    continue;
                }
                if matches!(old_val, crate::types::Value::Null) {
                    continue;
                }

                let old_lit = match &old_val {
                    crate::types::Value::Integer(i) => i.to_string(),
                    crate::types::Value::Real(f) => f.to_string(),
                    crate::types::Value::Text(t) => format!("'{}'", t.replace('\'', "''")),
                    _ => continue,
                };
                let new_lit = match &new_val {
                    crate::types::Value::Integer(i) => i.to_string(),
                    crate::types::Value::Real(f) => f.to_string(),
                    crate::types::Value::Text(t) => format!("'{}'", t.replace('\'', "''")),
                    crate::types::Value::Null => "NULL".to_string(),
                    _ => continue,
                };

                match fk.on_update {
                    FkAction::Cascade => {
                        let sql = format!(
                            "UPDATE {} SET {} = {} WHERE {} = {}",
                            child_table, fk.col_name, new_lit, fk.col_name, old_lit
                        );
                        self.execute_sql(&sql)?;
                    }
                    FkAction::SetNull => {
                        let sql = format!(
                            "UPDATE {} SET {} = NULL WHERE {} = {}",
                            child_table, fk.col_name, fk.col_name, old_lit
                        );
                        self.execute_sql(&sql)?;
                    }
                    FkAction::Restrict => {
                        let has_child = self.fk_value_exists_in_parent(
                            &child_table,
                            Some(fk.col_name.as_str()),
                            &old_val,
                        )?;
                        if has_child {
                            return Err(KkdbError::ConstraintViolation(format!(
                                "FOREIGN KEY constraint failed: updating {} changes referenced value \
                                 in {}.{}",
                                parent_table, child_table, fk.col_name
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

// 鈹€鈹€ L2: CHECK Constraint Validation 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€
impl VM {
    /// Evaluate CHECK constraints for a row before INSERT.
    pub(crate) fn check_constraints_for_row(
        &mut self,
        table_name: &str,
        row: &[crate::types::Value],
        _hint: &[String],
    ) -> crate::error::Result<()> {
        use crate::error::KkdbError;
        use crate::sql::ast::Expr;
        use crate::types::Value;
        let (checks, col_names): (Vec<(Option<String>, Expr)>, Vec<String>) = {
            let ts = self.schema.get_table(table_name)?;
            (ts.check_constraints.clone(), ts.col_names.clone())
        };
        for (name, check_expr) in checks {
            let val = self.eval_check_expr_simple(&check_expr, row, &col_names)?;
            // SQL standard: NULL (UNKNOWN) in CHECK → not a violation; only explicit FALSE/0 fails
            let passed = !matches!(val, Value::Integer(0));
            if !passed {
                let id = name.as_deref().unwrap_or("CHECK");
                return Err(KkdbError::ConstraintViolation(format!(
                    "CHECK constraint {} failed for table {}",
                    id, table_name
                )));
            }
        }
        Ok(())
    }

    fn eval_check_expr_simple(
        &mut self,
        expr: &crate::sql::ast::Expr,
        row: &[crate::types::Value],
        col_names: &[String],
    ) -> crate::error::Result<crate::types::Value> {
        use crate::sql::ast::{BinaryOperator as BOp, Expr, UnaryOperator as UOp};
        use crate::types::Value;
        Ok(match expr {
            Expr::IntegerLiteral(i) => Value::Integer(*i),
            Expr::RealLiteral(r) => Value::Real(*r),
            Expr::StringLiteral(s) => Value::Text(s.as_str().into()),
            Expr::Null => Value::Null,
            Expr::ColumnRef { column, .. } => {
                let lc = column.to_lowercase();
                col_names
                    .iter()
                    .position(|n| n.to_lowercase() == lc)
                    .and_then(|i| row.get(i))
                    .cloned()
                    .unwrap_or(Value::Null)
            }
            Expr::BinaryOp { left, op, right } => {
                let l = self.eval_check_expr_simple(left, row, col_names)?;
                let r = self.eval_check_expr_simple(right, row, col_names)?;
                match op {
                    // NULL propagation: any comparison involving NULL → UNKNOWN (passes CHECK)
                    BOp::Equal
                    | BOp::NotEqual
                    | BOp::LessThan
                    | BOp::LessThanOrEqual
                    | BOp::GreaterThan
                    | BOp::GreaterThanOrEqual
                        if matches!(l, Value::Null) || matches!(r, Value::Null) =>
                    {
                        Value::Null
                    }
                    BOp::Equal => Value::Integer(if l == r { 1 } else { 0 }),
                    BOp::NotEqual => Value::Integer(if l != r { 1 } else { 0 }),
                    BOp::LessThan => Value::Integer(if chk_cmp(&l, &r) < 0 { 1 } else { 0 }),
                    BOp::LessThanOrEqual => {
                        Value::Integer(if chk_cmp(&l, &r) <= 0 { 1 } else { 0 })
                    }
                    BOp::GreaterThan => Value::Integer(if chk_cmp(&l, &r) > 0 { 1 } else { 0 }),
                    BOp::GreaterThanOrEqual => {
                        Value::Integer(if chk_cmp(&l, &r) >= 0 { 1 } else { 0 })
                    }
                    BOp::Add => chk_arith(&l, &r, |a, b| a + b, |a, b| a + b),
                    BOp::Subtract => chk_arith(&l, &r, |a, b| a - b, |a, b| a - b),
                    BOp::Multiply => chk_arith(&l, &r, |a, b| a * b, |a, b| a * b),
                    BOp::Divide => match (&l, &r) {
                        (Value::Integer(a), Value::Integer(b)) if *b != 0 => Value::Integer(a / b),
                        _ => Value::Null,
                    },
                    BOp::And => Value::Integer(if chk_truthy(&l) && chk_truthy(&r) {
                        1
                    } else {
                        0
                    }),
                    BOp::Or => Value::Integer(if chk_truthy(&l) || chk_truthy(&r) {
                        1
                    } else {
                        0
                    }),
                    _ => Value::Null,
                }
            }
            Expr::IsNull {
                expr: inner,
                negated,
            } => {
                let v = self.eval_check_expr_simple(inner, row, col_names)?;
                let is_null = matches!(v, Value::Null);
                Value::Integer(if is_null != *negated { 1 } else { 0 })
            }
            Expr::UnaryOp { op, expr: inner } => {
                let v = self.eval_check_expr_simple(inner, row, col_names)?;
                match op {
                    UOp::Not => Value::Integer(if chk_truthy(&v) { 0 } else { 1 }),
                    UOp::Minus => match v {
                        Value::Integer(i) => Value::Integer(-i),
                        Value::Real(r) => Value::Real(-r),
                        _ => Value::Null,
                    },
                }
            }
            // Complex exprs (functions, subqueries, etc.) 鈫?Null 鈫?check passes
            _ => Value::Null,
        })
    }

    /// L3: Fire triggers for (table_name, timing, event).
    /// body_sql is re-parsed from the persisted trigger definition and executed.
    pub(crate) fn fire_triggers(
        &mut self,
        table_name: &str,
        timing: &crate::sql::ast::TriggerTiming,
        event: &crate::sql::ast::TriggerEvent,
    ) -> crate::error::Result<()> {
        // Collect body SQLs to avoid borrow issues
        let body_sqls: Vec<String> = self
            .schema
            .get_triggers(table_name, timing, event)
            .into_iter()
            .map(|t| t.body_sql.clone())
            .collect();

        for body_sql in body_sqls {
            let mut inner_sql = body_sql.trim();
            if inner_sql.eq_ignore_ascii_case("BEGIN") || inner_sql.eq_ignore_ascii_case("END") {
                continue;
            }
            if inner_sql.to_uppercase().starts_with("BEGIN")
                && inner_sql.to_uppercase().ends_with("END")
            {
                inner_sql = inner_sql[5..inner_sql.len() - 3].trim();
            }

            // Each body_sql may be a semicolon-separated list of statements
            for stmt_sql in inner_sql.split(';') {
                let trimmed = stmt_sql.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let _ = self.execute_sql(trimmed);
            }
        }
        Ok(())
    }

    // L4: Maintain FTS inverted index (Phase 4 — real B-Tree postings)
    pub(crate) fn maintain_fts_insert(
        &mut self,
        table_name: &str,
        rowid: i64,
        row: &[Value],
    ) -> Result<()> {
        // Guard: skip FTS system tables to prevent infinite recursion
        if table_name.starts_with("_kkdb_fts_") {
            return Ok(());
        }

        // --- New Path: CREATE FULLTEXT INDEX (IndexSchema.is_fts) ---
        // Collect all FTS indexes for this table
        let fts_indexes: Vec<(u32, Vec<usize>)> = {
            let fts_idxs: Vec<_> = self
                .schema
                .indexes_for_table(table_name)
                .into_iter()
                .filter(|idx| idx.is_fts)
                .cloned()
                .collect();
            if fts_idxs.is_empty() {
                return Ok(());
            }
            let tbl = self.schema.get_table(table_name)?.clone();
            fts_idxs
                .into_iter()
                .map(|idx| {
                    let col_indices: Vec<usize> = idx
                        .columns
                        .iter()
                        .filter_map(|col_name| {
                            tbl.columns
                                .iter()
                                .find(|c| c.name.eq_ignore_ascii_case(col_name))
                                .map(|c| c.col_index)
                        })
                        .collect();
                    (idx.root_page, col_indices) // root_page is repurposed as index_id for FTS
                })
                .collect()
        };

        for (index_id, col_indices) in fts_indexes {
            // Tokenize the relevant columns
            let mut tf_map: std::collections::HashMap<String, u32> =
                std::collections::HashMap::new();
            let mut field_len: u32 = 0;
            for &ci in &col_indices {
                if let Some(Value::Text(s)) = row.get(ci) {
                    let tokens = crate::fulltext::tokenizer::simple_tokenize(s);
                    field_len += tokens.len() as u32;
                    for token in tokens {
                        *tf_map.entry(token).or_insert(0) += 1;
                    }
                }
            }
            if tf_map.is_empty() {
                continue;
            }

            // Enqueue for deferred write at execute_sql boundary (avoid reentrant execute_sql)
            let tfs: Vec<(String, u32)> = tf_map.into_iter().collect();
            self.pending_fts_inserts
                .push((index_id, rowid, tfs, field_len));
        }
        Ok(())
    }

    pub(crate) fn maintain_fts_delete(&mut self, table_name: &str, rowid: i64) -> Result<()> {
        use crate::storage::btree::BTree;
        // Guard: skip FTS system tables to prevent infinite recursion
        if table_name.starts_with("_kkdb_fts_") {
            return Ok(());
        }

        // --- New Path: CREATE FULLTEXT INDEX ---
        // FTS data is stored directly in B-Tree pages (via self.pager) identified
        // by the FTS index's root_page (= index_id). There is NO schema table for
        // _kkdb_fts_N in the new path; we scan self.pager directly, mirroring
        // how scan_fts_postings / write_fts_postings_raw work.
        let fts_indexes: Vec<u32> = self
            .schema
            .indexes_for_table(table_name)
            .into_iter()
            .filter(|idx| idx.is_fts)
            .map(|idx| idx.root_page) // root_page == index_id for FTS
            .collect();

        for index_id in fts_indexes {
            if index_id == 0 {
                continue;
            }

            // Scan ALL rows in the FTS B-Tree for this index
            let all_fts_rows = {
                let mut btree = BTree::new(&mut self.pager);
                btree.scan_all(index_id).unwrap_or_default()
            };

            let mut rowids_to_delete: Vec<i64> = Vec::new();
            let mut tokens_deleted: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut field_len_deleted: u32 = 0;

            // Row format: [token, doc_id, tf, field_len, meta_key]
            // Posting rows have meta_key = NULL
            // DF rows have meta_key = "DF:<token>"
            // GLOBAL row has meta_key = "GLOBAL"
            for (fts_rowid, fts_row) in &all_fts_rows {
                if let Some(Value::Integer(doc_id)) = fts_row.get(1) {
                    if *doc_id == rowid {
                        // This is a posting row for our doc — mark for deletion
                        if fts_row.get(4) == Some(&Value::Null) {
                            rowids_to_delete.push(*fts_rowid);
                            if let Some(Value::Text(token)) = fts_row.get(0) {
                                tokens_deleted.insert(token.to_string());
                            }
                            if let Some(Value::Integer(fl)) = fts_row.get(3) {
                                field_len_deleted = *fl as u32;
                            }
                        }
                    }
                }
            }

            // Delete posting rows one by one (root may change after each deletion)
            let mut current_root = index_id;
            for fts_rowid in rowids_to_delete {
                let mut btree = BTree::new(&mut self.pager);
                let (_, new_root) = btree.delete_by_rowid(current_root, fts_rowid)?;
                current_root = new_root;
            }
            // Update the FTS index root_page if it changed after deletion
            if current_root != index_id {
                // Patch the in-memory index schema so future scans use the new root.
                for idx in self.schema.indexes.values_mut() {
                    if idx.is_fts && idx.root_page == index_id {
                        idx.root_page = current_root;
                        break;
                    }
                }
            }

            // Update DF and GLOBAL stats (decrement by 1 doc)
            if !tokens_deleted.is_empty() {
                let (cur_docs, cur_fl) = self.read_fts_global_stats(index_id);
                let new_docs = cur_docs.saturating_sub(1);
                let new_fl = cur_fl.saturating_sub(field_len_deleted as u64);

                let mut new_df_map: std::collections::HashMap<String, u64> =
                    std::collections::HashMap::new();
                for token in &tokens_deleted {
                    let df = self.get_fts_doc_freq(index_id, token);
                    new_df_map.insert(token.clone(), df.saturating_sub(1));
                }

                // Write updated stats (appended — query reads latest)
                let empty_postings = std::collections::HashMap::new();
                self.write_fts_postings(index_id, &empty_postings, &new_df_map, new_docs, new_fl)?;
            }
        }
        Ok(())
    }

    pub(crate) fn tokenize(text: &str) -> Vec<String> {
        crate::fulltext::tokenizer::query_tokenize(text)
    }

    // ── Vector index DML maintenance ──────────────────────────────────────────

    /// Insert a vector from `row` into every HNSW graph registered on `table_name`.
    pub(crate) fn maintain_vec_insert(
        &mut self,
        table_name: &str,
        rowid: i64,
        row: &[Value],
    ) -> Result<()> {
        use crate::vector::index::decode_vector;

        let indexes: Vec<_> = self
            .schema
            .vector_indexes
            .for_table(table_name)
            .into_iter()
            .map(|vi| (vi.col_idx, vi.dim, vi.clone()))
            .collect();

        for (col_idx, dim, vi) in indexes {
            if let Some(Value::Blob(blob)) = row.get(col_idx) {
                if let Some(vec) = decode_vector(blob) {
                    if vec.len() as u32 == dim {
                        let _ = vi.insert_vec(rowid as u64, vec);
                    }
                }
            }
        }
        Ok(())
    }

    /// Lazily delete `rowid` from every HNSW graph registered on `table_name`.
    pub(crate) fn maintain_vec_delete(&mut self, table_name: &str, rowid: i64) {
        let indexes: Vec<_> = self
            .schema
            .vector_indexes
            .for_table(table_name)
            .into_iter()
            .map(|vi| vi.clone())
            .collect();

        for vi in indexes {
            vi.delete_vec(rowid as u64);
        }
    }
}

fn chk_cmp(l: &crate::types::Value, r: &crate::types::Value) -> i32 {
    use crate::types::Value;
    match (l, r) {
        (Value::Integer(a), Value::Integer(b)) => a.cmp(b) as i32,
        (Value::Real(a), Value::Real(b)) => {
            if a < b {
                -1
            } else if a > b {
                1
            } else {
                0
            }
        }
        (Value::Integer(a), Value::Real(b)) => {
            let af = *a as f64;
            if af < *b {
                -1
            } else if af > *b {
                1
            } else {
                0
            }
        }
        (Value::Real(a), Value::Integer(b)) => {
            let bf = *b as f64;
            if *a < bf {
                -1
            } else if *a > bf {
                1
            } else {
                0
            }
        }
        (Value::Text(a), Value::Text(b)) => {
            if a < b {
                -1
            } else if a > b {
                1
            } else {
                0
            }
        }
        _ => 0,
    }
}

fn chk_arith(
    l: &crate::types::Value,
    r: &crate::types::Value,
    int_op: impl Fn(i64, i64) -> i64,
    float_op: impl Fn(f64, f64) -> f64,
) -> crate::types::Value {
    use crate::types::Value;
    match (l, r) {
        (Value::Integer(a), Value::Integer(b)) => Value::Integer(int_op(*a, *b)),
        (Value::Real(a), Value::Real(b)) => Value::Real(float_op(*a, *b)),
        (Value::Integer(a), Value::Real(b)) => Value::Real(float_op(*a as f64, *b)),
        (Value::Real(a), Value::Integer(b)) => Value::Real(float_op(*a, *b as f64)),
        _ => Value::Null,
    }
}

fn chk_truthy(v: &crate::types::Value) -> bool {
    use crate::types::Value;
    !matches!(v, Value::Integer(0) | Value::Null)
}
