//! MySQL Wire Protocol server for KKDB.
//!
//! Allows any standard MySQL client (DBeaver, Navicat, mysql2, mysqlclient,
//! JDBC, etc.) to connect directly without code changes.
//!
//! ## Protocol overview
//!
//! ```text
//! TCP connect  →  Server sends Handshake v10 greeting
//! Client       →  HandshakeResponse (user, password, db)
//! Server       →  OK packet (auth accepted — simple check)
//! Client       →  COM_QUERY  (0x03) | COM_PING (0x0e) | COM_QUIT (0x01)
//! Server       →  Text result-set or OK/ERR packet
//! ```
//!
//! ## Supported commands
//!
//! | COM byte | Name       | Description         |
//! |----------|------------|---------------------|
//! | 0x03     | COM_QUERY  | Execute SQL         |
//! | 0x0e     | COM_PING   | Keepalive            |
//! | 0x01     | COM_QUIT   | Disconnect           |
//! | 0x02     | COM_INIT_DB| USE database         |
//!
//! ## Packet format
//!
//! `[3 bytes LE: payload_len][1 byte: seq][payload_bytes]`

use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use rand::RngCore;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::server::http_api::AppState;

// ─── Capability flags (subset we advertise) ───────────────────────────────────

const CLIENT_PROTOCOL_41: u32      = 1 << 9;
const CLIENT_SECURE_CONNECTION: u32 = 1 << 15;
const CLIENT_PLUGIN_AUTH: u32      = 1 << 19;
const CLIENT_CONNECT_WITH_DB: u32  = 1 << 3;
const CLIENT_LONG_FLAG: u32        = 1 << 2;
const CLIENT_TRANSACTIONS: u32     = 1 << 13;

const SERVER_CAPS: u32 = CLIENT_PROTOCOL_41
    | CLIENT_SECURE_CONNECTION
    | CLIENT_PLUGIN_AUTH
    | CLIENT_CONNECT_WITH_DB
    | CLIENT_LONG_FLAG
    | CLIENT_TRANSACTIONS;

// ─── Status flags ─────────────────────────────────────────────────────────────
const SERVER_STATUS_AUTOCOMMIT: u16 = 0x0002;

// I32 fix: global atomic counter for unique connection IDs.
// Previous code used a fixed connection_id = 1 for every connection, which
// confused JDBC and other stateful MySQL drivers.
static MYSQL_CONN_ID: AtomicU32 = AtomicU32::new(1);

// ─── COM byte values ──────────────────────────────────────────────────────────
const COM_QUIT:    u8 = 0x01;
const COM_INIT_DB: u8 = 0x02;
const COM_QUERY:   u8 = 0x03;
const COM_PING:    u8 = 0x0e;

// ─── Column type constants (MySQL text protocol) ──────────────────────────────
const MYSQL_TYPE_VAR_STRING: u8 = 0xfd;

// ─── SHA-1 helpers ────────────────────────────────────────────────────────────

/// Compute SHA-1 of a byte slice.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h = Sha1::new();
    h.update(data);
    h.finalize().into()
}

/// Hex-encode a byte slice (lowercase).
fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse a 40-char hex string into 20 bytes. Returns None if invalid.
fn hex_decode_20(s: &str) -> Option<[u8; 20]> {
    if s.len() != 40 { return None; }
    let mut out = [0u8; 20];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)? as u8;
        let lo = (chunk[1] as char).to_digit(16)? as u8;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

/// Compute `SHA1(SHA1(plaintext))` — the value we store for MySQL native auth.
pub fn mysql_double_sha1(password: &str) -> String {
    hex_encode(&sha1(&sha1(password.as_bytes())))
}

/// Verify a mysql_native_password challenge-response.
///
/// Protocol:
///   client_auth = SHA1(password) XOR SHA1(scramble ‖ SHA1(SHA1(password)))
///
/// Given `stored_sha1_sha1` = SHA1(SHA1(password)) we can verify without
/// knowing the plaintext.
///
/// Returns `true` if the auth response is correct.
pub fn verify_native_password(
    scramble: &[u8; 20],
    client_response: &[u8],  // exactly 20 bytes from client
    stored_double_sha1_hex: &str, // hex(SHA1(SHA1(pwd))) stored in users table
) -> bool {
    if client_response.len() != 20 { return false; }
    let stored = match hex_decode_20(stored_double_sha1_hex) {
        Some(b) => b,
        None    => return false,
    };

    // SHA1(scramble ‖ stored_double_sha1)
    let mut h = Sha1::new();
    h.update(scramble);
    h.update(stored);
    let hash: [u8; 20] = h.finalize().into();

    // Recover SHA1(password) = client_response XOR hash
    let mut recovered_sha1 = [0u8; 20];
    for i in 0..20 {
        recovered_sha1[i] = client_response[i] ^ hash[i];
    }

    // Check SHA1(recovered) == stored_double_sha1
    sha1(&recovered_sha1) == stored
}


// ─── Public API ───────────────────────────────────────────────────────────────

/// Start the MySQL protocol listener on `addr` (e.g. "0.0.0.0:3307").
///
/// Each incoming connection gets its own task and its own VM (shared via
/// `app_state.user_vms` if data_dir is set, or a fresh in-memory VM otherwise).
pub async fn serve_mysql(addr: &str, app_state: AppState) -> io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    println!("[MySQL] Listening on {addr}");

    // I35 fix: run the mysql_auth_hash column migration ONCE at startup instead of
    // on every connection. This avoids repeated DDL execution and auth_vm lock contention
    // under high connection rates.
    {
        let mut vm = app_state.auth_vm.lock().unwrap();
        let _ = vm.execute_sql(
            "ALTER TABLE kkdb_auth_users ADD COLUMN mysql_auth_hash TEXT DEFAULT ''"
        );
    }

    let app = Arc::new(app_state);
    loop {
        let (stream, peer) = listener.accept().await?;
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, app).await {
                if e.kind() != io::ErrorKind::ConnectionReset
                    && e.kind() != io::ErrorKind::BrokenPipe
                {
                    eprintln!("[MySQL] {peer}: {e}");
                }
            }
        });
    }
}

// ─── Conn state ───────────────────────────────────────────────────────────────

struct Conn {
    stream: TcpStream,
    seq: u8,
    app: Arc<AppState>,
    user: String,
    selected_db: String,
    /// Random 20-byte auth scramble sent in the greeting.
    scramble: [u8; 20],
}

impl Conn {
    fn new(stream: TcpStream, app: Arc<AppState>) -> Self {
        let mut scramble = [0u8; 20];
        rand::thread_rng().fill_bytes(&mut scramble);
        Self { stream, seq: 0, app, user: String::new(), selected_db: String::new(), scramble }
    }

    // ── Packet I/O ────────────────────────────────────────────────────────────

    async fn send_packet(&mut self, payload: &[u8]) -> io::Result<()> {
        let len = payload.len() as u32;
        let header = [
            (len & 0xFF) as u8,
            ((len >> 8) & 0xFF) as u8,
            ((len >> 16) & 0xFF) as u8,
            self.seq,
        ];
        self.seq = self.seq.wrapping_add(1);
        self.stream.write_all(&header).await?;
        self.stream.write_all(payload).await?;
        self.stream.flush().await
    }

    async fn read_packet(&mut self) -> io::Result<Vec<u8>> {
        let mut header = [0u8; 4];
        self.stream.read_exact(&mut header).await?;
        let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
        self.seq = header[3].wrapping_add(1);
        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload).await?;
        Ok(payload)
    }

    // ── Packet builders ───────────────────────────────────────────────────────

    fn handshake_v10(&self) -> Vec<u8> {
        let auth_plugin = b"mysql_native_password\0";
        // Split the 20-byte scramble into two parts: 8 + 12 (MySQL wire format)
        let (part1, part2) = self.scramble.split_at(8);

        let mut p = Vec::with_capacity(128);
        p.push(10); // protocol version 10
        p.extend_from_slice(b"8.0.33-kkdb\0"); // server version
        // I32 fix: use a globally unique, incrementing connection ID
        let conn_id = MYSQL_CONN_ID.fetch_add(1, Ordering::Relaxed);
        p.extend_from_slice(&conn_id.to_le_bytes()); // connection id (4 bytes LE)
        p.extend_from_slice(part1);             // scramble part 1 (8 bytes)
        p.push(0);                              // filler
        p.extend_from_slice(&(SERVER_CAPS as u16).to_le_bytes()); // caps low
        p.push(33);  // charset: utf8mb4
        p.extend_from_slice(&SERVER_STATUS_AUTOCOMMIT.to_le_bytes());
        p.extend_from_slice(&((SERVER_CAPS >> 16) as u16).to_le_bytes()); // caps high
        p.push(21); // auth plugin data length (8 + 12 + 1 null)
        p.extend_from_slice(&[0u8; 10]); // reserved
        p.extend_from_slice(part2);  // scramble part 2 (12 bytes)
        p.push(0);                   // null terminator
        p.extend_from_slice(auth_plugin);
        p
    }

    fn ok_packet(&self, affected: u64, last_insert_id: u64) -> Vec<u8> {
        let mut p = vec![0x00]; // OK marker
        p.extend(encode_lenenc_int(affected));
        p.extend(encode_lenenc_int(last_insert_id));
        p.extend_from_slice(&SERVER_STATUS_AUTOCOMMIT.to_le_bytes());
        p.extend_from_slice(&[0u8, 0]); // warnings
        p
    }

    fn err_packet(&self, code: u16, msg: &str) -> Vec<u8> {
        let mut p = vec![0xff]; // ERR marker
        p.extend_from_slice(&code.to_le_bytes());
        p.push(b'#');
        p.extend_from_slice(b"HY000"); // SQL state
        p.extend_from_slice(msg.as_bytes());
        p
    }

    fn eof_packet(&self) -> Vec<u8> {
        vec![0xfe, 0, 0, SERVER_STATUS_AUTOCOMMIT as u8, (SERVER_STATUS_AUTOCOMMIT >> 8) as u8]
    }

    // ── Handshake ─────────────────────────────────────────────────────────────

    async fn do_handshake(&mut self) -> io::Result<()> {
        // Server greeting
        let greeting = self.handshake_v10();
        self.send_packet(&greeting).await?;

        // Client HandshakeResponse
        let pkt = self.read_packet().await?;
        if pkt.len() < 32 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short handshake"));
        }

        // Parse: capabilities(4) + max_packet(4) + charset(1) + reserved(23) = 32 bytes
        let mut pos = 32usize;

        // username (null-terminated)
        let username_end = pkt[pos..].iter().position(|&b| b == 0).unwrap_or(pkt.len() - pos);
        self.user = String::from_utf8_lossy(&pkt[pos..pos + username_end]).into_owned();
        pos += username_end + 1;

        // S8 fix: validate username format to prevent path traversal via the MySQL protocol.
        // Only allow characters safe for use as directory names / user identifiers.
        let user_safe = self.user.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '@' || c == '.'
        }) && !self.user.contains("..") && !self.user.starts_with('.');
        if !user_safe && !self.user.is_empty() {
            let err = self.err_packet(1045, &format!(
                "Access denied: invalid characters in username '{}'",
                self.user
            ));
            self.send_packet(&err).await?;
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "invalid username format"));
        }

        // auth-response (length-encoded string in Protocol 4.1)
        let mut auth_response: Vec<u8> = Vec::new();
        if pos < pkt.len() {
            let auth_len = pkt[pos] as usize;
            pos += 1;
            if pos + auth_len <= pkt.len() {
                auth_response = pkt[pos..pos + auth_len].to_vec();
                pos += auth_len;
            }
        }

        // optional: database name
        if pos < pkt.len() {
            let db_end = pkt[pos..].iter().position(|&b| b == 0).unwrap_or(pkt.len() - pos);
            self.selected_db = String::from_utf8_lossy(&pkt[pos..pos + db_end]).into_owned();
        }

        // Authenticate
        let auth_ok = self.authenticate(&auth_response);
        if auth_ok {
            let ok = self.ok_packet(0, 0);
            self.send_packet(&ok).await
        } else {
            let err = self.err_packet(1045, &format!(
                "Access denied for user '{}' (using password: YES)",
                self.user
            ));
            self.send_packet(&err).await?;
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "auth failed"))
        }
    }

    /// Verify the mysql_native_password challenge-response against the
    /// stored `mysql_auth_hash` in `kkdb_auth_users`.
    ///
    /// root / admin: always accepted if `kkdb_auth_users` is empty (first boot).
    /// Other users: must pass native_password verification.
    fn authenticate(&self, auth_response: &[u8]) -> bool {
        let user = self.user.trim();

        // Empty auth response with empty password → check if user exists at all
        let empty_pass = auth_response.is_empty();

        // Query auth VM for this user's mysql_auth_hash
        let hash_opt = {
            let vm_arc = Arc::clone(&self.app.auth_vm);
            let mut vm = vm_arc.lock().unwrap();

            // Note: the mysql_auth_hash column migration is performed once at server start
            // in serve_mysql() — no DDL needed here (I35 fix).
            let sql = format!(
                "SELECT mysql_auth_hash FROM kkdb_auth_users WHERE email = '{}' LIMIT 1",
                user.replace('\'', "''")
            );
            match vm.execute_sql(&sql) {
                Ok(crate::vm::execute::ExecResult::QueryResult { rows, .. }) => {
                    rows.into_iter().next()
                        .and_then(|row| row.into_iter().next())
                        .map(|v| format!("{v}"))
                        .filter(|s| !s.is_empty() && s != "NULL")
                }
                _ => None,
            }
        };

        match hash_opt {
            Some(stored_hash) => {
                if empty_pass {
                    // Client sent no password — only match if password is empty hash
                    let empty_hash = mysql_double_sha1("");
                    stored_hash == empty_hash
                } else {
                    verify_native_password(&self.scramble, auth_response, &stored_hash)
                }
            }
            None => {
                // S7 fix: user not found in auth table — always deny.
                // The old dev-mode bypass (accepting root/admin without password)
                // has been removed to prevent unauthorized access in production.
                // To create initial credentials, use the HTTP API /auth/register endpoint.
                eprintln!("[MySQL] WARN: user '{}' not found or has no mysql_auth_hash — denying access", user);
                false
            }
        }
    }

    // ── Result-set encoding (MySQL text protocol) ─────────────────────────────

    /// Send a full MySQL text result-set for the given rows.
    ///
    /// `headers` = column names.
    /// `rows`    = rows, each row is a Vec<Option<String>>.
    async fn send_result_set(
        &mut self,
        headers: Vec<String>,
        rows: Vec<Vec<Option<String>>>,
    ) -> io::Result<()> {
        let ncols = headers.len();

        // 1. Column count packet
        self.send_packet(&encode_lenenc_int(ncols as u64)).await?;

        // 2. Column definition packets
        for col_name in &headers {
            self.send_packet(&column_def_packet(col_name)).await?;
        }

        // 3. EOF after column defs
        let eof = self.eof_packet();
        self.send_packet(&eof).await?;

        // 4. Row data packets
        for row in &rows {
            let mut pkt = Vec::new();
            for cell in row {
                match cell {
                    None => pkt.push(0xfb), // NULL
                    Some(s) => {
                        pkt.extend(encode_lenenc_int(s.len() as u64));
                        pkt.extend_from_slice(s.as_bytes());
                    }
                }
            }
            self.send_packet(&pkt).await?;
        }

        // 5. EOF after rows
        self.send_packet(&eof).await
    }

    // ── COM_QUERY execution ───────────────────────────────────────────────────

    async fn handle_query(&mut self, sql: &str) -> io::Result<()> {
        let sql = sql.trim();

        // Intercept MySQL client introspection queries
        if let Some(resp) = handle_client_introspection(sql) {
            return self.send_result_set(resp.0, resp.1).await;
        }

        // Route to VM
        let result = {
            let user_id = self.user.clone();
            let selected_db = self.selected_db.clone();
            execute_sql_for_user(&self.app, &user_id, &selected_db, sql)
        };

        match result {
            Ok(SqlResult::Rows { columns, rows }) => {
                self.send_result_set(columns, rows).await
            }
            Ok(SqlResult::Ok { affected, last_insert_id }) => {
                let pkt = self.ok_packet(affected, last_insert_id);
                self.send_packet(&pkt).await
            }
            Err(e) => {
                let pkt = self.err_packet(1064, &e.to_string());
                self.send_packet(&pkt).await
            }
        }
    }

    // ── Main command loop ─────────────────────────────────────────────────────

    /// After successful auth, inject session variables into the user VM so that
    /// RLS policies (using `current_user()`, `auth.uid()`) work correctly.
    fn inject_session_context(&self) {
        let user = self.user.trim().to_string();
        if user.is_empty() { return; }

        // Look up the user's UUID (for auth.uid())
        let user_id = {
            let mut vm = self.app.auth_vm.lock().unwrap();
            let sql = format!(
                "SELECT id FROM kkdb_auth_users WHERE email = '{}' LIMIT 1",
                user.replace('\'', "''")
            );
            match vm.execute_sql(&sql) {
                Ok(crate::vm::execute::ExecResult::QueryResult { rows, .. }) => {
                    rows.into_iter().next()
                        .and_then(|row| row.into_iter().next())
                        .map(|v| format!("{v}"))
                        .unwrap_or_else(|| user.clone())
                }
                _ => user.clone(),
            }
        };

        // Inject into the user's own VM
        let vm_arc = {
            let key = if user == "root" || user == "admin" { "root".to_string() } else { user.clone() };
            let cache = self.app.user_vms.lock().unwrap();
            if let Some(vm) = cache.get(&key) {
                Arc::clone(vm)
            } else {
                return; // VM not yet created — context will be set on first query
            }
        };

        let mut vm = vm_arc.lock().unwrap();
        let _ = vm.execute_sql(&format!("SET kkdb.current_user = '{}'", user.replace('\'', "''")));
        let _ = vm.execute_sql(&format!("SET request.jwt.sub = '{}'", user_id.replace('\'', "''")));
    }

    async fn run(&mut self) -> io::Result<()> {
        self.do_handshake().await?;
        // Bind kkdb.current_user in user VM so RLS policies work over MySQL
        self.inject_session_context();

        loop {
            self.seq = 0;
            let pkt = match self.read_packet().await {
                Ok(p) => p,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            };
            if pkt.is_empty() {
                continue;
            }
            match pkt[0] {
                COM_QUIT => break,
                COM_PING => {
                    let ok = self.ok_packet(0, 0);
                    self.send_packet(&ok).await?;
                }
                COM_INIT_DB => {
                    let db = String::from_utf8_lossy(&pkt[1..]).trim_end_matches('\0').to_string();
                    self.selected_db = db;
                    let ok = self.ok_packet(0, 0);
                    self.send_packet(&ok).await?;
                }
                COM_QUERY => {
                    let sql = String::from_utf8_lossy(&pkt[1..]).to_string();
                    self.handle_query(&sql).await?;
                }
                _ => {
                    // Unrecognised command — reply with error
                    let e = self.err_packet(1047, "Unknown command");
                    self.send_packet(&e).await?;
                }
            }
        }
        Ok(())
    }
}

async fn handle_connection(stream: TcpStream, app: Arc<AppState>) -> io::Result<()> {
    let _ = stream.set_nodelay(true);
    let mut conn = Conn::new(stream, app);
    conn.run().await
}

// ─── SQL result type ──────────────────────────────────────────────────────────

enum SqlResult {
    Rows { columns: Vec<String>, rows: Vec<Vec<Option<String>>> },
    Ok { affected: u64, last_insert_id: u64 },
}

/// Execute SQL against the user's VM (or auth VM).
fn execute_sql_for_user(
    app: &AppState,
    user_id: &str,
    _selected_db: &str,
    sql: &str,
) -> Result<SqlResult, Box<dyn std::error::Error + Send + Sync>> {
    use crate::vm::execute::{ExecResult, VM};

    let vm_arc = if user_id.is_empty() || user_id == "root" || user_id == "admin" {
        Arc::clone(&app.auth_vm)
    } else {
        let mut cache = app.user_vms.lock().unwrap();
        if let Some(vm) = cache.get(user_id) {
            Arc::clone(vm)
        } else {
            let vm = match &app.data_dir {
                Some(base) => {
                    let path = base.as_ref().join(user_id);
                    VM::open(&path.to_string_lossy()).map_err(|e| format!("{e}"))?
                }
                None => VM::new_memory(),
            };
            let arc = Arc::new(Mutex::new(vm));
            cache.insert(user_id.to_string(), Arc::clone(&arc));
            arc
        }
    };

    let mut vm = vm_arc.lock().unwrap();
    match vm.execute_sql(sql) {
        Ok(ExecResult::QueryResult { columns, rows }) => {
            // rows: Vec<Vec<crate::vm::execute::Value>> — convert to Option<String>
            let col_names: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
            let out_rows: Vec<Vec<Option<String>>> = rows
                .into_iter()
                .map(|row| row.into_iter().map(|cell| Some(format!("{cell}"))).collect())
                .collect();
            Ok(SqlResult::Rows { columns: col_names, rows: out_rows })
        }
        Ok(ExecResult::Ok { .. }) => {
            Ok(SqlResult::Ok { affected: 0, last_insert_id: 0 })
        }
        Ok(ExecResult::RowsAffected { count, .. }) => {
            Ok(SqlResult::Ok { affected: count as u64, last_insert_id: 0 })
        }
        Ok(ExecResult::Explain { plan }) => {
            // Return the plan as a single-column result
            Ok(SqlResult::Rows {
                columns: vec!["Plan".into()],
                rows: vec![vec![Some(plan)]],
            })
        }
        Err(e) => Err(format!("{e}").into()),
    }
}

// ─── Client introspection query interceptor ───────────────────────────────────

/// Handle MySQL client "magic" queries that many clients send on connect.
/// Returns `Some((columns, rows))` when intercepted, `None` to fall through.
fn handle_client_introspection(sql: &str) -> Option<(Vec<String>, Vec<Vec<Option<String>>>)> {
    let raw = sql.trim().trim_end_matches(';');
    let upper = raw.to_uppercase();
    let upper = upper.trim();

    // ── SET statements (session vars, charset, etc.) ───────────────────────────
    // mysql_async, DBeaver, Navicat all send SET @@ on connect. Accept all silently.
    if upper.starts_with("SET ") {
        return Some((vec![], vec![])); // → OK packet
    }

    // ── SELECT @@ system variables ────────────────────────────────────────────
    // mysql_async 0.36 sends several @@variable queries right after connecting.
    if upper.starts_with("SELECT @@") || upper.starts_with("SELECT  @@") {
        let var_upper = upper
            .trim_start_matches("SELECT ")
            .trim()
            .trim_start_matches('@')
            .trim_start_matches('@');

        // Strip SESSION. prefix
        let var_upper = var_upper
            .trim_start_matches("SESSION.")
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .next()
            .unwrap_or("VAR");

        let col_name = format!("@@{}", var_upper.to_lowercase());
        let val = match var_upper {
            "MAX_ALLOWED_PACKET"       => "67108864",
            "TIME_ZONE"                => "SYSTEM",
            "SYSTEM_TIME_ZONE"         => "UTC",
            "SQL_MODE"                 => "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES",
            "AUTOCOMMIT"               => "1",
            "TRANSACTION_ISOLATION"
            | "TX_ISOLATION"           => "REPEATABLE-READ",
            "TRANSACTION_READ_ONLY"    => "0",
            "LOWER_CASE_TABLE_NAMES"   => "0",
            "WAIT_TIMEOUT"
            | "INTERACTIVE_TIMEOUT"    => "28800",
            "NET_WRITE_TIMEOUT"
            | "NET_READ_TIMEOUT"       => "60",
            "CHARACTER_SET_SERVER"
            | "CHARACTER_SET_CLIENT"
            | "CHARACTER_SET_RESULTS"  => "utf8mb4",
            "COLLATION_SERVER"         => "utf8mb4_general_ci",
            "VERSION"                  => "8.0.33-kkdb",
            "VERSION_COMMENT"          => "KKDB MySQL Compatible",
            _                          => "",
        };
        return Some((
            vec![col_name],
            vec![vec![Some(val.into())]],
        ));
    }

    // ── SELECT VERSION() ───────────────────────────────────────────────────────
    if upper.starts_with("SELECT VERSION()") {
        return Some((
            vec!["version()".into()],
            vec![vec![Some("8.0.33-kkdb".into())]],
        ));
    }

    // ── SELECT DATABASE() ─────────────────────────────────────────────────────
    if upper.starts_with("SELECT DATABASE()") {
        return Some((
            vec!["DATABASE()".into()],
            vec![vec![Some("kkdb".into())]],
        ));
    }

    // ── SELECT 1 ─────────────────────────────────────────────────────────────
    if upper == "SELECT 1" {
        return Some((vec!["1".into()], vec![vec![Some("1".into())]]));
    }

    // ── DO 1 ─────────────────────────────────────────────────────────────────
    if upper.starts_with("DO ") {
        return Some((vec![], vec![]));
    }

    // ── SHOW DATABASES ────────────────────────────────────────────────────────
    if upper == "SHOW DATABASES" {
        return Some((
            vec!["Database".into()],
            vec![vec![Some("kkdb".into())], vec![Some("information_schema".into())]],
        ));
    }

    // ── SHOW VARIABLES ────────────────────────────────────────────────────────
    if upper.starts_with("SHOW VARIABLES") {
        return Some((
            vec!["Variable_name".into(), "Value".into()],
            vec![
                vec![Some("character_set_server".into()), Some("utf8mb4".into())],
                vec![Some("collation_server".into()),     Some("utf8mb4_general_ci".into())],
                vec![Some("max_allowed_packet".into()),   Some("67108864".into())],
                vec![Some("wait_timeout".into()),         Some("28800".into())],
            ],
        ));
    }

    // ── SHOW COLLATION ────────────────────────────────────────────────────────
    if upper.starts_with("SHOW COLLATION") {
        return Some((
            vec!["Collation".into(), "Charset".into(), "Id".into(),
                 "Default".into(), "Compiled".into(), "Sortlen".into()],
            vec![vec![
                Some("utf8mb4_general_ci".into()),
                Some("utf8mb4".into()),
                Some("45".into()),
                Some("Yes".into()),
                Some("Yes".into()),
                Some("1".into()),
            ]],
        ));
    }

    // ── SHOW TABLES ───────────────────────────────────────────────────────────
    if upper.starts_with("SHOW TABLES") {
        return Some((vec!["Tables_in_kkdb".into()], vec![]));
    }

    // ── SHOW TABLE STATUS ─────────────────────────────────────────────────────
    if upper.starts_with("SHOW TABLE STATUS") {
        return Some((
            vec!["Name".into(), "Engine".into(), "Rows".into(), "Comment".into()],
            vec![],
        ));
    }

    None
}



// ─── Packet format helpers ────────────────────────────────────────────────────

fn encode_lenenc_int(v: u64) -> Vec<u8> {
    if v < 251 {
        vec![v as u8]
    } else if v < 65536 {
        let mut b = vec![0xfc];
        b.extend_from_slice(&(v as u16).to_le_bytes());
        b
    } else if v < 16_777_216 {
        let mut b = vec![0xfd];
        b.push((v & 0xFF) as u8);
        b.push(((v >> 8) & 0xFF) as u8);
        b.push(((v >> 16) & 0xFF) as u8);
        b
    } else {
        let mut b = vec![0xfe];
        b.extend_from_slice(&v.to_le_bytes());
        b
    }
}

fn encode_lenenc_str(s: &str) -> Vec<u8> {
    let mut b = encode_lenenc_int(s.len() as u64);
    b.extend_from_slice(s.as_bytes());
    b
}

fn column_def_packet(name: &str) -> Vec<u8> {
    let mut p = Vec::new();
    // catalog = def
    p.extend(encode_lenenc_str("def"));
    // schema, table, org_table, name, org_name
    p.extend(encode_lenenc_str(""));
    p.extend(encode_lenenc_str(""));
    p.extend(encode_lenenc_str(""));
    p.extend(encode_lenenc_str(name));
    p.extend(encode_lenenc_str(name));
    // fixed-length fields length = 0x0c
    p.push(0x0c);
    p.extend_from_slice(&[0x21u8, 0]); // charset: utf8
    p.extend_from_slice(&[0xff, 0xff, 0, 0]); // column length
    p.push(MYSQL_TYPE_VAR_STRING); // type
    p.extend_from_slice(&[0u8, 0]); // flags
    p.push(0); // decimals
    p.extend_from_slice(&[0u8, 0]); // filler
    p
}
