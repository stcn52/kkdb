// ── Connection Pool ─────────────────────────────────────────────────────────
//
// Thread-safe connection pool for multi-connection access to a single kkdb
// storage engine. Follows the same pattern as SQLite: single writer,
// multiple readers, serialised via a shared Mutex<VM>.
//
// ## Design
//
// - The pool wraps `Arc<Mutex<VM>>` — all connections share one storage engine.
// - Each `ConnectionHandle` carries its own session state (user, session vars).
// - The pool tracks active/idle counts and enforces `max_connections`.
// - Checkout with timeout prevents unbounded waiting.
//
// ## Usage
//
// ```ignore
// let pool = ConnectionPool::new_memory(16);
// let mut conn = pool.checkout()?;
// conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)")?;
// drop(conn); // returns to pool
// ```

use crate::error::{KkdbError, Result};
use crate::vm::execute::{ExecResult, VM};
use std::sync::{Arc, Mutex, Condvar};
use std::time::{Duration, Instant};

/// Connection pool configuration.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of concurrent connections (handles) allowed.
    pub max_connections: usize,
    /// Timeout for `checkout()`. `None` = wait indefinitely.
    pub checkout_timeout: Option<Duration>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 64,
            checkout_timeout: Some(Duration::from_secs(30)),
        }
    }
}

/// Thread-safe connection pool for kkdb.
pub struct ConnectionPool {
    /// Shared VM instance — all connections share storage.
    vm: Arc<Mutex<VM>>,
    /// Pool configuration.
    config: PoolConfig,
    /// State: (active_connections, )
    state: Arc<(Mutex<PoolState>, Condvar)>,
}

#[derive(Debug)]
struct PoolState {
    active_connections: usize,
    total_checkouts: u64,
    total_timeout_errors: u64,
}

impl ConnectionPool {
    /// Create a connection pool wrapping an existing `Arc<Mutex<VM>>`.
    pub fn new(vm: Arc<Mutex<VM>>, config: PoolConfig) -> Self {
        Self {
            vm,
            config,
            state: Arc::new((
                Mutex::new(PoolState {
                    active_connections: 0,
                    total_checkouts: 0,
                    total_timeout_errors: 0,
                }),
                Condvar::new(),
            )),
        }
    }

    /// Create a connection pool backed by an in-memory VM.
    pub fn new_memory(max_connections: usize) -> Self {
        let vm = Arc::new(Mutex::new(VM::new_memory()));
        Self::new(vm, PoolConfig {
            max_connections,
            ..PoolConfig::default()
        })
    }

    /// Create a connection pool backed by a file-based VM.
    pub fn open(path: &str, max_connections: usize) -> Result<Self> {
        let vm = Arc::new(Mutex::new(VM::open(path)?));
        Ok(Self::new(vm, PoolConfig {
            max_connections,
            ..PoolConfig::default()
        }))
    }

    /// Checkout a connection handle from the pool.
    ///
    /// Blocks until a slot is available or the timeout expires.
    /// Returns `Err(KkdbError::RuntimeError)` if the pool is exhausted.
    pub fn checkout(&self) -> Result<ConnectionHandle> {
        let (lock, cvar) = &*self.state;
        let mut state = lock.lock().map_err(|_| {
            KkdbError::RuntimeError("connection pool lock poisoned".into())
        })?;

        let deadline = self.config.checkout_timeout.map(|t| Instant::now() + t);

        // Wait for a free slot
        while state.active_connections >= self.config.max_connections {
            if let Some(dl) = deadline {
                let remaining = dl.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    state.total_timeout_errors += 1;
                    return Err(KkdbError::RuntimeError(format!(
                        "connection pool exhausted: {} / {} active (timeout)",
                        state.active_connections, self.config.max_connections
                    )));
                }
                let result = cvar.wait_timeout(state, remaining).map_err(|_| {
                    KkdbError::RuntimeError("connection pool condvar poisoned".into())
                })?;
                state = result.0;
                if result.1.timed_out() && state.active_connections >= self.config.max_connections {
                    state.total_timeout_errors += 1;
                    return Err(KkdbError::RuntimeError(format!(
                        "connection pool exhausted: {} / {} active (timeout)",
                        state.active_connections, self.config.max_connections
                    )));
                }
            } else {
                state = cvar.wait(state).map_err(|_| {
                    KkdbError::RuntimeError("connection pool condvar poisoned".into())
                })?;
            }
        }

        state.active_connections += 1;
        state.total_checkouts += 1;

        Ok(ConnectionHandle {
            vm: Arc::clone(&self.vm),
            pool_state: Arc::clone(&self.state),
            session_vars: std::collections::HashMap::new(),
            current_user: None,
            connection_id: state.total_checkouts,
        })
    }

    /// Number of currently active (checked-out) connections.
    pub fn active_connections(&self) -> usize {
        self.state.0.lock().map(|s| s.active_connections).unwrap_or(0)
    }

    /// Maximum connections allowed.
    pub fn max_connections(&self) -> usize {
        self.config.max_connections
    }

    /// Total number of successful checkouts since pool creation.
    pub fn total_checkouts(&self) -> u64 {
        self.state.0.lock().map(|s| s.total_checkouts).unwrap_or(0)
    }

    /// Total timeout errors.
    pub fn total_timeout_errors(&self) -> u64 {
        self.state.0.lock().map(|s| s.total_timeout_errors).unwrap_or(0)
    }

    /// Get a reference to the underlying VM (for admin operations).
    pub fn vm(&self) -> &Arc<Mutex<VM>> {
        &self.vm
    }
}

/// A connection handle checked out from the pool.
///
/// Holds session-level state (user, session vars) and a reference to
/// the shared VM. On drop, the handle is automatically returned to the pool.
pub struct ConnectionHandle {
    vm: Arc<Mutex<VM>>,
    pool_state: Arc<(Mutex<PoolState>, Condvar)>,
    /// Per-connection session variables.
    pub session_vars: std::collections::HashMap<String, String>,
    /// Authenticated user for this connection.
    pub current_user: Option<String>,
    /// Monotonically increasing connection ID.
    pub connection_id: u64,
}

impl ConnectionHandle {
    /// Execute a SQL statement on the shared VM.
    pub fn execute(&self, sql: &str) -> Result<ExecResult> {
        let mut vm = self.vm.lock().map_err(|_| {
            KkdbError::RuntimeError("VM lock poisoned".into())
        })?;
        vm.execute_sql(sql)
    }

    /// Execute a SQL statement with bound parameters.
    pub fn execute_params(&self, sql: &str, params: Vec<crate::types::Value>) -> Result<ExecResult> {
        let mut vm = self.vm.lock().map_err(|_| {
            KkdbError::RuntimeError("VM lock poisoned".into())
        })?;
        vm.execute_params(sql, &params)
    }

    /// Get the connection's unique ID.
    pub fn id(&self) -> u64 {
        self.connection_id
    }
}

impl Drop for ConnectionHandle {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.pool_state;
        if let Ok(mut state) = lock.lock() {
            state.active_connections = state.active_connections.saturating_sub(1);
            cvar.notify_one();
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Value;

    #[test]
    fn test_pool_creation() {
        let pool = ConnectionPool::new_memory(4);
        assert_eq!(pool.max_connections(), 4);
        assert_eq!(pool.active_connections(), 0);
    }

    #[test]
    fn test_pool_checkout_and_drop() {
        let pool = ConnectionPool::new_memory(4);
        assert_eq!(pool.active_connections(), 0);

        {
            let _conn = pool.checkout().unwrap();
            assert_eq!(pool.active_connections(), 1);
        }
        // After drop, slot is released
        assert_eq!(pool.active_connections(), 0);
        assert_eq!(pool.total_checkouts(), 1);
    }

    #[test]
    fn test_pool_multiple_connections() {
        let pool = ConnectionPool::new_memory(4);

        let c1 = pool.checkout().unwrap();
        let c2 = pool.checkout().unwrap();
        let c3 = pool.checkout().unwrap();
        assert_eq!(pool.active_connections(), 3);

        drop(c2);
        assert_eq!(pool.active_connections(), 2);

        drop(c1);
        drop(c3);
        assert_eq!(pool.active_connections(), 0);
    }

    #[test]
    fn test_pool_execute_sql() {
        let pool = ConnectionPool::new_memory(4);
        let conn = pool.checkout().unwrap();

        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
        conn.execute("INSERT INTO t VALUES (1, 'hello')").unwrap();

        match conn.execute("SELECT * FROM t").unwrap() {
            ExecResult::QueryResult { columns, rows } => {
                assert_eq!(columns, vec!["id", "v"]);
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0][1], Value::Text("hello".into()));
            }
            _ => panic!("expected QueryResult"),
        }
    }

    #[test]
    fn test_pool_exhaustion_timeout() {
        let pool = ConnectionPool::new(
            Arc::new(Mutex::new(VM::new_memory())),
            PoolConfig {
                max_connections: 1,
                checkout_timeout: Some(Duration::from_millis(50)),
            },
        );

        let _c1 = pool.checkout().unwrap();
        // Pool is full — second checkout should timeout
        let result = pool.checkout();
        assert!(result.is_err());
        assert_eq!(pool.total_timeout_errors(), 1);
    }

    #[test]
    fn test_pool_connection_id_monotonic() {
        let pool = ConnectionPool::new_memory(4);
        let c1 = pool.checkout().unwrap();
        let c2 = pool.checkout().unwrap();
        assert!(c2.id() > c1.id());
    }

    #[test]
    fn test_pool_multithreaded_access() {
        use std::thread;
        let pool = Arc::new(ConnectionPool::new_memory(4));

        // Setup: create table
        {
            let conn = pool.checkout().unwrap();
            conn.execute("CREATE TABLE mt (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
        }

        // Spawn threads that each insert a row
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let pool_clone = Arc::clone(&pool);
                thread::spawn(move || {
                    let conn = pool_clone.checkout().unwrap();
                    conn.execute(&format!("INSERT INTO mt VALUES ({}, {})", i + 1, i * 10))
                        .unwrap();
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Verify all rows are inserted
        let conn = pool.checkout().unwrap();
        match conn.execute("SELECT COUNT(*) FROM mt").unwrap() {
            ExecResult::QueryResult { rows, .. } => {
                assert_eq!(rows[0][0], Value::Integer(4));
            }
            _ => panic!("expected QueryResult"),
        }
    }

    #[test]
    fn test_pool_session_vars_per_connection() {
        let pool = ConnectionPool::new_memory(4);
        let mut c1 = pool.checkout().unwrap();
        let mut c2 = pool.checkout().unwrap();

        c1.session_vars.insert("role".into(), "admin".into());
        c2.session_vars.insert("role".into(), "user".into());

        assert_eq!(c1.session_vars.get("role"), Some(&"admin".to_string()));
        assert_eq!(c2.session_vars.get("role"), Some(&"user".to_string()));
    }

    #[test]
    fn test_pool_drop_releases_slot_for_waiting() {
        use std::thread;
        use std::sync::Arc;

        let pool = Arc::new(ConnectionPool::new(
            Arc::new(Mutex::new(VM::new_memory())),
            PoolConfig {
                max_connections: 1,
                checkout_timeout: Some(Duration::from_secs(5)),
            },
        ));

        let conn = pool.checkout().unwrap();
        assert_eq!(pool.active_connections(), 1);

        // Spawn a thread that tries to checkout (will block)
        let pool2 = Arc::clone(&pool);
        let handle = thread::spawn(move || {
            let _conn = pool2.checkout().unwrap();
            assert_eq!(pool2.active_connections(), 1);
        });

        // Small delay to let the thread start waiting
        thread::sleep(Duration::from_millis(50));

        // Release our connection — waiting thread should unblock
        drop(conn);

        handle.join().unwrap();
        assert_eq!(pool.active_connections(), 0);
    }
}
