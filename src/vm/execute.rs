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
use std::path::{Path, PathBuf};

/// Result of executing a single SQL statement.
///
/// Returned by [`VM::execute_sql`].
#[derive(Debug)]
pub enum ExecResult {
    /// Successful DDL (CREATE / DROP / ALTER) or transaction command.
    Ok { message: String },
    /// Completed DML (INSERT / UPDATE / DELETE): number of rows affected.
    RowsAffected { count: usize, message: String },
    /// Completed SELECT: column names and result rows.
    QueryResult {
        columns: Vec<String>,
        rows: Vec<Vec<crate::types::Value>>,
    },
    /// Query plan text from EXPLAIN.
    Explain { plan: String },
}

/// The virtual machine that parses and executes SQL statements.
///
/// The recommended entry point for all database operations.
///
/// # Storage modes
///
/// | Mode | Constructor |
/// |------|-------------|
/// | In-memory (no persistence) | [`VM::new_memory`] |
/// | File-based, per-table files | [`VM::open`] |
/// | Legacy single-file (backward compat) | [`VM::open_legacy`] |
///
/// # File layout (multi-file mode)
///
/// ```text
/// mydb/
///   catalog.kkdb  ← schema B-Tree
///   users.kkdb    ← users table data
///   orders.kkdb   ← orders table data
///   binlog.bin
/// ```
///
/// # Flushing
///
/// In auto-commit mode every DML/DDL call triggers an `auto_flush`.
/// Additionally, `VM` implements `Drop` so all dirty pages are written on
/// destruction (best-effort).
pub struct VM {
    /// Catalog pager: holds the schema B-Tree.
    /// In single-file/memory mode this also holds all table data.
    pub pager: Pager,
    /// Per-table pagers (multi-file directory mode):
    /// `table_name_lowercase → Pager` for each table's `.kkdb` file.
    pub(crate) table_pagers: HashMap<String, Pager>,
    /// Database directory path (multi-file mode). `None` in memory/legacy mode.
    pub(crate) db_dir: Option<PathBuf>,
    /// In-memory schema metadata cache (tables, indexes, views).
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
    pub binlog: crate::binlog::BinlogManager,
}

impl Drop for VM {
    fn drop(&mut self) {
        // Best-effort flush of all pagers on VM destruction.
        // Errors are silently ignored here since we can't propagate from Drop.
        let _ = self.pager.flush();
        for tbl_pager in self.table_pagers.values_mut() {
            let _ = tbl_pager.flush();
        }
    }
}

impl VM {
    /// Validate that a table name is safe to use as a filename component.
    pub(crate) fn is_safe_table_name(name: &str) -> bool {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// Open or create a per-table pager in `db_dir`, registering it in `table_pagers`.
    /// Returns a mutable reference to the pager for this table.
    pub(crate) fn open_or_create_table_pager(
        table_pagers: &mut HashMap<String, Pager>,
        db_dir: &Path,
        table_name: &str,
    ) -> Result<()> {
        let key = table_name.to_ascii_lowercase();
        if table_pagers.contains_key(&key) {
            return Ok(());
        }
        let path = db_dir.join(format!("{}.kkdb", key));
        let pager = Pager::open(&path)?;
        table_pagers.insert(key, pager);
        Ok(())
    }

    /// Return the pager that owns `table_name`'s data.
    /// Falls back to the catalog pager in memory / legacy single-file mode.
    #[inline]
    pub(crate) fn get_table_pager_mut(&mut self, table_name: &str) -> &mut Pager {
        let key = table_name.to_ascii_lowercase();
        if self.table_pagers.contains_key(&key) {
            self.table_pagers.get_mut(&key).unwrap()
        } else {
            &mut self.pager
        }
    }

    /// Create a VM backed by a pure in-memory database.
    ///
    /// No data is persisted. Useful for testing and transient query execution.
    pub fn new_memory() -> Self {
        let pager = Pager::open_memory();
        VM {
            pager,
            table_pagers: HashMap::new(),
            db_dir: None,
            schema: Schema::new(),
            stmt_cache: HashMap::with_capacity(64),
            stmt_cache_fifo: VecDeque::with_capacity(256),
            index_eq_cache: HashMap::with_capacity(32),
            index_rowid_cache: HashMap::with_capacity(32),
            index_ordered_cache: HashMap::with_capacity(32),
            schema_snapshot: None,
            window_results: None,
            current_window_row_idx: 0,
            binlog: crate::binlog::BinlogManager::open_memory(),
        }
    }

    /// Open a VM backed by a single legacy `.kkdb` / `.db` file.
    ///
    /// All tables share one file. Use for backward compatibility with databases
    /// created before per-table storage was introduced.
    pub fn open_legacy(path: &str) -> Result<Self> {
        let pager = Pager::open(path)?;
        let mut vm = VM {
            pager,
            table_pagers: HashMap::new(),
            db_dir: None,
            schema: Schema::new(),
            stmt_cache: HashMap::with_capacity(64),
            stmt_cache_fifo: VecDeque::with_capacity(256),
            index_eq_cache: HashMap::with_capacity(32),
            index_rowid_cache: HashMap::with_capacity(32),
            index_ordered_cache: HashMap::with_capacity(32),
            schema_snapshot: None,
            window_results: None,
            current_window_row_idx: 0,
            binlog: crate::binlog::BinlogManager::open(path)?,
        };
        vm.binlog.recover()?;
        vm.schema.load_from_pager(&mut vm.pager)?;
        Ok(vm)
    }

    /// Open or create a database at `path`.
    ///
    /// Behaviour:
    /// - `path` is an existing **regular file** → legacy single-file mode (backward-compatible).
    /// - `path` is a **directory** or does not exist → multi-file directory mode:
    ///   creates the directory, opens `catalog.kkdb` for schema, and opens (or creates)
    ///   a separate `<table>.kkdb` file for each table's data.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use kkdb::vm::execute::VM;
    /// {
    ///     let mut vm = VM::open("mydb").unwrap();
    ///     vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)").unwrap();
    ///     // data flushed on drop
    /// }
    /// // reopen and query
    /// let mut vm = VM::open("mydb").unwrap();
    /// ```
    pub fn open(path: &str) -> Result<Self> {
        let p = Path::new(path);
        // Detect legacy single-file format.
        if p.is_file() {
            return Self::open_legacy(path);
        }
        // Multi-file directory mode.
        std::fs::create_dir_all(p)
            .map_err(|e| crate::error::KkdbError::Io(e))?;
        let catalog_path = p.join("catalog.kkdb");
        let pager = Pager::open(&catalog_path)?;
        let mut vm = VM {
            pager,
            table_pagers: HashMap::new(),
            db_dir: Some(p.to_path_buf()),
            schema: Schema::new(),
            stmt_cache: HashMap::with_capacity(64),
            stmt_cache_fifo: VecDeque::with_capacity(256),
            index_eq_cache: HashMap::with_capacity(32),
            index_rowid_cache: HashMap::with_capacity(32),
            index_ordered_cache: HashMap::with_capacity(32),
            schema_snapshot: None,
            window_results: None,
            current_window_row_idx: 0,
            binlog: crate::binlog::BinlogManager::open(
                &p.join("binlog.bin").to_string_lossy().into_owned(),
            )?,
        };
        vm.binlog.recover()?;
        vm.schema.load_from_pager(&mut vm.pager)?;
        // Open a pager for each known table and fix up next_rowid from the table's own pager.
        let table_names: Vec<String> = vm.schema.list_tables();
        for name in &table_names {
            let key = name.to_ascii_lowercase();
            let tbl_path = p.join(format!("{}.kkdb", key));
            let mut tbl_pager = Pager::open(&tbl_path)?;
            // Recompute next_rowid using the table's actual pager (load_from_pager used catalog).
            if let Some(tbl_schema) = vm.schema.tables.get_mut(&key) {
                let root_page = tbl_schema.root_page;
                let max_rid = {
                    let mut btree = crate::storage::btree::BTree::new(&mut tbl_pager);
                    btree.max_rowid(root_page).unwrap_or(0)
                };
                tbl_schema.next_rowid = max_rid + 1;
            }
            vm.table_pagers.insert(key, tbl_pager);
        }
        Ok(vm)
    }

    /// Parse and execute a single SQL statement.
    ///
    /// Statements are parsed and cached (up to 256 entries, FIFO eviction).
    /// Supports DDL, DML, SELECT, EXPLAIN, and transaction commands.
    ///
    /// Returns [`ExecResult::Ok`] / [`ExecResult::RowsAffected`] / [`ExecResult::QueryResult`].
    ///
    /// # Example
    /// ```rust,no_run
    /// # use kkdb::vm::execute::{VM, ExecResult};
    /// # let mut vm = VM::new_memory();
    /// vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")?;
    /// vm.execute_sql("INSERT INTO t1 VALUES (1)")?;
    /// if let ExecResult::QueryResult { rows, .. } = vm.execute_sql("SELECT * FROM t1")? {
    ///     println!("{:?}", rows);
    /// }
    /// # Ok::<(), kkdb::error::KkdbError>(())
    /// ```
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
                let actual_txid = self.pager.active_txid().unwrap_or(0);
                self.pager.begin_transaction()?;
                
                // Read txid from active_tx if available, else stick to previous
                let new_txid = self.pager.active_txid().unwrap_or(actual_txid);
                
                let _ = self.binlog.append(&crate::binlog::LogRecord::Begin(new_txid));
                self.schema_snapshot = Some(self.schema.clone());
                self.clear_index_caches();
                Ok(ExecResult::Ok {
                    message: "Transaction started".into(),
                })
            }
            Statement::Commit => {
                let txid = self.pager.active_txid().unwrap_or(0);
                    
                // Phase 1: Prepare
                let _ = self.binlog.append(&crate::binlog::LogRecord::Prepare(txid));
                let _ = self.binlog.fsync();
                
                // Phase 2: DB Commit
                self.pager.commit_transaction()?;
                
                // Phase 3: Binlog Commit Confirm
                let _ = self.binlog.append(&crate::binlog::LogRecord::Commit(txid));
                let _ = self.binlog.fsync();
                
                self.schema_snapshot = None;
                self.clear_index_caches();
                Ok(ExecResult::Ok {
                    message: "Committed".into(),
                })
            }
            Statement::Rollback => {
                let txid = self.pager.active_txid().unwrap_or(0);
                    
                let _ = self.binlog.append(&crate::binlog::LogRecord::Rollback(txid));
                
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
            Statement::Vacuum => self.exec_vacuum(),
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
            // Also flush all per-table pagers.
            for tbl_pager in self.table_pagers.values_mut() {
                tbl_pager.flush()?;
            }
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

        // Indexes live in the same pager as their table.
        let tbl_pager = self.get_table_pager_mut(&index.table_name);
        let mut btree = crate::storage::btree::BTree::new(tbl_pager);
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
        table_name: &str,
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
            // To avoid borrow conflict, we get the pager mutably, scan all, then drop the borrow.
            // Then we can filter the results.
            let all_rows = {
                let tbl_pager = self.get_table_pager_mut(table_name);
                let mut btree = BTree::new(tbl_pager);
                btree.scan_all(root_page)?
            };

            for (rid, row) in all_rows {
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
        let tbl_pager = self.get_table_pager_mut(table_name);
        let mut btree = BTree::new(tbl_pager);
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

#[cfg(test)]
#[path = "optimization_tests.rs"]
mod optimization_tests;
