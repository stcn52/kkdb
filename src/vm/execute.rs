#[cfg(test)]
pub(crate) use super::eval_expr::like_match;
use crate::error::Result;
use crate::schema::Schema;
use crate::sql::ast::*;
use crate::storage::btree::BTree;
use crate::storage::pager::Pager;
pub(crate) use crate::types::Value;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

/// Result of executing a statement
#[derive(Debug)]
pub enum ExecResult {
    /// DDL result (CREATE, DROP)
    Ok { message: String },
    /// DML result (INSERT, UPDATE, DELETE)
    RowsAffected { count: usize, message: String },
    /// Query result (SELECT)
    QueryResult {
        columns: Vec<String>,
        rows: Vec<Vec<crate::types::Value>>,
    },
    /// EXPLAIN result
    Explain { plan: String },
}

/// The virtual machine that executes SQL statements
pub struct VM {
    pub pager: Pager,
    pub schema: Schema,
    pub(crate) stmt_cache: HashMap<String, Statement>,
    pub(crate) stmt_cache_fifo: VecDeque<String>,
    /// index_name(lowercase) -> (encoded first-column key -> table rowids)
    pub(crate) index_eq_cache: HashMap<String, HashMap<Vec<u8>, Vec<i64>>>,
    /// index_name(lowercase) -> (table rowid -> index-entry rowid)
    pub(crate) index_rowid_cache: HashMap<String, HashMap<i64, i64>>,
    /// index_name(lowercase) -> sorted (first-column value, table rowid)
    pub(crate) index_ordered_cache: HashMap<String, Vec<(Value, i64)>>,
    /// Schema snapshot saved at BEGIN for ROLLBACK
    pub(crate) schema_snapshot: Option<Schema>,
    pub(crate) window_results: Option<Vec<Vec<Value>>>,
    pub(crate) current_window_row_idx: usize,
}

impl VM {
    /// Create a new VM with an in-memory database
    pub fn new_memory() -> Self {
        let pager = Pager::open_memory();
        VM {
            pager,
            schema: Schema::new(),
            stmt_cache: HashMap::with_capacity(64),
            stmt_cache_fifo: VecDeque::with_capacity(256),
            index_eq_cache: HashMap::with_capacity(32),
            index_rowid_cache: HashMap::with_capacity(32),
            index_ordered_cache: HashMap::with_capacity(32),
            schema_snapshot: None,
            window_results: None,
            current_window_row_idx: 0,
        }
    }

    /// Create a new VM with a file-based database
    pub fn open(path: &str) -> Result<Self> {
        let pager = Pager::open(path)?;
        let mut vm = VM {
            pager,
            schema: Schema::new(),
            stmt_cache: HashMap::with_capacity(64),
            stmt_cache_fifo: VecDeque::with_capacity(256),
            index_eq_cache: HashMap::with_capacity(32),
            index_rowid_cache: HashMap::with_capacity(32),
            index_ordered_cache: HashMap::with_capacity(32),
            schema_snapshot: None,
            window_results: None,
            current_window_row_idx: 0,
        };
        vm.schema.load_from_pager(&mut vm.pager)?;
        Ok(vm)
    }

    /// Execute a SQL string (with statement cache)
    #[inline]
    pub fn execute_sql(&mut self, sql: &str) -> Result<ExecResult> {
        if let Some(cached) = self.stmt_cache.get(sql) {
            let stmt = cached.clone();
            return self.execute_statement(&stmt, sql);
        }

        let stmt = crate::sql::parser::parse_sql(sql)?;
        // Cache bounded to 256 entries, evicted in FIFO order.
        if !self.stmt_cache.contains_key(sql) {
            if self.stmt_cache.len() >= 256 {
                while let Some(victim) = self.stmt_cache_fifo.pop_front() {
                    if self.stmt_cache.remove(&victim).is_some() {
                        break;
                    }
                }
            }
            self.stmt_cache_fifo.push_back(sql.to_string());
        }
        self.stmt_cache.insert(sql.to_string(), stmt.clone());
        self.execute_statement(&stmt, sql)
    }

    /// Execute a parsed statement
    #[inline]
    fn execute_statement(&mut self, stmt: &Statement, original_sql: &str) -> Result<ExecResult> {
        match stmt {
            Statement::CreateTable(create) => self.exec_create_table(create, original_sql),
            Statement::DropTable(drop) => self.exec_drop_table(drop),
            Statement::Insert(insert) => self.exec_insert(insert),
            Statement::Select(select) => self.exec_select(select),
            Statement::Update(update) => self.exec_update(update),
            Statement::Delete(delete) => self.exec_delete(delete),
            Statement::CreateIndex(create_idx) => self.exec_create_index(create_idx),
            Statement::DropIndex(drop_idx) => self.exec_drop_index(drop_idx),
            Statement::AlterTable(alter) => self.exec_alter_table(alter),
            Statement::Begin => {
                self.pager.begin_transaction()?;
                self.schema_snapshot = Some(self.schema.clone());
                self.clear_index_caches();
                Ok(ExecResult::Ok {
                    message: "Transaction started".into(),
                })
            }
            Statement::Commit => {
                self.pager.commit_transaction()?;
                self.schema_snapshot = None;
                self.clear_index_caches();
                Ok(ExecResult::Ok {
                    message: "Committed".into(),
                })
            }
            Statement::Rollback => {
                self.pager.rollback_transaction()?;
                if let Some(snapshot) = self.schema_snapshot.take() {
                    self.schema = snapshot;
                }
                self.clear_index_caches();
                Ok(ExecResult::Ok {
                    message: "Rolled back".into(),
                })
            }
            Statement::Savepoint(name) => {
                self.pager.savepoint(name)?;
                Ok(ExecResult::Ok { message: format!("SAVEPOINT {name}") })
            }
            Statement::ReleaseSavepoint(name) => {
                self.pager.release_savepoint(name)?;
                Ok(ExecResult::Ok { message: format!("RELEASE SAVEPOINT {name}") })
            }
            Statement::RollbackToSavepoint(name) => {
                self.pager.rollback_to_savepoint(name)?;
                if let Some(snapshot) = self.schema_snapshot.take() {
                    self.schema = snapshot;
                }
                self.clear_index_caches();
                Ok(ExecResult::Ok { message: format!("ROLLBACK TO SAVEPOINT {name}") })
            }
            Statement::SetOp(setop) => self.exec_set_op(setop),
            Statement::ShowTables => self.exec_show_tables(),
            // Batch E: CREATE VIEW
            Statement::CreateView(create) => self.exec_create_view(create),
            Statement::Explain(inner) => self.exec_explain(inner),
        }
    }

    /// Flush to disk only if not inside an explicit transaction (auto-commit mode).
    /// Inside a transaction, changes accumulate until COMMIT.
    #[inline]
    pub(crate) fn auto_flush(&mut self) -> Result<()> {
        if !self.pager.in_transaction() {
            self.pager.flush()?;
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn clear_index_caches(&mut self) {
        self.index_eq_cache.clear();
        self.index_rowid_cache.clear();
        self.index_ordered_cache.clear();
    }

    #[inline]
    pub(crate) fn eval_constant_expr(&mut self, expr: &Expr) -> Option<Value> {
        let empty_row = Vec::new();
        let empty_map = HashMap::new();
        self.eval_expr(expr, &empty_row, &empty_map).ok()
    }

    #[inline]
    pub(crate) fn index_eq_key(value: &Value) -> Vec<u8> {
        match value {
            // Keep numeric equality consistent with Value::PartialEq.
            Value::Integer(v) => {
                let mut out = Vec::with_capacity(1 + 8);
                out.push(b'N');
                out.extend_from_slice(&(*v as f64).to_le_bytes());
                out
            }
            Value::Real(v) => {
                let mut out = Vec::with_capacity(1 + 8);
                out.push(b'N');
                out.extend_from_slice(&v.to_le_bytes());
                out
            }
            Value::Text(v) => {
                let bytes = v.as_bytes();
                let mut out = Vec::with_capacity(1 + bytes.len());
                out.push(b'T');
                out.extend_from_slice(bytes);
                out
            }
            Value::Blob(v) => {
                let mut out = Vec::with_capacity(1 + v.len());
                out.push(b'B');
                out.extend_from_slice(v);
                out
            }
            Value::Null => vec![b'Z'],
        }
    }

    pub(crate) fn ensure_index_cache_loaded(
        &mut self,
        index: &crate::schema::IndexSchema,
    ) -> Result<()> {
        let index_key = index.name.to_lowercase();
        if self.index_eq_cache.contains_key(&index_key)
            && self.index_rowid_cache.contains_key(&index_key)
            && self.index_ordered_cache.contains_key(&index_key)
        {
            return Ok(());
        }

        let mut eq_map: HashMap<Vec<u8>, Vec<i64>> = HashMap::new();
        let mut rowid_map: HashMap<i64, i64> = HashMap::new();
        let mut ordered_entries: Vec<(Value, i64)> = Vec::new();

        let mut btree = crate::storage::btree::BTree::new(&mut self.pager);
        let entries = btree.scan_all(index.root_page)?;
        for (idx_rowid, idx_row) in entries {
            if idx_row.len() < 2 {
                continue;
            }
            if let Some(Value::Integer(table_rowid)) = idx_row.last() {
                let k = Self::index_eq_key(&idx_row[0]);
                eq_map.entry(k).or_default().push(*table_rowid);
                rowid_map.insert(*table_rowid, idx_rowid);
                if Self::value_has_total_order(&idx_row[0]) {
                    ordered_entries.push((idx_row[0].clone(), *table_rowid));
                }
            }
        }
        ordered_entries.sort_unstable_by(|(lv, lr), (rv, rr)| {
            let ord = Self::cmp_value_total(lv, rv);
            if ord == Ordering::Equal {
                lr.cmp(rr)
            } else {
                ord
            }
        });

        self.index_eq_cache.insert(index_key.clone(), eq_map);
        self.index_rowid_cache.insert(index_key.clone(), rowid_map);
        self.index_ordered_cache.insert(index_key, ordered_entries);
        Ok(())
    }

    pub(crate) fn index_rowids_for_value(
        &mut self,
        index: &crate::schema::IndexSchema,
        value: &Value,
    ) -> Result<Vec<i64>> {
        self.ensure_index_cache_loaded(index)?;
        let key = index.name.to_lowercase();
        if let Some(map) = self.index_eq_cache.get(&key) {
            let encoded = Self::index_eq_key(value);
            Ok(map.get(&encoded).cloned().unwrap_or_default())
        } else {
            Ok(Vec::new())
        }
    }

    #[inline]
    pub(crate) fn flip_comparison_operator(op: &BinaryOperator) -> Option<BinaryOperator> {
        match op {
            BinaryOperator::Equal => Some(BinaryOperator::Equal),
            BinaryOperator::LessThan => Some(BinaryOperator::GreaterThan),
            BinaryOperator::LessThanOrEqual => Some(BinaryOperator::GreaterThanOrEqual),
            BinaryOperator::GreaterThan => Some(BinaryOperator::LessThan),
            BinaryOperator::GreaterThanOrEqual => Some(BinaryOperator::LessThanOrEqual),
            _ => None,
        }
    }

    #[inline]
    fn value_has_total_order(value: &Value) -> bool {
        value.partial_cmp(value).is_some()
    }

    #[inline]
    fn cmp_value_total(left: &Value, right: &Value) -> Ordering {
        left.partial_cmp(right).unwrap_or(Ordering::Equal)
    }

    #[inline]
    fn lower_bound_index_entries(entries: &[(Value, i64)], value: &Value) -> usize {
        entries.partition_point(|(v, _)| Self::cmp_value_total(v, value) == Ordering::Less)
    }

    #[inline]
    fn upper_bound_index_entries(entries: &[(Value, i64)], value: &Value) -> usize {
        entries.partition_point(|(v, _)| {
            matches!(
                Self::cmp_value_total(v, value),
                Ordering::Less | Ordering::Equal
            )
        })
    }

    #[inline]
    fn slice_table_rowids(entries: &[(Value, i64)], start: usize, end: usize) -> Vec<i64> {
        entries[start..end].iter().map(|(_, rid)| *rid).collect()
    }

    #[inline]
    fn ordered_index_entries_for<'b>(
        &'b mut self,
        index: &crate::schema::IndexSchema,
    ) -> Result<&'b Vec<(Value, i64)>> {
        self.ensure_index_cache_loaded(index)?;
        let key = index.name.to_lowercase();
        Ok(self.index_ordered_cache.entry(key).or_default())
    }

    #[inline]
    fn range_bounds_for_comparison(
        entries: &[(Value, i64)],
        op: &BinaryOperator,
        value: &Value,
    ) -> (usize, usize) {
        match op {
            BinaryOperator::LessThan => (0, Self::lower_bound_index_entries(entries, value)),
            BinaryOperator::LessThanOrEqual => (0, Self::upper_bound_index_entries(entries, value)),
            BinaryOperator::GreaterThan => (
                Self::upper_bound_index_entries(entries, value),
                entries.len(),
            ),
            BinaryOperator::GreaterThanOrEqual => (
                Self::lower_bound_index_entries(entries, value),
                entries.len(),
            ),
            _ => (0, 0),
        }
    }

    pub(crate) fn index_rowids_for_comparison(
        &mut self,
        index: &crate::schema::IndexSchema,
        op: &BinaryOperator,
        value: &Value,
    ) -> Result<Vec<i64>> {
        // SQL comparisons with NULL never evaluate to TRUE in WHERE.
        if matches!(value, Value::Null) {
            return Ok(Vec::new());
        }

        if *op == BinaryOperator::Equal {
            return self.index_rowids_for_value(index, value);
        }
        if !Self::value_has_total_order(value) {
            return Ok(Vec::new());
        }

        let entries = self.ordered_index_entries_for(index)?;
        let (start, end) = Self::range_bounds_for_comparison(entries, op, value);
        Ok(Self::slice_table_rowids(entries, start, end))
    }

    pub(crate) fn index_rowids_for_between(
        &mut self,
        index: &crate::schema::IndexSchema,
        low: &Value,
        high: &Value,
    ) -> Result<Vec<i64>> {
        if matches!(low, Value::Null) || matches!(high, Value::Null) {
            return Ok(Vec::new());
        }
        if !Self::value_has_total_order(low) || !Self::value_has_total_order(high) {
            return Ok(Vec::new());
        }
        if Self::cmp_value_total(low, high) == Ordering::Greater {
            return Ok(Vec::new());
        }

        let entries = self.ordered_index_entries_for(index)?;
        let start = Self::lower_bound_index_entries(entries, low);
        let end = Self::upper_bound_index_entries(entries, high);
        Ok(Self::slice_table_rowids(entries, start, end))
    }

    pub(crate) fn index_entry_rowid_for_table_rowid(
        &mut self,
        index: &crate::schema::IndexSchema,
        table_rowid: i64,
    ) -> Result<Option<i64>> {
        self.ensure_index_cache_loaded(index)?;
        let key = index.name.to_lowercase();
        Ok(self
            .index_rowid_cache
            .get(&key)
            .and_then(|m| m.get(&table_rowid).copied()))
    }

    pub(crate) fn index_cache_insert(
        &mut self,
        index: &crate::schema::IndexSchema,
        first_col_value: &Value,
        table_rowid: i64,
        index_entry_rowid: i64,
    ) {
        let key = index.name.to_lowercase();
        if !self.index_eq_cache.contains_key(&key) || !self.index_rowid_cache.contains_key(&key) {
            return;
        }
        let encoded = Self::index_eq_key(first_col_value);
        if let Some(eq_map) = self.index_eq_cache.get_mut(&key) {
            eq_map.entry(encoded).or_default().push(table_rowid);
        }
        if let Some(rowid_map) = self.index_rowid_cache.get_mut(&key) {
            rowid_map.insert(table_rowid, index_entry_rowid);
        }
        if let Some(ordered) = self.index_ordered_cache.get_mut(&key) {
            if Self::value_has_total_order(first_col_value) {
                let pos = ordered.partition_point(|(v, rid)| {
                    let ord = Self::cmp_value_total(v, first_col_value);
                    ord == Ordering::Less || (ord == Ordering::Equal && *rid < table_rowid)
                });
                ordered.insert(pos, (first_col_value.clone(), table_rowid));
            }
        }
    }

    pub(crate) fn index_cache_delete(
        &mut self,
        index: &crate::schema::IndexSchema,
        first_col_value: &Value,
        table_rowid: i64,
    ) {
        let key = index.name.to_lowercase();
        if let Some(eq_map) = self.index_eq_cache.get_mut(&key) {
            let encoded = Self::index_eq_key(first_col_value);
            if let Some(rows) = eq_map.get_mut(&encoded) {
                rows.retain(|rid| *rid != table_rowid);
                if rows.is_empty() {
                    eq_map.remove(&encoded);
                }
            }
        }
        if let Some(map) = self.index_rowid_cache.get_mut(&key) {
            map.remove(&table_rowid);
        }
        if let Some(ordered) = self.index_ordered_cache.get_mut(&key) {
            let pos = ordered.partition_point(|(v, rid)| {
                let ord = Self::cmp_value_total(v, first_col_value);
                ord == Ordering::Less || (ord == Ordering::Equal && *rid < table_rowid)
            });
            if pos < ordered.len()
                && ordered[pos].0 == *first_col_value
                && ordered[pos].1 == table_rowid
            {
                ordered.remove(pos);
            }
        }
    }

    pub(crate) fn fetch_rows_by_rowids(
        &mut self,
        root_page: u32,
        rowids: &[i64],
    ) -> Result<Vec<(i64, crate::types::Row)>> {
        if rowids.is_empty() {
            return Ok(Vec::new());
        }

        // For large candidate sets, a single scan + hash probe is usually cheaper
        // than many random point lookups.
        const BULK_SCAN_THRESHOLD: usize = 96;
        if rowids.len() >= BULK_SCAN_THRESHOLD {
            let mut wanted: HashSet<i64> = rowids.iter().copied().collect();
            let mut by_id: HashMap<i64, crate::types::Row> = HashMap::with_capacity(wanted.len());
            let mut btree = BTree::new(&mut self.pager);
            for (rid, row) in btree.scan_all(root_page)? {
                if wanted.remove(&rid) {
                    by_id.insert(rid, row);
                    if wanted.is_empty() {
                        break;
                    }
                }
            }
            let mut out = Vec::with_capacity(rowids.len());
            for rid in rowids {
                if let Some(row) = by_id.remove(rid) {
                    out.push((*rid, row));
                }
            }
            return Ok(out);
        }

        let mut out = Vec::with_capacity(rowids.len());
        let mut btree = BTree::new(&mut self.pager);
        for rid in rowids {
            if let Some((found_rid, row)) = btree.find_by_rowid(root_page, *rid)? {
                out.push((found_rid, row));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "execute_tests.rs"]
mod tests;
