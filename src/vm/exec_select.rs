//! SELECT statement execution for KKDB.
//!
//! This module implements the full `SELECT` pipeline, including:
//! - **FROM / JOIN evaluation** — table scans, view expansion, hash/nested-loop JOINs,
//!   and subquery materialisation.
//! - **CTE support** — `WITH` (non-recursive) and `WITH RECURSIVE` (UNION ALL loop).
//! - **WHERE / RLS filtering** — predicate pushdown and Row-Level Security policy injection.
//! - **Aggregation** — `GROUP BY`, `HAVING`, implicit aggregation; window functions.
//! - **Projection** — column aliases, expression evaluation via [`super::eval_expr`].
//! - **DISTINCT**, **ORDER BY** (with precomputed sort keys and Top-N optimisation),
//!   **LIMIT / OFFSET**.
//! - **Full-Text Search** — BM25 index scan short-circuit for `FTS_MATCH(...)`.
//! - **Correlated subqueries** — via the [`VM::outer_rows`] stack pushed/popped
//!   during `Exists` / `InSubquery` / scalar subquery evaluation.
//!
//! ## Execution order
//!
//! ```text
//! FROM / JOIN  →  WHERE (+ RLS)  →  GROUP BY + HAVING  →  SELECT (projections)
//!              →  DISTINCT  →  ORDER BY  →  OFFSET  →  LIMIT
//! ```
//!
//! Simple (non-aggregate) queries are sorted **before** projection so that
//! `ORDER BY` can reference any source column, not just the selected ones.

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

        // L7: Evaluate CTEs (WITH clause) — store results in cte_cache
        let ctes_to_eval: Vec<CteDefinition> = std::mem::take(&mut select_mut.ctes);
        for cte in &ctes_to_eval {
            let key = cte.name.to_ascii_lowercase();
            let (mut col_names, rows) = if cte.is_recursive {
                // Recursive CTE: query must be `anchor UNION ALL recursive_part`
                self.eval_recursive_cte(cte)?
            } else {
                // Non-recursive CTE: just evaluate the query once
                match self.exec_select(&cte.query)? {
                    ExecResult::QueryResult { columns, rows } => (columns, rows),
                    _ => (Vec::new(), Vec::new()),
                }
            };
            // Apply declared column name aliases from `cte_name(col1, col2, ...)`
            if !cte.columns.is_empty() {
                for (i, alias) in cte.columns.iter().enumerate() {
                    if i < col_names.len() {
                        col_names[i] = alias.clone();
                    }
                }
            }
            self.cte_cache.insert(key, (col_names, rows));
        }

        for col in &mut select_mut.columns {
            if let SelectColumn::Expr { expr, .. } = col {
                Self::extract_window_funcs(expr, &mut window_funcs);
            }
        }
        for item in &mut select_mut.order_by {
            Self::extract_window_funcs(&mut item.expr, &mut window_funcs);
        }
        let select = &select_mut;

        // Cleanup CTEs from cache after we're done with this SELECT
        // (deferred — we clean up after exec to allow nested references)

        // Check if LIMIT pushdown is safe:
        // no WHERE, no ORDER BY, no GROUP BY, no DISTINCT, no HAVING, simple table FROM
        let limit_pushdown = select.where_clause.is_none()
            && select.order_by.is_empty()
            && select.group_by.is_empty()
            && !select.distinct
            && select.having.is_none()
            && select.limit.is_some()
            && !Self::has_aggregate(&select.columns);

        // Track if a FTS index scan handled the full WHERE so we can skip post-scan re-eval.
        let mut fts_index_scan_used = false;

        // Get the source rows
        let (mut rows, col_names) = if let Some(ref from) = select.from {
            // ── Q3: COUNT(*) fast path ──────────────────────────────────────────
            // Detect: SELECT COUNT(*) FROM t  (no WHERE, GROUP BY, DISTINCT, HAVING)
            let is_count_star = select.where_clause.is_none()
                && select.group_by.is_empty()
                && !select.distinct
                && select.having.is_none()
                && select.columns.len() == 1
                && {
                    {
                        let col = &select.columns[0];
                        if let SelectColumn::Expr {
                            expr: Expr::Function { name, args, .. },
                            ..
                        } = col
                        {
                            name.eq_ignore_ascii_case("count")
                                && (args.is_empty()
                                    || matches!(args.first(), Some(Expr::IntegerLiteral(1))))
                        } else {
                            false
                        }
                    }
                };
            if is_count_star {
                if let FromClause::Table { name, .. } = from {
                    // S-NEW-2 fix: skip fast path if RLS is enabled on this table
                    let rls_on = self
                        .schema
                        .get_table(name)
                        .map(|t| t.rls_enabled)
                        .unwrap_or(false);
                    if !rls_on {
                        let table_root = self.schema.get_table(name)?.root_page;
                        let mut btree = BTree::new(self.get_table_pager_mut(name));
                        let count = btree.count_rows(table_root)? as i64;
                        return Ok(ExecResult::QueryResult {
                            columns: vec!["COUNT(*)".to_string()],
                            rows: vec![vec![Value::Integer(count)]],
                        });
                    }
                }
            }
            // ────────────────────────────────────────────────────────────────────
            if limit_pushdown {
                // LIMIT pushdown: scan only as many rows as needed
                if let FromClause::Table { name, .. } = from {
                    // S-NEW-3 fix: skip fast path if RLS is enabled on this table
                    let rls_on = self
                        .schema
                        .get_table(name)
                        .map(|t| t.rls_enabled)
                        .unwrap_or(false);
                    if !rls_on {
                        let empty = Vec::new();
                        let empty_map = HashMap::new();
                        // SAFETY: `limit_pushdown` is true only when `select.limit.is_some()`
                        let limit_val = match self.eval_expr(
                            select.limit.as_ref().unwrap(),
                            &empty,
                            &empty_map,
                        )? {
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
                        let mut btree = BTree::new(self.get_table_pager_mut(name));
                        let rows = btree.scan_rows_limit(root_page, total_needed)?;
                        (rows, col_names)
                    } else {
                        self.eval_from(from)?
                    }
                } else {
                    self.eval_from(from)?
                }
            } else if let Some(ref where_expr) = select.where_clause {
                // Try index-accelerated scan for simple WHERE conditions.
                // For FTS MATCH queries the index scan already filters and ranks rows;
                // we MUST NOT re-apply the WHERE clause (that path does literal string
                // containment, which would incorrectly drop OR-matched rows).
                let fts_handled = Self::where_is_fts_match(where_expr);
                if let Some(result) = self.try_index_scan(from, where_expr)? {
                    if fts_handled {
                        fts_index_scan_used = true;
                    }
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
        // For simple single-table FROM (no JOIN), extract rowids in parallel so that
        // VEC_SEARCH can look up each row's score by setting self.current_rowid.
        // JOINs / subqueries / CTEs get rowid 0 (VEC_SEARCH simply returns 0 there).
        let row_ids: Vec<i64> = if !rows.is_empty() {
            match &select.from {
                Some(FromClause::Table { name, .. })
                    if !self.cte_cache.contains_key(&name.to_ascii_lowercase()) =>
                {
                    let (root_page, _is_view) = {
                        if let Ok(t) = self.schema.get_table(name.as_str()) {
                            (t.root_page, t.view_select.is_some())
                        } else {
                            (0, false)
                        }
                    };
                    if root_page == 0 {
                        vec![0i64; rows.len()]
                    } else {
                        let with_ids = {
                            let p = self.get_table_pager_mut(name.as_str());
                            let mut bt = crate::storage::btree::BTree::new(p);
                            bt.scan_all(root_page).unwrap_or_default()
                        };
                        with_ids.into_iter().map(|(id, _)| id).collect()
                    }
                }
                _ => vec![0i64; rows.len()],
            }
        } else {
            Vec::new()
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

        // ── Q6: Rewrite non-correlated IN (subquery) → IN (list) ──────────────────
        // If InSubquery does not reference any outer columns, run the subquery
        // once and replace with InList to avoid O(rows * subquery) complexity.
        // ── RLS: inject USING predicates for tables with RLS enabled ────────────
        let mut rewritten_where = select.where_clause.clone();
        // If a FTS index scan already produced semantically-correct rows, do NOT
        // re-apply the WHERE clause (eval_expr for FtsMatch does literal containment
        // which would incorrectly drop OR-matched rows).
        if fts_index_scan_used {
            rewritten_where = None;
        }
        if let Some(ref mut where_expr) = rewritten_where {
            self.rewrite_uncorrelated_subqueries(where_expr, &col_names)?;
        }

        // Inject RLS policy USING expressions as additional WHERE conditions.
        // B12-3 fix: collect ALL base tables (including JOIN legs) so RLS cannot be bypassed
        // by wrapping a protected table inside a JOIN expression.
        if let Some(ref from) = select.from {
            let base_tables = Self::collect_base_tables(from);
            let current_user = self
                .session_vars
                .get("request.jwt.sub")
                .or_else(|| self.session_vars.get("kkdb.current_user"))
                .cloned()
                .unwrap_or_default();
            for tname in &base_tables {
                let tbl_key: String = tname.to_ascii_lowercase();
                let rls_exprs: Vec<crate::sql::ast::Expr> =
                    if let Some(tbl) = self.schema.tables.get(&tbl_key) {
                        if tbl.rls_enabled {
                            tbl.policies
                                .iter()
                                .filter(|p| {
                                    p.role.is_none() || p.role.as_deref() == Some(&current_user)
                                })
                                .filter_map(|p| p.using_expr.clone())
                                .collect()
                        } else {
                            Vec::new()
                        }
                    } else {
                        Vec::new()
                    };
                // Combine all USING expressions with AND
                for using_expr in rls_exprs {
                    rewritten_where = Some(match rewritten_where.take() {
                        None => using_expr,
                        Some(existing) => Expr::BinaryOp {
                            left: Box::new(existing),
                            op: BinaryOperator::And,
                            right: Box::new(using_expr),
                        },
                    });
                }
            }
        }

        // Apply WHERE filter
        if let Some(ref where_expr) = rewritten_where {
            let mut filtered = Vec::with_capacity(rows.len());
            let mut filtered_ids = Vec::with_capacity(rows.len());
            // Use zip to pair rows with their rowids for VEC_SEARCH support.
            let zipped: Vec<(i64, Vec<Value>)> = if row_ids.len() == rows.len() {
                row_ids.iter().copied().zip(rows).collect()
            } else {
                rows.into_iter().map(|r| (0i64, r)).collect()
            };
            for (rowid, row) in zipped {
                self.current_rowid = rowid;
                let val = self.eval_expr(where_expr, &row, &col_map)?;
                if val.is_truthy() {
                    filtered.push(row);
                    filtered_ids.push(rowid);
                }
            }
            rows = filtered;
            // Update row_ids to match filtered rows (for pre_sort below).
            let _ = filtered_ids; // kept in scope for future ORDER BY VEC_SEARCH support
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
                        } else {
                            0
                        };
                        Some((v as usize).saturating_add(offset))
                    }
                    _ => None,
                }
            } else {
                None
            };

            // Build sort keys (evaluate ORDER BY expressions against source rows).
            // We materialise keys once per row to avoid repeated evaluation in the comparator.
            let mut keyed: Vec<(Vec<Value>, Row)> = rows
                .into_iter()
                .enumerate()
                .map(|(i, row)| {
                    // Set current_rowid so VEC_SEARCH in ORDER BY works correctly.
                    self.current_rowid = *row_ids.get(i).unwrap_or(&0);
                    let keys: Vec<Value> = order_exprs
                        .iter()
                        .map(|(expr, _)| {
                            self.eval_expr(expr, &row, &col_map).unwrap_or(Value::Null)
                        })
                        .collect();
                    (keys, row)
                })
                .collect();

            // B12-4 fix: capture nulls_first info alongside ascending, and use
            // compare_value_pair so that NULLS FIRST/LAST is respected.
            let sort_params: Vec<(bool, Option<bool>)> = select
                .order_by
                .iter()
                .map(|item| (item.ascending, item.nulls_first))
                .collect();

            let cmp_fn = |a: &(Vec<Value>, Row), b: &(Vec<Value>, Row)| {
                for (i, &(ascending, nulls_first)) in sort_params.iter().enumerate() {
                    let ord = Self::compare_value_pair(&a.0[i], &b.0[i], nulls_first, ascending);
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
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
            self.apply_implicit_aggregate(
                &rows,
                &select.columns,
                &col_names,
                &col_map,
                &window_funcs,
                &select.window_defs,
            )?
        } else {
            self.project_columns(
                &select.columns,
                rows,
                &col_names,
                &col_map,
                false,
                &window_funcs,
                &select.window_defs,
            )?
        };

        // Apply DISTINCT
        if select.distinct {
            let mut seen = std::collections::HashSet::new();
            let mut key_buf = String::with_capacity(128);
            let mut val_buf = String::with_capacity(32);
            output_rows.retain(|row| {
                    key_buf.clear();
                    for v in row {
                        Self::typed_key_into(v, &mut val_buf);
                        key_buf.push_str(&val_buf);
                        key_buf.push('\0');
                    }
                    seen.insert(key_buf.clone())
                });
        }

        // Apply ORDER BY (aggregate/group-by path only — simple SELECT was sorted pre-projection)
        if !select.order_by.is_empty() && !pre_sort {
            let output_col_map: HashMap<String, usize> = output_col_names
                .iter()
                .enumerate()
                .map(|(i, name)| (name.to_lowercase(), i))
                .collect();

            // B-NEW-3 fix: precompute ORDER BY key values to support integer positions,
            // column names, aggregate functions, and arbitrary expressions.
            // We std::mem::take the rows so &mut self is free for eval_expr calls.
            let rows_snapshot = std::mem::take(&mut output_rows);
            let mut keyed_rows: Vec<(Vec<Value>, Vec<Value>)> = rows_snapshot
                .into_iter()
                .map(|row| {
                    let keys: Vec<Value> = select
                        .order_by
                        .iter()
                        .map(|item| match &item.expr {
                            // ORDER BY <integer> — 1-indexed column position
                            Expr::IntegerLiteral(n) => {
                                let idx = (*n as usize).saturating_sub(1);
                                row.get(idx).cloned().unwrap_or(Value::Null)
                            }
                            // ORDER BY <column_name>
                            Expr::ColumnRef { column, .. } => {
                                let lower = column.to_ascii_lowercase();
                                output_col_map
                                    .get(lower.as_str())
                                    .and_then(|&i| row.get(i).cloned())
                                    .unwrap_or(Value::Null)
                            }
                            // ORDER BY <expression> (COUNT(*), SUM, BinaryOp, etc.)
                            expr => self
                                .eval_expr(expr, &row, &output_col_map)
                                .unwrap_or(Value::Null),
                        })
                        .collect();
                    (keys, row)
                })
                .collect();

            let order_by_items = &select.order_by;
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

            let compare_keyed =
                |(ka, _): &(Vec<Value>, Vec<Value>), (kb, _): &(Vec<Value>, Vec<Value>)| {
                    for (i, item) in order_by_items.iter().enumerate() {
                        let a = ka.get(i).unwrap_or(&Value::Null);
                        let b = kb.get(i).unwrap_or(&Value::Null);
                        let ord = Self::compare_value_pair(a, b, item.nulls_first, item.ascending);
                        if ord != std::cmp::Ordering::Equal {
                            return ord;
                        }
                    }
                    std::cmp::Ordering::Equal
                };

            if let Some(k) = top_n_limit {
                if k == 0 {
                    keyed_rows.clear();
                } else if k < keyed_rows.len() {
                    keyed_rows.select_nth_unstable_by(k - 1, compare_keyed);
                    keyed_rows.truncate(k);
                    keyed_rows.sort_by(compare_keyed);
                } else {
                    keyed_rows.sort_by(compare_keyed);
                }
            } else {
                keyed_rows.sort_by(compare_keyed);
            }

            output_rows = keyed_rows.into_iter().map(|(_, row)| row).collect();
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

        // P-NEW-1 fix: clean up only the CTE keys inserted by this SELECT invocation.
        // Do NOT call cte_cache.clear() — nested/recursive CTEs may still be in use.
        for cte in &ctes_to_eval {
            self.cte_cache.remove(&cte.name.to_ascii_lowercase());
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

    /// B12-3: Recursively collect all base table names from a FROM clause.
    /// This ensures RLS policies are applied even when tables are accessed via JOINs.
    fn collect_base_tables(from: &FromClause) -> Vec<String> {
        match from {
            FromClause::Table { name, .. } => vec![name.clone()],
            FromClause::Join { left, right, .. } => {
                let mut names = Self::collect_base_tables(left);
                names.extend(Self::collect_base_tables(right));
                names
            }
            // Subqueries/table functions have their own RLS context; skip here.
            _ => vec![],
        }
    }

    #[inline]
    /// Compare a single pair of Values with NULL semantics and direction.
    /// Used by the precomputed ORDER BY key approach (B-NEW-3 fix).
    fn compare_value_pair(
        a: &Value,
        b: &Value,
        nulls_first: Option<bool>,
        ascending: bool,
    ) -> std::cmp::Ordering {
        let a_null = matches!(a, Value::Null);
        let b_null = matches!(b, Value::Null);
        if a_null || b_null {
            if a_null && b_null {
                return std::cmp::Ordering::Equal;
            }
            let nf = nulls_first.unwrap_or(!ascending);
            return if a_null {
                if nf {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            } else if nf {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            };
        }
        match a.partial_cmp(b) {
            Some(std::cmp::Ordering::Equal) | None => std::cmp::Ordering::Equal,
            Some(ord) => {
                if ascending {
                    ord
                } else {
                    ord.reverse()
                }
            }
        }
    }

    /// L7: Evaluate a recursive CTE (WITH RECURSIVE name AS (anchor UNION ALL recursive_part))
    ///
    /// The CTE's query is a `SelectStmt` whose `from` is a `SetOp { stmt: UNION ALL, alias: __setop__ }`.
    /// We extract the left (anchor) and right (recursive) parts, evaluate anchor first, then
    /// iterate the recursive part using the last iteration's rows until no new rows are produced.
    fn eval_recursive_cte(
        &mut self,
        cte: &CteDefinition,
    ) -> Result<(Vec<String>, Vec<crate::types::Row>)> {
        use crate::sql::ast::SetOpKind;

        // The CTE query body can be either a SetOp wrapping a UNION ALL, or
        // a SelectStmt whose FROM is a SetOp.  We detect the latter case.
        let (anchor_query, recursive_query) = {
            // Check if the query's FROM is a SetOp(__setop__) — this is what the parser generates
            // for `WITH RECURSIVE t AS (anchor UNION ALL rec_part)`
            let q = &cte.query;
            if let Some(crate::sql::ast::FromClause::SetOp { stmt, .. }) = q.from.as_ref() {
                if matches!(stmt.kind, SetOpKind::UnionAll | SetOpKind::UnionDistinct) {
                    (
                        stmt.left.as_ref().clone(),
                        Some(stmt.right.as_ref().clone()),
                    )
                } else {
                    // Non-union: just evaluate once (non-recursive)
                    return match self.exec_select(&cte.query)? {
                        ExecResult::QueryResult { columns, rows } => Ok((columns, rows)),
                        _ => Ok((Vec::new(), Vec::new())),
                    };
                }
            } else {
                // No SetOp in FROM — treat as non-recursive
                return match self.exec_select(&cte.query)? {
                    ExecResult::QueryResult { columns, rows } => Ok((columns, rows)),
                    _ => Ok((Vec::new(), Vec::new())),
                };
            }
        };

        // 1. Evaluate anchor
        let (mut col_names, mut all_rows) = match self.exec_select(&anchor_query)? {
            ExecResult::QueryResult { columns, rows } => (columns, rows),
            _ => return Ok((Vec::new(), Vec::new())),
        };

        // Apply declared column name aliases from `WITH RECURSIVE name(col1, col2, ...)` clause
        // This must be done early so the recursive part can reference columns by the declared names
        if !cte.columns.is_empty() {
            for (i, alias) in cte.columns.iter().enumerate() {
                if i < col_names.len() {
                    col_names[i] = alias.clone();
                }
            }
        }

        // 2. Install working table in cte_cache so the recursive part can reference `cte.name`
        let cte_key = cte.name.to_ascii_lowercase();

        if let Some(ref rec_q) = recursive_query {
            let mut iteration_rows = all_rows.clone();
            const MAX_RECURSION_DEPTH: usize = 1000;

            for _ in 0..MAX_RECURSION_DEPTH {
                if iteration_rows.is_empty() {
                    break;
                }
                // Expose current iteration as the CTE working table
                self.cte_cache
                    .insert(cte_key.clone(), (col_names.clone(), iteration_rows.clone()));

                let next = match self.exec_select(rec_q)? {
                    ExecResult::QueryResult { rows, .. } => rows,
                    _ => break,
                };

                if next.is_empty() {
                    break;
                }
                all_rows.extend(next.clone());
                iteration_rows = next;
            }
        }

        Ok((col_names, all_rows))
    }

    /// Evaluate FROM clause - returns (rows, column_names)
    fn eval_from(&mut self, from: &FromClause) -> Result<(Vec<Row>, Vec<String>)> {
        match from {
            FromClause::Table { name, alias: _ } => {
                // L7: Check CTE cache first — CTEs shadow real tables by name
                let cte_key = name.to_ascii_lowercase();
                if let Some((cols, rows)) = self.cte_cache.get(&cte_key).cloned() {
                    return Ok((rows, cols));
                }

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

                let mut btree = BTree::new(self.get_table_pager_mut(name));
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
                                    .or_default()
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
                                    .or_default()
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
                                    .or_default()
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
                            let mut hash: HashMap<String, ()> =
                                HashMap::with_capacity(right_rows.len());
                            for right_row in right_rows.iter() {
                                if matches!(right_row[right_idx], Value::Null) {
                                    continue;
                                }
                                Self::typed_key_into(&right_row[right_idx], &mut key_buf);
                                hash.insert(key_buf.clone(), ());
                            }
                            for left_row in &left_rows {
                                if matches!(left_row[left_idx], Value::Null) {
                                    continue;
                                }
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
                                        let val =
                                            self.eval_expr(on_expr, &combined, &combined_col_map)?;
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
                            let mut hash: HashMap<String, ()> =
                                HashMap::with_capacity(left_rows.len());
                            for left_row in left_rows.iter() {
                                if matches!(left_row[left_idx], Value::Null) {
                                    continue;
                                }
                                Self::typed_key_into(&left_row[left_idx], &mut key_buf);
                                hash.insert(key_buf.clone(), ());
                            }
                            for right_row in &right_rows {
                                if matches!(right_row[right_idx], Value::Null) {
                                    continue;
                                }
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
                                        let val =
                                            self.eval_expr(on_expr, &combined, &combined_col_map)?;
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
                                    self.eval_expr(cond, &combined, &combined_col_map)?
                                        .is_truthy()
                                } else {
                                    true
                                };
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
                        let common_cols: Vec<(usize, usize)> = left_cols
                            .iter()
                            .enumerate()
                            .filter_map(|(li, lname)| {
                                right_cols
                                    .iter()
                                    .position(|rname| rname.eq_ignore_ascii_case(lname))
                                    .map(|ri| (li, ri))
                            })
                            .collect();
                        // D-NEW-6 fix: right-side columns that duplicate join keys should be excluded
                        let right_shared: std::collections::HashSet<usize> =
                            common_cols.iter().map(|(_, ri)| *ri).collect();
                        for left_row in &left_rows {
                            'outer: for right_row in &right_rows {
                                for (li, ri) in &common_cols {
                                    if left_row[*li] != right_row[*ri] {
                                        continue 'outer;
                                    }
                                }
                                let mut combined = Vec::with_capacity(combined_width);
                                combined.extend_from_slice(left_row);
                                for (ri, val) in right_row.iter().enumerate() {
                                    if !right_shared.contains(&ri) {
                                        combined.push(val.clone());
                                    }
                                }
                                result_rows.push(combined);
                            }
                        }
                        // Build deduplicated column list
                        let cols_fixed: Vec<String> = left_cols
                            .iter()
                            .cloned()
                            .chain(
                                right_cols
                                    .iter()
                                    .enumerate()
                                    .filter(|(ri, _)| !right_shared.contains(ri))
                                    .map(|(_, name)| name.clone()),
                            )
                            .collect();
                        return Ok((result_rows, cols_fixed));
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
            FromClause::TableFunction {
                name,
                args,
                alias,
                column,
            } => {
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
                    .unwrap_or(match func_upper.as_str() {
                        "UNNEST" => "unnest",
                        "GENERATE_SERIES" => "generate_series",
                        _ => "value",
                    })
                    .to_string();

                // Optional alias-qualified column name for the col_map (t.col)
                let _qualified = alias
                    .as_ref()
                    .map(|a| format!("{}.{}", a.to_lowercase(), col_name.to_lowercase()));

                let rows: Vec<Row> = match func_upper.as_str() {
                    "UNNEST" => {
                        // UNNEST(array_value) — expand a JSON array or comma-separated list into rows
                        let arr_val = eval_args.into_iter().next().unwrap_or(Value::Null);
                        Self::unnest_value(arr_val)
                            .into_iter()
                            .map(|v| vec![v])
                            .collect()
                    }
                    "GENERATE_SERIES" => {
                        // generate_series(start, stop[, step])
                        let start = match eval_args.first() {
                            Some(Value::Integer(v)) => *v,
                            Some(Value::Real(v)) => *v as i64,
                            _ => {
                                return Err(KkdbError::RuntimeError(
                                    "generate_series: start must be integer".into(),
                                ))
                            }
                        };
                        let stop = match eval_args.get(1) {
                            Some(Value::Integer(v)) => *v,
                            Some(Value::Real(v)) => *v as i64,
                            _ => {
                                return Err(KkdbError::RuntimeError(
                                    "generate_series: stop must be integer".into(),
                                ))
                            }
                        };
                        let step = match eval_args.get(2) {
                            Some(Value::Integer(v)) => *v,
                            Some(Value::Real(v)) => *v as i64,
                            None => 1,
                            _ => 1,
                        };
                        if step == 0 {
                            return Err(KkdbError::RuntimeError(
                                "generate_series: step must not be zero".into(),
                            ));
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
                            if rows.len() >= 1_000_000 {
                                break;
                            }
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
                            Some('\\') if in_string => {
                                escape = true;
                                token.push('\\');
                            }
                            Some(c) if escape => {
                                escape = false;
                                token.push(c);
                            }
                            Some('"') => {
                                in_string = !in_string;
                                token.push('"');
                            }
                            Some(',') if !in_string => {
                                let t = token.trim();
                                if !t.is_empty() {
                                    result.push(Self::parse_json_scalar(t));
                                }
                                token.clear();
                            }
                            Some(c) => {
                                token.push(c);
                            }
                        }
                    }
                    result
                } else {
                    // CSV: split on commas
                    s.split(',')
                        .map(|p| Value::Text(p.trim().to_string().into()))
                        .collect()
                }
            }
            Value::Null => Vec::new(),
            scalar => vec![scalar],
        }
    }

    /// Parse a single JSON scalar token to a Value.
    fn parse_json_scalar(s: &str) -> Value {
        let s = s.trim();
        if s == "null" {
            return Value::Null;
        }
        if s == "true" {
            return Value::Integer(1);
        }
        if s == "false" {
            return Value::Integer(0);
        }
        // Quoted string
        if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
            return Value::Text(s[1..s.len() - 1].replace("\\\"", "\"").into());
        }
        // Number
        if let Ok(i) = s.parse::<i64>() {
            return Value::Integer(i);
        }
        if let Ok(f) = s.parse::<f64>() {
            return Value::Real(f);
        }
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

                            // Also insert unqualified name if not already present
                            // This allows window functions like `PARTITION BY category` to work
                            let unqual = col_names[idx].to_ascii_lowercase();
                            col_map.entry(unqual).or_insert(idx);
                        }
                    }
                    *offset += ncols;
                }
            }
            // Batch B: SetOp - columns unknown statically, skip injection
            FromClause::SetOp { .. } => {}
            // TableFunction: 1 column, qualified as alias.col
            FromClause::TableFunction {
                alias,
                column,
                name,
                ..
            } => {
                let col_label =
                    column
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
    #[allow(clippy::too_many_arguments)]
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
        let win_res =
            self.eval_window_functions_for_groups(window_funcs, &win_groups, col_map, window_defs)?;
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
                args.iter().any(Self::expr_has_aggregate)
            }
            Expr::BinaryOp { left, right, .. } => {
                Self::expr_has_aggregate(left) || Self::expr_has_aggregate(right)
            }
            Expr::UnaryOp { expr, .. } => Self::expr_has_aggregate(expr),
            Expr::Nested(inner) => Self::expr_has_aggregate(inner),
            Expr::IsNull { expr, .. } => Self::expr_has_aggregate(expr),
            Expr::InList { expr, list, .. } => {
                Self::expr_has_aggregate(expr) || list.iter().any(Self::expr_has_aggregate)
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
                table: Some(_),
                column,
            } => column.clone(),
            Expr::ColumnRef { column, .. } => column.clone(),
            Expr::Function { name, .. } => name.clone(),
            Expr::IntegerLiteral(v) => format!("{}", v),
            Expr::RealLiteral(v) => format!("{}", v),
            Expr::StringLiteral(v) => format!("'{}'", v),
            _ => "?".to_string(),
        }
    }

    #[allow(clippy::too_many_arguments)]
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
        // Build alias → expression map from SELECT column list.
        // This lets GROUP BY reference aliases defined in SELECT (e.g. GROUP BY doubled).
        let alias_map: HashMap<String, &Expr> = select_cols
            .iter()
            .filter_map(|c| {
                if let SelectColumn::Expr {
                    expr,
                    alias: Some(a),
                } = c
                {
                    Some((a.to_ascii_lowercase(), expr))
                } else {
                    None
                }
            })
            .collect();

        // Group rows by the GROUP BY expressions (store references, not clones)
        let mut group_index: HashMap<String, usize> = HashMap::new();
        let mut groups: Vec<Vec<&Row>> = Vec::new();
        let mut key_buf = String::with_capacity(128);
        let mut val_buf = String::with_capacity(32);

        for row in rows {
            key_buf.clear();
            for expr in group_exprs {
                // If the GROUP BY term is a bare column reference that matches a
                // SELECT alias, evaluate the aliased expression instead.
                let resolved: &Expr = if let Expr::ColumnRef {
                    column,
                    table: None,
                } = expr
                {
                    let lower = column.to_ascii_lowercase();
                    alias_map.get(lower.as_str()).copied().unwrap_or(expr)
                } else {
                    expr
                };
                let v = self.eval_expr(resolved, row, col_map)?;
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

        let win_res =
            self.eval_window_functions_for_groups(window_funcs, &groups, col_map, window_defs)?;
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
        let win_res =
            self.eval_window_functions_for_groups(window_funcs, &win_groups, col_map, window_defs)?;
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
                            // D-NEW-1 fix: checked_add to avoid silent overflow; promote to Real
                            match int_sum.checked_add(v) {
                                Some(s) => int_sum = s,
                                None => {
                                    real_sum = int_sum as f64 + v as f64;
                                    is_int = false;
                                }
                            }
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
                    return Ok(self.window_results.as_ref().map_or(Value::Null, |res| {
                        res[self.current_window_row_idx][idx].clone()
                    }));
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
                    .is_some_and(|o| o != std::cmp::Ordering::Less)
                    && val
                        .partial_cmp(&hi)
                        .is_some_and(|o| o != std::cmp::Ordering::Greater);
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
            Expr::Collate {
                expr: inner,
                collation: _,
            } => {
                // Ignore collation sorting rules during aggregate evaluation for now
                self.eval_expr_with_aggregates(inner, row, col_map, group_rows)
            }
            Expr::Interval {
                value,
                leading_field,
            } => {
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

        // L4/L5: Intercept MATCH for FTS tables before normal index lookups
        // Check for new CREATE FULLTEXT INDEX path (IndexSchema.is_fts=true)
        let fts_schema_indexes: Vec<_> = self
            .schema
            .indexes_for_table(&table_name)
            .into_iter()
            .filter(|idx| idx.is_fts)
            .cloned()
            .collect();

        if !fts_schema_indexes.is_empty() {
            // New BM25 path: CREATE FULLTEXT INDEX
            if let Expr::BinaryOp {
                left: _,
                op: BinaryOperator::FtsMatch,
                right,
            } = where_expr
            {
                if let Some(Value::Text(keyword)) = self.eval_constant_expr(right) {
                    return self.exec_fts_bm25_query(&table_name, &fts_schema_indexes, &keyword);
                }
            }
        }

        // Legacy FTS5 virtual table path (TableSchema.is_fts + *_fts_idx)
        let is_fts = self
            .schema
            .get_table(&table_name)
            .map(|t| t.is_fts)
            .unwrap_or(false);
        if is_fts {
            if let Expr::BinaryOp {
                left: _,
                op: BinaryOperator::FtsMatch,
                right,
            } = where_expr
            {
                if let Some(Value::Text(keyword)) = self.eval_constant_expr(right) {
                    let tokens = VM::tokenize(&keyword);
                    if tokens.is_empty() {
                        return Ok(Some((vec![], vec![])));
                    }

                    // Simple inverted index scan: query idx_<name>_fts_term
                    let fts_tbl = format!("{}_fts_idx", table_name);
                    let idx_term = format!("idx_{}_fts_term", table_name);

                    let mut doc_ids = std::collections::HashSet::new();
                    if let Some(idx) = self.schema.indexes.get(&idx_term).cloned() {
                        let search_val = Value::Text(tokens[0].clone().into());
                        // index_rowids_for_value gives us rowids of _fts_idx table rows
                        if let Ok(fts_entry_rowids) = self.index_rowids_for_value(&idx, &search_val)
                        {
                            if !fts_entry_rowids.is_empty() {
                                // Fetch those rows from _fts_idx to get the actual doc_id column
                                let fts_idx_table = self.schema.tables.get(&fts_tbl).cloned();
                                if let Some(fts_schema) = fts_idx_table {
                                    let fts_root = fts_schema.root_page;
                                    let fts_rows = self.fetch_rows_by_rowids(
                                        &fts_tbl,
                                        fts_root,
                                        &fts_entry_rowids,
                                    )?;
                                    for (_, fts_row) in fts_rows {
                                        // fts_idx schema: (id, term, doc_id) → doc_id is index 2
                                        if let Some(Value::Integer(doc_id)) = fts_row.get(2) {
                                            doc_ids.insert(*doc_id);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if doc_ids.is_empty() {
                        return Ok(Some((vec![], vec![])));
                    }

                    let table = self.schema.get_table(&table_name)?.clone();
                    let rowids: Vec<i64> = doc_ids.into_iter().collect();
                    let fetched_rows =
                        self.fetch_rows_by_rowids(&table_name, table.root_page, &rowids)?;

                    let mut out_rows = Vec::with_capacity(fetched_rows.len());
                    for (_, row) in fetched_rows {
                        out_rows.push(row);
                    }
                    return Ok(Some((out_rows, table.col_names.clone())));
                }
            }
        }

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
            None => {
                // O3: No index covers this column — track full-scan access
                self.record_full_scan_access(&table_name, &col_name);
                return Ok(None);
            }
        };

        let table = self.schema.get_table(&table_name)?.clone();
        let col_names = table.col_names.clone();

        // ── O2: Cost-Based Index Selection ───────────────────────────────────
        if let Some(col_info) = table
            .columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&col_name))
        {
            if let Some(ref stats) = col_info.stats {
                let total = stats.total_count as f64;
                if total > 0.0 {
                    // Estimate selectivity from predicate type and column statistics
                    let selectivity: f64 = match &lookup {
                        Lookup::Eq(_) => {
                            if stats.ndv > 0 {
                                1.0 / stats.ndv as f64
                            } else {
                                0.1
                            }
                        }
                        Lookup::In(vals) => {
                            if stats.ndv > 0 {
                                (vals.len() as f64 / stats.ndv as f64).min(1.0)
                            } else {
                                0.1
                            }
                        }
                        Lookup::Between(low, high) => {
                            // Linear interpolation over [min,max]
                            
                            match (&stats.min, &stats.max) {
                                (Some(Value::Integer(mn)), Some(Value::Integer(mx))) if mx > mn => {
                                    let lo = if let Value::Integer(v) = low {
                                        *v as f64
                                    } else {
                                        *mn as f64
                                    };
                                    let hi = if let Value::Integer(v) = high {
                                        *v as f64
                                    } else {
                                        *mx as f64
                                    };
                                    ((hi - lo) / (*mx - *mn) as f64).clamp(0.0, 1.0)
                                }
                                _ => 0.25,
                            }
                        }
                        Lookup::Comparison(op, val) => {
                            use crate::sql::ast::BinaryOperator;
                            match (&stats.min, &stats.max, val) {
                                (
                                    Some(Value::Integer(mn)),
                                    Some(Value::Integer(mx)),
                                    Value::Integer(v),
                                ) if mx > mn => {
                                    let range = (*mx - *mn) as f64;
                                    match op {
                                        BinaryOperator::LessThan
                                        | BinaryOperator::LessThanOrEqual => {
                                            ((*v - *mn) as f64 / range).clamp(0.0, 1.0)
                                        }
                                        BinaryOperator::GreaterThan
                                        | BinaryOperator::GreaterThanOrEqual => {
                                            ((*mx - *v) as f64 / range).clamp(0.0, 1.0)
                                        }
                                        _ => 0.1,
                                    }
                                }
                                _ => 0.1,
                            }
                        }
                    };
                    let expected_rows = (selectivity * total).max(1.0);
                    // seq scan cost ~= total_count; index cost = btree_lookup + 1.5×rowid_fetch
                    if 1.0 + expected_rows * 1.5 >= total {
                        // O3: Index exists but CBO prefers seq scan — still track for threshold
                        self.record_full_scan_access(&table_name, &col_name);
                        return Ok(None); // full scan is cheaper
                    }
                }
            }
        }
        // ── End O2 ──────────────────────────────────────────────────────────

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
        let fetched_rows =
            self.fetch_rows_by_rowids(&table_name, table.root_page, &matching_rowids)?;
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
            _ => {
                return Err(KkdbError::Internal(
                    "set-op right not a query result".into(),
                ))
            }
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
                left_rows
                    .into_iter()
                    .filter(|r| {
                        let k = row_key(r);
                        if let Some(c) = counts.get_mut(&k) {
                            if *c > 0 {
                                *c -= 1;
                                return true;
                            }
                        }
                        false
                    })
                    .collect()
            }
            SetOpKind::IntersectDistinct => {
                let right_keys: HashSet<Vec<u8>> = right_rows.iter().map(|r| row_key(r)).collect();
                let mut seen: HashSet<Vec<u8>> = HashSet::new();
                left_rows
                    .into_iter()
                    .filter(|r| {
                        let k = row_key(r);
                        right_keys.contains(&k) && seen.insert(k)
                    })
                    .collect()
            }
            SetOpKind::ExceptAll => {
                let mut counts: HashMap<Vec<u8>, usize> = HashMap::new();
                for row in &right_rows {
                    *counts.entry(row_key(row)).or_insert(0) += 1;
                }
                left_rows
                    .into_iter()
                    .filter(|r| {
                        let k = row_key(r);
                        if let Some(c) = counts.get_mut(&k) {
                            if *c > 0 {
                                *c -= 1;
                                return false;
                            }
                        }
                        true
                    })
                    .collect()
            }
            SetOpKind::ExceptDistinct => {
                let right_keys: HashSet<Vec<u8>> = right_rows.iter().map(|r| row_key(r)).collect();
                let mut seen: HashSet<Vec<u8>> = HashSet::new();
                left_rows
                    .into_iter()
                    .filter(|r| {
                        let k = row_key(r);
                        !right_keys.contains(&k) && seen.insert(k)
                    })
                    .collect()
            }
        };

        // Bug #1 fix: apply ORDER BY and LIMIT/OFFSET to the combined result
        let col_map: HashMap<String, usize> = col_names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i))
            .collect();
        let empty_row: Vec<Value> = Vec::new();

        // ORDER BY: evaluate expressions on each row, then sort
        if !setop.order_by.is_empty() {
            // Pre-evaluate sort keys to avoid repeated eval and borrow issues
            let mut keyed: Vec<(Vec<Value>, Vec<Value>)> = result_rows
                .into_iter()
                .map(|row| {
                    let keys: Vec<Value> = setop
                        .order_by
                        .iter()
                        .map(|item| {
                            self.eval_expr(&item.expr, &row, &col_map)
                                .unwrap_or(Value::Null)
                        })
                        .collect();
                    (keys, row)
                })
                .collect();
            keyed.sort_by(|(ka, _), (kb, _)| {
                for (i, item) in setop.order_by.iter().enumerate() {
                    let mut cmp = ka[i]
                        .partial_cmp(&kb[i])
                        .unwrap_or(std::cmp::Ordering::Equal);
                    if !item.ascending {
                        cmp = cmp.reverse();
                    }
                    if cmp != std::cmp::Ordering::Equal {
                        return cmp;
                    }
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
        let rows: Vec<Vec<Value>> = names
            .into_iter()
            .map(|n| vec![Value::Text(n.into())])
            .collect();
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
            Expr::UnaryOp { expr, .. }
            | Expr::Nested(expr)
            | Expr::Cast { expr, .. }
            | Expr::Collate { expr, .. }
            | Expr::Interval { value: expr, .. }
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
            Expr::Between {
                expr, low, high, ..
            } => {
                Self::extract_window_funcs(expr, extracted);
                Self::extract_window_funcs(low, extracted);
                Self::extract_window_funcs(high, extracted);
            }
            Expr::Like { expr, pattern, .. } => {
                Self::extract_window_funcs(expr, extracted);
                Self::extract_window_funcs(pattern, extracted);
            }
            Expr::Case {
                operand,
                when_clauses,
                else_clause,
            } => {
                if let Some(op) = operand {
                    Self::extract_window_funcs(op, extracted);
                }
                for (cond, res) in when_clauses {
                    Self::extract_window_funcs(cond, extracted);
                    Self::extract_window_funcs(res, extracted);
                }
                if let Some(el) = else_clause {
                    Self::extract_window_funcs(el, extracted);
                }
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
            let Expr::WindowFunction {
                func,
                partition_by,
                order_by,
                frame,
            } = window_expr
            else {
                continue;
            };

            let mut active_partition_by = partition_by;
            let mut active_order_by = order_by;
            let mut active_frame = frame;
            if let Some(Expr::ColumnRef { column, .. }) = partition_by.first() {
                if let Some(name) = column.strip_prefix("__named_window_") {
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
                        let v =
                            self.eval_expr_with_aggregates(expr, first_row, col_map, group_rows)?;
                        Self::typed_key_into(&v, &mut val_buf);
                        key_buf.push_str(&val_buf);
                        key_buf.push('\0');
                    }
                    partitions
                        .entry(key_buf.clone().into_bytes())
                        .or_default()
                        .push(g_idx);
                }
            }

            for (_, mut part_indices) in partitions {
                if !active_order_by.is_empty() {
                    let mut sort_keys: Vec<(usize, Vec<Value>)> = part_indices
                        .into_iter()
                        .map(|g_idx| {
                            let group_rows = &groups[g_idx];
                            let first_row = group_rows.first().unwrap_or(&empty_row_ref);
                            let keys: Vec<Value> = active_order_by
                                .iter()
                                .map(|item| {
                                    self.eval_expr_with_aggregates(
                                        &item.expr, first_row, col_map, group_rows,
                                    )
                                    .unwrap_or(Value::Null)
                                })
                                .collect();
                            (g_idx, keys)
                        })
                        .collect();

                    sort_keys.sort_by(|a, b| {
                        for (i, item) in active_order_by.iter().enumerate() {
                            let mut cmp = a.1[i]
                                .partial_cmp(&b.1[i])
                                .unwrap_or(std::cmp::Ordering::Equal);
                            if !item.ascending {
                                cmp = cmp.reverse();
                            }
                            if cmp != std::cmp::Ordering::Equal {
                                return cmp;
                            }
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
                                    let val =
                                        self.eval_constant_expr(expr).unwrap_or(Value::Integer(0));
                                    if let Value::Integer(v) = val {
                                        p.saturating_sub(v as usize)
                                    } else {
                                        0
                                    }
                                }
                                WindowBound::Following(expr) => {
                                    let val =
                                        self.eval_constant_expr(expr).unwrap_or(Value::Integer(0));
                                    if let Value::Integer(v) = val {
                                        p.saturating_add(v as usize)
                                    } else {
                                        p
                                    }
                                }
                                WindowBound::UnboundedFollowing => {
                                    part_indices.len().saturating_sub(1)
                                }
                            };
                            if let Some(eb) = &wf.end {
                                end = match eb {
                                    WindowBound::CurrentRow => p,
                                    WindowBound::UnboundedPreceding => 0,
                                    WindowBound::Preceding(expr) => {
                                        let val = self
                                            .eval_constant_expr(expr)
                                            .unwrap_or(Value::Integer(0));
                                        if let Value::Integer(v) = val {
                                            p.saturating_sub(v as usize)
                                        } else {
                                            p
                                        }
                                    }
                                    WindowBound::Following(expr) => {
                                        let val = self
                                            .eval_constant_expr(expr)
                                            .unwrap_or(Value::Integer(0));
                                        if let Value::Integer(v) = val {
                                            p.saturating_add(v as usize)
                                        } else {
                                            p
                                        }
                                    }
                                    WindowBound::UnboundedFollowing => {
                                        part_indices.len().saturating_sub(1)
                                    }
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
                            // RANK: peers (same ORDER BY keys) share the same rank
                            // Find position of first row with same ORDER BY key as p
                            let rank = if active_order_by.is_empty() {
                                p + 1
                            } else {
                                let mut rank = p + 1;
                                let g_rows = &groups[part_indices[p]];
                                let first_row = g_rows.first().unwrap_or(&empty_row_ref);
                                let cur_keys: Vec<Value> = active_order_by
                                    .iter()
                                    .map(|item| {
                                        self.eval_expr_with_aggregates(
                                            &item.expr, first_row, col_map, g_rows,
                                        )
                                        .unwrap_or(Value::Null)
                                    })
                                    .collect();
                                for q in 0..p {
                                    let g_rows_q = &groups[part_indices[q]];
                                    let first_row_q = g_rows_q.first().unwrap_or(&empty_row_ref);
                                    let q_keys: Vec<Value> = active_order_by
                                        .iter()
                                        .map(|item| {
                                            self.eval_expr_with_aggregates(
                                                &item.expr,
                                                first_row_q,
                                                col_map,
                                                g_rows_q,
                                            )
                                            .unwrap_or(Value::Null)
                                        })
                                        .collect();
                                    if q_keys == cur_keys {
                                        rank = q + 1;
                                        break;
                                    }
                                }
                                rank
                            };
                            Value::Integer(rank as i64)
                        }
                        WindowFunc::DenseRank => {
                            // DENSE_RANK: no gaps — count distinct ORDER BY key groups up to p
                            if active_order_by.is_empty() {
                                Value::Integer((p + 1) as i64)
                            } else {
                                let mut seen_keys: Vec<Vec<Value>> = Vec::new();
                                let g_rows = &groups[part_indices[p]];
                                let first_row = g_rows.first().unwrap_or(&empty_row_ref);
                                let cur_keys: Vec<Value> = active_order_by
                                    .iter()
                                    .map(|item| {
                                        self.eval_expr_with_aggregates(
                                            &item.expr, first_row, col_map, g_rows,
                                        )
                                        .unwrap_or(Value::Null)
                                    })
                                    .collect();
                                for q in 0..=p {
                                    let g_rows_q = &groups[part_indices[q]];
                                    let first_row_q = g_rows_q.first().unwrap_or(&empty_row_ref);
                                    let q_keys: Vec<Value> = active_order_by
                                        .iter()
                                        .map(|item| {
                                            self.eval_expr_with_aggregates(
                                                &item.expr,
                                                first_row_q,
                                                col_map,
                                                g_rows_q,
                                            )
                                            .unwrap_or(Value::Null)
                                        })
                                        .collect();
                                    if !seen_keys.contains(&q_keys) {
                                        seen_keys.push(q_keys);
                                    }
                                    if seen_keys[seen_keys.len() - 1] == cur_keys && q == p {
                                        break;
                                    }
                                }
                                Value::Integer(seen_keys.len() as i64)
                            }
                        }
                        WindowFunc::PercentRank => {
                            // PERCENT_RANK = (rank - 1) / (N - 1)
                            let n = part_indices.len();
                            if n <= 1 {
                                Value::Real(0.0)
                            } else {
                                let rank = if active_order_by.is_empty() {
                                    p + 1
                                } else {
                                    let g_rows = &groups[part_indices[p]];
                                    let first_row = g_rows.first().unwrap_or(&empty_row_ref);
                                    let cur_keys: Vec<Value> = active_order_by
                                        .iter()
                                        .map(|item| {
                                            self.eval_expr_with_aggregates(
                                                &item.expr, first_row, col_map, g_rows,
                                            )
                                            .unwrap_or(Value::Null)
                                        })
                                        .collect();
                                    let mut rk = p + 1;
                                    for q in 0..p {
                                        let g_rows_q = &groups[part_indices[q]];
                                        let fr_q = g_rows_q.first().unwrap_or(&empty_row_ref);
                                        let q_keys: Vec<Value> = active_order_by
                                            .iter()
                                            .map(|item| {
                                                self.eval_expr_with_aggregates(
                                                    &item.expr, fr_q, col_map, g_rows_q,
                                                )
                                                .unwrap_or(Value::Null)
                                            })
                                            .collect();
                                        if q_keys == cur_keys {
                                            rk = q + 1;
                                            break;
                                        }
                                    }
                                    rk
                                };
                                Value::Real((rank - 1) as f64 / (n - 1) as f64)
                            }
                        }
                        WindowFunc::CumeDist => {
                            // CUME_DIST = number of rows with order_by key <= current / N
                            let n = part_indices.len();
                            if n == 0 {
                                Value::Real(0.0)
                            } else {
                                let g_rows = &groups[part_indices[p]];
                                let first_row = g_rows.first().unwrap_or(&empty_row_ref);
                                let cur_keys: Vec<Value> = active_order_by
                                    .iter()
                                    .map(|item| {
                                        self.eval_expr_with_aggregates(
                                            &item.expr, first_row, col_map, g_rows,
                                        )
                                        .unwrap_or(Value::Null)
                                    })
                                    .collect();
                                // Count how many rows have key <= cur_keys (last position with same key)
                                let mut last_pos = p;
                                for q in p + 1..part_indices.len() {
                                    let g_rows_q = &groups[part_indices[q]];
                                    let fr_q = g_rows_q.first().unwrap_or(&empty_row_ref);
                                    let q_keys: Vec<Value> = active_order_by
                                        .iter()
                                        .map(|item| {
                                            self.eval_expr_with_aggregates(
                                                &item.expr, fr_q, col_map, g_rows_q,
                                            )
                                            .unwrap_or(Value::Null)
                                        })
                                        .collect();
                                    if q_keys == cur_keys {
                                        last_pos = q;
                                    } else {
                                        break;
                                    }
                                }
                                Value::Real((last_pos + 1) as f64 / n as f64)
                            }
                        }
                        WindowFunc::Ntile(n_expr) => {
                            // NTILE(n): divide partition into n roughly equal buckets
                            let n_total = part_indices.len();
                            let n_buckets = match self
                                .eval_constant_expr(n_expr)
                                .unwrap_or(Value::Integer(1))
                            {
                                Value::Integer(v) if v > 0 => v as usize,
                                _ => 1,
                            };
                            let bucket_size = n_total / n_buckets;
                            let remainder = n_total % n_buckets;
                            // First `remainder` buckets get bucket_size+1 rows
                            let bucket = if p < remainder * (bucket_size + 1) {
                                p / (bucket_size + 1) + 1
                            } else {
                                (p - remainder * (bucket_size + 1)) / bucket_size.max(1)
                                    + remainder
                                    + 1
                            };
                            Value::Integer(bucket.min(n_buckets) as i64)
                        }
                        WindowFunc::Lag {
                            expr: lag_expr,
                            offset,
                            default: default_expr,
                        } => {
                            let off = match offset {
                                Some(o) => {
                                    match self.eval_constant_expr(o).unwrap_or(Value::Integer(1)) {
                                        Value::Integer(v) => v as usize,
                                        _ => 1,
                                    }
                                }
                                None => 1,
                            };
                            let target_p = p.checked_sub(off);
                            if let Some(tp) = target_p {
                                let g_rows = &groups[part_indices[tp]];
                                let first_row = g_rows.first().unwrap_or(&empty_row_ref);
                                self.eval_expr_with_aggregates(lag_expr, first_row, col_map, g_rows)
                                    .unwrap_or(Value::Null)
                            } else {
                                match default_expr {
                                    Some(d) => self.eval_constant_expr(d).unwrap_or(Value::Null),
                                    None => Value::Null,
                                }
                            }
                        }
                        WindowFunc::Lead {
                            expr: lead_expr,
                            offset,
                            default: default_expr,
                        } => {
                            let off = match offset {
                                Some(o) => {
                                    match self.eval_constant_expr(o).unwrap_or(Value::Integer(1)) {
                                        Value::Integer(v) => v as usize,
                                        _ => 1,
                                    }
                                }
                                None => 1,
                            };
                            let target_p = p + off;
                            if target_p < part_indices.len() {
                                let g_rows = &groups[part_indices[target_p]];
                                let first_row = g_rows.first().unwrap_or(&empty_row_ref);
                                self.eval_expr_with_aggregates(
                                    lead_expr, first_row, col_map, g_rows,
                                )
                                .unwrap_or(Value::Null)
                            } else {
                                match default_expr {
                                    Some(d) => self.eval_constant_expr(d).unwrap_or(Value::Null),
                                    None => Value::Null,
                                }
                            }
                        }
                        WindowFunc::FirstValue(fv_expr) => {
                            if frame_indices.is_empty() {
                                Value::Null
                            } else {
                                let first_g = frame_indices[0];
                                let g_rows = &groups[first_g];
                                let first_row = g_rows.first().unwrap_or(&empty_row_ref);
                                self.eval_expr_with_aggregates(fv_expr, first_row, col_map, g_rows)
                                    .unwrap_or(Value::Null)
                            }
                        }
                        WindowFunc::LastValue(lv_expr) => {
                            if frame_indices.is_empty() {
                                Value::Null
                            } else {
                                // SAFETY: `frame_indices` is non-empty (checked above)
                                let last_g = *frame_indices.last().unwrap();
                                let g_rows = &groups[last_g];
                                let first_row = g_rows.first().unwrap_or(&empty_row_ref);
                                self.eval_expr_with_aggregates(lv_expr, first_row, col_map, g_rows)
                                    .unwrap_or(Value::Null)
                            }
                        }
                        WindowFunc::NthValue(nv_expr, n_expr) => {
                            let nth = match self
                                .eval_constant_expr(n_expr)
                                .unwrap_or(Value::Integer(1))
                            {
                                Value::Integer(v) if v >= 1 => (v - 1) as usize,
                                _ => 0,
                            };
                            if nth < frame_indices.len() {
                                let g_idx2 = frame_indices[nth];
                                let g_rows = &groups[g_idx2];
                                let first_row = g_rows.first().unwrap_or(&empty_row_ref);
                                self.eval_expr_with_aggregates(nv_expr, first_row, col_map, g_rows)
                                    .unwrap_or(Value::Null)
                            } else {
                                Value::Null
                            }
                        }
                        WindowFunc::Aggregate { name, args, .. } => {
                            let mut frame_vals = Vec::with_capacity(frame_indices.len());
                            for &f_idx in frame_indices {
                                let group_rows = &groups[f_idx];
                                let first_row = group_rows.first().unwrap_or(&empty_row_ref);
                                if !args.is_empty() {
                                    let v = self
                                        .eval_expr_with_aggregates(
                                            &args[0], first_row, col_map, group_rows,
                                        )
                                        .unwrap_or(Value::Null);
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
                                            if is_int {
                                                i_sum = i_sum.wrapping_add(x);
                                            } else {
                                                f_sum += x as f64;
                                            }
                                            valid = true;
                                        }
                                        Value::Real(x) => {
                                            if is_int {
                                                f_sum = i_sum as f64 + x;
                                                is_int = false;
                                            } else {
                                                f_sum += x;
                                            }
                                            valid = true;
                                        }
                                        _ => {}
                                    }
                                }
                                if !valid {
                                    Value::Null
                                } else if is_int {
                                    Value::Integer(i_sum)
                                } else {
                                    Value::Real(f_sum)
                                }
                            } else if name.eq_ignore_ascii_case("COUNT") {
                                let count = frame_vals
                                    .into_iter()
                                    .filter(|v| !matches!(v, Value::Null))
                                    .count();
                                Value::Integer(count as i64)
                            } else if name.eq_ignore_ascii_case("AVG") {
                                let non_null: Vec<Value> = frame_vals
                                    .into_iter()
                                    .filter(|v| !matches!(v, Value::Null))
                                    .collect();
                                if non_null.is_empty() {
                                    Value::Null
                                } else {
                                    let mut sum = 0.0f64;
                                    for v in &non_null {
                                        match v {
                                            Value::Integer(x) => sum += *x as f64,
                                            Value::Real(x) => sum += x,
                                            _ => {}
                                        }
                                    }
                                    Value::Real(sum / non_null.len() as f64)
                                }
                            } else if name.eq_ignore_ascii_case("MAX") {
                                frame_vals
                                    .into_iter()
                                    .filter(|v| !matches!(v, Value::Null))
                                    .max_by(|a, b| {
                                        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                                    })
                                    .unwrap_or(Value::Null)
                            } else if name.eq_ignore_ascii_case("MIN") {
                                frame_vals
                                    .into_iter()
                                    .filter(|v| !matches!(v, Value::Null))
                                    .min_by(|a, b| {
                                        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                                    })
                                    .unwrap_or(Value::Null)
                            } else {
                                Value::Null
                            }
                        }
                    };
                    results[g_idx][w_idx] = val;
                }
            }
        }
        Ok(results)
    }

    // ── Q6: Sub-query Rewriter ────────────────────────────────────────────────────

    /// Walk an expression tree and convert non-correlated `InSubquery` nodes
    /// into `InList` nodes, executing the subquery once and caching the result.
    ///
    /// A subquery is considered *non-correlated* when its WHERE clause does not
    /// contain any `ColumnRef` whose name matches a column in `outer_cols`.
    fn rewrite_uncorrelated_subqueries(
        &mut self,
        expr: &mut Expr,
        outer_cols: &[String],
    ) -> Result<()> {
        match expr {
            Expr::InSubquery {
                expr: inner,
                subquery,
                negated,
            } => {
                // Check whether subquery is non-correlated (no outer column refs)
                if !Self::subquery_references_outer(subquery, outer_cols) {
                    let result = self.exec_select(subquery)?;
                    let list = match result {
                        ExecResult::QueryResult { rows, .. } => rows
                            .into_iter()
                            .filter_map(|r| r.into_iter().next())
                            .map(|v| match v {
                                Value::Integer(i) => Expr::IntegerLiteral(i),
                                Value::Real(r) => Expr::RealLiteral(r),
                                Value::Text(s) => Expr::StringLiteral(s.to_string()),
                                _ => Expr::Null,
                            })
                            .collect::<Vec<_>>(),
                        _ => Vec::new(),
                    };
                    // Replace the whole node with InList
                    let neg = *negated;
                    let inner_owned = std::mem::replace(inner.as_mut(), Expr::Null);
                    *expr = Expr::InList {
                        expr: Box::new(inner_owned),
                        list,
                        negated: neg,
                    };
                }
            }
            Expr::BinaryOp { left, right, .. } => {
                self.rewrite_uncorrelated_subqueries(left, outer_cols)?;
                self.rewrite_uncorrelated_subqueries(right, outer_cols)?;
            }
            Expr::UnaryOp { expr: inner, .. } | Expr::Nested(inner) => {
                self.rewrite_uncorrelated_subqueries(inner, outer_cols)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Returns true if the subquery's WHERE references any column in `outer_cols`.
    fn subquery_references_outer(subquery: &SelectStmt, outer_cols: &[String]) -> bool {
        if let Some(ref where_expr) = subquery.where_clause {
            Self::expr_references_cols(where_expr, outer_cols)
        } else {
            false
        }
    }

    fn expr_references_cols(expr: &Expr, cols: &[String]) -> bool {
        match expr {
            Expr::ColumnRef { column, .. } => cols.iter().any(|c| c.eq_ignore_ascii_case(column)),
            Expr::BinaryOp { left, right, .. } => {
                Self::expr_references_cols(left, cols) || Self::expr_references_cols(right, cols)
            }
            Expr::UnaryOp { expr: inner, .. } | Expr::Nested(inner) => {
                Self::expr_references_cols(inner, cols)
            }
            Expr::Function { args, .. } => args.iter().any(|a| Self::expr_references_cols(a, cols)),
            Expr::InList {
                expr: inner, list, ..
            } => {
                Self::expr_references_cols(inner, cols)
                    || list.iter().any(|e| Self::expr_references_cols(e, cols))
            }
            _ => false,
        }
    }

    /// Returns true if the WHERE expression is (or contains) an FTS MATCH predicate
    /// that would be handled entirely by `exec_fts_bm25_query`.
    /// Used to suppress post-scan WHERE re-evaluation for FTS queries.
    fn where_is_fts_match(expr: &Expr) -> bool {
        match expr {
            Expr::BinaryOp {
                op: BinaryOperator::FtsMatch,
                ..
            } => true,
            // AND / OR compound: if any branch is FTS MATCH, treat as FTS-handled
            Expr::BinaryOp {
                op: BinaryOperator::And,
                left,
                right,
            }
            | Expr::BinaryOp {
                op: BinaryOperator::Or,
                left,
                right,
            } => Self::where_is_fts_match(left) || Self::where_is_fts_match(right),
            _ => false,
        }
    }

    /// Execute a BM25 full-text search query against a table with CREATE FULLTEXT INDEX.
    ///
    /// For each FTS index on `table_name`:
    /// 1. Tokenize the query string
    /// 2. For each token, scan postings in `_kkdb_fts_{index_id}` table
    /// 3. Compute BM25 score per doc_id (k1=1.2, b=0.75)
    /// 4. Fetch matching rows sorted by score descending
    pub(crate) fn exec_fts_bm25_query(
        &mut self,
        table_name: &str,
        fts_indexes: &[crate::schema::IndexSchema],
        keyword: &str,
    ) -> Result<Option<(Vec<crate::types::Row>, Vec<String>)>> {
        use std::collections::HashMap;

        // BM25 parameters
        const K1: f64 = 1.2;
        const B: f64 = 0.75;

        let query_tokens = crate::fulltext::tokenizer::query_tokenize(keyword);
        if query_tokens.is_empty() {
            return Ok(Some((vec![], vec![])));
        }

        // Aggregate BM25 scores across all FTS indexes for this table
        // doc_id -> cumulative BM25 score
        let mut scores: HashMap<u64, f64> = HashMap::new();

        for fts_idx in fts_indexes {
            let index_id = fts_idx.root_page; // repurposed as index_id for FTS

            // Read global stats: (total_docs N, total_field_len)
            let (n_docs, total_field_len) = self.read_fts_global_stats(index_id);
            if n_docs == 0 {
                continue;
            }
            let avg_dl = total_field_len as f64 / n_docs as f64;

            for token in &query_tokens {
                // Scan postings first: Vec<(doc_id, tf, field_len)>
                let postings = self.scan_fts_postings(index_id, token);
                if postings.is_empty() {
                    continue;
                }

                // doc_freq (df) for IDF calculation.
                // If the stored DF row is missing/stale (can happen after B-Tree splits in
                // the DML write-path), fall back to counting the postings directly so the
                // IDF formula is always self-consistent.
                let stored_df = self.get_fts_doc_freq(index_id, token);
                let df = if stored_df > 0 {
                    stored_df
                } else {
                    postings.len() as u64
                };

                // IDF(t) = log((N - df + 0.5) / (df + 0.5) + 1)
                let idf = ((n_docs as f64 - df as f64 + 0.5) / (df as f64 + 0.5) + 1.0).ln();

                for (doc_id, tf, field_len) in postings {
                    let dl = field_len as f64;
                    // BM25 term score = IDF * (tf * (k1 + 1)) / (tf + k1 * (1 - b + b * dl / avgdl))
                    let tf_norm = tf as f64 * (K1 + 1.0)
                        / (tf as f64 + K1 * (1.0 - B + B * dl / avg_dl.max(1.0)));
                    let term_score = idf * tf_norm;
                    *scores.entry(doc_id).or_insert(0.0) += term_score;
                }
            }
        }

        if scores.is_empty() {
            let table = self.schema.get_table(table_name)?;
            return Ok(Some((vec![], table.col_names.clone())));
        }

        // Sort doc_ids by BM25 score descending
        let mut scored_docs: Vec<(u64, f64)> = scores.into_iter().collect();
        scored_docs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Fetch original rows by doc_id (rowid) — Double Lookup / 回表
        let table = self.schema.get_table(table_name)?.clone();
        let rowids: Vec<i64> = scored_docs
            .iter()
            .map(|(doc_id, _)| *doc_id as i64)
            .collect();
        let fetched = self.fetch_rows_by_rowids(table_name, table.root_page, &rowids)?;

        // Re-sort fetched rows to match BM25 score order
        let rowid_to_row: HashMap<i64, _> = fetched.into_iter().collect();
        let mut out_rows = Vec::with_capacity(rowids.len());
        for rowid in &rowids {
            if let Some(row) = rowid_to_row.get(rowid) {
                out_rows.push(row.clone());
            }
        }

        Ok(Some((out_rows, table.col_names)))
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
