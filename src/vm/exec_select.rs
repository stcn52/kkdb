use super::eval_expr::like_match;
use super::execute::{ExecResult, VM};
use crate::error::{KkdbError, Result};
use crate::sql::ast::*;
use crate::storage::btree::BTree;
use crate::types::{Row, Value};
use std::collections::{HashMap, HashSet};

impl VM {
    // ---- SELECT ----
    pub(crate) fn exec_select(&mut self, select: &SelectStmt) -> Result<ExecResult> {
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

        // Determine output columns (GROUP BY / implicit aggregate do projection internally)
        let (output_col_names, mut output_rows) = if !select.group_by.is_empty() {
            self.apply_group_by(
                &rows,
                &select.group_by,
                &select.columns,
                &col_names,
                &col_map,
                &select.having,
            )?
        } else if Self::has_aggregate(&select.columns) {
            // Implicit aggregation: no GROUP BY but SELECT contains aggregates
            // Treat all rows as a single group, return one output row
            self.apply_implicit_aggregate(&rows, &select.columns, &col_names, &col_map)?
        } else {
            self.project_columns(&select.columns, rows, &col_names, &col_map, false)?
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

        // Apply ORDER BY
        if !select.order_by.is_empty() {
            let output_col_map: HashMap<String, usize> = output_col_names
                .iter()
                .enumerate()
                .map(|(i, name)| (name.to_lowercase(), i))
                .collect();

            let order_items: Vec<(usize, bool)> = select
                .order_by
                .iter()
                .filter_map(|item| {
                    if let Expr::ColumnRef { column, .. } = &item.expr {
                        let lower = column.to_ascii_lowercase();
                        output_col_map
                            .get(lower.as_str())
                            .or_else(|| col_map.get(lower.as_str()))
                            .map(|&idx| (idx, item.ascending))
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
        order_items: &[(usize, bool)],
    ) -> std::cmp::Ordering {
        for &(col_idx, ascending) in order_items {
            if col_idx < a.len() && col_idx < b.len() {
                let cmp = a[col_idx].partial_cmp(&b[col_idx]);
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
        }
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
        _is_grouped: bool,
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

        if is_identity {
            return Ok((output_names, rows));
        }

        let num_out_cols = plan.len();
        let mut output_rows = Vec::with_capacity(rows.len());
        for row in &rows {
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

    /// Apply GROUP BY — returns (output_col_names, projected_rows)
    fn apply_group_by(
        &mut self,
        rows: &[Row],
        group_exprs: &[Expr],
        select_cols: &[SelectColumn],
        col_names: &[String],
        col_map: &HashMap<String, usize>,
        having: &Option<Expr>,
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

        // For each group, build a fully projected output row
        let mut result = Vec::new();
        for group_rows in &groups {
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
                negated,
            } => {
                let val = self.eval_expr_with_aggregates(inner, row, col_map, group_rows)?;
                let pat = self.eval_expr_with_aggregates(pattern, row, col_map, group_rows)?;
                if matches!(val, Value::Null) || matches!(pat, Value::Null) {
                    return Ok(Value::Null);
                }
                match (&val, &pat) {
                    (Value::Text(s), Value::Text(p)) => {
                        let matches = like_match(s, p);
                        Ok(Value::Integer(if matches != *negated { 1 } else { 0 }))
                    }
                    _ => Ok(Value::Integer(0)),
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
}
