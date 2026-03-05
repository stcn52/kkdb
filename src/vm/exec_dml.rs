use super::execute::{ExecResult, VM};
use crate::error::{KkdbError, Result};
use crate::sql::ast::*;
use crate::storage::btree::BTree;
use crate::types::{Row, Value};
use std::collections::{HashMap, HashSet};

impl VM {
    // ---- INSERT ----
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
                    _ => return Err(crate::error::KkdbError::Internal("INSERT SELECT: exec_select did not return rows".into())),
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
                    self.pager.commit_transaction()?;
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

            for (col_idx, col_name) in &not_null_cols {
            if matches!(row[*col_idx], Value::Null) {
                return Err(KkdbError::ConstraintViolation(format!(
                    "NOT NULL constraint failed: {}.{}",
                    table_name_owned, col_name
                )));
            }
        }

        // --- Conflict resolution ---
        match &insert.conflict {
            ConflictPolicy::Error => {
                self.validate_unique_indexes_for_row(&insert.table_name, rowid, &row, None)?;
                let mut btree = BTree::new(&mut self.pager);
                let new_root = btree.insert_with_buf(root_page, rowid, &row, &mut serialize_buf)?;
                root_page = new_root;
                self.insert_index_entries(&insert.table_name, rowid, &row)?;
                rows_inserted += 1;
            }
            ConflictPolicy::Ignore => {
                // Check for conflicts; skip row on any constraint violation
                let conflict = self
                    .validate_unique_indexes_for_row(&insert.table_name, rowid, &row, None)
                    .err()
                    .filter(|e| matches!(e, KkdbError::ConstraintViolation(_)));
                // Also check if rowid already exists (PK conflict)
                let pk_exists = if pk_col_idx.is_some() {
                    let mut btree = BTree::new(&mut self.pager);
                    btree.find_by_rowid(root_page, rowid)?.is_some()
                } else {
                    false
                };
                if conflict.is_none() && !pk_exists {
                    let mut btree = BTree::new(&mut self.pager);
                    let new_root = btree.insert_with_buf(root_page, rowid, &row, &mut serialize_buf)?;
                    root_page = new_root;
                    self.insert_index_entries(&insert.table_name, rowid, &row)?;
                    rows_inserted += 1;
                }
                // else: silently skip this row
            }
            ConflictPolicy::Replace => {
                // Delete existing row with the same rowid (PK) if it exists
                let pk_exists = if pk_col_idx.is_some() {
                    let mut btree = BTree::new(&mut self.pager);
                    btree.find_by_rowid(root_page, rowid)?.is_some()
                } else {
                    false
                };
                if pk_exists {
                    self.delete_index_entries(&insert.table_name, rowid)?;
                    let mut btree = BTree::new(&mut self.pager);
                    let (_, new_root) = btree.delete_by_rowid(root_page, rowid)?;
                    root_page = new_root;
                }
                // Also remove any rows that would conflict on UNIQUE indexes
                let conflict_rowids: Vec<i64> = {
                    let indexes = self.schema.indexes_for_table(&insert.table_name);
                    let mut cids: Vec<i64> = Vec::new();
                    for idx in indexes {
                        let tbl_schema = self.schema.get_table(&insert.table_name)?;
                        if !idx.unique { continue; }
                        let key_vals: Vec<Value> = idx
                            .columns
                            .iter()
                            .filter_map(|c| {
                                tbl_schema.columns.iter().find(|col| col.name.eq_ignore_ascii_case(c))
                                    .map(|col| row[col.col_index].clone())
                            })
                            .collect();
                        // Skip if any key part is NULL (unique index doesn't cover NULLs)
                        if key_vals.iter().any(|v| matches!(v, Value::Null)) { continue; }
                        let key = crate::schema::Schema::index_key(&key_vals);
                        let mut btree = BTree::new(&mut self.pager);
                        let idx_rows = btree.scan_all(idx.root_page)?;
                        for (_, idx_row) in idx_rows {
                            if let Some(Value::Integer(tbl_rowid)) = idx_row.last() {
                                if *tbl_rowid != rowid {
                                    let entry_key = crate::schema::Schema::index_key(
                                        &idx_row[..idx_row.len() - 1]
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
                    self.delete_index_entries(&insert.table_name, conflict_rid)?;
                    let mut btree = BTree::new(&mut self.pager);
                    let (_, new_root) = btree.delete_by_rowid(root_page, conflict_rid)?;
                    root_page = new_root;
                }
                let mut btree = BTree::new(&mut self.pager);
                let new_root = btree.insert_with_buf(root_page, rowid, &row, &mut serialize_buf)?;
                root_page = new_root;
                self.insert_index_entries(&insert.table_name, rowid, &row)?;
                rows_inserted += 1;
            }
            // Batch G: ON CONFLICT DO UPDATE SET col = expr ...
            ConflictPolicy::Update(assignments) => {
                // Find conflicting row by PK using scan_all which returns (rowid, row) pairs
                let pk_idx = pk_col_idx.unwrap_or(0);
                let pk_val = row.get(pk_idx).cloned().unwrap_or(Value::Null);
                let existing = {
                    let mut btree = BTree::new(&mut self.pager);
                    btree.scan_all(root_page)?
                };
                let existing_rowid = existing.iter().find_map(|(id, r)| {
                    if r.get(pk_idx) == Some(&pk_val) { Some(*id) } else { None }
                });
                if let Some(conflict_rowid) = existing_rowid {
                    // Load and update the existing row
                    let mut ex_row = {
                        let mut bt = BTree::new(&mut self.pager);
                        bt.find_by_rowid(root_page, conflict_rowid)?.map(|(_, r)| r).unwrap_or_default()
                    };
                    let col_map_tmp: std::collections::HashMap<String, usize> = {
                        let table_schema = self.schema.get_table(&insert.table_name)?;
                        table_schema.col_names.iter().enumerate()
                            .map(|(i, n)| (n.to_ascii_lowercase(), i))
                            .collect()
                    };
                    for (col_name, expr) in assignments.iter() {
                        if let Ok(idx) = self.schema.find_column(&insert.table_name, col_name) {
                            let new_val = self.eval_expr(expr, &ex_row, &col_map_tmp)?;
                            if idx < ex_row.len() {
                                ex_row[idx] = new_val;
                            }
                        }
                    }
                    let new_root = {
                        let mut btree = BTree::new(&mut self.pager);
                        btree.update_row(root_page, conflict_rowid, &ex_row)?
                    };
                    root_page = new_root;
                } else {
                    // No conflict: plain insert
                    let new_root = {
                        let mut btree = BTree::new(&mut self.pager);
                        btree.insert_with_buf(root_page, rowid, &row, &mut serialize_buf)?
                    };
                    root_page = new_root;
                    self.insert_index_entries(&insert.table_name, rowid, &row)?;
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

        let mut rows_to_update: Vec<(i64, Row)> = Vec::new();

        if let Some(rowids) = index_rowids {
            // Index path: fetch only matching rows by rowid (bulk-capable helper).
            let fetched_rows = self.fetch_rows_by_rowids(original_root, &rowids)?;
            for (rid, row) in fetched_rows {
                let mut new_row = row.clone();
                for &(col_idx, expr) in &assignment_indices {
                    let val = self.eval_expr(expr, &row, &col_map)?;
                    new_row[col_idx] = val;
                }
                rows_to_update.push((rid, new_row));
            }
        } else {
            // Full scan path
            let mut btree = BTree::new(&mut self.pager);
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
                    rows_to_update.push((rowid, new_row));
                }
            }
        }

        let count = rows_to_update.len();
        let mut root_page = original_root;
        let mut serialize_buf: Vec<u8> = Vec::new();

        for (rowid, new_row) in rows_to_update {
            // Update indexes: delete old entry, insert new entry
            self.delete_index_entries(&update.table_name, rowid)?;
            self.validate_unique_indexes_for_row(&update.table_name, rowid, &new_row, Some(rowid))?;

            let mut btree = BTree::new(&mut self.pager);
            let new_root =
                btree.update_row_with_buf(root_page, rowid, &new_row, &mut serialize_buf)?;
            root_page = new_root;

            self.insert_index_entries(&update.table_name, rowid, &new_row)?;
        }

        // Update root page if changed
        if root_page != original_root {
            let table_schema = self.schema.get_table_mut(&update.table_name)?;
            table_schema.root_page = root_page;
            self.update_schema_root_page(&update.table_name, root_page)?;
        }

        self.auto_flush()?;

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
            let mut btree = BTree::new(&mut self.pager);
            let all_rows = btree.scan_all(original_root)?;

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

        for rowid in rowids_to_delete {
            // Delete index entries before deleting the row
            self.delete_index_entries(&delete.table_name, rowid)?;

            let mut btree = BTree::new(&mut self.pager);
            let (_, new_root) = btree.delete_by_rowid(root_page, rowid)?;
            root_page = new_root;
        }

        // Update root page if changed (future: rebalancing may change root)
        if root_page != original_root {
            let table_schema = self.schema.get_table_mut(&delete.table_name)?;
            table_schema.root_page = root_page;
            self.update_schema_root_page(&delete.table_name, root_page)?;
        }

        self.auto_flush()?;

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
            let mut btree = BTree::new(&mut self.pager);
            let idx_next = btree.max_rowid(current_root).unwrap_or(0) + 1;
            let new_root = btree.insert(current_root, idx_next, &index_row)?;

            if let Some(ref first_val) = first_col_value {
                self.index_cache_insert(idx, first_val, rowid, idx_next);
            }

            if new_root != current_root {
                if let Some(schema_idx) = self.schema.indexes.get_mut(&idx.name.to_lowercase()) {
                    schema_idx.root_page = new_root;
                }
                self.update_schema_object_root_page(&idx.name, new_root)?;
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
                let mut btree = BTree::new(&mut self.pager);
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

        let mut btree = BTree::new(&mut self.pager);
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
