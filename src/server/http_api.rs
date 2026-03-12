//! HTTP REST API — Supabase-style auth and query endpoints.
//!
//! Endpoints:
//!   POST /auth/signup   { email, password }  → { user_id, token }
//!   POST /auth/signin   { email, password }  → { user_id, token }
//!   POST /auth/refresh                        → renewed token
//!   POST /auth/apikeys                        → new API key
//!   POST /rest/query    { sql }              → { columns, rows }
//!   POST /rest/execute  { sql }              → (DML / DDL)
//!   POST /rest/batch    { statements, .. }   → per-statement results
//!   POST /rest/bulk     { table, rows, .. }  → bulk insert
//!   GET  /health                             → { status: "ok" }
//!
//! Each authenticated user has an isolated VM/database in
//!   `{data_dir}/{user_id}/`
//! The global `_auth` VM holds only the auth system tables.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

use crate::raft::types::KkdbRequest;
use crate::vm::execute::{ExecResult, VM};

// ─── JWT secret ───────────────────────────────────────────────────────────────
/// Returns the JWT signing secret.
/// Priority: `KKDB_JWT_SECRET` env var → compile-time default.
/// In production, always set `KKDB_JWT_SECRET` to a random 256-bit value.
fn jwt_secret() -> Vec<u8> {
    std::env::var("KKDB_JWT_SECRET")
        .map(|s| s.into_bytes())
        .unwrap_or_else(|_| b"kkdb-super-secret-jwt-key-change-in-production".to_vec())
}

/// JWT expiry in seconds (default 1 hour). Override with `KKDB_JWT_EXPIRY`.
fn jwt_expiry_secs() -> i64 {
    std::env::var("KKDB_JWT_EXPIRY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600)
}

// ─── JWT Claims ───────────────────────────────────────────────────────────────
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// Subject: the user_id (UUID)
    pub sub: String,
    /// Email address
    pub email: String,
    /// Role: "authenticated" or "anon"
    pub role: String,
    /// Expiry (Unix timestamp)
    pub exp: usize,
}

// ─── Request / Response shapes ────────────────────────────────────────────────
#[derive(Debug, Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct SigninRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub user_id: String,
    pub email: String,
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub sql: String,
}

/// Batch execution request — run multiple SQL statements in one HTTP call.
#[derive(Debug, Deserialize)]
pub struct BatchRequest {
    /// SQL statements to execute in order
    pub statements: Vec<String>,
    /// If true, wrap all statements in BEGIN / COMMIT.
    /// On any error the transaction is rolled back and the response
    /// contains the error at the failing index.
    #[serde(default)]
    pub transaction: bool,
}

/// Result for a single statement in a batch.
#[derive(Debug, Serialize)]
#[serde(tag = "status")]
pub enum BatchStatementResult {
    #[serde(rename = "ok")]
    Ok {
        statement: String,
        columns: Vec<String>,
        rows: Vec<Vec<serde_json::Value>>,
        rows_affected: Option<u64>,
    },
    #[serde(rename = "error")]
    Error { statement: String, error: String },
}

#[derive(Debug, Serialize)]
pub struct BatchResponse {
    /// Per-statement results (same order as input).
    pub results: Vec<BatchStatementResult>,
    /// Total statements attempted.
    pub count: usize,
    /// Number of successful statements.
    pub succeeded: usize,
    /// Whether a transaction was used.
    pub transaction: bool,
    /// If a transaction was used and failed, the 0-based index of the failing statement.
    pub failed_at: Option<usize>,
}

/// Bulk write request — insert multiple rows into a single table efficiently.
///
/// The rows are JSON objects; the keys of the **first row** determine the
/// column list.  All rows must have the same keys.
#[derive(Debug, Deserialize)]
pub struct BulkWriteRequest {
    /// Target table name
    pub table: String,
    /// Rows to insert — each is a JSON object { column: value }
    pub rows: Vec<serde_json::Map<String, serde_json::Value>>,
    /// If true, wrap the entire batch in BEGIN / COMMIT.
    /// On any error the transaction is physically rolled back (MVCC/COW).
    #[serde(default = "bool_true")]
    pub transaction: bool,
    /// If true, send as a single multi-row INSERT VALUES (r1),(r2),...
    /// If false, send as N individual INSERTs (still in one transaction).
    #[serde(default = "bool_true")]
    pub bulk_insert: bool,
}
fn bool_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct BulkWriteResponse {
    pub table: String,
    pub rows_written: usize,
    pub transaction: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QueryResponse {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

// ─── Application state ───────────────────────────────────────────────────────

/// Per-user lazy VM cache: user_id → Arc<Mutex<VM>>
pub type UserVmMap = Arc<Mutex<HashMap<String, Arc<Mutex<VM>>>>>;

/// Multi-tenant application state.
///
/// - `auth_vm`   — global VM for auth tables.
/// - `user_vms`  — lazy-loaded map of per-user VMs keyed by `user_id`.
/// - `data_dir`  — root dir for user databases; `None` = in-memory (tests).
/// - `raft_node` — if set, all writes go through Raft consensus (cluster mode).
/// - `peer_rest_addrs` — maps node_id → REST base URL for write forwarding.
#[derive(Clone)]
pub struct AppState {
    pub auth_vm: Arc<Mutex<VM>>,
    pub user_vms: UserVmMap,
    pub data_dir: Option<Arc<PathBuf>>,
    /// Raft node handle (set in cluster mode).
    pub raft_node: Option<Arc<crate::raft::node::KkdbNode>>,
    /// peer node_id → "http://host:rest_port" for leader forwarding.
    pub peer_rest_addrs: Arc<Mutex<std::collections::BTreeMap<u64, String>>>,
}

impl AppState {
    pub fn in_memory() -> Self {
        let vm = Arc::new(Mutex::new(VM::new_memory()));
        Self {
            auth_vm: Arc::clone(&vm),
            user_vms: Arc::new(Mutex::new(HashMap::new())),
            data_dir: None,
            raft_node: None,
            peer_rest_addrs: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        }
    }

    /// Like `in_memory()` but pre-registers a `root` user with an empty password
    /// so that tests can authenticate via the normal MySQL native-password flow
    /// without relying on the dev-mode bypass.
    ///
    /// **Only use this in test code — never in production.**
    pub fn in_memory_with_test_user() -> Self {
        let state = Self::in_memory();
        {
            let mut vm = state.auth_vm.lock().unwrap_or_else(|e| e.into_inner());
            // Ensure auth table exists
            let _ = vm.execute_sql(
                "CREATE TABLE IF NOT EXISTS kkdb_auth_users (email TEXT, mysql_auth_hash TEXT)",
            );
            // Insert root user with double-SHA1 of empty password
            let empty_hash = crate::server::mysql::mysql_double_sha1("");
            let _ = vm.execute_sql(&format!(
                "INSERT INTO kkdb_auth_users (email, mysql_auth_hash) VALUES ('root', '{empty_hash}')"
            ));
        }
        state
    }

    pub fn with_dir(data_dir: PathBuf) -> Result<Self, String> {
        let auth_dir = data_dir.join("_auth");
        let auth_vm = VM::open(auth_dir.to_str().unwrap_or("_auth")).map_err(|e| e.to_string())?;
        Ok(Self {
            auth_vm: Arc::new(Mutex::new(auth_vm)),
            user_vms: Arc::new(Mutex::new(HashMap::new())),
            data_dir: Some(Arc::new(data_dir)),
            raft_node: None,
            peer_rest_addrs: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        })
    }

    /// Attach a Raft node to this AppState for cluster-mode write routing.
    pub fn with_raft(
        mut self,
        node: Arc<crate::raft::node::KkdbNode>,
        peer_rest_addrs: std::collections::BTreeMap<u64, String>,
    ) -> Self {
        self.raft_node = Some(node);
        *self
            .peer_rest_addrs
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = peer_rest_addrs;
        self
    }
}

// ─── Per-user VM helper ───────────────────────────────────────────────────────

/// Get (or open) the per-user VM for `user_id`.
///
/// - If `data_dir` is set, opens/creates `{data_dir}/{user_id}/` (directory mode).
/// - If `data_dir` is None (test / in-memory mode), returns a fresh in-memory VM
///   or a cached one (tests share the same in-memory auth_vm intentionally).
fn get_user_vm(
    state: &AppState,
    user_id: &str,
) -> Result<Arc<Mutex<VM>>, (StatusCode, Json<ErrorResponse>)> {
    // Fast path: already cached
    {
        let cache = state.user_vms.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(vm) = cache.get(user_id) {
            return Ok(Arc::clone(vm));
        }
    }

    // S1 fix: validate user_id before joining into a filesystem path.
    // Only allow characters safe for directory names: letters, digits, hyphens, underscores,
    // and at-signs (to support email-style user ids). Reject anything containing path
    // separators or parent-directory components.
    if !user_id.is_empty() {
        let safe = user_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '@' || c == '.')
            && !user_id.contains("..")
            && !user_id.starts_with('.');
        if !safe {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("invalid user_id: '{user_id}'"),
                }),
            ));
        }
    }

    // Slow path: open / create the user's DB
    let vm = match &state.data_dir {
        Some(base) => {
            let user_dir = base.as_ref().join(user_id);
            let path = user_dir.to_string_lossy().to_string();
            VM::open(&path).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: e.to_string(),
                    }),
                )
            })?
        }
        None => {
            // In-memory mode (tests): each user gets a fresh in-memory VM
            VM::new_memory()
        }
    };

    let vm_arc = Arc::new(Mutex::new(vm));
    state
        .user_vms
        .lock()
        .unwrap()
        .insert(user_id.to_string(), Arc::clone(&vm_arc));
    Ok(vm_arc)
}

// ─── Raft write helper ────────────────────────────────────────────────────────────────

/// Result from routing a SQL mutation through Raft.
pub enum RaftWriteResult {
    /// Raft was not configured; caller should use local VM.
    NotEnabled,
    /// Write accepted and applied by this node (Leader).
    Applied(String),
    /// Write transparently proxied to leader; response follows.
    Forwarded(serde_json::Value),
    /// Leader address unknown; cluster not ready.
    Redirect(String),
    /// Raft or network error.
    Err(String),
}

/// Submit `sql` through Raft consensus.
///
/// - **Leader**: calls `client_write()` directly.
/// - **Follower** + leader addr known: transparently **proxies** the request
///   to the leader via HTTP and returns the response (client never retries).
/// - **Follower** + leader unknown: returns `Redirect` with an error message.
/// - **Standalone** (no `raft_node`): returns `NotEnabled`.
pub async fn raft_write(state: &AppState, sql: &str, user_id: &str) -> RaftWriteResult {
    let Some(ref raft_node) = state.raft_node else {
        return RaftWriteResult::NotEnabled;
    };

    if raft_node.is_leader() {
        // ── Leader: commit locally ────────────────────────────────────────────
        return match raft_node
            .write(KkdbRequest {
                sql: sql.to_string(),
                user_id: user_id.to_string(),
            })
            .await
        {
            Ok(r) if r.ok => RaftWriteResult::Applied(r.message),
            Ok(r) => RaftWriteResult::Err(r.message),
            Err(e) => RaftWriteResult::Err(e.to_string()),
        };
    }

    // ── Follower: proxy to leader ─────────────────────────────────────────────
    let leader_url = {
        let m = raft_node.metrics();
        let addrs = state
            .peer_rest_addrs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        m.current_leader.and_then(|id| addrs.get(&id).cloned())
    };

    match leader_url {
        None => RaftWriteResult::Redirect("no leader elected yet".into()),
        Some(base_url) => {
            let target = format!("{base_url}/rest/query");
            let payload = serde_json::json!({ "sql": sql });
            match reqwest::Client::new()
                .post(&target)
                .header("X-Raft-Forward", "1") // prevent infinite proxy loops
                .header("Content-Type", "application/json")
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) => {
                    let ok = resp.status().is_success();
                    match resp.json::<serde_json::Value>().await {
                        Ok(body) if ok => RaftWriteResult::Forwarded(body),
                        Ok(body) => RaftWriteResult::Err(body.to_string()),
                        Err(e) => RaftWriteResult::Err(format!("proxy parse: {e}")),
                    }
                }
                Err(e) => RaftWriteResult::Err(format!("proxy request: {e}")),
            }
        }
    }
}

// ─── Router ───────────────────────────────────────────────────────────────────
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/auth/signup", post(signup_handler))
        .route("/auth/signin", post(signin_handler))
        .route("/auth/refresh", post(refresh_handler))
        .route("/auth/apikeys", post(create_apikey_handler))
        // Single-statement endpoints (routed to user's own VM)
        .route("/rest/query", post(sql_handler))
        .route("/rest/execute", post(sql_handler))
        .route("/rest/sql", post(sql_handler))
        // Batch: run multiple arbitrary SQL statements in one call
        .route("/rest/batch", post(batch_handler))
        // Bulk write: insert many rows into one table with one optimised INSERT
        .route("/rest/bulk", post(bulk_write_handler))
        .with_state(state)
}

// Helper: build AppState for tests (keeps backward compat in test helpers)
pub fn build_router_with_vm(vm: Arc<Mutex<VM>>) -> Router {
    let state = AppState {
        auth_vm: Arc::clone(&vm),
        user_vms: Arc::new(Mutex::new(HashMap::new())),
        data_dir: None,
        raft_node: None,
        peer_rest_addrs: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
    };
    build_router(state)
}

// ─── GET /health ─────────────────────────────────────────────────────────────
async fn health_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "engine": "kkdb" }))
}

// ─── POST /auth/signup ────────────────────────────────────────────────────────
async fn signup_handler(
    State(state): State<AppState>,
    Json(body): Json<SignupRequest>,
) -> Result<Json<AuthResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Basic validation
    if body.email.is_empty() || body.password.is_empty() {
        return api_err(StatusCode::BAD_REQUEST, "email and password required");
    }

    let user_id = Uuid::new_v4().to_string();
    let password_hash = bcrypt::hash(&body.password, bcrypt::DEFAULT_COST).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let mut vm = state.auth_vm.lock().unwrap_or_else(|e| e.into_inner());

    // Ensure the kkdb_auth_users table exists
    let _ = vm.execute_sql(
        "CREATE TABLE IF NOT EXISTS kkdb_auth_users \
         (id TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL, \
          password_hash TEXT NOT NULL, role TEXT DEFAULT 'authenticated', \
          created_at TEXT, mysql_auth_hash TEXT DEFAULT '')",
    );
    // Migration: add column for existing installs
    let _ =
        vm.execute_sql("ALTER TABLE kkdb_auth_users ADD COLUMN mysql_auth_hash TEXT DEFAULT ''");

    // Check duplicate email
    let check_sql = format!(
        "SELECT id FROM kkdb_auth_users WHERE email = '{}'",
        body.email.replace('\'', "''")
    );
    if let Ok(ExecResult::QueryResult { rows, .. }) = vm.execute_sql(&check_sql) {
        if !rows.is_empty() {
            return api_err(StatusCode::CONFLICT, "user already exists");
        }
    }

    // Insert new user with both bcrypt and MySQL native password hash
    let mysql_hash = crate::server::mysql::mysql_double_sha1(&body.password);
    let insert_sql = format!(
        "INSERT INTO kkdb_auth_users (id, email, password_hash, role, created_at, mysql_auth_hash) \
         VALUES ('{}', '{}', '{}', 'authenticated', '{}', '{}')",
        user_id,
        body.email.replace('\'', "''"),
        password_hash.replace('\'', "''"),
        chrono_now(),
        mysql_hash,
    );
    vm.execute_sql(&insert_sql).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    let token = issue_token(&user_id, &body.email, "authenticated")?;
    Ok(Json(AuthResponse {
        user_id,
        email: body.email,
        token,
    }))
}

// ─── POST /auth/refresh ───────────────────────────────────────────────────────
async fn refresh_handler(
    headers: HeaderMap,
) -> Result<Json<AuthResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Validate the existing JWT (even if nearly expired) and issue a fresh one
    let claims = extract_jwt(&headers)?;
    let token = issue_token(&claims.sub, &claims.email, &claims.role)?;
    Ok(Json(AuthResponse {
        user_id: claims.sub,
        email: claims.email,
        token,
    }))
}

// ─── POST /auth/apikeys ───────────────────────────────────────────────────────
#[derive(Debug, Serialize)]
pub struct ApiKeyResponse {
    pub key: String,
    pub key_id: String,
}

async fn create_apikey_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiKeyResponse>, (StatusCode, Json<ErrorResponse>)> {
    let claims = extract_jwt(&headers)?;

    let key_id = Uuid::new_v4().to_string();
    // Generate a 32-byte random key, encode as hex
    let raw_key = format!("kkdb_{}", Uuid::new_v4().to_string().replace('-', ""));
    let key_hash = bcrypt::hash(&raw_key, 4) // cost=4 for speed (API key, not password)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    let mut vm = state.auth_vm.lock().unwrap_or_else(|e| e.into_inner());
    let _ = vm.execute_sql(
        "CREATE TABLE IF NOT EXISTS kkdb_api_keys \
         (key_id TEXT PRIMARY KEY, user_id TEXT NOT NULL, \
          key_hash TEXT NOT NULL, created_at TEXT)",
    );
    let sql = format!(
        "INSERT INTO kkdb_api_keys (key_id, user_id, key_hash, created_at) \
         VALUES ('{}', '{}', '{}', '{}')",
        key_id,
        claims.sub,
        key_hash.replace('\'', "''"),
        chrono_now()
    );
    vm.execute_sql(&sql).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    // Return the raw key — only shown once, store securely
    Ok(Json(ApiKeyResponse {
        key: raw_key,
        key_id,
    }))
}

// ─── POST /auth/signin ────────────────────────────────────────────────────────
async fn signin_handler(
    State(state): State<AppState>,
    Json(body): Json<SigninRequest>,
) -> Result<Json<AuthResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut vm = state.auth_vm.lock().unwrap_or_else(|e| e.into_inner());

    let sql = format!(
        "SELECT id, password_hash, role FROM kkdb_auth_users WHERE email = '{}'",
        body.email.replace('\'', "''")
    );
    let (user_id, stored_hash, role) = match vm.execute_sql(&sql) {
        Ok(ExecResult::QueryResult { rows, .. }) if !rows.is_empty() => {
            let row = &rows[0];
            let id = row.first().map(|v| format!("{v}")).unwrap_or_default();
            let hash = row.get(1).map(|v| format!("{v}")).unwrap_or_default();
            let role = row
                .get(2)
                .map(|v| format!("{v}"))
                .unwrap_or("authenticated".into());
            (id, hash, role)
        }
        _ => return api_err(StatusCode::UNAUTHORIZED, "invalid email or password"),
    };

    // Verify password hash
    let valid = bcrypt::verify(&body.password, &stored_hash).unwrap_or(false);
    if !valid {
        return api_err(StatusCode::UNAUTHORIZED, "invalid email or password");
    }

    // Lazy-migration: back-fill mysql_auth_hash for existing users who don't have it yet.
    // On each successful HTTP login, we regenerate the hash so the MySQL client can connect.
    let mysql_hash = crate::server::mysql::mysql_double_sha1(&body.password);
    let update_sql = format!(
        "UPDATE kkdb_auth_users SET mysql_auth_hash = '{}' WHERE id = '{}'",
        mysql_hash, user_id
    );
    let _ = vm.execute_sql(&update_sql); // best-effort: don't fail signin if this somehow errors

    let token = issue_token(&user_id, &body.email, &role)?;
    Ok(Json(AuthResponse {
        user_id,
        email: body.email,
        token,
    }))
}

// ─── POST /rest/query ─────────────────────────────────────────────────────────
async fn sql_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, (StatusCode, Json<ErrorResponse>)> {
    // ── Step 1: Auth (all sync, drops guards in scoped block) ────────────────
    let (user_id, email, role): (String, String, String) = {
        if let Some(api_key_hdr) = headers.get("X-API-Key") {
            let raw_key = api_key_hdr.to_str().unwrap_or("").to_string();
            let (uid, role) = {
                let mut vm = state.auth_vm.lock().unwrap_or_else(|e| e.into_inner());
                let sql = "SELECT key_id, user_id, key_hash FROM kkdb_api_keys";
                match vm.execute_sql(sql) {
                    Ok(ExecResult::QueryResult { rows, .. }) => {
                        let found = rows.iter().find(|row| {
                            let stored = row.get(2).map(|v| format!("{v}")).unwrap_or_default();
                            bcrypt::verify(&raw_key, &stored).unwrap_or(false)
                        });
                        match found {
                            Some(row) => {
                                let uid = row.get(1).map(|v| format!("{v}")).unwrap_or_default();
                                (uid, "authenticated".to_string())
                            }
                            None => return api_err(StatusCode::UNAUTHORIZED, "invalid API key"),
                        }
                    }
                    _ => return api_err(StatusCode::UNAUTHORIZED, "invalid API key"),
                }
            }; // vm guard dropped here
            (uid, String::new(), role)
        } else {
            let claims = extract_jwt(&headers)?;
            (claims.sub, claims.email, claims.role)
        }
    };

    // ── Step 2: Classify SQL (sync, no guard) ────────────────────────────────
    let sql_upper = body.sql.trim_start().to_ascii_uppercase();
    // I7 fix: WITH...SELECT CTEs are reads; peek past the CTE preamble.
    let is_write = if sql_upper.starts_with("WITH") {
        // Lightweight check: look for a write-verb that follows the WITH preamble.
        // Write verbs may appear after a ')' or whitespace inside the WITH body too,
        // so the check is conservative: if ANY write verb appears anywhere outside
        // a SELECT context we treat it as a write.
        let write_verbs = [
            "INSERT ",
            "UPDATE ",
            "DELETE ",
            "CREATE ",
            "DROP ",
            "ALTER ",
            "TRUNCATE ",
            "VACUUM",
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT",
            "RELEASE ",
        ];
        write_verbs.iter().any(|&v| {
            if let Some(pos) = sql_upper.find(v) {
                // The verb must not be the first word (that's "WITH")
                // and must be preceded by whitespace or ')'
                pos > 0
                    && (sql_upper.as_bytes()[pos - 1] == b' '
                        || sql_upper.as_bytes()[pos - 1] == b'\n'
                        || sql_upper.as_bytes()[pos - 1] == b')')
            } else {
                false
            }
        })
    } else {
        // M6+I7 fix: extend the explicit read-only whitelist
        !sql_upper.starts_with("SELECT")
            && !sql_upper.starts_with("EXPLAIN")
            && !sql_upper.starts_with("PRAGMA")
            && !sql_upper.starts_with("SHOW")
            && !sql_upper.starts_with("ANALYZE")
            && !sql_upper.starts_with("DESCRIBE")
            && !sql_upper.starts_with("DESC ")
    };

    // ── Step 3: Cluster mode ─────────────────────────────────────────────────
    // No MutexGuard is alive at this point, so the future is Send.
    let already_forwarded = headers
        .get("X-Raft-Forward")
        .map(|v| v == "1")
        .unwrap_or(false);

    if is_write && !already_forwarded {
        match raft_write(&state, &body.sql, &user_id).await {
            RaftWriteResult::NotEnabled => {} // standalone: fall through to local VM
            RaftWriteResult::Applied(msg) => {
                return Ok(Json(QueryResponse {
                    columns: vec!["message".into()],
                    rows: vec![vec![serde_json::json!(msg)]],
                }));
            }
            RaftWriteResult::Forwarded(body_json) => {
                match serde_json::from_value::<QueryResponse>(body_json.clone()) {
                    Ok(qr) => return Ok(Json(qr)),
                    Err(_) => {
                        return Err((
                            StatusCode::BAD_GATEWAY,
                            Json(ErrorResponse {
                                error: format!("leader proxy response: {body_json}"),
                            }),
                        ))
                    }
                }
            }
            RaftWriteResult::Redirect(info) => {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ErrorResponse {
                        error: format!("cluster not ready: {info}"),
                    }),
                ));
            }
            RaftWriteResult::Err(e) => {
                return api_err(StatusCode::INTERNAL_SERVER_ERROR, &e);
            }
        }
    } else if !is_write {
        // ── ReadIndex fence (linearizable Follower reads) ─────────────────────
        // Ensures all committed entries are applied before serving a SELECT.
        if let Some(ref raft_node) = state.raft_node {
            if !raft_node.is_leader() {
                let _ = raft_node.ensure_linearizable().await; // best-effort
            }
        }
    }

    // ── Step 4: Local execution (standalone mode or reads in cluster) ─────────
    // Re-enter a scoped block so the guard is dropped before this fn returns.
    let exec_result = {
        let user_vm = get_user_vm(&state, &user_id)?;
        let mut vm = user_vm.lock().unwrap_or_else(|e| e.into_inner());
        vm.session_vars
            .insert("request.jwt.sub".to_string(), user_id.clone());
        vm.session_vars
            .insert("request.jwt.email".to_string(), email);
        vm.session_vars.insert("request.jwt.role".to_string(), role);
        vm.session_vars
            .insert("kkdb.current_user".to_string(), user_id.clone());
        vm.execute_sql(&body.sql)
    }; // guard dropped here

    match exec_result {
        Ok(ExecResult::QueryResult { columns, rows }) => {
            let json_rows: Vec<Vec<serde_json::Value>> = rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|v| match v {
                            crate::types::Value::Null => serde_json::Value::Null,
                            crate::types::Value::Integer(i) => serde_json::json!(i),
                            crate::types::Value::Real(f) => serde_json::json!(f),
                            crate::types::Value::Text(s) => serde_json::json!(s.to_string()),
                            crate::types::Value::Blob(b) => serde_json::json!(base64_encode(&b)),
                        })
                        .collect()
                })
                .collect();
            Ok(Json(QueryResponse {
                columns,
                rows: json_rows,
            }))
        }
        Ok(ExecResult::Ok { message }) => Ok(Json(QueryResponse {
            columns: vec!["message".into()],
            rows: vec![vec![serde_json::json!(message)]],
        })),
        Ok(ExecResult::RowsAffected { message, .. }) => Ok(Json(QueryResponse {
            columns: vec!["message".into()],
            rows: vec![vec![serde_json::json!(message)]],
        })),
        Err(e) => api_err(StatusCode::BAD_REQUEST, e.to_string().as_str()),
        _ => api_err(StatusCode::INTERNAL_SERVER_ERROR, "unexpected result"),
    }
}

// ─── POST /rest/batch ─────────────────────────────────────────────────────────
async fn batch_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BatchRequest>,
) -> Result<Json<BatchResponse>, (StatusCode, Json<ErrorResponse>)> {
    if body.statements.is_empty() {
        return api_err(
            StatusCode::BAD_REQUEST,
            "statements must be a non-empty array",
        );
    }

    // ── Same dual-auth as sql_handler ─────────────────────────────────────────
    let (user_id, email, role) = if let Some(api_key_hdr) = headers.get("X-API-Key") {
        let raw_key = api_key_hdr.to_str().unwrap_or("").to_string();
        let mut vm = state.auth_vm.lock().unwrap_or_else(|e| e.into_inner());
        let sql = "SELECT key_id, user_id, key_hash FROM kkdb_api_keys";
        let (uid, role) = match vm.execute_sql(sql) {
            Ok(ExecResult::QueryResult { rows, .. }) => {
                let found = rows.iter().find(|row| {
                    let stored = row.get(2).map(|v| format!("{v}")).unwrap_or_default();
                    bcrypt::verify(&raw_key, &stored).unwrap_or(false)
                });
                match found {
                    Some(row) => {
                        let uid = row.get(1).map(|v| format!("{v}")).unwrap_or_default();
                        (uid, "authenticated".to_string())
                    }
                    None => return api_err(StatusCode::UNAUTHORIZED, "invalid API key"),
                }
            }
            _ => return api_err(StatusCode::UNAUTHORIZED, "invalid API key"),
        };
        drop(vm);
        (uid, String::new(), role)
    } else {
        let claims = extract_jwt(&headers)?;
        (claims.sub, claims.email, claims.role)
    };

    let user_vm = get_user_vm(&state, &user_id)?;
    let mut vm = user_vm.lock().unwrap_or_else(|e| e.into_inner());
    // Inject identity for RLS
    vm.session_vars
        .insert("request.jwt.sub".to_string(), user_id.clone());
    vm.session_vars
        .insert("request.jwt.email".to_string(), email);
    vm.session_vars.insert("request.jwt.role".to_string(), role);
    vm.session_vars
        .insert("kkdb.current_user".to_string(), user_id);

    // ── BEGIN transaction if requested ────────────────────────────────────────
    if body.transaction {
        let _ = vm.execute_sql("BEGIN");
    }

    let count = body.statements.len();
    let mut results: Vec<BatchStatementResult> = Vec::with_capacity(count);
    let mut succeeded = 0usize;
    #[allow(unused_assignments)]
    let mut failed_at: Option<usize> = None;

    for (idx, stmt) in body.statements.iter().enumerate() {
        match vm.execute_sql(stmt) {
            Ok(ExecResult::QueryResult { columns, rows }) => {
                let json_rows = exec_result_to_json_rows(rows);
                results.push(BatchStatementResult::Ok {
                    statement: stmt.clone(),
                    columns,
                    rows: json_rows,
                    rows_affected: None,
                });
                succeeded += 1;
            }
            Ok(ExecResult::RowsAffected { message, count, .. }) => {
                results.push(BatchStatementResult::Ok {
                    statement: stmt.clone(),
                    columns: vec!["message".into()],
                    rows: vec![vec![serde_json::json!(message)]],
                    rows_affected: Some(count as u64),
                });
                succeeded += 1;
            }
            Ok(ExecResult::Ok { message }) => {
                results.push(BatchStatementResult::Ok {
                    statement: stmt.clone(),
                    columns: vec!["message".into()],
                    rows: vec![vec![serde_json::json!(message)]],
                    rows_affected: None,
                });
                succeeded += 1;
            }
            Ok(_) => {
                results.push(BatchStatementResult::Ok {
                    statement: stmt.clone(),
                    columns: vec![],
                    rows: vec![],
                    rows_affected: None,
                });
                succeeded += 1;
            }
            Err(e) => {
                results.push(BatchStatementResult::Error {
                    statement: stmt.clone(),
                    error: e.to_string(),
                });
                if body.transaction {
                    failed_at = Some(idx);
                    // Fill remaining with not-executed
                    for skipped in body.statements.iter().skip(idx + 1) {
                        results.push(BatchStatementResult::Error {
                            statement: skipped.clone(),
                            error: "skipped due to transaction rollback".into(),
                        });
                    }
                    let _ = vm.execute_sql("ROLLBACK");
                    return Ok(Json(BatchResponse {
                        results,
                        count,
                        succeeded,
                        transaction: true,
                        failed_at,
                    }));
                }
                // Non-transaction: continue with remaining statements
            }
        }
    }

    if body.transaction {
        let _ = vm.execute_sql("COMMIT");
    }

    Ok(Json(BatchResponse {
        results,
        count,
        succeeded,
        transaction: body.transaction,
        failed_at: None,
    }))
}

/// Convert VM result rows to serde_json values (shared by sql_handler and batch_handler).
fn exec_result_to_json_rows(rows: Vec<Vec<crate::types::Value>>) -> Vec<Vec<serde_json::Value>> {
    rows.into_iter()
        .map(|row| {
            row.into_iter()
                .map(|v| match v {
                    crate::types::Value::Null => serde_json::Value::Null,
                    crate::types::Value::Integer(i) => serde_json::json!(i),
                    crate::types::Value::Real(f) => serde_json::json!(f),
                    crate::types::Value::Text(s) => serde_json::json!(s.to_string()),
                    crate::types::Value::Blob(b) => serde_json::json!(base64_encode(&b)),
                })
                .collect()
        })
        .collect()
}

// ─── POST /rest/bulk ──────────────────────────────────────────────────────────
async fn bulk_write_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BulkWriteRequest>,
) -> Result<Json<BulkWriteResponse>, (StatusCode, Json<ErrorResponse>)> {
    if body.rows.is_empty() {
        return api_err(StatusCode::BAD_REQUEST, "rows must be a non-empty array");
    }
    // Basic table-name sanity check (no semicolons / quotes)
    let table = body.table.trim().to_string();
    if table.is_empty() || table.contains('\'') || table.contains(';') {
        return api_err(StatusCode::BAD_REQUEST, "invalid table name");
    }

    // ── Dual auth (JWT | X-API-Key) ──────────────────────────────────────────
    let (user_id, email, role) = if let Some(api_key_hdr) = headers.get("X-API-Key") {
        let raw_key = api_key_hdr.to_str().unwrap_or("").to_string();
        let mut vm = state.auth_vm.lock().unwrap_or_else(|e| e.into_inner());
        let sql = "SELECT key_id, user_id, key_hash FROM kkdb_api_keys";
        let (uid, role) = match vm.execute_sql(sql) {
            Ok(ExecResult::QueryResult { rows, .. }) => {
                let found = rows.iter().find(|row| {
                    let stored = row.get(2).map(|v| format!("{v}")).unwrap_or_default();
                    bcrypt::verify(&raw_key, &stored).unwrap_or(false)
                });
                match found {
                    Some(row) => (
                        row.get(1).map(|v| format!("{v}")).unwrap_or_default(),
                        "authenticated".to_string(),
                    ),
                    None => return api_err(StatusCode::UNAUTHORIZED, "invalid API key"),
                }
            }
            _ => return api_err(StatusCode::UNAUTHORIZED, "invalid API key"),
        };
        drop(vm);
        (uid, String::new(), role)
    } else {
        let claims = extract_jwt(&headers)?;
        (claims.sub, claims.email, claims.role)
    };

    let user_vm = get_user_vm(&state, &user_id)?;
    let mut vm = user_vm.lock().unwrap_or_else(|e| e.into_inner());
    // Inject identity for RLS
    vm.session_vars
        .insert("request.jwt.sub".to_string(), user_id.clone());
    vm.session_vars
        .insert("request.jwt.email".to_string(), email);
    vm.session_vars.insert("request.jwt.role".to_string(), role);
    vm.session_vars
        .insert("kkdb.current_user".to_string(), user_id);

    // ── Extract column order from the first row ───────────────────────────────
    let columns: Vec<String> = body.rows[0].keys().cloned().collect();
    if columns.is_empty() {
        return api_err(StatusCode::BAD_REQUEST, "rows contain no columns");
    }

    // ── BEGIN transaction (real MVCC/COW) ────────────────────────────────────
    if body.transaction {
        vm.execute_sql("BEGIN").map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;
    }

    let col_list = columns.join(", ");
    let mut rows_written = 0usize;

    let result: Result<(), String> = (|| {
        if body.bulk_insert {
            // ── Single multi-row INSERT ──────────────────────────────────────
            let values_list: Vec<String> = body
                .rows
                .iter()
                .map(|row| {
                    let vals: Vec<String> = columns
                        .iter()
                        .map(|col| {
                            json_value_to_sql(row.get(col).unwrap_or(&serde_json::Value::Null))
                        })
                        .collect();
                    format!("({})", vals.join(", "))
                })
                .collect();
            let sql = format!(
                "INSERT INTO {} ({}) VALUES {}",
                table,
                col_list,
                values_list.join(", ")
            );
            vm.execute_sql(&sql).map_err(|e| e.to_string())?;
            rows_written = body.rows.len();
        } else {
            // ── N individual INSERTs (inside one transaction) ────────────────
            for row in &body.rows {
                let vals: Vec<String> = columns
                    .iter()
                    .map(|col| json_value_to_sql(row.get(col).unwrap_or(&serde_json::Value::Null)))
                    .collect();
                let sql = format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    table,
                    col_list,
                    vals.join(", ")
                );
                vm.execute_sql(&sql).map_err(|e| e.to_string())?;
                rows_written += 1;
            }
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            if body.transaction {
                vm.execute_sql("COMMIT").map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorResponse {
                            error: e.to_string(),
                        }),
                    )
                })?;
            }
            Ok(Json(BulkWriteResponse {
                table,
                rows_written,
                transaction: body.transaction,
                error: None,
            }))
        }
        Err(e) => {
            if body.transaction {
                let _ = vm.execute_sql("ROLLBACK");
            }
            Ok(Json(BulkWriteResponse {
                table,
                rows_written, // how many succeeded before the error (individual mode)
                transaction: body.transaction,
                error: Some(e),
            }))
        }
    }
}

/// Convert a serde_json::Value to an SQL literal string (safe, no injection).
fn json_value_to_sql(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(b) => {
            if *b {
                "1".to_string()
            } else {
                "0".to_string()
            }
        }
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => {
            // Escape single quotes by doubling them
            format!("'{}'", s.replace('\'', "''"))
        }
        // Arrays / objects: serialize to JSON string
        other => format!("'{}'", other.to_string().replace('\'', "''")),
    }
}

// ─── JWT helpers ──────────────────────────────────────────────────────────────
fn issue_token(
    user_id: &str,
    email: &str,
    role: &str,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    // SAFETY: UNIX_EPOCH is always in the past; duration_since never fails here
    let exp = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + jwt_expiry_secs()) as usize;

    let claims = Claims {
        sub: user_id.to_string(),
        email: email.to_string(),
        role: role.to_string(),
        exp,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(&jwt_secret()),
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })
}

fn extract_jwt(headers: &HeaderMap) -> Result<Claims, (StatusCode, Json<ErrorResponse>)> {
    let auth_header = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let token = auth_header.strip_prefix("Bearer ").unwrap_or("");
    if token.is_empty() {
        return api_err(
            StatusCode::UNAUTHORIZED,
            "missing Authorization: Bearer <token>",
        );
    }

    decode::<Claims>(
        token,
        &DecodingKey::from_secret(&jwt_secret()),
        &Validation::default(),
    )
    .map(|data| data.claims)
    .map_err(|e| {
        (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: format!("invalid token: {e}"),
            }),
        )
    })
}

// ─── Minor utilities ─────────────────────────────────────────────────────────
fn api_err<T>(status: StatusCode, msg: &str) -> Result<T, (StatusCode, Json<ErrorResponse>)> {
    Err((
        status,
        Json(ErrorResponse {
            error: msg.to_string(),
        }),
    ))
}

fn chrono_now() -> String {
    // Simple RFC3339-like timestamp without chrono dependency
    use std::time::{SystemTime, UNIX_EPOCH};
    // SAFETY: UNIX_EPOCH is always in the past; duration_since never fails here
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!("{}", secs)
}

fn base64_encode(data: &[u8]) -> String {
    // Minimal Base64 without external crate
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
        result.push(CHARS[b0 >> 2] as char);
        result.push(CHARS[((b0 & 3) << 4) | (b1 >> 4)] as char);
        result.push(if chunk.len() > 1 {
            CHARS[((b1 & 0xf) << 2) | (b2 >> 6)] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            CHARS[b2 & 0x3f] as char
        } else {
            '='
        });
    }
    result
}
