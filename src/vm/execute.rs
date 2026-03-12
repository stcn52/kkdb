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

/// Type alias for FTS pending insert entries: (index_id, doc_id, Vec<(token, tf)>, field_len)
pub(crate) type FtsPendingInsert = (u32, i64, Vec<(String, u32)>, u32);

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
/// | *(legacy single-file — removed)* | — |
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
    /// L7: In-memory CTE result cache — cte_name → (col_names, rows)
    pub(crate) cte_cache: HashMap<String, (Vec<String>, Vec<crate::types::Row>)>,
    /// C1: MVCC managed undo log — supports savepoints, purge, statistics
    pub(crate) mvcc_undo_log: crate::vm::mvcc::UndoLog,
    /// C1: Current active transaction ID (0 = no active txn)
    pub(crate) current_txn_id: u64,
    /// C1: MVCC transaction registry — tracks active txns, provides snapshots
    pub(crate) txn_registry: crate::vm::mvcc::TransactionRegistry,
    /// C1: MVCC snapshot for current transaction (snapshot isolation).
    /// Set at BEGIN time; SELECT uses this to filter uncommitted rows from
    /// other transactions. `None` when no explicit transaction is active.
    pub(crate) mvcc_snapshot: Option<crate::vm::mvcc::MvccSnapshot>,
    /// C3: Shared cross-connection lock table reference
    pub(crate) lock_table: std::sync::Arc<std::sync::Mutex<crate::vm::lock_manager::LockTable>>,
    /// R5: Row-level lock manager for write-write conflict detection + OCC
    pub(crate) row_lock_manager: crate::vm::mvcc::RowLockManager,
    /// R6: MVCC isolation level for the current (or next) transaction
    pub(crate) isolation_level: crate::vm::mvcc::IsolationLevel,
    /// O3: Per-column full-scan access counter: (table_lowercase, col_lowercase) -> count
    pub query_access_counter: HashMap<(String, String), u32>,
    /// O3: Number of full-scan hits before an index is auto-suggested (default: 5)
    pub adaptive_threshold: u32,
    /// O3: Deferred auto-index creation queue — drained at next execute_sql boundary
    pub(crate) pending_auto_indexes: Vec<(String, String)>,
    /// Binary log 管理器，记录 DML 变更以支持复制与增量恢复。
    pub binlog: crate::binlog::BinlogManager,
    /// Correlated subquery outer-row stack.
    /// Each entry is (row_values, col_map) from an enclosing SELECT level.
    /// Pushed/popped by Exists/InSubquery/Subquery evaluators.
    pub(crate) outer_rows: Vec<(crate::types::Row, std::collections::HashMap<String, usize>)>,
    /// RLS/Web session variables set via SET kkdb.key = 'value'
    pub session_vars: HashMap<String, String>,
    /// FTS pending inserts: collected during DML, drained at execute_sql boundary.
    /// Each entry: (index_id, doc_id, Vec<(token, tf)>, field_len)
    pub(crate) pending_fts_inserts: Vec<FtsPendingInsert>,
    /// FTS rowid sequences: in-memory counter per FTS BTree (keyed by fts_root/index_id).
    /// Prevents rowid races in multi-pager mode by avoiding repeated max_rowid scans.
    pub(crate) fts_rowid_sequences: HashMap<u32, i64>,
    /// Phase 3: Rowid of the row currently being evaluated by eval_expr.
    /// Set by exec_select before each per-row eval_expr call so that VEC_SEARCH
    /// can look up the current row's HNSW score without injecting _rowid_ into the row data.
    pub(crate) current_rowid: i64,
    /// Bound parameter values for the current `execute_params` call.
    /// Empty when `execute_sql` is used directly (no placeholders).
    pub(crate) current_params: Vec<Value>,
    /// Query cache — caches SELECT results keyed by SQL string.
    /// Invalidated on DML. Disabled during explicit transactions.
    pub(crate) query_cache: crate::vm::query_cache::QueryCache,
    /// R10: Prepared statement store.
    pub(crate) prepared_store: crate::vm::prepared::PreparedStore,
    /// R10: Wait-for graph for deadlock detection.
    #[allow(dead_code)]
    pub(crate) wait_for_graph: crate::vm::mvcc::WaitForGraph,
    /// R10: Transaction timeout manager.
    #[allow(dead_code)]
    pub(crate) txn_timeout_mgr: crate::vm::mvcc::TransactionTimeoutManager,
    /// R29: Audit log for SQL operation recording.
    pub audit_log: crate::vm::auth::audit::AuditLog,
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
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// Open or create a per-table pager in `db_dir`, registering it in `table_pagers`.
    /// If `in_txn` is true (caller is inside an explicit BEGIN), immediately calls
    /// `begin_transaction()` on the newly opened pager so it participates in COW rollback.
    pub(crate) fn open_or_create_table_pager(
        table_pagers: &mut HashMap<String, Pager>,
        db_dir: &Path,
        table_name: &str,
        in_txn: bool,
    ) -> Result<()> {
        let key = table_name.to_ascii_lowercase();
        if table_pagers.contains_key(&key) {
            return Ok(());
        }
        let path = db_dir.join(format!("{}.kkdb", key));
        let mut pager = Pager::open(&path)?;
        // C1: If we're inside an explicit transaction, join it immediately
        if in_txn {
            let _ = pager.begin_transaction();
        }
        table_pagers.insert(key, pager);
        Ok(())
    }

    /// Internal helper to ensure system tables exist
    pub(crate) fn init_system_tables(&mut self) -> Result<()> {
        // Create the system tables for user management and privileges if they don't exist
        let _ = self.execute_sql("CREATE TABLE IF NOT EXISTS kkdb_users (username TEXT PRIMARY KEY, password_hash TEXT);")?;
        let _ = self.execute_sql("CREATE TABLE IF NOT EXISTS kkdb_privileges (username TEXT, obj_name TEXT, priv_type TEXT);")?;
        Ok(())
    }

    /// Return the pager that owns `table_name`'s data.
    /// Falls back to the catalog pager in memory / legacy single-file mode.
    #[inline]
    pub(crate) fn get_table_pager_mut(&mut self, table_name: &str) -> &mut Pager {
        let key = table_name.to_ascii_lowercase();
        if self.table_pagers.contains_key(&key) {
            // SAFETY: contains_key check above guarantees the key exists
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
            cte_cache: HashMap::new(),
            mvcc_undo_log: crate::vm::mvcc::UndoLog::new(),
            current_txn_id: 0,
            txn_registry: crate::vm::mvcc::TransactionRegistry::new(),
            mvcc_snapshot: None,
            lock_table: crate::vm::lock_manager::global_lock_table(),
            row_lock_manager: crate::vm::mvcc::RowLockManager::new(),
            isolation_level: crate::vm::mvcc::IsolationLevel::default(),
            query_access_counter: HashMap::new(),
            adaptive_threshold: 5,
            pending_auto_indexes: Vec::new(),
            binlog: crate::binlog::BinlogManager::open_memory(),
            outer_rows: Vec::new(),
            session_vars: HashMap::new(),
            pending_fts_inserts: Vec::new(),
            fts_rowid_sequences: HashMap::new(),
            current_rowid: 0,
            current_params: Vec::new(),
            query_cache: crate::vm::query_cache::QueryCache::default(),
            prepared_store: crate::vm::prepared::PreparedStore::new(),
            wait_for_graph: crate::vm::mvcc::WaitForGraph::new(),
            txn_timeout_mgr: crate::vm::mvcc::TransactionTimeoutManager::new(
                std::time::Duration::from_secs(30),
            ),
            audit_log: crate::vm::auth::audit::AuditLog::new(),
        };
        let _ = vm.init_system_tables();
        vm
    }

    /// Open or create a database at `path` (multi-file directory mode).
    ///
    /// Creates the directory if it does not exist, opens `catalog.kkdb` for schema,
    /// and opens (or creates) a separate `<table>.kkdb` file for each table's data.
    ///
    /// Legacy single-file `.db` format is no longer supported.
    /// If `path` is an existing regular file, returns an error.
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
        // Reject legacy single-file format.
        if p.is_file() {
            return Err(crate::error::KkdbError::Internal(format!(
                "Legacy single-file database format is no longer supported. \
                 Path '{}' is a regular file; expected a directory.",
                path
            )));
        }
        // Multi-file directory mode.
        std::fs::create_dir_all(p).map_err(crate::error::KkdbError::Io)?;
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
            cte_cache: HashMap::new(),
            mvcc_undo_log: crate::vm::mvcc::UndoLog::new(),
            current_txn_id: 0,
            txn_registry: crate::vm::mvcc::TransactionRegistry::new(),
            mvcc_snapshot: None,
            lock_table: crate::vm::lock_manager::global_lock_table(),
            row_lock_manager: crate::vm::mvcc::RowLockManager::new(),
            isolation_level: crate::vm::mvcc::IsolationLevel::default(),
            query_access_counter: HashMap::new(),
            adaptive_threshold: 5,
            pending_auto_indexes: Vec::new(),
            binlog: crate::binlog::BinlogManager::open(
                p.join("binlog.bin").to_string_lossy().into_owned(),
            )?,
            outer_rows: Vec::new(),
            session_vars: HashMap::new(),
            pending_fts_inserts: Vec::new(),
            fts_rowid_sequences: HashMap::new(),
            current_rowid: 0,
            current_params: Vec::new(),
            query_cache: crate::vm::query_cache::QueryCache::default(),
            prepared_store: crate::vm::prepared::PreparedStore::new(),
            wait_for_graph: crate::vm::mvcc::WaitForGraph::new(),
            txn_timeout_mgr: crate::vm::mvcc::TransactionTimeoutManager::new(
                std::time::Duration::from_secs(30),
            ),
            audit_log: crate::vm::auth::audit::AuditLog::new(),
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
        // Phase 3: backfill HNSW graphs for any vector indexes restored from the catalog.
        // For each registered VectorIndex, scan its table and insert row vectors.
        vm.rebuild_hnsw_from_table_data();
        let _ = vm.init_system_tables();
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
        // O3: Drain any pending auto-index creation (deferred to avoid recursive borrow).
        // Skip if this call IS itself a CREATE INDEX (we're already inside drain).
        if !self.pending_auto_indexes.is_empty()
            && !sql
                .trim_start()
                .to_ascii_uppercase()
                .starts_with("CREATE INDEX")
        {
            self.drain_pending_auto_indexes();
        }

        // R29: Handle SHOW AUDIT LOG before normal parsing
        let upper = sql.trim().to_ascii_uppercase();
        if upper.starts_with("SHOW AUDIT LOG") || upper.starts_with("SHOW AUDIT_LOG") {
            return Ok(self.exec_show_audit_log());
        }

        // R30: PRAGMA wal_checkpoint — force WAL checkpoint
        if upper == "PRAGMA WAL_CHECKPOINT" || upper == "PRAGMA WAL_CHECKPOINT;" {
            return self.exec_pragma_wal_checkpoint();
        }

        // R30: REINDEX <table> — rebuild all indexes for a table
        if upper.starts_with("REINDEX ") {
            let table_name = sql.trim()[8..]
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_string();
            if table_name.is_empty() {
                return Err(crate::error::KkdbError::SyntaxError(
                    "REINDEX requires a table name".into(),
                ));
            }
            return self.exec_reindex(&table_name);
        }

        // R30: SHUTDOWN — graceful database shutdown (flush + checkpoint)
        if upper == "SHUTDOWN" || upper == "SHUTDOWN;" {
            return self.exec_shutdown();
        }

        let result = if let Some(cached) = self.stmt_cache.get(sql) {
            let stmt = cached.clone();
            self.execute_statement(&stmt, sql)
        } else {
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
        };

        // R29: Audit log recording — after execution, before draining FTS
        if self.audit_log.is_enabled() {
            let user = self
                .session_vars
                .get("kkdb.user")
                .cloned()
                .unwrap_or_default();
            match &result {
                Ok(ref r) => {
                    let rows_affected = match r {
                        ExecResult::RowsAffected { count, .. } => *count,
                        ExecResult::QueryResult { rows, .. } => rows.len(),
                        _ => 0,
                    };
                    self.audit_log.record(&user, sql, true, rows_affected, None);
                }
                Err(ref e) => {
                    self.audit_log
                        .record(&user, sql, false, 0, Some(&e.to_string()));
                }
            }
        }

        // Drain deferred FTS inserts after statement completion (not inside DML).
        self.drain_pending_fts_inserts();
        let _ = self.auto_flush();
        result
    }

    /// Execute a SQL statement with positional `?` parameter bindings.
    ///
    /// Parameters are bound left-to-right: the first `?` in the statement
    /// receives `params[0]`, the second `?` receives `params[1]`, and so on.
    ///
    /// The parsed AST is cached (keyed on the SQL text with `?` placeholders),
    /// so repeated calls with different parameter values reuse the same plan.
    ///
    /// Returns an error if a `?` index exceeds the length of `params`.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use kkdb::vm::execute::{VM, ExecResult};
    /// # use kkdb::types::Value;
    /// # let mut vm = VM::new_memory();
    /// vm.execute_params(
    ///     "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
    ///     &[],
    /// )?;
    /// vm.execute_params(
    ///     "INSERT INTO t VALUES (?, ?)",
    ///     &[Value::Integer(1), Value::Text("Alice".into())],
    /// )?;
    /// if let ExecResult::QueryResult { rows, .. } = vm.execute_params(
    ///     "SELECT * FROM t WHERE id = ?",
    ///     &[Value::Integer(1)],
    /// )? {
    ///     println!("{:?}", rows);
    /// }
    /// # Ok::<(), kkdb::error::KkdbError>(())
    /// ```
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<ExecResult> {
        self.current_params = params.to_vec();
        let result = self.execute_sql(sql);
        self.current_params.clear();
        result
    }

    /// Execute a parsed statement
    #[inline]
    fn execute_statement(&mut self, stmt: &Statement, original_sql: &str) -> Result<ExecResult> {
        // R6: Read Committed isolation — refresh MVCC snapshot before every statement
        if self.current_txn_id != 0
            && self.isolation_level == crate::vm::mvcc::IsolationLevel::ReadCommitted
        {
            self.mvcc_snapshot = Some(self.txn_registry.snapshot(self.current_txn_id));
        }
        // R9: ReadUncommitted — refresh dirty-read snapshot before every statement
        if self.current_txn_id != 0
            && self.isolation_level == crate::vm::mvcc::IsolationLevel::ReadUncommitted
        {
            self.mvcc_snapshot = Some(
                self.txn_registry
                    .snapshot_read_uncommitted(self.current_txn_id),
            );
        }

        match stmt {
            Statement::CreateTable(create) => self.exec_create_table(create, original_sql),
            Statement::DropTable(drop) => {
                let table_name = drop.table_name.clone();
                let result = self.exec_drop_table(drop);
                if result.is_ok() {
                    self.query_cache.invalidate_table(&table_name);
                }
                result
            }
            Statement::Insert(insert) => {
                let result = self.exec_insert(insert);
                // Invalidate query cache for the affected table
                if result.is_ok() {
                    self.query_cache.invalidate_table(&insert.table_name);
                }
                result
            }
            Statement::Select(select) => {
                // Query cache: skip when in a transaction, using params, or cache disabled.
                // Parameterized queries are not cacheable (same SQL, different results).
                let cache_eligible = self.mvcc_snapshot.is_none()
                    && self.current_params.is_empty()
                    && self.query_cache.is_enabled();

                if cache_eligible {
                    if let Some((columns, rows)) = self.query_cache.get(original_sql) {
                        return Ok(ExecResult::QueryResult { columns, rows });
                    }
                }
                let result = self.exec_select(select)?;
                if cache_eligible {
                    if let ExecResult::QueryResult {
                        ref columns,
                        ref rows,
                    } = result
                    {
                        let tables = Self::extract_table_names_from_select(select);
                        // Skip caching queries with no table references (e.g. SELECT auth_uid())
                        // since they may depend on session state and are trivially fast.
                        if !tables.is_empty() {
                            self.query_cache.put(
                                original_sql,
                                tables,
                                columns.clone(),
                                rows.clone(),
                            );
                        }
                    }
                }
                Ok(result)
            }
            Statement::Update(update) => {
                let result = self.exec_update(update);
                if result.is_ok() {
                    self.query_cache.invalidate_table(&update.table_name);
                }
                result
            }
            Statement::Delete(delete) => {
                let result = self.exec_delete(delete);
                if result.is_ok() {
                    self.query_cache.invalidate_table(&delete.table_name);
                }
                result
            }
            Statement::CreateIndex(create_idx) => self.exec_create_index(create_idx),
            Statement::DropIndex(drop_idx) => self.exec_drop_index(drop_idx),
            Statement::AlterTable(alter) => {
                let table_name = alter.table_name.clone();
                let result = self.exec_alter_table(alter);
                if result.is_ok() {
                    self.query_cache.invalidate_table(&table_name);
                }
                result
            }
            Statement::Begin => {
                let actual_txid = self.pager.active_txid().unwrap_or(0);
                self.pager.begin_transaction()?;
                // C1: Also begin transaction on all per-table pagers (multi-file mode)
                for tbl_pager in self.table_pagers.values_mut() {
                    let _ = tbl_pager.begin_transaction();
                }

                // Assign a new MVCC transaction ID via the registry
                let new_txid = self.pager.active_txid().unwrap_or(actual_txid);
                self.current_txn_id = self.txn_registry.begin();
                // Create MVCC snapshot using isolation-level-aware factory
                self.mvcc_snapshot = Some(
                    self.txn_registry
                        .snapshot_for_isolation(self.current_txn_id, self.isolation_level),
                );
                // Reset undo log for the new transaction
                self.mvcc_undo_log.clear();

                let _ = self
                    .binlog
                    .append(&crate::binlog::LogRecord::Begin(new_txid));
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
                // C1: Also commit per-table pagers (multi-file mode)
                for tbl_pager in self.table_pagers.values_mut() {
                    if tbl_pager.in_transaction() {
                        let _ = tbl_pager.commit_transaction();
                    }
                }

                // Phase 3: Binlog Commit Confirm
                let _ = self.binlog.append(&crate::binlog::LogRecord::Commit(txid));
                let _ = self.binlog.fsync();

                // C1: Clear MVCC undo log (changes are now committed)
                self.mvcc_undo_log.clear();
                // C1: Mark transaction as committed in registry
                self.txn_registry.commit(self.current_txn_id);
                // C1: Clear MVCC snapshot (transaction ended)
                self.mvcc_snapshot = None;
                // C1: Purge old undo entries no longer needed by any active reader
                let min_active = self.txn_registry.min_active_txn_id();
                self.mvcc_undo_log.purge(min_active);
                // C3: Release all locks held by this transaction
                {
                    let txn_id = self.current_txn_id;
                    if let Ok(mut lt) = self.lock_table.lock() {
                        lt.release_all(txn_id);
                    }
                }
                // R5: OCC validation + commit row-level versions + release row locks
                if let Some(ref snap) = self.mvcc_snapshot {
                    self.row_lock_manager
                        .validate_read_set(self.current_txn_id, snap.max_committed_txn_id)?;
                }
                self.row_lock_manager.commit_version(self.current_txn_id);
                self.row_lock_manager.release_all(self.current_txn_id);
                self.current_txn_id = 0;

                self.schema_snapshot = None;
                self.clear_index_caches();
                Ok(ExecResult::Ok {
                    message: "Committed".into(),
                })
            }
            Statement::Rollback => {
                let txid = self.pager.active_txid().unwrap_or(0);

                let _ = self
                    .binlog
                    .append(&crate::binlog::LogRecord::Rollback(txid));

                // C1: COW pager.rollback_transaction() physically reverts all DML changes.
                // The undo log is cleared here; it would be needed for non-COW engines.
                self.mvcc_undo_log.clear();
                // C1: Mark transaction as aborted in registry
                self.txn_registry.abort(self.current_txn_id);
                // C1: Clear MVCC snapshot (transaction ended)
                self.mvcc_snapshot = None;

                self.pager.rollback_transaction()?;
                // C1: Also rollback per-table pagers in multi-file directory mode
                for tbl_pager in self.table_pagers.values_mut() {
                    if tbl_pager.in_transaction() {
                        let _ = tbl_pager.rollback_transaction();
                    }
                }
                if let Some(snapshot) = self.schema_snapshot.take() {
                    self.schema = snapshot;
                }
                // C3: Release all locks held by this transaction
                {
                    let txn_id = self.current_txn_id;
                    if let Ok(mut lt) = self.lock_table.lock() {
                        lt.release_all(txn_id);
                    }
                }
                // R5: Release row-level locks (no commit_version — transaction aborted)
                self.row_lock_manager.release_all(self.current_txn_id);
                self.current_txn_id = 0;

                self.clear_index_caches();
                Ok(ExecResult::Ok {
                    message: "Rolled back".into(),
                })
            }
            Statement::Savepoint(name) => {
                self.pager.savepoint(name)?;
                // C1: Record savepoint marker in undo log for partial rollback
                self.mvcc_undo_log.savepoint(name, self.current_txn_id);
                Ok(ExecResult::Ok {
                    message: format!("SAVEPOINT {name}"),
                })
            }
            Statement::ReleaseSavepoint(name) => {
                self.pager.release_savepoint(name)?;
                Ok(ExecResult::Ok {
                    message: format!("RELEASE SAVEPOINT {name}"),
                })
            }
            Statement::RollbackToSavepoint(name) => {
                self.pager.rollback_to_savepoint(name)?;
                if let Some(snapshot) = self.schema_snapshot.take() {
                    self.schema = snapshot;
                }
                self.clear_index_caches();
                Ok(ExecResult::Ok {
                    message: format!("ROLLBACK TO SAVEPOINT {name}"),
                })
            }
            Statement::SetOp(setop) => self.exec_set_op(setop),
            Statement::ShowTables => self.exec_show_tables(),
            Statement::ShowEngineStatus => self.exec_show_engine_status(),
            Statement::Vacuum => self.exec_vacuum(),
            Statement::AnalyzeTable(table_name) => self.exec_analyze_table(table_name.to_string()),
            // Batch E: CREATE VIEW
            Statement::CreateView(create) => self.exec_create_view(create),
            Statement::Explain(inner) => self.exec_explain(inner),
            Statement::ExplainAnalyze(inner) => self.exec_explain_analyze(inner),
            Statement::ExplainFormatTree(inner) => self.exec_explain_format_tree(inner),
            Statement::ExplainFormatJson(inner) => self.exec_explain_format_json(inner),
            // L3: TRiggers
            Statement::CreateTrigger(stmt) => self.exec_create_trigger(stmt),
            Statement::DropTrigger { name, if_exists } => self.exec_drop_trigger(name, *if_exists),
            // User management — invalidate kkdb_users cache
            Statement::CreateUser(stmt) => {
                let result = self.exec_create_user(stmt);
                if result.is_ok() {
                    self.query_cache.invalidate_table("kkdb_users");
                }
                result
            }
            Statement::AlterUser(stmt) => {
                let result = self.exec_alter_user(stmt);
                if result.is_ok() {
                    self.query_cache.invalidate_table("kkdb_users");
                }
                result
            }
            Statement::DropUser(stmt) => {
                let result = self.exec_drop_user(stmt);
                if result.is_ok() {
                    self.query_cache.invalidate_table("kkdb_users");
                }
                result
            }
            Statement::Grant(stmt) => {
                let result = self.exec_grant(stmt);
                if result.is_ok() {
                    self.query_cache.invalidate_table("kkdb_users");
                }
                result
            }
            Statement::Revoke(stmt) => {
                let result = self.exec_revoke(stmt);
                if result.is_ok() {
                    self.query_cache.invalidate_table("kkdb_users");
                }
                result
            }
            // RLS / Session
            Statement::SetSessionVar { key, value } => {
                // InnoDB-style storage engine settings
                let key_lower = key.to_ascii_lowercase();
                match key_lower.as_str() {
                    "innodb_buffer_pool_pages" | "buffer_pool_pages" => {
                        let n: usize = value.parse().map_err(|_| {
                            crate::error::KkdbError::RuntimeError(format!(
                                "invalid value for {}: expected integer",
                                key
                            ))
                        })?;
                        self.pager.set_max_buffer_pages(n);
                        for tp in self.table_pagers.values_mut() {
                            tp.set_max_buffer_pages(n);
                        }
                        self.session_vars.insert(key.clone(), value.clone());
                        return Ok(ExecResult::Ok {
                            message: format!("SET {} = {}", key, n),
                        });
                    }
                    "innodb_wal_enabled" | "wal_enabled" => {
                        let enabled =
                            matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "on");
                        if enabled {
                            self.pager.enable_wal()?;
                            for tp in self.table_pagers.values_mut() {
                                let _ = tp.enable_wal();
                            }
                        }
                        self.pager.engine_config.wal_enabled = enabled;
                        self.session_vars.insert(key.clone(), value.clone());
                        return Ok(ExecResult::Ok {
                            message: format!("SET {} = {}", key, enabled),
                        });
                    }
                    "innodb_wal_auto_checkpoint" | "wal_auto_checkpoint" => {
                        let n: usize = value.parse().map_err(|_| {
                            crate::error::KkdbError::RuntimeError(format!(
                                "invalid value for {}: expected integer",
                                key
                            ))
                        })?;
                        self.pager.engine_config.wal_auto_checkpoint = n;
                        for tp in self.table_pagers.values_mut() {
                            tp.engine_config.wal_auto_checkpoint = n;
                        }
                        self.session_vars.insert(key.clone(), value.clone());
                        return Ok(ExecResult::Ok {
                            message: format!("SET {} = {}", key, n),
                        });
                    }
                    "innodb_flush_method" | "flush_method" => {
                        let method = match value.to_ascii_lowercase().as_str() {
                            "fsync" => crate::storage::pager::FlushMethod::Fsync,
                            "fdatasync" => crate::storage::pager::FlushMethod::FdataSync,
                            "none" | "nosync" => crate::storage::pager::FlushMethod::None,
                            _ => {
                                return Err(crate::error::KkdbError::RuntimeError(format!(
                                    "unknown flush method '{}': use fsync, fdatasync, or none",
                                    value
                                )));
                            }
                        };
                        self.pager.engine_config.flush_method = method;
                        for tp in self.table_pagers.values_mut() {
                            tp.engine_config.flush_method = method;
                        }
                        self.session_vars.insert(key.clone(), value.clone());
                        return Ok(ExecResult::Ok {
                            message: format!("SET {} = '{}'", key, value),
                        });
                    }
                    "query_cache_enabled" | "query_cache" => {
                        let enabled =
                            matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "on");
                        self.query_cache.set_enabled(enabled);
                        self.session_vars.insert(key.clone(), value.clone());
                        return Ok(ExecResult::Ok {
                            message: format!("SET {} = {}", key, enabled),
                        });
                    }
                    "audit_log_enabled" | "audit_log" => {
                        let enabled =
                            matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "on");
                        if enabled {
                            self.audit_log.enable();
                        } else {
                            self.audit_log.disable();
                        }
                        self.session_vars.insert(key.clone(), value.clone());
                        return Ok(ExecResult::Ok {
                            message: format!("SET {} = {}", key, enabled),
                        });
                    }
                    "use_lz4" => {
                        let enabled =
                            matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "on");
                        self.pager.engine_config.use_lz4 = enabled;
                        for tp in self.table_pagers.values_mut() {
                            tp.engine_config.use_lz4 = enabled;
                        }
                        self.session_vars.insert(key.clone(), value.clone());
                        return Ok(ExecResult::Ok {
                            message: format!("SET {} = {}", key, enabled),
                        });
                    }
                    _ => {}
                }
                // R6: Transaction isolation level
                match key_lower.as_str() {
                    "transaction_isolation" | "isolation_level" | "transaction isolation level" => {
                        let level = match crate::vm::mvcc::IsolationLevel::from_str_loose(&value) {
                            Some(l) => l,
                            None => {
                                return Err(crate::error::KkdbError::RuntimeError(format!(
                                    "unknown isolation level '{}': use 'serializable', 'repeatable read', 'read committed', or 'read uncommitted'", value
                                )));
                            }
                        };
                        self.isolation_level = level;
                        self.session_vars.insert(key.clone(), value.clone());
                        return Ok(ExecResult::Ok {
                            message: format!("SET isolation_level = {}", level),
                        });
                    }
                    _ => {}
                }
                self.session_vars.insert(key.clone(), value.clone());
                Ok(ExecResult::Ok {
                    message: format!("SET {} = '{}'", key, value),
                })
            }
            Statement::CreatePolicy(stmt) => self.exec_create_policy(stmt),
            Statement::DropPolicy(stmt) => self.exec_drop_policy(stmt),
            // BM25 Full-Text Search: CREATE FULLTEXT INDEX — Phase 4 write path pending
            Statement::CreateFulltextIndex(stmt) => self.exec_create_fulltext_index(stmt),
            // HNSW Vector Index: CREATE VECTOR INDEX
            Statement::CreateVectorIndex(stmt) => self.exec_create_vector_index(stmt),
            // HNSW Vector Index: DROP VECTOR INDEX
            Statement::DropVectorIndex {
                index_name,
                if_exists,
            } => self.exec_drop_vector_index(index_name, *if_exists),
            // R10: Prepared Statements
            Statement::Prepare { name, sql } => {
                self.prepared_store.prepare(name, sql)?;
                Ok(ExecResult::Ok {
                    message: format!("PREPARE {}", name),
                })
            }
            Statement::Execute { name, params } => {
                // Evaluate parameter expressions
                let empty_row: Vec<crate::types::Value> = Vec::new();
                let empty_map: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                let mut param_values = Vec::with_capacity(params.len());
                for p in params {
                    let v = self.eval_expr(p, &empty_row, &empty_map)?;
                    param_values.push(v);
                }
                let (sql, _param_count) = self.prepared_store.get_for_execute(name)?;
                // Stash params for $1, $2, ... substitution and execute
                let old_params = std::mem::replace(&mut self.current_params, param_values);
                let result = self.execute_sql(&sql);
                self.current_params = old_params;
                result
            }
            Statement::Deallocate { name } => {
                let upper = name.to_ascii_uppercase();
                if upper == "ALL" {
                    let count = self.prepared_store.count();
                    self.prepared_store.clear();
                    Ok(ExecResult::Ok {
                        message: format!("DEALLOCATE ALL ({} statements)", count),
                    })
                } else {
                    if !self.prepared_store.deallocate(name) {
                        return Err(crate::error::KkdbError::RuntimeError(format!(
                            "prepared statement '{}' not found",
                            name
                        )));
                    }
                    Ok(ExecResult::Ok {
                        message: format!("DEALLOCATE {}", name),
                    })
                }
            }
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

    /// Phase 3 — Startup HNSW backfill.
    ///
    /// After `schema.load_from_pager` has registered empty `VectorIndex` entries for
    /// each `vector_index` row in the catalog, this method scans each indexed table and
    /// inserts all stored vectors into the corresponding HNSW graph.
    ///
    /// Called once per `VM::open`, after all table pagers are open.
    /// Row-level errors (wrong dimension, malformed BLOB) are silently skipped; they
    /// cannot cause startup failure.
    pub(crate) fn rebuild_hnsw_from_table_data(&mut self) {
        use crate::vector::index::decode_vector;

        // Collect what we need from schema to avoid borrow conflicts with get_table_pager_mut.
        let specs: Vec<(String, u32, usize, u32, crate::vector::VectorIndex)> = self
            .schema
            .vector_indexes
            .iter()
            .map(|vi| {
                let root_page = self
                    .schema
                    .get_table(&vi.table)
                    .map(|t| t.root_page)
                    .unwrap_or(0);
                (vi.table.clone(), root_page, vi.col_idx, vi.dim, vi.clone())
            })
            .collect();

        for (table_name, root_page, col_idx, dim, vi) in specs {
            if root_page == 0 {
                continue;
            }
            // Scan table rows using the correct pager for this table.
            let rows = {
                let pager = self.get_table_pager_mut(&table_name);
                let mut btree = crate::storage::btree::BTree::new(pager);
                match btree.scan_all(root_page) {
                    Ok(r) => r,
                    Err(_) => continue,
                }
            };
            for (rowid, row) in rows {
                if let Some(Value::Blob(blob)) = row.get(col_idx) {
                    if let Some(vec) = decode_vector(blob) {
                        if vec.len() as u32 == dim {
                            let _ = vi.insert_vec(rowid as u64, vec);
                        }
                    }
                }
            }
        }
    }

    /// R29: SHOW AUDIT LOG — returns recent audit log entries as a query result.
    fn exec_show_audit_log(&self) -> ExecResult {
        let columns = vec![
            "seq".to_string(),
            "timestamp".to_string(),
            "user".to_string(),
            "category".to_string(),
            "success".to_string(),
            "rows_affected".to_string(),
            "sql".to_string(),
            "error".to_string(),
        ];
        let entries = self.audit_log.last_n(100);
        let rows: Vec<Vec<Value>> = entries
            .iter()
            .map(|e| {
                let ts = e
                    .timestamp
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                vec![
                    Value::Integer(e.seq as i64),
                    Value::Integer(ts),
                    Value::Text(std::sync::Arc::from(e.user.as_str())),
                    Value::Text(std::sync::Arc::from(e.category.to_string().as_str())),
                    Value::Integer(if e.success { 1 } else { 0 }),
                    Value::Integer(e.rows_affected as i64),
                    Value::Text(std::sync::Arc::from(e.sql.as_str())),
                    Value::Text(std::sync::Arc::from(e.error.as_deref().unwrap_or(""))),
                ]
            })
            .collect();
        ExecResult::QueryResult { columns, rows }
    }

    /// R30: PRAGMA wal_checkpoint — force an immediate WAL checkpoint.
    ///
    /// Flushes all WAL entries back to the main database file and resets the WAL.
    /// Returns a QueryResult with checkpoint statistics.
    fn exec_pragma_wal_checkpoint(&mut self) -> Result<ExecResult> {
        if !self.pager.is_wal_enabled() {
            return Ok(ExecResult::Ok {
                message: "WAL is not enabled; nothing to checkpoint".to_string(),
            });
        }
        self.pager.wal_checkpoint()?;
        Ok(ExecResult::Ok {
            message: "WAL checkpoint completed".to_string(),
        })
    }

    /// R30: REINDEX <table> — drop and recreate all indexes for the given table.
    ///
    /// This is useful after bulk inserts or data corruption to rebuild index structures.
    fn exec_reindex(&mut self, table_name: &str) -> Result<ExecResult> {
        // Find all indexes belonging to this table
        let indexes: Vec<(String, Vec<String>, bool)> = self
            .schema
            .indexes_for_table(table_name)
            .iter()
            .map(|idx| (idx.name.clone(), idx.columns.clone(), idx.unique))
            .collect();

        if indexes.is_empty() {
            return Ok(ExecResult::Ok {
                message: format!("No indexes found for table '{}'", table_name),
            });
        }

        let mut rebuilt = 0usize;
        for (idx_name, columns, unique) in &indexes {
            // Drop the old index
            self.schema.drop_index(&mut self.pager, idx_name, false)?;

            // Recreate it
            let unique_kw = if *unique { "UNIQUE " } else { "" };
            let cols = columns.join(", ");
            let create_sql = format!(
                "CREATE {}INDEX {} ON {} ({})",
                unique_kw, idx_name, table_name, cols
            );
            self.execute_sql(&create_sql)?;
            rebuilt += 1;
        }

        Ok(ExecResult::Ok {
            message: format!("Rebuilt {} index(es) for table '{}'", rebuilt, table_name),
        })
    }

    /// R30: SHUTDOWN — perform graceful database shutdown.
    ///
    /// This flushes all dirty pages, performs a WAL checkpoint (if WAL is enabled),
    /// and ensures all data is safely persisted to disk.
    fn exec_shutdown(&mut self) -> Result<ExecResult> {
        // 1. Flush all dirty pages
        self.pager.flush()?;

        // 2. WAL checkpoint if enabled
        if self.pager.is_wal_enabled() {
            self.pager.wal_checkpoint()?;
        }

        // 3. Clear caches
        self.stmt_cache.clear();
        self.stmt_cache_fifo.clear();
        self.query_cache.clear();

        Ok(ExecResult::Ok {
            message: "Database shutdown completed: all data flushed and checkpointed".to_string(),
        })
    }

    /// R29: Verify a password against a bcrypt hash stored in kkdb_users.
    /// Returns true if the password matches the stored hash.
    pub fn verify_user_password(&mut self, username: &str, password: &str) -> bool {
        let sql = format!(
            "SELECT password_hash FROM kkdb_users WHERE username = '{}'",
            username.replace('\'', "''")
        );
        match self.execute_sql(&sql) {
            Ok(ExecResult::QueryResult { rows, .. }) => {
                if let Some(row) = rows.first() {
                    if let Some(Value::Text(hash)) = row.first() {
                        return bcrypt::verify(password, hash).unwrap_or(false);
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// O3: Record that a full table scan was performed with a WHERE predicate on `col`
    /// of `table`. Called from try_index_scan when no index covers the predicate.
    pub(crate) fn record_full_scan_access(&mut self, table: &str, col: &str) {
        let key = (table.to_ascii_lowercase(), col.to_ascii_lowercase());
        let count = self.query_access_counter.entry(key.clone()).or_insert(0);
        *count += 1;
        if *count >= self.adaptive_threshold {
            // Threshold hit: queue deferred auto-index creation (avoid recursive execute_sql)
            self.maybe_auto_create_index(&key.0, &key.1);
        }
    }

    /// O3: Checks if auto-indexing is needed and enqueues the SQL for deferred execution.
    /// The actual CREATE INDEX runs at the next `execute_sql` boundary via `drain_pending_auto_indexes`.
    pub(crate) fn maybe_auto_create_index(&mut self, table: &str, col: &str) {
        let idx_name = format!("idx_{table}_{col}_auto");
        // Skip if index already exists
        if self.schema.indexes.contains_key(&idx_name) {
            return;
        }
        // Skip if the column is a PRIMARY KEY or UNIQUE (B-Tree structure already indexes it)
        if let Ok(tbl) = self.schema.get_table(table) {
            if let Some(col_info) = tbl
                .columns
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(col))
            {
                if col_info.primary_key || col_info.unique {
                    self.query_access_counter
                        .remove(&(table.to_ascii_lowercase(), col.to_ascii_lowercase()));
                    return;
                }
            }
        }
        // Skip if any existing index already covers this column as the first column
        let already_indexed = self.schema.indexes_for_table(table).iter().any(|idx| {
            idx.columns
                .first()
                .map(|c| c.eq_ignore_ascii_case(col))
                .unwrap_or(false)
        });
        if already_indexed {
            self.query_access_counter
                .remove(&(table.to_ascii_lowercase(), col.to_ascii_lowercase()));
            return;
        }
        // Enqueue for deferred execution at next execute_sql boundary
        let entry = (table.to_ascii_lowercase(), col.to_ascii_lowercase());
        if !self.pending_auto_indexes.contains(&entry) {
            self.pending_auto_indexes.push(entry);
        }
    }

    /// O3: Drain the pending auto-index queue, executing each deferred CREATE INDEX.
    /// Called at the start of `execute_sql` to avoid recursive borrows.
    pub(crate) fn drain_pending_auto_indexes(&mut self) {
        let pending = std::mem::take(&mut self.pending_auto_indexes);
        for (table, col) in pending {
            let idx_name = format!("idx_{table}_{col}_auto");
            // Re-check: another drain cycle may have already created it
            if self.schema.indexes.contains_key(&idx_name) {
                continue;
            }
            let already_indexed = self.schema.indexes_for_table(&table).iter().any(|idx| {
                idx.columns
                    .first()
                    .map(|c| c.eq_ignore_ascii_case(&col))
                    .unwrap_or(false)
            });
            if already_indexed {
                self.query_access_counter
                    .remove(&(table.clone(), col.clone()));
                continue;
            }
            let sql = format!("CREATE INDEX {idx_name} ON {table} ({col})");
            if self.execute_sql(&sql).is_ok() {
                self.query_access_counter.remove(&(table, col));
            }
        }
    }

    #[inline]
    pub(crate) fn clear_index_caches(&mut self) {
        self.index_eq_cache.clear();
        self.index_rowid_cache.clear();
        self.index_ordered_cache.clear();
    }

    /// Extract referenced table names from a SELECT statement's FROM clause.
    /// Used by the query cache to know which tables a cached result depends on.
    fn extract_table_names_from_select(select: &crate::sql::ast::SelectStmt) -> Vec<String> {
        let mut tables = Vec::new();
        if let Some(ref from) = select.from {
            Self::collect_from_tables(from, &mut tables);
        }
        // Also collect tables from WHERE clause subqueries (EXISTS, IN, etc.)
        if let Some(ref where_clause) = select.where_clause {
            Self::collect_tables_from_expr(where_clause, &mut tables);
        }
        // Collect from HAVING clause
        if let Some(ref having) = select.having {
            Self::collect_tables_from_expr(having, &mut tables);
        }
        // Collect from select list subqueries
        for col in &select.columns {
            use crate::sql::ast::SelectColumn;
            match col {
                SelectColumn::Expr { expr, .. } => {
                    Self::collect_tables_from_expr(expr, &mut tables);
                }
                SelectColumn::AllColumns | SelectColumn::TableAllColumns { .. } => {}
            }
        }
        tables
    }

    fn collect_from_tables(from: &crate::sql::ast::FromClause, out: &mut Vec<String>) {
        use crate::sql::ast::FromClause;
        match from {
            FromClause::Table { name, .. } => {
                out.push(name.to_ascii_lowercase());
            }
            FromClause::Join { left, right, .. } => {
                Self::collect_from_tables(left, out);
                Self::collect_from_tables(right, out);
            }
            FromClause::Subquery { query, .. } => {
                let sub_tables = Self::extract_table_names_from_select(query);
                out.extend(sub_tables);
            }
            FromClause::SetOp { .. } => {}
            FromClause::TableFunction { .. } => {}
        }
    }

    /// Walk an expression tree to find table names referenced in subqueries.
    fn collect_tables_from_expr(expr: &crate::sql::ast::Expr, out: &mut Vec<String>) {
        use crate::sql::ast::Expr;
        match expr {
            Expr::Exists(subquery) | Expr::Subquery(subquery) => {
                let sub_tables = Self::extract_table_names_from_select(subquery);
                out.extend(sub_tables);
            }
            Expr::InSubquery { expr, subquery, .. } => {
                Self::collect_tables_from_expr(expr, out);
                let sub_tables = Self::extract_table_names_from_select(subquery);
                out.extend(sub_tables);
            }
            Expr::AnyOp { expr, subquery, .. } | Expr::AllOp { expr, subquery, .. } => {
                Self::collect_tables_from_expr(expr, out);
                let sub_tables = Self::extract_table_names_from_select(subquery);
                out.extend(sub_tables);
            }
            Expr::BinaryOp { left, right, .. } => {
                Self::collect_tables_from_expr(left, out);
                Self::collect_tables_from_expr(right, out);
            }
            Expr::UnaryOp { expr, .. }
            | Expr::IsNull { expr, .. }
            | Expr::Nested(expr)
            | Expr::Collate { expr, .. } => {
                Self::collect_tables_from_expr(expr, out);
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                Self::collect_tables_from_expr(expr, out);
                Self::collect_tables_from_expr(low, out);
                Self::collect_tables_from_expr(high, out);
            }
            Expr::InList { expr, list, .. } => {
                Self::collect_tables_from_expr(expr, out);
                for item in list {
                    Self::collect_tables_from_expr(item, out);
                }
            }
            Expr::Case {
                operand,
                when_clauses,
                else_clause,
                ..
            } => {
                if let Some(op) = operand {
                    Self::collect_tables_from_expr(op, out);
                }
                for (cond, result) in when_clauses {
                    Self::collect_tables_from_expr(cond, out);
                    Self::collect_tables_from_expr(result, out);
                }
                if let Some(el) = else_clause {
                    Self::collect_tables_from_expr(el, out);
                }
            }
            Expr::Function { args, .. } => {
                for arg in args {
                    Self::collect_tables_from_expr(arg, out);
                }
            }
            Expr::Like { expr, pattern, .. } => {
                Self::collect_tables_from_expr(expr, out);
                Self::collect_tables_from_expr(pattern, out);
            }
            // Leaf nodes: no table references
            _ => {}
        }
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
#[path = "optimization_tests.rs"]
mod optimization_tests;
