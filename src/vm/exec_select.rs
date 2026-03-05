use super::eval_expr::like_match;
use super::execute::{ExecResult, VM};
use crate::error::{KkdbError, Result};
use crate::sql::ast::*;
use crate::storage::btree::BTree;
use crate::types::{Row, Value};
use std::collections::{HashMap, HashSet};

impl VM {
    // ---- SELECT ----
    pub(crate) fn exec_select(&mut self, select_ref: &SelectStmt) -> Result<ExecResult> {
        let mut select_mut = select_ref.clone();
        let mut window_funcs = Vec::new();
        for col in &mut select_mut.columns {
            if let SelectColumn::Expr { expr, .. } = col {
                Self::extract_window_funcs(expr, &mut window_funcs);
            }
        }
        for item in &mut select_mut.order_by {
            Self::extract_window_funcs(&mut item.expr, &mut window_funcs);
        }
        let select = &select_mut;

        // Check if LIMIT pushdown is safe:
        // no WHERE, no ORDER BY, no GROUP BY, no DISTINCT, no HAVING, simple table FROM
        let limit_pushdown = select.where_clause.is_none()
            && select.order_by.is_empty()
            && select.group_by.is_empty()
            && !select.distinct
            && select.having.is_none()
            && select.limit.is_some()
            && !Self::has_aggregate(&select.columns);

        // Get the source rows
        let (mut rows, col_names) = if let Some(ref from) = select.from {
            if limit_pushdown {
                // LIMIT pushdown: scan only as many rows as needed
                if let FromClause::Table { name, .. } = from {
                    let empty = Vec::new();
                    let empty_map = HashMap::new();
                    let limit_val =
                        match self.eval_expr(select.limit.as_ref().unwrap(), &empty, &empty_map)? {
                            Value::Integer(v) if v >= 0 => v as usize,
                            _ => usize::MAX,
                        };
                    let offset_val = if let Some(ref offset_expr) = select.offset {
                        match self.eval_expr(offset_expr, &empty, &empty_map)? {
                            Value::Integer(v) if v > 0 => v as usize,
                            _ => 0,
                        }
                    } else {
                        0
                    };
                    let total_needed = limit_val.saturating_add(offset_val);
                    let (col_names, root_page) = {
                        let table = self.schema.get_table(name)?;
                        (table.col_names.clone(), table.root_page)
                    };
                    let mut btree = BTree::new(&mut self.pager);
                    let rows = btree.scan_rows_limit(root_page, total_needed)?;
                    (rows, col_names)
                } else {
                    self.eval_from(from)?
                }
            } else if let Some(ref where_expr) = select.where_clause {
                // Try index-accelerated scan for simple WHERE conditions
                if let Some(result) = self.try_index_scan(from, where_expr)? {
                    (result.0, result.1)
                } else {
                    self.eval_from(from)?
                }
            } else {
                self.eval_from(from)?
            }
        } else {
            // SELECT without FROM - single row with no columns
            (vec![Vec::new()], Vec::new())
        };

        // Build column name mapping (avoid to_lowercase heap alloc)
        let mut col_map: HashMap<String, usize> = HashMap::with_capacity(col_names.len());
        for (i, name) in col_names.iter().enumerate() {
            if name.bytes().any(|b| b.is_ascii_uppercase()) {
                let mut lower = String::with_capacity(name.len());
                for b in name.bytes() {
                    lower.push(b.to_ascii_lowercase() as char);
                }
                col_map.insert(lower, i);
            } else {
                col_map.insert(name.clone(), i);
            }
        }

        // Inject table-qualified names (e.g. "t1.id") for correct column resolution in JOINs
        if let Some(ref from) = select.from {
            let mut offset = 0;
            self.inject_qualified_names(from, &col_names, &mut col_map, &mut offset);
        }

        // Apply WHERE filter (may be partially or fully satisfied by index scan,
        // but we re-apply to handle any conditions the index didn't cover)
        if let Some(ref where_expr) = select.where_clause {
            let mut filtered = Vec::with_capacity(rows.len());
            for row in rows {
                let val = self.eval_expr(where_expr, &row, &col_map)?;
                if val.is_truthy() {
                    filtered.push(row);
                }
            }
            rows = filtered;
        }

        // For simple (non-aggregate, non-group-by) queries, sort source rows BEFORE projection.
        // This lets ORDER BY reference any column in the source table (not just selected ones).
        // For aggregate/group-by, we sort output_rows after projection as before.
        let pre_sort = !select.order_by.is_empty()
            && select.group_by.is_empty()
            && !Self::has_aggregate(&select.columns);

        if pre_sort {
            let order_exprs: Vec<(&Expr, bool)> = select
                .order_by
                .iter()
                .map(|item| (&item.expr, item.ascending))
                .collect();
            let top_n_limit = if let Some(ref limit_expr) = select.limit {
                match self.eval_expr(limit_expr, &Vec::new(), &HashMap::new())? {
                    Value::Integer(v) if v >= 0 => {
                        let offset = if let Some(ref off_expr) = select.offset {
                            match self.eval_expr(off_expr, &Vec::new(), &HashMap::new())? {
                                Value::Integer(v) if v > 0 => v as usize,
                                _ => 0,
                            }
                        } else { 0 };
                        Some((v as usize).saturating_add(offset))
                    }
                    _ => None,
                }
            } else { None };

            // Build sort keys (evaluate ORDER BY expressions against source rows).
            // We materialise keys once per row to avoid repeated evaluation in the comparator.
            let mut keyed: Vec<(Vec<Value>, Row)> = rows
                .into_iter()
                .map(|row| {
                    let keys: Vec<Value> = order_exprs
                        .iter()
                        .map(|(expr, _)| self.eval_expr(expr, &row, &col_map).unwrap_or(Value::Null))
                        .collect();
                    (keys, row)
                })
                .collect();

            let ascending_flags: Vec<bool> = order_exprs.iter().map(|(_, asc)| *asc).collect();

            let cmp_fn = |a: &(Vec<Value>, Row), b: &(Vec<Value>, Row)| {
                for (i, &asc) in ascending_flags.iter().enumerate() {
                    let ord = a.0[i].partial_cmp(&b.0[i]).unwrap_or(std::cmp::Ordering::Equal);
                    if ord != std::cmp::Ordering::Equal {
                        return if asc { ord } else { ord.reverse() };
                    }
                }
                std::cmp::Ordering::Equal
            };

            if let Some(k) = top_n_limit {
                if k == 0 {
                    keyed.clear();
                } else if k < keyed.len() {
                    keyed.select_nth_unstable_by(k - 1, cmp_fn);
                    keyed.truncate(k);
                    keyed.sort_by(cmp_fn);
                } else {
                    keyed.sort_by(cmp_fn);
                }
            } else {
                keyed.sort_by(cmp_fn);
            }

            rows = keyed.into_iter().map(|(_, r)| r).collect();
        }

        let (output_col_names, mut output_rows) = if !select.group_by.is_empty() {
            self.apply_group_by(
                &rows,
                &select.group_by,
                &select.columns,
                &col_names,
                &col_map,
                &select.having,
                &window_funcs,
                &select.window_defs,
            )?
        } else if Self::has_aggregate(&select.columns) {
            // Implicit aggregation: no GROUP BY but SELECT contains aggregates
            // Treat all rows as a single group, return one output row
            self.apply_implicit_aggregate(&rows, &select.columns, &col_names, &col_map, &window_funcs, &select.window_defs)?
        } else {
            self.project_columns(&select.columns, rows, &col_names, &col_map, false, &window_funcs, &select.window_defs)?
        };

        // Apply DISTINCT
        if select.distinct {
            let mut seen = std::collections::HashSet::new();
            let mut key_buf = String::with_capacity(128);
            let mut val_buf = String::with_capacity(32);
            output_rows = output_rows
                .into_iter()
                .filter(|row| {
                    key_buf.clear();
                    for v in row {
                        Self::typed_key_into(v, &mut val_buf);
                        key_buf.push_str(&val_buf);
                        key_buf.push('\0');
                    }
                    seen.insert(key_buf.clone())
                })
                .collect();
        }

        // Apply ORDER BY (aggregate/group-by path only — simple SELECT was sorted pre-projection)
        if !select.order_by.is_empty() && !pre_sort {
            let output_col_map: HashMap<String, usize> = output_col_names
                .iter()
                .enumerate()
                .map(|(i, name)| (name.to_lowercase(), i))
                .collect();

            let order_items: Vec<(usize, bool, Option<bool>)> = select
                .order_by
                .iter()
                .filter_map(|item| {
                    if let Expr::ColumnRef { column, .. } = &item.expr {
                        let lower = column.to_ascii_lowercase();
                        output_col_map
                            .get(lower.as_str())
                            .or_else(|| col_map.get(lower.as_str()))
                            .map(|&idx| (idx, item.ascending, item.nulls_first))
                    } else {
                        None
                    }
                })
                .collect();

            let top_n_limit = if let Some(ref limit_expr) = select.limit {
                let limit_val = match self.eval_expr(limit_expr, &Vec::new(), &HashMap::new())? {
                    Value::Integer(v) if v >= 0 => Some(v as usize),
                    _ => None,
                };
                if let Some(limit) = limit_val {
                    let offset = if let Some(ref offset_expr) = select.offset {
                        match self.eval_expr(offset_expr, &Vec::new(), &HashMap::new())? {
                            Value::Integer(v) if v > 0 => v as usize,
                            _ => 0,
                        }
                    } else {
                        0
                    };
                    Some(limit.saturating_add(offset))
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(k) = top_n_limit {
                if k == 0 {
                    output_rows.clear();
                } else if k < output_rows.len() {
                    output_rows.select_nth_unstable_by(k - 1, |a, b| {
                        Self::compare_rows_by_order_items(a, b, &order_items)
                    });
                    output_rows.truncate(k);
                    output_rows
                        .sort_by(|a, b| Self::compare_rows_by_order_items(a, b, &order_items));
                } else {
                    output_rows
                        .sort_by(|a, b| Self::compare_rows_by_order_items(a, b, &order_items));
                }
            } else {
                output_rows.sort_by(|a, b| Self::compare_rows_by_order_items(a, b, &order_items));
            }
        }

        // Apply OFFSET
        if let Some(ref offset_expr) = select.offset {
            let empty_row: Vec<Value> = Vec::new();
            let empty_map: HashMap<String, usize> = HashMap::new();
            let offset = match self.eval_expr(offset_expr, &empty_row, &empty_map)? {
                Value::Integer(v) if v > 0 => v as usize,
                _ => 0,
            };
            if offset < output_rows.len() {
                output_rows = output_rows.split_off(offset);
            } else {
                output_rows.clear();
            }
        }

        // Apply LIMIT
        if let Some(ref limit_expr) = select.limit {
            let limit = match self.eval_expr(limit_expr, &Vec::new(), &HashMap::new())? {
                Value::Integer(v) if v >= 0 => v as usize,
                _ => output_rows.len(), // negative LIMIT = no limit (SQLite behavior)
            };
            output_rows.truncate(limit);
        }

        Ok(ExecResult::QueryResult {
            columns: output_col_names,
            rows: output_rows,
        })
    }

    /// Write a type-prefixed string key for a Value into an existing buffer (zero-alloc)
    #[inline]
    pub(crate) fn typed_key_into(v: &Value, buf: &mut String) {
        buf.clear();
        match v {
            Value::Null => buf.push('N'),
            Value::Integer(i) => {
                buf.push('I');
                use std::fmt::Write;
                let _ = write!(buf, "{}", i);
            }
            Value::Text(t) => {
                buf.push('T');
                buf.push_str(t);
            }
            Value::Real(r) => {
                buf.push('R');
                use std::fmt::Write;
                let _ = write!(buf, "{}", r);
            }
            Value::Blob(b) => {
                buf.push('B');
                for byte in b {
                    use std::fmt::Write;
                    let _ = write!(buf, "{:02x}", byte);
                }
            }
        }
    }

    #[inline]
    fn compare_rows_by_order_items(
        a: &[Value],
        b: &[Value],
        order_items: &[(usize, bool, Option<bool>)],
    ) -> std::cmp::Ordering {
        for &(col_idx, ascending, nulls_first) in order_items {
            if col_idx < a.len() && col_idx < b.len() {
                let v1 = &a[col_idx];
                let v2 = &b[col_idx];
                
                let v1_is_null = matches!(v1, Value::Null);
                let v2_is_null = matches!(v2, Value::Null);
                
                if v1_is_null || v2_is_null {
                    if v1_is_null && v2_is_null {
                        continue;
                    }
                    // default: ascending -> nulls last (if !ascending -> nulls first)
                    let nf = nulls_first.unwrap_or(!ascending);
                    if v1_is_null {
                        return if nf { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
                    } else {
                        return if nf { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less };
                    }
                }
                
                let cmp = v1.partial_cmp(v2);
                match cmp {
                    Some(std::cmp::Ordering::Equal) => continue,
                    Some(ord) => return if ascending { ord } else { ord.reverse() },
                    None => continue,
                }
            }
        }
        std::cmp::Ordering::Equal
    }

    /// Evaluate FROM clause - returns (rows, column_names)
    fn eval_from(&mut self, from: &FromClause) -> Result<(Vec<Row>, Vec<String>)> {
        match from {
            FromClause::Table { name, alias: _ } => {
                // Batch E: if this is a view, expand by calling its stored SELECT
                let (is_view, view_query) = {
                    if let Ok(t) = self.schema.get_table(name) {
                        if let Some(ref qs) = t.view_select {
                            (true, Some(qs.clone()))
                        } else {
                            (false, None)
                        }
                    } else {
                        (false, None)
                    }
                };
                if is_view {
                    if let Some(q) = view_query {
                        let result = self.exec_select(&q)?;
                        return match result {
                            ExecResult::QueryResult { columns, rows } => Ok((rows, columns)),
                            _ => Err(KkdbError::Internal("view did not return rows".into())),
                        };
                    }
                }

                let (col_names, root_page) = {
                    let table = self.schema.get_table(name)?;
                    (table.col_names.clone(), table.root_page)
                };

                let mut btree = BTree::new(&mut self.pager);
                let rows = btree.scan_rows(root_page)?;

                Ok((rows, col_names))
            }
            FromClause::Join {
                left,
                join_type,
                right,
                on,
            } => {
                let (left_rows, left_cols) = self.eval_from(left)?;
                let (right_rows, right_cols) = self.eval_from(right)?;

                let mut combined_cols = Vec::with_capacity(left_cols.len() + right_cols.len());
                combined_cols.extend_from_slice(&left_cols);
                combined_cols.extend_from_slice(&right_cols);

                let mut combined_col_map: HashMap<String, usize> =
                    HashMap::with_capacity(combined_cols.len());
                for (i, name) in combined_cols.iter().enumerate() {
                    if name.bytes().any(|b| b.is_ascii_uppercase()) {
                        let mut lower = String::with_capacity(name.len());
                        for b in name.bytes() {
                            lower.push(b.to_ascii_lowercase() as char);
                        }
                        combined_col_map.insert(lower, i);
                    } else {
                        combined_col_map.insert(name.clone(), i);
                    }
                }

                // Inject table-qualified names for correct column resolution in ON conditions
                let mut offset = 0;
                self.inject_qualified_names(
                    left,
                    &combined_cols,
                    &mut combined_col_map,
                    &mut offset,
                );
                self.inject_qualified_names(
                    right,
                    &combined_cols,
                    &mut combined_col_map,
                    &mut offset,
                );

                let combined_width = left_cols.len() + right_cols.len();
                let mut result_rows = Vec::new();

                // Try hash join for equi-join conditions (ON a.col = b.col)
                let equi_join_indices =
                    Self::detect_equi_join(on.as_ref(), &combined_col_map, left_cols.len());

                match join_type {
                    JoinType::Inner | JoinType::Cross => {
                        if let Some((left_idx, right_idx)) = equi_join_indices {
                            // Hash JOIN: build hash on right, probe with left — O(n+m)
                            let mut key_buf = String::with_capacity(32);
                            let mut hash: HashMap<String, Vec<usize>> =
                                HashMap::with_capacity(right_rows.len());
                            for (ri, right_row) in right_rows.iter().enumerate() {
                                if matches!(right_row[right_idx], Value::Null) {
                                    continue;
                                }
                                Self::typed_key_into(&right_row[right_idx], &mut key_buf);
                                hash.entry(key_buf.clone())
                                    .or_insert_with(Vec::new)
                                    .push(ri);
                            }
                            for left_row in &left_rows {
                                if matches!(left_row[left_idx], Value::Null) {
                                    continue;
                                }
                                Self::typed_key_into(&left_row[left_idx], &mut key_buf);
                                if let Some(indices) = hash.get(key_buf.as_str()) {
                                    for &ri in indices {
                                        let mut combined = Vec::with_capacity(combined_width);
                                        combined.extend_from_slice(left_row);
                                        combined.extend_from_slice(&right_rows[ri]);
                                        result_rows.push(combined);
                                    }
                                }
                            }
                        } else {
                            for left_row in &left_rows {
                                for right_row in &right_rows {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(left_row);
                                    combined.extend_from_slice(right_row);
                                    if let Some(ref on_expr) = on {
                                        let val =
                                            self.eval_expr(on_expr, &combined, &combined_col_map)?;
                                        if val.is_truthy() {
                                            result_rows.push(combined);
                                        }
                                    } else {
                                        result_rows.push(combined);
                                    }
                                }
                            }
                        }
                    }
                    JoinType::Left => {
                        let null_right: Vec<Value> = vec![Value::Null; right_cols.len()];
                        if let Some((left_idx, right_idx)) = equi_join_indices {
                            let mut key_buf = String::with_capacity(32);
                            let mut hash: HashMap<String, Vec<usize>> =
                                HashMap::with_capacity(right_rows.len());
                            for (ri, right_row) in right_rows.iter().enumerate() {
                                if matches!(right_row[right_idx], Value::Null) {
                                    continue;
                                }
                                Self::typed_key_into(&right_row[right_idx], &mut key_buf);
                                hash.entry(key_buf.clone())
                                    .or_insert_with(Vec::new)
                                    .push(ri);
                            }
                            for left_row in &left_rows {
                                if matches!(left_row[left_idx], Value::Null) {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(left_row);
                                    combined.extend_from_slice(&null_right);
                                    result_rows.push(combined);
                                    continue;
                                }
                                Self::typed_key_into(&left_row[left_idx], &mut key_buf);
                                if let Some(indices) = hash.get(key_buf.as_str()) {
                                    for &ri in indices {
                                        let mut combined = Vec::with_capacity(combined_width);
                                        combined.extend_from_slice(left_row);
                                        combined.extend_from_slice(&right_rows[ri]);
                                        result_rows.push(combined);
                                    }
                                } else {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(left_row);
                                    combined.extend_from_slice(&null_right);
                                    result_rows.push(combined);
                                }
                            }
                        } else {
                            for left_row in &left_rows {
                                let mut matched = false;
                                for right_row in &right_rows {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(left_row);
                                    combined.extend_from_slice(right_row);
                                    if let Some(ref on_expr) = on {
                                        let val =
                                            self.eval_expr(on_expr, &combined, &combined_col_map)?;
                                        if val.is_truthy() {
                                            result_rows.push(combined);
                                            matched = true;
                                        }
                                    } else {
                                        result_rows.push(combined);
                                        matched = true;
                                    }
                                }
                                if !matched {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(left_row);
                                    combined.extend_from_slice(&null_right);
                                    result_rows.push(combined);
                                }
                            }
                        }
                    }
                    JoinType::Right => {
                        let null_left: Vec<Value> = vec![Value::Null; left_cols.len()];
                        if let Some((left_idx, right_idx)) = equi_join_indices {
                            let mut key_buf = String::with_capacity(32);
                            let mut hash: HashMap<String, Vec<usize>> =
                                HashMap::with_capacity(left_rows.len());
                            for (li, left_row) in left_rows.iter().enumerate() {
                                if matches!(left_row[left_idx], Value::Null) {
                                    continue;
                                }
                                Self::typed_key_into(&left_row[left_idx], &mut key_buf);
                                hash.entry(key_buf.clone())
                                    .or_insert_with(Vec::new)
                                    .push(li);
                            }
                            for right_row in &right_rows {
                                if matches!(right_row[right_idx], Value::Null) {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(&null_left);
                                    combined.extend_from_slice(right_row);
                                    result_rows.push(combined);
                                    continue;
                                }
                                Self::typed_key_into(&right_row[right_idx], &mut key_buf);
                                if let Some(indices) = hash.get(key_buf.as_str()) {
                                    for &li in indices {
                                        let mut combined = Vec::with_capacity(combined_width);
                                        combined.extend_from_slice(&left_rows[li]);
                                        combined.extend_from_slice(right_row);
                                        result_rows.push(combined);
                                    }
                                } else {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(&null_left);
                                    combined.extend_from_slice(right_row);
                                    result_rows.push(combined);
                                }
                            }
                        } else {
                            for right_row in &right_rows {
                                let mut matched = false;
                                for left_row in &left_rows {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(left_row);
                                    combined.extend_from_slice(right_row);
                                    if let Some(ref on_expr) = on {
                                        let val =
                                            self.eval_expr(on_expr, &combined, &combined_col_map)?;
                                        if val.is_truthy() {
                                            result_rows.push(combined);
                                            matched = true;
                                        }
                                    } else {
                                        result_rows.push(combined);
                                        matched = true;
                                    }
                                }
                                if !matched {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(&null_left);
                                    combined.extend_from_slice(right_row);
                                    result_rows.push(combined);
                                }
                            }
                        }
                    }
                    JoinType::LeftSemi => {
                        // LeftSemi: return left row if it matches AT LEAST ONE right row. Do not join right columns.
                        if let Some((left_idx, right_idx)) = equi_join_indices {
                            let mut key_buf = String::with_capacity(32);
                            let mut hash: HashMap<String, ()> = HashMap::with_capacity(right_rows.len());
                            for right_row in right_rows.iter() {
                                if matches!(right_row[right_idx], Value::Null) { continue; }
                                Self::typed_key_into(&right_row[right_idx], &mut key_buf);
                                hash.insert(key_buf.clone(), ());
                            }
                            for left_row in &left_rows {
                                if matches!(left_row[left_idx], Value::Null) { continue; }
                                Self::typed_key_into(&left_row[left_idx], &mut key_buf);
                                if hash.contains_key(key_buf.as_str()) {
                                    result_rows.push(left_row.clone());
                                }
                            }
                        } else {
                            for left_row in &left_rows {
                                for right_row in &right_rows {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(left_row);
                                    combined.extend_from_slice(right_row);
                                    if let Some(ref on_expr) = on {
                                        let val = self.eval_expr(on_expr, &combined, &combined_col_map)?;
                                        if val.is_truthy() {
                                            result_rows.push(left_row.clone());
                                            break; // only need one match for Semi
                                        }
                                    } else {
                                        result_rows.push(left_row.clone());
                                        break;
                                    }
                                }
                            }
                        }
                        // For semi joins, the result schema is ONLY the left side
                        combined_cols.truncate(left_cols.len());
                    }
                    JoinType::RightSemi => {
                        // RightSemi: return right row if it matches AT LEAST ONE left row. Do not join left columns.
                        if let Some((left_idx, right_idx)) = equi_join_indices {
                            let mut key_buf = String::with_capacity(32);
                            let mut hash: HashMap<String, ()> = HashMap::with_capacity(left_rows.len());
                            for left_row in left_rows.iter() {
                                if matches!(left_row[left_idx], Value::Null) { continue; }
                                Self::typed_key_into(&left_row[left_idx], &mut key_buf);
                                hash.insert(key_buf.clone(), ());
                            }
                            for right_row in &right_rows {
                                if matches!(right_row[right_idx], Value::Null) { continue; }
                                Self::typed_key_into(&right_row[right_idx], &mut key_buf);
                                if hash.contains_key(key_buf.as_str()) {
                                    result_rows.push(right_row.clone());
                                }
                            }
                        } else {
                            for right_row in &right_rows {
                                for left_row in &left_rows {
                                    let mut combined = Vec::with_capacity(combined_width);
                                    combined.extend_from_slice(left_row);
                                    combined.extend_from_slice(right_row);
                                    if let Some(ref on_expr) = on {
                                        let val = self.eval_expr(on_expr, &combined, &combined_col_map)?;
                                        if val.is_truthy() {
                                            result_rows.push(right_row.clone());
                                            break; 
                                        }
                                    } else {
                                        result_rows.push(right_row.clone());
                                        break;
                                    }
                                }
                            }
                        }
                        // For RightSemi, the result schema is ONLY the right side
                        combined_cols = right_cols.clone();
                    }
                    // Batch C: FULL OUTER JOIN = LEFT JOIN rows + right-side rows not matched
                    JoinType::Full => {
                        let null_left: Vec<Value> = vec![Value::Null; left_cols.len()];
                        let null_right: Vec<Value> = vec![Value::Null; right_cols.len()];
                        let mut right_matched = vec![false; right_rows.len()];
                        for left_row in &left_rows {
                            let mut matched = false;
                            for (ri, right_row) in right_rows.iter().enumerate() {
                                let mut combined = Vec::with_capacity(combined_width);
                                combined.extend_from_slice(left_row);
                                combined.extend_from_slice(right_row);
                                let include = if let Some(ref cond) = on {
                                    self.eval_expr(cond, &combined, &combined_col_map)?.is_truthy()
                                } else { true };
                                if include {
                                    result_rows.push(combined);
                                    matched = true;
                                    right_matched[ri] = true;
                                }
                            }
                            if !matched {
                                let mut combined = Vec::with_capacity(combined_width);
                                combined.extend_from_slice(left_row);
                                combined.extend_from_slice(&null_right);
                                result_rows.push(combined);
                            }
                        }
                        for (ri, matched) in right_matched.iter().enumerate() {
                            if !matched {
                                let mut combined = Vec::with_capacity(combined_width);
                                combined.extend_from_slice(&null_left);
                                combined.extend_from_slice(&right_rows[ri]);
                                result_rows.push(combined);
                            }
                        }
                    }
                    // Batch C: NATURAL JOIN — join on all common column names
                    JoinType::Natural => {
                        let common_cols: Vec<(usize, usize)> = left_cols.iter().enumerate()
                            .filter_map(|(li, lname)| {
                                right_cols.iter().position(|rname| {
                                    rname.eq_ignore_ascii_case(lname)
                                }).map(|ri| (li, ri))
                            })
                            .collect();
                        for left_row in &left_rows {
                            'outer: for right_row in &right_rows {
                                for (li, ri) in &common_cols {
                                    if left_row[*li] != right_row[*ri] { continue 'outer; }
                                }
                                let mut combined = Vec::with_capacity(combined_width);
                                combined.extend_from_slice(left_row);
                                combined.extend_from_slice(right_row);
                                result_rows.push(combined);
                            }
                        }
                    }
                }

                Ok((result_rows, combined_cols))
            }
            FromClause::Subquery { query, alias: _ } => {
                let result = self.exec_select(query)?;
                match result {
                    ExecResult::QueryResult { columns, rows } => Ok((rows, columns)),
                    _ => Err(KkdbError::Internal("subquery did not return rows".into())),
                }
            }
            // Batch B: nested UNION / INTERSECT / EXCEPT used as a FROM source
            FromClause::SetOp { stmt, alias: _ } => {
                let result = self.exec_set_op(stmt)?;
                match result {
                    ExecResult::QueryResult { columns, rows } => Ok((rows, columns)),
                    _ => Err(KkdbError::Internal("set-op did not return rows".into())),
                }
            }
            // #15: Table-valued functions — UNNEST / generate_series
            FromClause::TableFunction { name, args, alias, column } => {
                let empty_row: Vec<Value> = Vec::new();
                let empty_map: HashMap<String, usize> = HashMap::new();

                // Evaluate all args up front
                let mut eval_args = Vec::with_capacity(args.len());
                for a in args {
                    eval_args.push(self.eval_expr(a, &empty_row, &empty_map)?);
                }

                let func_upper = name.to_ascii_uppercase();
                let col_name = column
                    .as_deref()
                    .unwrap_or_else(|| match func_upper.as_str() {
                        "UNNEST" => "unnest",
                        "GENERATE_SERIES" => "generate_series",
                        _ => "value",
                    })
                    .to_string();

                // Optional alias-qualified column name for the col_map (t.col)
                let _qualified = alias.as_ref().map(|a| format!("{}.{}", a.to_lowercase(), col_name.to_lowercase()));

                let rows: Vec<Row> = match func_upper.as_str() {
                    "UNNEST" => {
                        // UNNEST(array_value) — expand a JSON array or comma-separated list into rows
                        let arr_val = eval_args.into_iter().next().unwrap_or(Value::Null);
                        Self::unnest_value(arr_val)
                            .into_iter().map(|v| vec![v]).collect()
                    }
                    "GENERATE_SERIES" => {
                        // generate_series(start, stop[, step])
                        let start = match eval_args.get(0) {
                            Some(Value::Integer(v)) => *v,
                            Some(Value::Real(v)) => *v as i64,
                            _ => return Err(KkdbError::RuntimeError("generate_series: start must be integer".into())),
                        };
                        let stop = match eval_args.get(1) {
                            Some(Value::Integer(v)) => *v,
                            Some(Value::Real(v)) => *v as i64,
                            _ => return Err(KkdbError::RuntimeError("generate_series: stop must be integer".into())),
                        };
                        let step = match eval_args.get(2) {
                            Some(Value::Integer(v)) => *v,
                            Some(Value::Real(v)) => *v as i64,
                            None => 1,
                            _ => 1,
                        };
                        if step == 0 {
                            return Err(KkdbError::RuntimeError("generate_series: step must not be zero".into()));
                        }
                        let mut rows = Vec::new();
                        let mut cur = start;
                        // Cap at 1 million rows for safety
                        while (step > 0 && cur <= stop) || (step < 0 && cur >= stop) {
                            rows.push(vec![Value::Integer(cur)]);
                            cur = match cur.checked_add(step) {
                                Some(v) => v,
                                None => break,
                            };
                            if rows.len() >= 1_000_000 { break; }
                        }
                        rows
                    }
                    other => {
                        return Err(KkdbError::RuntimeError(format!(
                            "unsupported table function `{other}` (supported: UNNEST, generate_series)"
                        )));
                    }
                };

                let col_names = vec![col_name];
                Ok((rows, col_names))
            }
        }
    }

    /// Expand a Value into a list of values for UNNEST:
    /// - JSON array `[1, 2, 3]` → elements
    /// - Comma-separated string `a,b,c` → trimmed parts
    /// - Any scalar → single-element list
    fn unnest_value(val: Value) -> Vec<Value> {
        match val {
            Value::Text(s) => {
                let trimmed = s.trim();
                if trimmed.starts_with('[') && trimmed.ends_with(']') {
                    // Parse JSON array: simple tokenizer (handles strings, numbers, null, bool)
                    let inner = &trimmed[1..trimmed.len() - 1];
                    let mut result = Vec::new();
                    let mut chars = inner.chars().peekable();
                    let mut token = String::new();
                    let mut in_string = false;
                    let mut escape = false;
                    loop {
                        match chars.next() {
                            None => {
                                let t = token.trim();
                                if !t.is_empty() {
                                    result.push(Self::parse_json_scalar(t));
                                }
                                break;
                            }
                            Some('\\') if in_string => { escape = true; token.push('\\'); }
                            Some(c) if escape => { escape = false; token.push(c); }
                            Some('"') => { in_string = !in_string; token.push('"'); }
                            Some(',') if !in_string => {
                                let t = token.trim();
                                if !t.is_empty() {
                                    result.push(Self::parse_json_scalar(t));
                                }
                                token.clear();
                            }
                            Some(c) => { token.push(c); }
                        }
                    }
                    result
                } else {
                    // CSV: split on commas
                    s.split(',').map(|p| Value::Text(p.trim().to_string().into())).collect()
                }
            }
            Value::Null => Vec::new(),
            scalar => vec![scalar],
        }
    }

    /// Parse a single JSON scalar token to a Value.
    fn parse_json_scalar(s: &str) -> Value {
        let s = s.trim();
        if s == "null" { return Value::Null; }
        if s == "true" { return Value::Integer(1); }
        if s == "false" { return Value::Integer(0); }
        // Quoted string
        if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
            return Value::Text(s[1..s.len()-1].replace("\\\"", "\"").into());
        }
        // Number
        if let Ok(i) = s.parse::<i64>() { return Value::Integer(i); }
        if let Ok(f) = s.parse::<f64>() { return Value::Real(f); }
        Value::Text(s.to_string().into())
    }

    /// Walk the FROM tree and inject "table.column" qualified entries into col_map.
    /// This enables table-qualified column references like `t1.id` in JOINs.
    fn inject_qualified_names(
        &self,
        from: &FromClause,
        col_names: &[String],
        col_map: &mut HashMap<String, usize>,
        offset: &mut usize,
    ) {
        match from {
            FromClause::Table { name, alias } => {
                if let Ok(table) = self.schema.get_table(name) {
                    let ncols = table.columns.len();
                    let mut buf = String::with_capacity(name.len() + 32);
                    for i in 0..ncols {
                        let idx = *offset + i;
                        if idx < col_names.len() {
                            // Build "name.col" without format!
                            buf.clear();
                            for b in name.bytes() {
                                buf.push(b.to_ascii_lowercase() as char);
                            }
                            buf.push('.');
                            for b in col_names[idx].bytes() {
                                buf.push(b.to_ascii_lowercase() as char);
                            }
                            col_map.insert(buf.clone(), idx);
                            if let Some(a) = alias {
                                buf.clear();
                                for b in a.bytes() {
                                    buf.push(b.to_ascii_lowercase() as char);
                                }
                                buf.push('.');
                                for b in col_names[idx].bytes() {
                                    buf.push(b.to_ascii_lowercase() as char);
                                }
                                col_map.insert(buf.clone(), idx);
                            }
                        }
                    }
                    *offset += ncols;
                }
            }
            FromClause::Join { left, right, .. } => {
                self.inject_qualified_names(left, col_names, col_map, offset);
                self.inject_qualified_names(right, col_names, col_map, offset);
            }
            FromClause::Subquery { query, alias } => {
                if let Some(ncols) = self.count_select_output_columns(query) {
                    let mut buf = String::with_capacity(alias.len() + 32);
                    for i in 0..ncols {
                        let idx = *offset + i;
                        if idx < col_names.len() {
                            buf.clear();
                            for b in alias.bytes() {
                                buf.push(b.to_ascii_lowercase() as char);
                            }
                            buf.push('.');
                            for b in col_names[idx].bytes() {
                                buf.push(b.to_ascii_lowercase() as char);
                            }
                            col_map.insert(buf.clone(), idx);
                        }
                    }
                    *offset += ncols;
                }
            }
            // Batch B: SetOp - columns unknown statically, skip injection
            FromClause::SetOp { .. } => {}
            // TableFunction: 1 column, qualified as alias.col
            FromClause::TableFunction { alias, column, name, .. } => {
                let col_label = column
                    .as_deref()
                    .unwrap_or_else(|| match name.to_ascii_uppercase().as_str() {
                        "UNNEST" => "unnest",
                        "GENERATE_SERIES" => "generate_series",
                        _ => "value",
                    });
                let table_ref = alias.as_deref().unwrap_or(name.as_str());
                if *offset < col_names.len() {
                    col_map.insert(
                        format!("{}.{}", table_ref.to_lowercase(), col_label.to_lowercase()),
                        *offset,
                    );
                }
                *offset += 1;
            }
        }
    }

    /// Count the number of output columns a SELECT statement would produce (statically).
    fn count_select_output_columns(&self, query: &SelectStmt) -> Option<usize> {
        let mut count = 0;
        for col in &query.columns {
            match col {
                SelectColumn::Expr { .. } => count += 1,
                SelectColumn::AllColumns => {
                    count += self.count_from_columns(query.from.as_ref()?)?;
                }
                SelectColumn::TableAllColumns(table) => {
                    count += self.schema.get_table(table).ok()?.columns.len();
                }
            }
        }
        Some(count)
    }

    /// Count total columns produced by a FROM clause (statically).
    fn count_from_columns(&self, from: &FromClause) -> Option<usize> {
        match from {
            FromClause::Table { name, .. } => Some(self.schema.get_table(name).ok()?.columns.len()),
            FromClause::Join { left, right, .. } => {
                Some(self.count_from_columns(left)? + self.count_from_columns(right)?)
            }
            FromClause::Subquery { query, .. } => self.count_select_output_columns(query),
            // Batch B: SetOp column count unknown statically
            FromClause::SetOp { .. } => None,
            // TableFunction always emits exactly 1 column
            FromClause::TableFunction { .. } => Some(1),
        }
    }

    /// Returns column indices belonging to a given table name, using qualified names in col_map.
    /// Returns None if no qualified names found (fallback to all columns).
    fn table_column_indices(
        table: &str,
        col_names: &[String],
        col_map: &HashMap<String, usize>,
    ) -> Option<Vec<usize>> {
        let mut buf = String::with_capacity(table.len() + 32);
        let mut indices = Vec::new();
        for (idx, name) in col_names.iter().enumerate() {
            buf.clear();
            for b in table.bytes() {
                buf.push(b.to_ascii_lowercase() as char);
            }
            buf.push('.');
            for b in name.bytes() {
                buf.push(b.to_ascii_lowercase() as char);
            }
            if col_map.get(buf.as_str()) == Some(&idx) {
                indices.push(idx);
            }
        }
        if indices.is_empty() {
            None
        } else {
            Some(indices)
        }
    }

    /// Detect if a JOIN ON condition is a simple equi-join (col_a = col_b).
    /// Returns (left_col_index, right_col_index_in_right_table) if detected.
    fn detect_equi_join(
        on: Option<&Expr>,
        col_map: &HashMap<String, usize>,
        left_col_count: usize,
    ) -> Option<(usize, usize)> {
        let on_expr = on?;
        if let Expr::BinaryOp {
            left,
            op: BinaryOperator::Equal,
            right,
        } = on_expr
        {
            if let (
                Expr::ColumnRef {
                    table: lt,
                    column: lc,
                },
                Expr::ColumnRef {
                    table: rt,
                    column: rc,
                },
            ) = (left.as_ref(), right.as_ref())
            {
                // Try qualified lookup first, then plain name
                let mut buf = String::with_capacity(32);
                let li = lt
                    .as_ref()
                    .and_then(|t| {
                        buf.clear();
                        for b in t.bytes() {
                            buf.push(b.to_ascii_lowercase() as char);
                        }
                        buf.push('.');
                        for b in lc.bytes() {
                            buf.push(b.to_ascii_lowercase() as char);
                        }
                        col_map.get(buf.as_str())
                    })
                    .or_else(|| {
                        buf.clear();
                        for b in lc.bytes() {
                            buf.push(b.to_ascii_lowercase() as char);
                        }
                        col_map.get(buf.as_str())
                    })
                    .or_else(|| col_map.get(lc.as_str()))?;
                let ri = rt
                    .as_ref()
                    .and_then(|t| {
                        buf.clear();
                        for b in t.bytes() {
                            buf.push(b.to_ascii_lowercase() as char);
                        }
                        buf.push('.');
                        for b in rc.bytes() {
                            buf.push(b.to_ascii_lowercase() as char);
                        }
                        col_map.get(buf.as_str())
                    })
                    .or_else(|| {
                        buf.clear();
                        for b in rc.bytes() {
                            buf.push(b.to_ascii_lowercase() as char);
                        }
                        col_map.get(buf.as_str())
                    })
                    .or_else(|| col_map.get(rc.as_str()))?;
                // One index must be in left table, other in right table
                if *li < left_col_count && *ri >= left_col_count {
                    return Some((*li, *ri - left_col_count));
                }
                if *ri < left_col_count && *li >= left_col_count {
                    return Some((*ri, *li - left_col_count));
                }
            }
        }
        None
    }

    /// Project columns from source rows into output rows
    fn project_columns(
        &mut self,
        select_cols: &[SelectColumn],
        rows: Vec<Row>,
        col_names: &[String],
        col_map: &HashMap<String, usize>,
        _is_aggregate: bool,
        window_funcs: &[Expr],
        window_defs: &[NamedWindowDefinition],
    ) -> Result<(Vec<String>, Vec<Row>)> {
        // Build a projection plan: either copy by index (cheap) or evaluate expression
        enum Proj<'a> {
            Index(usize),
            Expr(&'a Expr),
        }
        let mut output_names = Vec::new();
        let mut plan: Vec<Proj> = Vec::new();

        for col in select_cols {
            match col {
                SelectColumn::AllColumns => {
                    for (i, name) in col_names.iter().enumerate() {
                        output_names.push(name.clone());
                        plan.push(Proj::Index(i));
                    }
                }
                SelectColumn::TableAllColumns(table) => {
                    if let Some(indices) = Self::table_column_indices(table, col_names, col_map) {
                        for &idx in &indices {
                            output_names.push(col_names[idx].clone());
                            plan.push(Proj::Index(idx));
                        }
                    } else {
                        for (i, name) in col_names.iter().enumerate() {
                            output_names.push(name.clone());
                            plan.push(Proj::Index(i));
                        }
                    }
                }
                SelectColumn::Expr { expr, alias } => {
                    let name = alias
                        .clone()
                        .unwrap_or_else(|| self.expr_display_name(expr));
                    output_names.push(name);
                    plan.push(Proj::Expr(expr));
                }
            }
        }

        // Fast path: if plan is identity projection (all sequential indices 0..N),
        // just move rows directly without cloning any values
        let is_identity = plan.len() == col_names.len()
            && plan
                .iter()
                .enumerate()
                .all(|(i, p)| matches!(p, Proj::Index(idx) if *idx == i));

        if is_identity && window_funcs.is_empty() {
            return Ok((output_names, rows));
        }

        let win_groups: Vec<Vec<&Row>> = rows.iter().map(|r| vec![r]).collect();
        let win_res = self.eval_window_functions_for_groups(window_funcs, &win_groups, col_map, window_defs)?;
        self.window_results = Some(win_res);

        let num_out_cols = plan.len();
        let mut output_rows = Vec::with_capacity(rows.len());
        for (idx, row) in rows.iter().enumerate() {
            self.current_window_row_idx = idx;
            let mut out_row = Vec::with_capacity(num_out_cols);
            for p in &plan {
                match p {
                    Proj::Index(idx) => {
                        out_row.push(if *idx < row.len() {
                            row[*idx].clone()
                        } else {
                            Value::Null
                        });
                    }
                    Proj::Expr(expr) => {
                        out_row.push(self.eval_expr(expr, row, col_map)?);
                    }
                }
            }
            output_rows.push(out_row);
        }

        Ok((output_names, output_rows))
    }

    /// Check if any SELECT column contains an aggregate function
    fn has_aggregate(cols: &[SelectColumn]) -> bool {
        for col in cols {
            if let SelectColumn::Expr { expr, .. } = col {
                if Self::expr_has_aggregate(expr) {
                    return true;
                }
            }
        }
        false
    }

    /// Convert a Value back into a literal Expr (for aggregate-aware function evaluation)
    fn value_to_expr(val: Value) -> Expr {
        match val {
            Value::Integer(v) => Expr::IntegerLiteral(v),
            Value::Real(v) => Expr::RealLiteral(v),
            Value::Text(s) => Expr::StringLiteral(s.to_string()),
            Value::Null => Expr::Null,
            Value::Blob(b) => Expr::BlobLiteral(b),
        }
    }

    /// Recursively check if an expression contains an aggregate function
    fn expr_has_aggregate(expr: &Expr) -> bool {
        match expr {
            Expr::Function { name, args, .. } => {
                let n = name.as_str();
                if n.eq_ignore_ascii_case("COUNT")
                    || n.eq_ignore_ascii_case("SUM")
                    || n.eq_ignore_ascii_case("AVG")
                    || n.eq_ignore_ascii_case("MIN")
                    || n.eq_ignore_ascii_case("MAX")
                {
                    return true;
                }
                // Recurse into non-aggregate function args (e.g. ABS(COUNT(*)))
                args.iter().any(|a| Self::expr_has_aggregate(a))
            }
            Expr::BinaryOp { left, right, .. } => {
                Self::expr_has_aggregate(left) || Self::expr_has_aggregate(right)
            }
            Expr::UnaryOp { expr, .. } => Self::expr_has_aggregate(expr),
            Expr::Nested(inner) => Self::expr_has_aggregate(inner),
            Expr::IsNull { expr, .. } => Self::expr_has_aggregate(expr),
            Expr::InList { expr, list, .. } => {
                Self::expr_has_aggregate(expr) || list.iter().any(|e| Self::expr_has_aggregate(e))
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                Self::expr_has_aggregate(expr)
                    || Self::expr_has_aggregate(low)
                    || Self::expr_has_aggregate(high)
            }
            Expr::Like { expr, pattern, .. } => {
                Self::expr_has_aggregate(expr) || Self::expr_has_aggregate(pattern)
            }
            Expr::InSubquery { expr, .. } => Self::expr_has_aggregate(expr),
            // Subquery/Exists have their own aggregation context
            _ => false,
        }
    }

    /// Get a display name for an expression
    fn expr_display_name(&self, expr: &Expr) -> String {
        match expr {
            Expr::ColumnRef {
                table: Some(t),
                column,
            } => format!("{}.{}", t, column),
            Expr::ColumnRef { column, .. } => column.clone(),
            Expr::Function { name, .. } => name.clone(),
            Expr::IntegerLiteral(v) => format!("{}", v),
            Expr::RealLiteral(v) => format!("{}", v),
            Expr::StringLiteral(v) => format!("'{}'", v),
            _ => "?".to_string(),
        }
    }

    fn apply_group_by(
        &mut self,
        rows: &[Row],
        group_exprs: &[Expr],
        select_cols: &[SelectColumn],
        col_names: &[String],
        col_map: &HashMap<String, usize>,
        having: &Option<Expr>,
        window_funcs: &[Expr],
        window_defs: &[NamedWindowDefinition],
    ) -> Result<(Vec<String>, Vec<Row>)> {
        // Group rows by the GROUP BY expressions (store references, not clones)
        let mut group_index: HashMap<String, usize> = HashMap::new();
        let mut groups: Vec<Vec<&Row>> = Vec::new();
        let mut key_buf = String::with_capacity(128);
        let mut val_buf = String::with_capacity(32);

        for row in rows {
            key_buf.clear();
            for expr in group_exprs {
                let v = self.eval_expr(expr, row, col_map)?;
                Self::typed_key_into(&v, &mut val_buf);
                key_buf.push_str(&val_buf);
                key_buf.push('\0');
            }

            if let Some(&idx) = group_index.get(key_buf.as_str()) {
                groups[idx].push(row);
            } else {
                let idx = groups.len();
                group_index.insert(key_buf.clone(), idx);
                groups.push(vec![row]);
            }
        }

        // Build output column names
        let mut output_names = Vec::new();
        for col in select_cols {
            match col {
                SelectColumn::AllColumns => {
                    for name in col_names {
                        output_names.push(name.clone());
                    }
                }
                SelectColumn::TableAllColumns(table) => {
                    if let Some(indices) = Self::table_column_indices(table, col_names, col_map) {
                        for &idx in &indices {
                            output_names.push(col_names[idx].clone());
                        }
                    } else {
                        for name in col_names {
                            output_names.push(name.clone());
                        }
                    }
                }
                SelectColumn::Expr { expr, alias } => {
                    let name = alias
                        .clone()
                        .unwrap_or_else(|| self.expr_display_name(expr));
                    output_names.push(name);
                }
            }
        }

        let win_res = self.eval_window_functions_for_groups(window_funcs, &groups, col_map, window_defs)?;
        self.window_results = Some(win_res);

        // For each group, build a fully projected output row
        let mut result = Vec::new();
        for (g_idx, group_rows) in groups.iter().enumerate() {
            self.current_window_row_idx = g_idx;
            let first_row = &group_rows[0];

            // Apply HAVING (uses eval_expr_with_aggregates which correctly computes aggregates)
            if let Some(ref having_expr) = having {
                let val =
                    self.eval_expr_with_aggregates(having_expr, first_row, col_map, group_rows)?;
                if !val.is_truthy() {
                    continue;
                }
            }

            // Project each SELECT column
            let mut out_row = Vec::with_capacity(output_names.len());
            for col in select_cols {
                match col {
                    SelectColumn::AllColumns => {
                        for i in 0..col_names.len() {
                            out_row.push(if i < first_row.len() {
                                first_row[i].clone()
                            } else {
                                Value::Null
                            });
                        }
                    }
                    SelectColumn::TableAllColumns(table) => {
                        if let Some(indices) = Self::table_column_indices(table, col_names, col_map)
                        {
                            for &idx in &indices {
                                out_row.push(if idx < first_row.len() {
                                    first_row[idx].clone()
                                } else {
                                    Value::Null
                                });
                            }
                        } else {
                            for i in 0..col_names.len() {
                                out_row.push(if i < first_row.len() {
                                    first_row[i].clone()
                                } else {
                                    Value::Null
                                });
                            }
                        }
                    }
                    SelectColumn::Expr { expr, .. } => {
                        let val =
                            self.eval_expr_with_aggregates(expr, first_row, col_map, group_rows)?;
                        out_row.push(val);
                    }
                }
            }

            result.push(out_row);
        }

        Ok((output_names, result))
    }

    /// Handle implicit aggregation (SELECT with aggregates but no GROUP BY)
    /// Treats all rows as a single group, returns one output row
    fn apply_implicit_aggregate(
        &mut self,
        rows: &[Row],
        select_cols: &[SelectColumn],
        col_names: &[String],
        col_map: &HashMap<String, usize>,
        window_funcs: &[Expr],
        window_defs: &[NamedWindowDefinition],
    ) -> Result<(Vec<String>, Vec<Row>)> {
        // Build output column names
        let mut output_names = Vec::new();
        for col in select_cols {
            match col {
                SelectColumn::AllColumns => {
                    for name in col_names {
                        output_names.push(name.clone());
                    }
                }
                SelectColumn::TableAllColumns(table) => {
                    if let Some(indices) = Self::table_column_indices(table, col_names, col_map) {
                        for &idx in &indices {
                            output_names.push(col_names[idx].clone());
                        }
                    } else {
                        for name in col_names {
                            output_names.push(name.clone());
                        }
                    }
                }
                SelectColumn::Expr { expr, alias } => {
                    let name = alias
                        .clone()
                        .unwrap_or_else(|| self.expr_display_name(expr));
                    output_names.push(name);
                }
            }
        }

        let row_refs: Vec<&Row> = rows.iter().collect();
        let win_groups = vec![row_refs.clone()];
        let win_res = self.eval_window_functions_for_groups(window_funcs, &win_groups, col_map, window_defs)?;
        self.window_results = Some(win_res);
        self.current_window_row_idx = 0;

        // Use first row (or empty row) for non-aggregate column evaluation
        let empty_row: Vec<Value> = Vec::new();
        let first_row = rows.first().unwrap_or(&empty_row);

        // Build single output row (use eval_expr_with_aggregates to handle nested aggregates)
        let mut out_row = Vec::with_capacity(output_names.len());
        for col in select_cols {
            match col {
                SelectColumn::AllColumns => {
                    for i in 0..col_names.len() {
                        out_row.push(if i < first_row.len() {
                            first_row[i].clone()
                        } else {
                            Value::Null
                        });
                    }
                }
                SelectColumn::TableAllColumns(table) => {
                    if let Some(indices) = Self::table_column_indices(table, col_names, col_map) {
                        for &idx in &indices {
                            out_row.push(if idx < first_row.len() {
                                first_row[idx].clone()
                            } else {
                                Value::Null
                            });
                        }
                    } else {
                        for i in 0..col_names.len() {
                            out_row.push(if i < first_row.len() {
                                first_row[i].clone()
                            } else {
                                Value::Null
                            });
                        }
                    }
                }
                SelectColumn::Expr { expr, .. } => {
                    let row_refs: Vec<&Row> = rows.iter().collect();
                    let val =
                        self.eval_expr_with_aggregates(expr, first_row, col_map, &row_refs)?;
                    out_row.push(val);
                }
            }
        }

        Ok((output_names, vec![out_row]))
    }

    /// Evaluate an aggregate function over a group
    fn eval_aggregate(
        &mut self,
        name: &str,
        args: &[Expr],
        distinct: bool,
        group_rows: &[&Row],
        col_map: &HashMap<String, usize>,
    ) -> Result<Value> {
        // Use eq_ignore_ascii_case to avoid to_uppercase() allocation
        if name.eq_ignore_ascii_case("COUNT") {
            if args.is_empty() || matches!(args[0], Expr::IntegerLiteral(1)) {
                Ok(Value::Integer(group_rows.len() as i64))
            } else {
                let mut count = 0i64;
                let mut seen = std::collections::HashSet::new();
                let mut key_buf = String::with_capacity(32);
                for row in group_rows {
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    if !matches!(val, Value::Null) {
                        if distinct {
                            Self::typed_key_into(&val, &mut key_buf);
                            if seen.insert(key_buf.clone()) {
                                count += 1;
                            }
                        } else {
                            count += 1;
                        }
                    }
                }
                Ok(Value::Integer(count))
            }
        } else if name.eq_ignore_ascii_case("SUM") {
            if args.is_empty() {
                return Ok(Value::Null);
            }
            let mut int_sum = 0i64;
            let mut real_sum = 0.0f64;
            let mut is_int = true;
            let mut has_value = false;
            for row in group_rows {
                let val = self.eval_expr(&args[0], row, col_map)?;
                match val {
                    Value::Integer(v) => {
                        if is_int {
                            int_sum = int_sum.wrapping_add(v);
                        } else {
                            real_sum += v as f64;
                        }
                        has_value = true;
                    }
                    Value::Real(v) => {
                        if is_int {
                            real_sum = int_sum as f64 + v;
                            is_int = false;
                        } else {
                            real_sum += v;
                        }
                        has_value = true;
                    }
                    Value::Null => {}
                    _ => {
                        if let Some(v) = val.to_f64() {
                            if is_int {
                                real_sum = int_sum as f64 + v;
                                is_int = false;
                            } else {
                                real_sum += v;
                            }
                            has_value = true;
                        }
                    }
                }
            }
            if !has_value {
                return Ok(Value::Null);
            }
            if is_int {
                Ok(Value::Integer(int_sum))
            } else {
                Ok(Value::Real(real_sum))
            }
        } else if name.eq_ignore_ascii_case("AVG") {
            if args.is_empty() {
                return Ok(Value::Null);
            }
            let mut sum = 0.0f64;
            let mut count = 0i64;
            for row in group_rows {
                let val = self.eval_expr(&args[0], row, col_map)?;
                if let Some(v) = val.to_f64() {
                    sum += v;
                    count += 1;
                }
            }
            if count == 0 {
                Ok(Value::Null)
            } else {
                Ok(Value::Real(sum / count as f64))
            }
        } else if name.eq_ignore_ascii_case("MIN") {
            if args.is_empty() {
                return Ok(Value::Null);
            }
            let mut min_val: Option<Value> = None;
            for row in group_rows {
                let val = self.eval_expr(&args[0], row, col_map)?;
                if matches!(val, Value::Null) {
                    continue;
                }
                min_val = Some(match min_val {
                    None => val,
                    Some(current) => {
                        if val.partial_cmp(&current) == Some(std::cmp::Ordering::Less) {
                            val
                        } else {
                            current
                        }
                    }
                });
            }
            Ok(min_val.unwrap_or(Value::Null))
        } else if name.eq_ignore_ascii_case("MAX") {
            if args.is_empty() {
                return Ok(Value::Null);
            }
            let mut max_val: Option<Value> = None;
            for row in group_rows {
                let val = self.eval_expr(&args[0], row, col_map)?;
                if matches!(val, Value::Null) {
                    continue;
                }
                max_val = Some(match max_val {
                    None => val,
                    Some(current) => {
                        if val.partial_cmp(&current) == Some(std::cmp::Ordering::Greater) {
                            val
                        } else {
                            current
                        }
                    }
                });
            }
            Ok(max_val.unwrap_or(Value::Null))
        } else {
            Err(KkdbError::RuntimeError(format!(
                "unknown aggregate function: {}",
                name
            )))
        }
    }

    /// Eval expression that might contain aggregates (for HAVING clause)
    fn eval_expr_with_aggregates(
        &mut self,
        expr: &Expr,
        row: &Row,
        col_map: &HashMap<String, usize>,
        group_rows: &[&Row],
    ) -> Result<Value> {
        match expr {
            Expr::Function {
                name,
                args,
                distinct,
            } => {
                let n = name.as_str();
                if n == "__win" {
                    let idx = match args.first() {
                        Some(Expr::IntegerLiteral(i)) => *i as usize,
                        _ => 0,
                    };
                    return Ok(self.window_results.as_ref().map_or(Value::Null, |res| res[self.current_window_row_idx][idx].clone()));
                }
                if n.eq_ignore_ascii_case("COUNT")
                    || n.eq_ignore_ascii_case("SUM")
                    || n.eq_ignore_ascii_case("AVG")
                    || n.eq_ignore_ascii_case("MIN")
                    || n.eq_ignore_ascii_case("MAX")
                {
                    self.eval_aggregate(n, args, *distinct, group_rows, col_map)
                } else {
                    // Non-aggregate function: evaluate args with aggregate support first
                    let mut eval_args: Vec<Expr> = Vec::with_capacity(args.len());
                    for arg in args {
                        let val = self.eval_expr_with_aggregates(arg, row, col_map, group_rows)?;
                        eval_args.push(Self::value_to_expr(val));
                    }
                    let temp = Expr::Function {
                        name: name.clone(),
                        args: eval_args,
                        distinct: *distinct,
                    };
                    self.eval_expr(&temp, row, col_map)
                }
            }
            Expr::BinaryOp { left, op, right } => match op {
                BinaryOperator::And => {
                    let l = self.eval_expr_with_aggregates(left, row, col_map, group_rows)?;
                    if !matches!(l, Value::Null) && !l.is_truthy() {
                        return Ok(Value::Integer(0));
                    }
                    let r = self.eval_expr_with_aggregates(right, row, col_map, group_rows)?;
                    if !matches!(r, Value::Null) && !r.is_truthy() {
                        return Ok(Value::Integer(0));
                    }
                    if matches!(l, Value::Null) || matches!(r, Value::Null) {
                        return Ok(Value::Null);
                    }
                    Ok(Value::Integer(1))
                }
                BinaryOperator::Or => {
                    let l = self.eval_expr_with_aggregates(left, row, col_map, group_rows)?;
                    if !matches!(l, Value::Null) && l.is_truthy() {
                        return Ok(Value::Integer(1));
                    }
                    let r = self.eval_expr_with_aggregates(right, row, col_map, group_rows)?;
                    if !matches!(r, Value::Null) && r.is_truthy() {
                        return Ok(Value::Integer(1));
                    }
                    if matches!(l, Value::Null) || matches!(r, Value::Null) {
                        return Ok(Value::Null);
                    }
                    Ok(Value::Integer(0))
                }
                _ => {
                    let l = self.eval_expr_with_aggregates(left, row, col_map, group_rows)?;
                    let r = self.eval_expr_with_aggregates(right, row, col_map, group_rows)?;
                    self.apply_binary_op(op, &l, &r)
                }
            },
            Expr::UnaryOp { op, expr: inner } => {
                let val = self.eval_expr_with_aggregates(inner, row, col_map, group_rows)?;
                match op {
                    UnaryOperator::Minus => match val {
                        Value::Integer(v) => Ok(Value::Integer(v.wrapping_neg())),
                        Value::Real(v) => Ok(Value::Real(-v)),
                        Value::Null => Ok(Value::Null),
                        _ => Err(KkdbError::TypeError(
                            "cannot negate non-numeric value".into(),
                        )),
                    },
                    UnaryOperator::Not => {
                        if matches!(val, Value::Null) {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Integer(if val.is_truthy() { 0 } else { 1 }))
                        }
                    }
                }
            }
            Expr::Nested(inner) => self.eval_expr_with_aggregates(inner, row, col_map, group_rows),
            Expr::IsNull {
                expr: inner,
                negated,
            } => {
                let val = self.eval_expr_with_aggregates(inner, row, col_map, group_rows)?;
                let is_null = matches!(val, Value::Null);
                Ok(Value::Integer(if is_null != *negated { 1 } else { 0 }))
            }
            Expr::InList {
                expr: inner,
                list,
                negated,
            } => {
                let val = self.eval_expr_with_aggregates(inner, row, col_map, group_rows)?;
                if matches!(val, Value::Null) {
                    return Ok(Value::Null);
                }
                let mut found = false;
                let mut has_null = false;
                for item in list {
                    let item_val =
                        self.eval_expr_with_aggregates(item, row, col_map, group_rows)?;
                    if matches!(item_val, Value::Null) {
                        has_null = true;
                        continue;
                    }
                    if val == item_val {
                        found = true;
                        break;
                    }
                }
                if found {
                    Ok(Value::Integer(if !*negated { 1 } else { 0 }))
                } else if has_null {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Integer(if *negated { 1 } else { 0 }))
                }
            }
            Expr::Between {
                expr: inner,
                low,
                high,
                negated,
            } => {
                let val = self.eval_expr_with_aggregates(inner, row, col_map, group_rows)?;
                let lo = self.eval_expr_with_aggregates(low, row, col_map, group_rows)?;
                let hi = self.eval_expr_with_aggregates(high, row, col_map, group_rows)?;
                if matches!(val, Value::Null)
                    || matches!(lo, Value::Null)
                    || matches!(hi, Value::Null)
                {
                    return Ok(Value::Null);
                }
                let in_range = val
                    .partial_cmp(&lo)
                    .map_or(false, |o| o != std::cmp::Ordering::Less)
                    && val
                        .partial_cmp(&hi)
                        .map_or(false, |o| o != std::cmp::Ordering::Greater);
                Ok(Value::Integer(if in_range != *negated { 1 } else { 0 }))
            }
            Expr::Like {
                expr: inner,
                pattern,
                escape_char,
                case_insensitive,
                negated,
            } => {
                let val = self.eval_expr_with_aggregates(inner, row, col_map, group_rows)?;
                let pat = self.eval_expr_with_aggregates(pattern, row, col_map, group_rows)?;
                if matches!(val, Value::Null) || matches!(pat, Value::Null) {
                    return Ok(Value::Null);
                }
                match (&val, &pat) {
                    (Value::Text(s), Value::Text(p)) => {
                        let matches = like_match(s, p, *escape_char, *case_insensitive);
                        Ok(Value::Integer(if matches != *negated { 1 } else { 0 }))
                    }
                    _ => Ok(Value::Integer(0)),
                }
            }
            Expr::Collate { expr: inner, collation: _ } => {
                // Ignore collation sorting rules during aggregate evaluation for now
                self.eval_expr_with_aggregates(inner, row, col_map, group_rows)
            }
            Expr::Interval { value, leading_field } => {
                let val = self.eval_expr_with_aggregates(value, row, col_map, group_rows)?;
                if let Some(field) = leading_field {
                    match val {
                        Value::Text(s) => Ok(Value::Text(format!("{} {}", s.trim(), field).into())),
                        Value::Integer(v) => Ok(Value::Text(format!("{} {}", v, field).into())),
                        Value::Real(v) => Ok(Value::Text(format!("{} {}", v, field).into())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(val)
                }
            }
            _ => self.eval_expr(expr, row, col_map),
        }
    }

    /// Try to use an index for a simple WHERE condition on a single table.
    /// Returns Some((rows, col_names)) if an index was used, None otherwise.
    fn try_index_scan(
        &mut self,
        from: &FromClause,
        where_expr: &Expr,
    ) -> Result<Option<(Vec<Row>, Vec<String>)>> {
        // Only optimize single-table FROM
        let table_name = match from {
            FromClause::Table { name, .. } => name.clone(),
            _ => return Ok(None),
        };

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
            .indexes_for_table(&table_name)
            .into_iter()
            .cloned()
            .collect();
        let matching_idx = indexes
            .iter()
            .find(|idx| !idx.columns.is_empty() && idx.columns[0].eq_ignore_ascii_case(&col_name));

        let idx = match matching_idx {
            Some(i) => i.clone(),
            None => return Ok(None),
        };

        let table = self.schema.get_table(&table_name)?.clone();
        let col_names = table.col_names.clone();

        let matching_rowids = match lookup {
            Lookup::Eq(search_val) => {
                // SQL '=' with NULL is unknown, so WHERE never matches.
                if matches!(search_val, Value::Null) {
                    return Ok(Some((Vec::new(), col_names)));
                }
                self.index_rowids_for_value(&idx, &search_val)?
            }
            Lookup::In(search_vals) => {
                if search_vals.is_empty() {
                    return Ok(Some((Vec::new(), col_names)));
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
                out
            }
            Lookup::Comparison(op, search_val) => {
                self.index_rowids_for_comparison(&idx, &op, &search_val)?
            }
            Lookup::Between(low, high) => self.index_rowids_for_between(&idx, &low, &high)?,
        };

        // Fetch full rows by rowid.
        let fetched_rows = self.fetch_rows_by_rowids(table.root_page, &matching_rowids)?;
        let mut result_rows: Vec<Row> = Vec::with_capacity(fetched_rows.len());
        for (_rid, row) in fetched_rows {
            result_rows.push(row);
        }

        Ok(Some((result_rows, col_names)))
    }

    /// Execute a SET OPERATION (UNION / INTERSECT / EXCEPT)
    pub(crate) fn exec_set_op(&mut self, setop: &SetOpStmt) -> Result<ExecResult> {
        let left_result = self.exec_select(&setop.left)?;
        let right_result = self.exec_select(&setop.right)?;

        let (mut left_rows, col_names) = match left_result {
            ExecResult::QueryResult { rows, columns } => (rows, columns),
            _ => return Err(KkdbError::Internal("set-op left not a query result".into())),
        };
        let (right_rows, _) = match right_result {
            ExecResult::QueryResult { rows, columns } => (rows, columns),
            _ => return Err(KkdbError::Internal("set-op right not a query result".into())),
        };

        let mut result_rows = match setop.kind {
            SetOpKind::UnionAll => {
                left_rows.extend(right_rows);
                left_rows
            }
            SetOpKind::UnionDistinct => {
                let mut seen: HashSet<Vec<u8>> = HashSet::new();
                let mut out = Vec::new();
                for row in left_rows.into_iter().chain(right_rows.into_iter()) {
                    let key = row_key(&row);
                    if seen.insert(key) {
                        out.push(row);
                    }
                }
                out
            }
            SetOpKind::IntersectAll => {
                // Count occurrences in right, consume as encountered
                let mut counts: HashMap<Vec<u8>, usize> = HashMap::new();
                for row in &right_rows {
                    *counts.entry(row_key(row)).or_insert(0) += 1;
                }
                left_rows.into_iter().filter(|r| {
                    let k = row_key(r);
                    if let Some(c) = counts.get_mut(&k) {
                        if *c > 0 { *c -= 1; return true; }
                    }
                    false
                }).collect()
            }
            SetOpKind::IntersectDistinct => {
                let right_keys: HashSet<Vec<u8>> = right_rows.iter().map(|r| row_key(r)).collect();
                let mut seen: HashSet<Vec<u8>> = HashSet::new();
                left_rows.into_iter().filter(|r| {
                    let k = row_key(r);
                    right_keys.contains(&k) && seen.insert(k)
                }).collect()
            }
            SetOpKind::ExceptAll => {
                let mut counts: HashMap<Vec<u8>, usize> = HashMap::new();
                for row in &right_rows {
                    *counts.entry(row_key(row)).or_insert(0) += 1;
                }
                left_rows.into_iter().filter(|r| {
                    let k = row_key(r);
                    if let Some(c) = counts.get_mut(&k) {
                        if *c > 0 { *c -= 1; return false; }
                    }
                    true
                }).collect()
            }
            SetOpKind::ExceptDistinct => {
                let right_keys: HashSet<Vec<u8>> = right_rows.iter().map(|r| row_key(r)).collect();
                let mut seen: HashSet<Vec<u8>> = HashSet::new();
                left_rows.into_iter().filter(|r| {
                    let k = row_key(r);
                    !right_keys.contains(&k) && seen.insert(k)
                }).collect()
            }
        };

        // Bug #1 fix: apply ORDER BY and LIMIT/OFFSET to the combined result
        let col_map: HashMap<String, usize> =
            col_names.iter().enumerate().map(|(i, n)| (n.clone(), i)).collect();
        let empty_row: Vec<Value> = Vec::new();

        // ORDER BY: evaluate expressions on each row, then sort
        if !setop.order_by.is_empty() {
            // Pre-evaluate sort keys to avoid repeated eval and borrow issues
            let mut keyed: Vec<(Vec<Value>, Vec<Value>)> = result_rows
                .into_iter()
                .map(|row| {
                    let keys: Vec<Value> = setop.order_by.iter()
                        .map(|item| self.eval_expr(&item.expr, &row, &col_map).unwrap_or(Value::Null))
                        .collect();
                    (keys, row)
                })
                .collect();
            keyed.sort_by(|(ka, _), (kb, _)| {
                for (i, item) in setop.order_by.iter().enumerate() {
                    let mut cmp = ka[i].partial_cmp(&kb[i]).unwrap_or(std::cmp::Ordering::Equal);
                    if !item.ascending { cmp = cmp.reverse(); }
                    if cmp != std::cmp::Ordering::Equal { return cmp; }
                }
                std::cmp::Ordering::Equal
            });
            result_rows = keyed.into_iter().map(|(_, row)| row).collect();
        }

        // OFFSET
        if let Some(ref off_expr) = setop.offset {
            let off = match self.eval_expr(off_expr, &empty_row, &col_map)? {
                Value::Integer(v) => v.max(0) as usize,
                _ => 0,
            };
            if off >= result_rows.len() {
                result_rows.clear();
            } else {
                result_rows.drain(0..off);
            }
        }

        // LIMIT
        if let Some(ref lim_expr) = setop.limit {
            let lim = match self.eval_expr(lim_expr, &empty_row, &col_map)? {
                Value::Integer(v) => v.max(0) as usize,
                _ => usize::MAX,
            };
            result_rows.truncate(lim);
        }

        Ok(ExecResult::QueryResult {
            columns: col_names,
            rows: result_rows,
        })
    }

    /// Execute SHOW TABLES — returns a sorted list of all user table names
    pub(crate) fn exec_show_tables(&mut self) -> Result<ExecResult> {
        let mut names = self.schema.list_tables();
        names.sort_unstable(); // Bug #5 fix: deterministic order
        let rows: Vec<Vec<Value>> = names.into_iter().map(|n| vec![Value::Text(n.into())]).collect();
        Ok(ExecResult::QueryResult {
            columns: vec!["name".to_string()],
            rows,
        })
    }

    fn extract_window_funcs(expr: &mut Expr, extracted: &mut Vec<Expr>) {
        if matches!(expr, Expr::WindowFunction { .. }) {
            let mut original = Expr::Null;
            std::mem::swap(expr, &mut original);
            let idx = extracted.len();
            extracted.push(original);
            *expr = Expr::Function {
                name: "__win".to_string(),
                args: vec![Expr::IntegerLiteral(idx as i64)],
                distinct: false,
            };
            return;
        }
        match expr {
            Expr::BinaryOp { left, right, .. } => {
                Self::extract_window_funcs(left, extracted);
                Self::extract_window_funcs(right, extracted);
            }
            Expr::UnaryOp { expr, .. } | Expr::Nested(expr) | Expr::Cast { expr, .. }
            | Expr::Collate { expr, .. } | Expr::Interval { value: expr, .. }
            | Expr::IsNull { expr, .. } => Self::extract_window_funcs(expr, extracted),
            Expr::Function { args, .. } => {
                for arg in args {
                    Self::extract_window_funcs(arg, extracted);
                }
            }
            Expr::InList { expr, list, .. } => {
                Self::extract_window_funcs(expr, extracted);
                for i in list {
                    Self::extract_window_funcs(i, extracted);
                }
            }
            Expr::Between { expr, low, high, .. } => {
                Self::extract_window_funcs(expr, extracted);
                Self::extract_window_funcs(low, extracted);
                Self::extract_window_funcs(high, extracted);
            }
            Expr::Like { expr, pattern, .. } => {
                Self::extract_window_funcs(expr, extracted);
                Self::extract_window_funcs(pattern, extracted);
            }
            Expr::Case { operand, when_clauses, else_clause } => {
                if let Some(op) = operand { Self::extract_window_funcs(op, extracted); }
                for (cond, res) in when_clauses {
                    Self::extract_window_funcs(cond, extracted);
                    Self::extract_window_funcs(res, extracted);
                }
                if let Some(el) = else_clause { Self::extract_window_funcs(el, extracted); }
            }
            _ => {}
        }
    }

    fn eval_window_functions_for_groups(
        &mut self,
        window_exprs: &[Expr],
        groups: &[Vec<&Row>],
        col_map: &HashMap<String, usize>,
        window_defs: &[NamedWindowDefinition],
    ) -> Result<Vec<Vec<Value>>> {
        let mut results = vec![vec![Value::Null; window_exprs.len()]; groups.len()];
        let empty_row = Vec::new();
        let empty_row_ref = &empty_row;

        for (w_idx, window_expr) in window_exprs.iter().enumerate() {
            let Expr::WindowFunction { func, partition_by, order_by, frame } = window_expr else {
                continue;
            };

            let mut active_partition_by = partition_by;
            let mut active_order_by = order_by;
            let mut active_frame = frame;
            if let Some(Expr::ColumnRef { column, .. }) = partition_by.first() {
                if column.starts_with("__named_window_") {
                    let name = &column["__named_window_".len()..];
                     if let Some(def) = window_defs.iter().find(|d| d.name == name) {
                        active_partition_by = &def.partition_by;
                        active_order_by = &def.order_by;
                        active_frame = &def.frame;
                    }
                }
            }

            let mut partitions: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
            if active_partition_by.is_empty() {
                partitions.insert(vec![], (0..groups.len()).collect());
            } else {
                let mut key_buf = String::with_capacity(64);
                let mut val_buf = String::with_capacity(32);
                for (g_idx, group_rows) in groups.iter().enumerate() {
                    let first_row = group_rows.first().unwrap_or(&empty_row_ref);
                    key_buf.clear();
                    for expr in active_partition_by {
                        let v = self.eval_expr_with_aggregates(expr, first_row, col_map, group_rows)?;
                        Self::typed_key_into(&v, &mut val_buf);
                        key_buf.push_str(&val_buf);
                        key_buf.push('\0');
                    }
                    partitions.entry(key_buf.clone().into_bytes()).or_default().push(g_idx);
                }
            }

            for (_, mut part_indices) in partitions {
                if !active_order_by.is_empty() {
                    let mut sort_keys: Vec<(usize, Vec<Value>)> = part_indices.into_iter().map(|g_idx| {
                        let group_rows = &groups[g_idx];
                        let first_row = group_rows.first().unwrap_or(&empty_row_ref);
                        let keys: Vec<Value> = active_order_by.iter().map(|item| {
                            self.eval_expr_with_aggregates(&item.expr, first_row, col_map, group_rows).unwrap_or(Value::Null)
                        }).collect();
                        (g_idx, keys)
                    }).collect();

                    sort_keys.sort_by(|a, b| {
                        for (i, item) in active_order_by.iter().enumerate() {
                            let mut cmp = a.1[i].partial_cmp(&b.1[i]).unwrap_or(std::cmp::Ordering::Equal);
                            if !item.ascending { cmp = cmp.reverse(); }
                            if cmp != std::cmp::Ordering::Equal { return cmp; }
                        }
                        std::cmp::Ordering::Equal
                    });
                    part_indices = sort_keys.into_iter().map(|(idx, _)| idx).collect();
                }

                for p in 0..part_indices.len() {
                    let g_idx = part_indices[p];
                    let mut start = 0;
                    let mut end = part_indices.len().saturating_sub(1);

                    if let Some(wf) = active_frame {
                        if wf.unit == WindowFrameUnit::Rows {
                            start = match &wf.start {
                                WindowBound::CurrentRow => p,
                                WindowBound::UnboundedPreceding => 0,
                                WindowBound::Preceding(expr) => {
                                    let val = self.eval_constant_expr(expr).unwrap_or(Value::Integer(0));
                                    if let Value::Integer(v) = val { p.saturating_sub(v as usize) } else { 0 }
                                }
                                WindowBound::Following(expr) => {
                                    let val = self.eval_constant_expr(expr).unwrap_or(Value::Integer(0));
                                    if let Value::Integer(v) = val { p.saturating_add(v as usize) } else { p }
                                }
                                WindowBound::UnboundedFollowing => part_indices.len().saturating_sub(1),
                            };
                            if let Some(eb) = &wf.end {
                                end = match eb {
                                    WindowBound::CurrentRow => p,
                                    WindowBound::UnboundedPreceding => 0,
                                    WindowBound::Preceding(expr) => {
                                        let val = self.eval_constant_expr(expr).unwrap_or(Value::Integer(0));
                                        if let Value::Integer(v) = val { p.saturating_sub(v as usize) } else { p }
                                    }
                                    WindowBound::Following(expr) => {
                                        let val = self.eval_constant_expr(expr).unwrap_or(Value::Integer(0));
                                        if let Value::Integer(v) = val { p.saturating_add(v as usize) } else { p }
                                    }
                                    WindowBound::UnboundedFollowing => part_indices.len().saturating_sub(1),
                                };
                            } else {
                                end = p;
                            }
                        }
                    } else if !active_order_by.is_empty() {
                         end = p;
                    }

                    start = start.min(part_indices.len().saturating_sub(1));
                    end = end.min(part_indices.len().saturating_sub(1)).max(start);

                    let frame_indices = &part_indices[start..=end];

                    let val = match func {
                        WindowFunc::RowNumber => Value::Integer((p + 1) as i64),
                        WindowFunc::Rank => {
                            Value::Integer((p + 1) as i64)
                        }
                        WindowFunc::Aggregate { name, args, .. } => {
                             let mut frame_vals = Vec::with_capacity(frame_indices.len());
                             for &f_idx in frame_indices {
                                 let group_rows = &groups[f_idx];
                                 let first_row = group_rows.first().unwrap_or(&empty_row_ref);
                                 if !args.is_empty() {
                                    let v = self.eval_expr_with_aggregates(&args[0], first_row, col_map, group_rows).unwrap_or(Value::Null);
                                    frame_vals.push(v);
                                 }
                             }
                             if name.eq_ignore_ascii_case("SUM") {
                                 let mut i_sum = 0i64;
                                 let mut f_sum = 0.0f64;
                                 let mut is_int = true;
                                 let mut valid = false;
                                 for v in frame_vals {
                                     match v {
                                         Value::Integer(x) => {
                                             if is_int { i_sum = i_sum.wrapping_add(x); } else { f_sum += x as f64; }
                                             valid = true;
                                         }
                                         Value::Real(x) => {
                                             if is_int { f_sum = i_sum as f64 + x; is_int = false; } else { f_sum += x; }
                                             valid = true;
                                         }
                                         _ => {}
                                     }
                                 }
                                 if !valid { Value::Null } else if is_int { Value::Integer(i_sum) } else { Value::Real(f_sum) }
                             } else if name.eq_ignore_ascii_case("COUNT") {
                                 let count = frame_vals.into_iter().filter(|v| !matches!(v, Value::Null)).count();
                                 Value::Integer(count as i64)
                             } else if name.eq_ignore_ascii_case("MAX") {
                                 frame_vals.into_iter().filter(|v| !matches!(v, Value::Null)).max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(Value::Null)
                             } else if name.eq_ignore_ascii_case("MIN") {
                                 frame_vals.into_iter().filter(|v| !matches!(v, Value::Null)).min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(Value::Null)
                             } else {
                                 Value::Null
                             }
                        }
                        _ => Value::Null,
                    };
                    results[g_idx][w_idx] = val;
                }
            }
        }
        Ok(results)
    }
}

/// Generate a deterministic sort key for a row (used for UNION/INTERSECT/EXCEPT dedup)
fn row_key(row: &[crate::types::Value]) -> Vec<u8> {
    let mut key = Vec::new();
    for v in row {
        let bytes = v.serialize();
        key.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        key.extend(bytes);
    }
    key
}
