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
//! | COM byte | Name              | Description                    |
//! |----------|-------------------|--------------------------------|
//! | 0x01     | COM_QUIT          | Disconnect                     |
//! | 0x02     | COM_INIT_DB       | USE database                   |
//! | 0x03     | COM_QUERY         | Execute SQL                    |
//! | 0x04     | COM_FIELD_LIST    | List table columns             |
//! | 0x09     | COM_STATISTICS    | Server statistics string       |
//! | 0x0e     | COM_PING          | Keepalive                      |
//! | 0x16     | COM_STMT_PREPARE  | Prepare a statement            |
//! | 0x17     | COM_STMT_EXECUTE  | Execute a prepared statement   |
//! | 0x19     | COM_STMT_CLOSE    | Close a prepared statement     |
//! | 0x1f     | COM_RESET_CONNECTION | Reset session state          |
//!
//! ## Packet format
//!
//! `[3 bytes LE: payload_len][1 byte: seq][payload_bytes]`

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use rand::RngCore;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::server::http_api::AppState;

// ─── Capability flags (subset we advertise) ───────────────────────────────────

const CLIENT_PROTOCOL_41: u32 = 1 << 9;
const CLIENT_SECURE_CONNECTION: u32 = 1 << 15;
const CLIENT_PLUGIN_AUTH: u32 = 1 << 19;
const CLIENT_CONNECT_WITH_DB: u32 = 1 << 3;
const CLIENT_LONG_FLAG: u32 = 1 << 2;
const CLIENT_TRANSACTIONS: u32 = 1 << 13;

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
const COM_QUIT: u8 = 0x01;
const COM_INIT_DB: u8 = 0x02;
const COM_QUERY: u8 = 0x03;
const COM_FIELD_LIST: u8 = 0x04;
const COM_STATISTICS: u8 = 0x09;
const COM_PING: u8 = 0x0e;
const COM_STMT_PREPARE: u8 = 0x16;
const COM_STMT_EXECUTE: u8 = 0x17;
const COM_STMT_CLOSE: u8 = 0x19;
const COM_RESET_CONNECTION: u8 = 0x1f;

// ─── Column type constants (MySQL text protocol) ──────────────────────────────
const MYSQL_TYPE_LONGLONG: u8 = 0x08; // BIGINT / INT
const MYSQL_TYPE_DOUBLE: u8 = 0x05;   // DOUBLE / REAL
const MYSQL_TYPE_BLOB: u8 = 0xfc;     // BLOB
const MYSQL_TYPE_NULL: u8 = 0x06;     // NULL
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
    if s.len() != 40 {
        return None;
    }
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
    client_response: &[u8],       // exactly 20 bytes from client
    stored_double_sha1_hex: &str, // hex(SHA1(SHA1(pwd))) stored in users table
) -> bool {
    if client_response.len() != 20 {
        return false;
    }
    let stored = match hex_decode_20(stored_double_sha1_hex) {
        Some(b) => b,
        None => return false,
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
        let mut vm = app_state.auth_vm.lock().unwrap_or_else(|e| e.into_inner());
        let _ = vm
            .execute_sql("ALTER TABLE kkdb_auth_users ADD COLUMN mysql_auth_hash TEXT DEFAULT ''");
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
    /// Prepared statements: stmt_id -> SQL text.
    stmts: HashMap<u32, String>,
    /// Next available prepared statement ID.
    next_stmt_id: u32,
}

impl Conn {
    fn new(stream: TcpStream, app: Arc<AppState>) -> Self {
        let mut scramble = [0u8; 20];
        rand::thread_rng().fill_bytes(&mut scramble);
        Self {
            stream,
            seq: 0,
            app,
            user: String::new(),
            selected_db: String::new(),
            scramble,
            stmts: HashMap::new(),
            next_stmt_id: 1,
        }
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
        p.extend_from_slice(part1); // scramble part 1 (8 bytes)
        p.push(0); // filler
        p.extend_from_slice(&(SERVER_CAPS as u16).to_le_bytes()); // caps low
        p.push(33); // charset: utf8mb4
        p.extend_from_slice(&SERVER_STATUS_AUTOCOMMIT.to_le_bytes());
        p.extend_from_slice(&((SERVER_CAPS >> 16) as u16).to_le_bytes()); // caps high
        p.push(21); // auth plugin data length (8 + 12 + 1 null)
        p.extend_from_slice(&[0u8; 10]); // reserved
        p.extend_from_slice(part2); // scramble part 2 (12 bytes)
        p.push(0); // null terminator
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
        vec![
            0xfe,
            0,
            0,
            SERVER_STATUS_AUTOCOMMIT as u8,
            (SERVER_STATUS_AUTOCOMMIT >> 8) as u8,
        ]
    }

    // ── Handshake ─────────────────────────────────────────────────────────────

    async fn do_handshake(&mut self) -> io::Result<()> {
        // Server greeting
        let greeting = self.handshake_v10();
        self.send_packet(&greeting).await?;

        // Client HandshakeResponse
        let pkt = self.read_packet().await?;
        if pkt.len() < 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short handshake",
            ));
        }

        // Parse: capabilities(4) + max_packet(4) + charset(1) + reserved(23) = 32 bytes
        let mut pos = 32usize;

        // username (null-terminated)
        let username_end = pkt[pos..]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(pkt.len() - pos);
        self.user = String::from_utf8_lossy(&pkt[pos..pos + username_end]).into_owned();
        pos += username_end + 1;

        // S8 fix: validate username format to prevent path traversal via the MySQL protocol.
        // Only allow characters safe for use as directory names / user identifiers.
        let user_safe = self
            .user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '@' || c == '.')
            && !self.user.contains("..")
            && !self.user.starts_with('.');
        if !user_safe && !self.user.is_empty() {
            let err = self.err_packet(
                1045,
                &format!(
                    "Access denied: invalid characters in username '{}'",
                    self.user
                ),
            );
            self.send_packet(&err).await?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "invalid username format",
            ));
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
            let db_end = pkt[pos..]
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(pkt.len() - pos);
            self.selected_db = String::from_utf8_lossy(&pkt[pos..pos + db_end]).into_owned();
        }

        // Authenticate
        let auth_ok = self.authenticate(&auth_response);
        if auth_ok {
            let ok = self.ok_packet(0, 0);
            self.send_packet(&ok).await
        } else {
            let err = self.err_packet(
                1045,
                &format!(
                    "Access denied for user '{}' (using password: YES)",
                    self.user
                ),
            );
            self.send_packet(&err).await?;
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "auth failed",
            ))
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
            let mut vm = vm_arc.lock().unwrap_or_else(|e| e.into_inner());

            // Note: the mysql_auth_hash column migration is performed once at server start
            // in serve_mysql() — no DDL needed here (I35 fix).
            let sql = format!(
                "SELECT mysql_auth_hash FROM kkdb_auth_users WHERE email = '{}' LIMIT 1",
                user.replace('\'', "''")
            );
            match vm.execute_sql(&sql) {
                Ok(crate::vm::execute::ExecResult::QueryResult { rows, .. }) => rows
                    .into_iter()
                    .next()
                    .and_then(|row| row.into_iter().next())
                    .map(|v| format!("{v}"))
                    .filter(|s| !s.is_empty() && s != "NULL"),
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
                eprintln!(
                    "[MySQL] WARN: user '{}' not found or has no mysql_auth_hash — denying access",
                    user
                );
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
            Ok(SqlResult::Rows { columns, rows }) => self.send_result_set(columns, rows).await,
            Ok(SqlResult::Ok {
                affected,
                last_insert_id,
            }) => {
                let pkt = self.ok_packet(affected, last_insert_id);
                self.send_packet(&pkt).await
            }
            Err(e) => {
                let pkt = self.err_packet(1064, &e.to_string());
                self.send_packet(&pkt).await
            }
        }
    }

    // ── COM_FIELD_LIST — list columns for a table ───────────────────────────

    async fn handle_field_list(&mut self, payload: &[u8]) -> io::Result<()> {
        // payload = table_name\0 [wildcard]
        let table = String::from_utf8_lossy(payload)
            .split('\0')
            .next()
            .unwrap_or("")
            .to_string();
        if table.is_empty() {
            let err = self.err_packet(1146, "No table specified");
            return self.send_packet(&err).await;
        }

        // Query schema from VM using PRAGMA or SELECT
        let sql = format!(
            "SELECT name, type FROM pragma_table_info('{}')",
            table.replace('\'', "''")
        );
        let result = {
            let user_id = self.user.clone();
            let selected_db = self.selected_db.clone();
            execute_sql_for_user(&self.app, &user_id, &selected_db, &sql)
        };

        match result {
            Ok(SqlResult::Rows { rows, .. }) => {
                for row in &rows {
                    let col_name = row.first().and_then(|v| v.as_deref()).unwrap_or("?");
                    let col_type = row.get(1).and_then(|v| v.as_deref()).unwrap_or("TEXT");
                    let mysql_type = sql_type_to_mysql(col_type);
                    self.send_packet(&column_def_packet_typed(col_name, &table, mysql_type))
                        .await?;
                }
                let eof = self.eof_packet();
                self.send_packet(&eof).await
            }
            _ => {
                let err = self.err_packet(
                    1146,
                    &format!("Table '{}' doesn't exist", table),
                );
                self.send_packet(&err).await
            }
        }
    }

    // ── COM_STATISTICS — server statistics string ────────────────────────────

    async fn handle_statistics(&mut self) -> io::Result<()> {
        // COM_STATISTICS response is NOT length-encoded — it's a raw string
        // prefixed only by the standard packet header (no 0x00 marker).
        let uptime = MYSQL_CONN_ID.load(Ordering::Relaxed);
        let stats = format!(
            "Uptime: {}  Threads: 1  Questions: 0  Slow queries: 0  \
             Opens: 0  Flush tables: 0  Open tables: 0  \
             Queries per second avg: 0.000",
            uptime
        );
        self.send_packet(stats.as_bytes()).await
    }

    // ── COM_STMT_PREPARE — prepare a statement ──────────────────────────────

    async fn handle_stmt_prepare(&mut self, sql: &str) -> io::Result<()> {
        let stmt_id = self.next_stmt_id;
        self.next_stmt_id += 1;
        self.stmts.insert(stmt_id, sql.to_string());

        // Count '?' placeholders for num_params
        let num_params = sql.chars().filter(|&c| c == '?').count() as u16;

        // COM_STMT_PREPARE_OK response:
        // status(1=0x00) + stmt_id(4) + num_columns(2) + num_params(2) + filler(1) + warning_count(2)
        let mut pkt = Vec::with_capacity(12);
        pkt.push(0x00); // OK status
        pkt.extend_from_slice(&stmt_id.to_le_bytes()); // statement_id
        pkt.extend_from_slice(&0u16.to_le_bytes()); // num_columns (determined at execute time)
        pkt.extend_from_slice(&num_params.to_le_bytes()); // num_params
        pkt.push(0x00); // filler
        pkt.extend_from_slice(&0u16.to_le_bytes()); // warning_count
        self.send_packet(&pkt).await?;

        // Send parameter definition packets if num_params > 0
        if num_params > 0 {
            for i in 0..num_params {
                let param_name = format!("param_{}", i);
                self.send_packet(&column_def_packet_typed(&param_name, "", MYSQL_TYPE_VAR_STRING))
                    .await?;
            }
            let eof = self.eof_packet();
            self.send_packet(&eof).await?;
        }

        Ok(())
    }

    // ── COM_STMT_EXECUTE — execute a prepared statement ─────────────────────

    async fn handle_stmt_execute(&mut self, payload: &[u8]) -> io::Result<()> {
        if payload.len() < 4 {
            let err = self.err_packet(1243, "Invalid stmt execute packet");
            return self.send_packet(&err).await;
        }
        let stmt_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);

        let sql = match self.stmts.get(&stmt_id) {
            Some(s) => s.clone(),
            None => {
                let err =
                    self.err_packet(1243, &format!("Unknown prepared statement id: {}", stmt_id));
                return self.send_packet(&err).await;
            }
        };

        // For now, execute the SQL as-is (no parameter substitution).
        // This covers SELECT, INSERT, UPDATE, DELETE without bind params.
        self.handle_query(&sql).await
    }

    // ── Main command loop ─────────────────────────────────────────────────────

    /// After successful auth, inject session variables into the user VM so that
    /// RLS policies (using `current_user()`, `auth.uid()`) work correctly.
    fn inject_session_context(&self) {
        let user = self.user.trim().to_string();
        if user.is_empty() {
            return;
        }

        // Look up the user's UUID (for auth.uid())
        let user_id = {
            let mut vm = self.app.auth_vm.lock().unwrap_or_else(|e| e.into_inner());
            let sql = format!(
                "SELECT id FROM kkdb_auth_users WHERE email = '{}' LIMIT 1",
                user.replace('\'', "''")
            );
            match vm.execute_sql(&sql) {
                Ok(crate::vm::execute::ExecResult::QueryResult { rows, .. }) => rows
                    .into_iter()
                    .next()
                    .and_then(|row| row.into_iter().next())
                    .map(|v| format!("{v}"))
                    .unwrap_or_else(|| user.clone()),
                _ => user.clone(),
            }
        };

        // Inject into the user's own VM
        let vm_arc = {
            let key = if user == "root" || user == "admin" {
                "root".to_string()
            } else {
                user.clone()
            };
            let cache = self.app.user_vms.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(vm) = cache.get(&key) {
                Arc::clone(vm)
            } else {
                return; // VM not yet created — context will be set on first query
            }
        };

        let mut vm = vm_arc.lock().unwrap_or_else(|e| e.into_inner());
        let _ = vm.execute_sql(&format!(
            "SET kkdb.current_user = '{}'",
            user.replace('\'', "''")
        ));
        let _ = vm.execute_sql(&format!(
            "SET request.jwt.sub = '{}'",
            user_id.replace('\'', "''")
        ));
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
                    let db = String::from_utf8_lossy(&pkt[1..])
                        .trim_end_matches('\0')
                        .to_string();
                    self.selected_db = db;
                    let ok = self.ok_packet(0, 0);
                    self.send_packet(&ok).await?;
                }
                COM_FIELD_LIST => {
                    self.handle_field_list(&pkt[1..]).await?;
                }
                COM_STATISTICS => {
                    self.handle_statistics().await?;
                }
                COM_QUERY => {
                    let sql = String::from_utf8_lossy(&pkt[1..]).to_string();
                    self.handle_query(&sql).await?;
                }
                COM_STMT_PREPARE => {
                    let sql = String::from_utf8_lossy(&pkt[1..]).to_string();
                    self.handle_stmt_prepare(&sql).await?;
                }
                COM_STMT_EXECUTE => {
                    self.handle_stmt_execute(&pkt[1..]).await?;
                }
                COM_STMT_CLOSE => {
                    if pkt.len() >= 5 {
                        let stmt_id = u32::from_le_bytes([pkt[1], pkt[2], pkt[3], pkt[4]]);
                        self.stmts.remove(&stmt_id);
                    }
                    // COM_STMT_CLOSE has no response
                }
                COM_RESET_CONNECTION => {
                    self.stmts.clear();
                    self.next_stmt_id = 1;
                    self.selected_db.clear();
                    let ok = self.ok_packet(0, 0);
                    self.send_packet(&ok).await?;
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
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<Option<String>>>,
    },
    Ok {
        affected: u64,
        last_insert_id: u64,
    },
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
        let mut cache = app.user_vms.lock().unwrap_or_else(|e| e.into_inner());
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

    let mut vm = vm_arc.lock().unwrap_or_else(|e| e.into_inner());
    match vm.execute_sql(sql) {
        Ok(ExecResult::QueryResult { columns, rows }) => {
            // rows: Vec<Vec<crate::vm::execute::Value>> — convert to Option<String>
            let col_names: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
            let out_rows: Vec<Vec<Option<String>>> = rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|cell| Some(format!("{cell}")))
                        .collect()
                })
                .collect();
            Ok(SqlResult::Rows {
                columns: col_names,
                rows: out_rows,
            })
        }
        Ok(ExecResult::Ok { .. }) => Ok(SqlResult::Ok {
            affected: 0,
            last_insert_id: 0,
        }),
        Ok(ExecResult::RowsAffected { count, .. }) => Ok(SqlResult::Ok {
            affected: count as u64,
            last_insert_id: 0,
        }),
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

type IntrospectionResult = (Vec<String>, Vec<Vec<Option<String>>>);

/// Handle MySQL client "magic" queries that many clients send on connect.
/// Returns `Some((columns, rows))` when intercepted, `None` to fall through.
fn handle_client_introspection(sql: &str) -> Option<IntrospectionResult> {
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
            "MAX_ALLOWED_PACKET" => "67108864",
            "TIME_ZONE" => "SYSTEM",
            "SYSTEM_TIME_ZONE" => "UTC",
            "SQL_MODE" => "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES",
            "AUTOCOMMIT" => "1",
            "TRANSACTION_ISOLATION" | "TX_ISOLATION" => "REPEATABLE-READ",
            "TRANSACTION_READ_ONLY" => "0",
            "LOWER_CASE_TABLE_NAMES" => "0",
            "WAIT_TIMEOUT" | "INTERACTIVE_TIMEOUT" => "28800",
            "NET_WRITE_TIMEOUT" | "NET_READ_TIMEOUT" => "60",
            "CHARACTER_SET_SERVER" | "CHARACTER_SET_CLIENT" | "CHARACTER_SET_RESULTS" => "utf8mb4",
            "COLLATION_SERVER" => "utf8mb4_general_ci",
            "VERSION" => "8.0.33-kkdb",
            "VERSION_COMMENT" => "KKDB MySQL Compatible",
            _ => "",
        };
        return Some((vec![col_name], vec![vec![Some(val.into())]]));
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
        return Some((vec!["DATABASE()".into()], vec![vec![Some("kkdb".into())]]));
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
            vec![
                vec![Some("kkdb".into())],
                vec![Some("information_schema".into())],
            ],
        ));
    }

    // ── SHOW VARIABLES ────────────────────────────────────────────────────────
    if upper.starts_with("SHOW VARIABLES") {
        return Some((
            vec!["Variable_name".into(), "Value".into()],
            vec![
                vec![Some("character_set_server".into()), Some("utf8mb4".into())],
                vec![
                    Some("collation_server".into()),
                    Some("utf8mb4_general_ci".into()),
                ],
                vec![Some("max_allowed_packet".into()), Some("67108864".into())],
                vec![Some("wait_timeout".into()), Some("28800".into())],
            ],
        ));
    }

    // ── SHOW COLLATION ────────────────────────────────────────────────────────
    if upper.starts_with("SHOW COLLATION") {
        return Some((
            vec![
                "Collation".into(),
                "Charset".into(),
                "Id".into(),
                "Default".into(),
                "Compiled".into(),
                "Sortlen".into(),
            ],
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
            vec![
                "Name".into(),
                "Engine".into(),
                "Rows".into(),
                "Comment".into(),
            ],
            vec![],
        ));
    }

    // ── SHOW WARNINGS ─────────────────────────────────────────────────────────
    if upper.starts_with("SHOW WARNINGS") {
        return Some((
            vec!["Level".into(), "Code".into(), "Message".into()],
            vec![],
        ));
    }

    // ── SHOW ERRORS ───────────────────────────────────────────────────────────
    if upper.starts_with("SHOW ERRORS") {
        return Some((
            vec!["Level".into(), "Code".into(), "Message".into()],
            vec![],
        ));
    }

    // ── SHOW ENGINES ──────────────────────────────────────────────────────────
    if upper.starts_with("SHOW ENGINES") {
        return Some((
            vec![
                "Engine".into(),
                "Support".into(),
                "Comment".into(),
                "Transactions".into(),
                "XA".into(),
                "Savepoints".into(),
            ],
            vec![vec![
                Some("KKDB".into()),
                Some("DEFAULT".into()),
                Some("KKDB COW B-Tree storage engine".into()),
                Some("YES".into()),
                Some("NO".into()),
                Some("NO".into()),
            ]],
        ));
    }

    // ── SHOW CHARACTER SET ────────────────────────────────────────────────────
    if upper.starts_with("SHOW CHARACTER SET") || upper.starts_with("SHOW CHARSET") {
        return Some((
            vec![
                "Charset".into(),
                "Description".into(),
                "Default collation".into(),
                "Maxlen".into(),
            ],
            vec![vec![
                Some("utf8mb4".into()),
                Some("UTF-8 Unicode".into()),
                Some("utf8mb4_general_ci".into()),
                Some("4".into()),
            ]],
        ));
    }

    // ── SHOW PROCESSLIST ──────────────────────────────────────────────────────
    if upper.starts_with("SHOW PROCESSLIST") || upper.starts_with("SHOW FULL PROCESSLIST") {
        return Some((
            vec![
                "Id".into(),
                "User".into(),
                "Host".into(),
                "db".into(),
                "Command".into(),
                "Time".into(),
                "State".into(),
                "Info".into(),
            ],
            vec![vec![
                Some("1".into()),
                Some("root".into()),
                Some("localhost".into()),
                Some("kkdb".into()),
                Some("Query".into()),
                Some("0".into()),
                Some("executing".into()),
                Some("SHOW PROCESSLIST".into()),
            ]],
        ));
    }

    // ── SHOW STATUS ───────────────────────────────────────────────────────────
    if upper.starts_with("SHOW STATUS") || upper.starts_with("SHOW GLOBAL STATUS") || upper.starts_with("SHOW SESSION STATUS") {
        return Some((
            vec!["Variable_name".into(), "Value".into()],
            vec![
                vec![Some("Uptime".into()), Some("1".into())],
                vec![Some("Threads_connected".into()), Some("1".into())],
                vec![Some("Questions".into()), Some("0".into())],
                vec![Some("Slow_queries".into()), Some("0".into())],
                vec![Some("Com_select".into()), Some("0".into())],
                vec![Some("Com_insert".into()), Some("0".into())],
                vec![Some("Com_update".into()), Some("0".into())],
                vec![Some("Com_delete".into()), Some("0".into())],
            ],
        ));
    }

    // ── SHOW GLOBAL VARIABLES ─────────────────────────────────────────────────
    if upper.starts_with("SHOW GLOBAL VARIABLES") {
        return Some((
            vec!["Variable_name".into(), "Value".into()],
            vec![
                vec![Some("version".into()), Some("8.0.33-kkdb".into())],
                vec![Some("version_comment".into()), Some("KKDB MySQL Compatible".into())],
                vec![Some("character_set_server".into()), Some("utf8mb4".into())],
                vec![Some("collation_server".into()), Some("utf8mb4_general_ci".into())],
                vec![Some("max_allowed_packet".into()), Some("67108864".into())],
                vec![Some("max_connections".into()), Some("100".into())],
                vec![Some("wait_timeout".into()), Some("28800".into())],
                vec![Some("innodb_version".into()), Some("8.0.33".into())],
            ],
        ));
    }

    // ── SHOW COLUMNS FROM / DESCRIBE / DESC / EXPLAIN (table) ───────────────
    // These are intercepted at the introspection level and return static metadata.
    // The full VM-based version is handled by COM_FIELD_LIST.
    if upper.starts_with("SHOW COLUMNS FROM ")
        || upper.starts_with("SHOW FULL COLUMNS FROM ")
        || upper.starts_with("DESCRIBE ")
        || upper.starts_with("DESC ")
    {
        // We can't query the VM here (no &self), so return the standard column headers.
        // The actual data will come from COM_QUERY → handle_query fallthrough to VM.
        return None; // fall through to VM execution
    }

    // ── SHOW CREATE TABLE ────────────────────────────────────────────────────
    if upper.starts_with("SHOW CREATE TABLE ") {
        // Fall through to VM which may handle it
        return None;
    }

    // ── SHOW INDEX FROM / SHOW KEYS FROM ──────────────────────────────────────
    if upper.starts_with("SHOW INDEX FROM ") || upper.starts_with("SHOW KEYS FROM ") {
        return Some((
            vec![
                "Table".into(),
                "Non_unique".into(),
                "Key_name".into(),
                "Seq_in_index".into(),
                "Column_name".into(),
                "Collation".into(),
                "Cardinality".into(),
                "Index_type".into(),
            ],
            vec![],
        ));
    }

    // ── SHOW GRANTS ───────────────────────────────────────────────────────────
    if upper.starts_with("SHOW GRANTS") {
        return Some((
            vec!["Grants for root@localhost".into()],
            vec![vec![Some(
                "GRANT ALL PRIVILEGES ON *.* TO 'root'@'localhost'".into(),
            )]],
        ));
    }

    // ── KILL query ────────────────────────────────────────────────────────────
    if upper.starts_with("KILL ") {
        return Some((vec![], vec![])); // no-op OK
    }

    // ── information_schema queries (common MySQL client introspection) ──────
    if upper.contains("INFORMATION_SCHEMA.SCHEMATA") {
        return Some((
            vec!["CATALOG_NAME".into(), "SCHEMA_NAME".into(), "DEFAULT_CHARACTER_SET_NAME".into(), "DEFAULT_COLLATION_NAME".into()],
            vec![
                vec![Some("def".into()), Some("kkdb".into()), Some("utf8mb4".into()), Some("utf8mb4_general_ci".into())],
                vec![Some("def".into()), Some("information_schema".into()), Some("utf8mb4".into()), Some("utf8mb4_general_ci".into())],
            ],
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
    column_def_packet_typed(name, "", MYSQL_TYPE_VAR_STRING)
}

/// Build a column definition packet with a specific MySQL column type and table name.
fn column_def_packet_typed(name: &str, table: &str, col_type: u8) -> Vec<u8> {
    let mut p = Vec::new();
    // catalog = def
    p.extend(encode_lenenc_str("def"));
    // schema, table, org_table, name, org_name
    p.extend(encode_lenenc_str(""));
    p.extend(encode_lenenc_str(table));
    p.extend(encode_lenenc_str(table));
    p.extend(encode_lenenc_str(name));
    p.extend(encode_lenenc_str(name));
    // fixed-length fields length = 0x0c
    p.push(0x0c);
    p.extend_from_slice(&[0x21u8, 0]); // charset: utf8
    p.extend_from_slice(&[0xff, 0xff, 0, 0]); // column length
    p.push(col_type); // type
    p.extend_from_slice(&[0u8, 0]); // flags
    p.push(0); // decimals
    p.extend_from_slice(&[0u8, 0]); // filler
    p
}

/// Map SQL type name to MySQL wire protocol column type constant.
fn sql_type_to_mysql(type_name: &str) -> u8 {
    let upper = type_name.to_uppercase();
    if upper.contains("INT") || upper.contains("SERIAL") || upper.contains("BOOL") {
        MYSQL_TYPE_LONGLONG
    } else if upper.contains("REAL")
        || upper.contains("FLOAT")
        || upper.contains("DOUBLE")
        || upper.contains("NUMERIC")
        || upper.contains("DECIMAL")
    {
        MYSQL_TYPE_DOUBLE
    } else if upper.contains("BLOB") {
        MYSQL_TYPE_BLOB
    } else {
        MYSQL_TYPE_VAR_STRING
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Packet encoding tests ──────────────────────────────────────────────

    #[test]
    fn test_encode_lenenc_int_small() {
        assert_eq!(encode_lenenc_int(0), vec![0]);
        assert_eq!(encode_lenenc_int(250), vec![250]);
    }

    #[test]
    fn test_encode_lenenc_int_medium() {
        let v = encode_lenenc_int(300);
        assert_eq!(v[0], 0xfc);
        let val = u16::from_le_bytes([v[1], v[2]]);
        assert_eq!(val, 300);
    }

    #[test]
    fn test_encode_lenenc_int_large() {
        let v = encode_lenenc_int(70000);
        assert_eq!(v[0], 0xfd);
        let val = (v[1] as u64) | ((v[2] as u64) << 8) | ((v[3] as u64) << 16);
        assert_eq!(val, 70000);
    }

    #[test]
    fn test_encode_lenenc_int_very_large() {
        let v = encode_lenenc_int(17_000_000);
        assert_eq!(v[0], 0xfe);
        let val = u64::from_le_bytes([v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]]);
        assert_eq!(val, 17_000_000);
    }

    #[test]
    fn test_encode_lenenc_str() {
        let v = encode_lenenc_str("hello");
        assert_eq!(v[0], 5); // length
        assert_eq!(&v[1..], b"hello");
    }

    #[test]
    fn test_encode_lenenc_str_empty() {
        let v = encode_lenenc_str("");
        assert_eq!(v, vec![0]);
    }

    // ── Column type mapping tests ──────────────────────────────────────────

    #[test]
    fn test_sql_type_to_mysql_integer() {
        assert_eq!(sql_type_to_mysql("INTEGER"), MYSQL_TYPE_LONGLONG);
        assert_eq!(sql_type_to_mysql("INT"), MYSQL_TYPE_LONGLONG);
        assert_eq!(sql_type_to_mysql("BIGINT"), MYSQL_TYPE_LONGLONG);
        assert_eq!(sql_type_to_mysql("SMALLINT"), MYSQL_TYPE_LONGLONG);
        assert_eq!(sql_type_to_mysql("SERIAL"), MYSQL_TYPE_LONGLONG);
        assert_eq!(sql_type_to_mysql("BOOLEAN"), MYSQL_TYPE_LONGLONG);
    }

    #[test]
    fn test_sql_type_to_mysql_real() {
        assert_eq!(sql_type_to_mysql("REAL"), MYSQL_TYPE_DOUBLE);
        assert_eq!(sql_type_to_mysql("FLOAT"), MYSQL_TYPE_DOUBLE);
        assert_eq!(sql_type_to_mysql("DOUBLE"), MYSQL_TYPE_DOUBLE);
        assert_eq!(sql_type_to_mysql("NUMERIC"), MYSQL_TYPE_DOUBLE);
        assert_eq!(sql_type_to_mysql("DECIMAL(10,2)"), MYSQL_TYPE_DOUBLE);
    }

    #[test]
    fn test_sql_type_to_mysql_blob() {
        assert_eq!(sql_type_to_mysql("BLOB"), MYSQL_TYPE_BLOB);
    }

    #[test]
    fn test_sql_type_to_mysql_text() {
        assert_eq!(sql_type_to_mysql("TEXT"), MYSQL_TYPE_VAR_STRING);
        assert_eq!(sql_type_to_mysql("VARCHAR(255)"), MYSQL_TYPE_VAR_STRING);
        assert_eq!(sql_type_to_mysql("CHAR(10)"), MYSQL_TYPE_VAR_STRING);
    }

    // ── Column def packet tests ────────────────────────────────────────────

    #[test]
    fn test_column_def_packet_default() {
        let pkt = column_def_packet("name");
        // Should contain "def" catalog and column name
        assert!(pkt.len() > 20);
        // The packet should contain the column type byte (MYSQL_TYPE_VAR_STRING)
        assert!(pkt.contains(&MYSQL_TYPE_VAR_STRING));
    }

    #[test]
    fn test_column_def_packet_typed() {
        let pkt = column_def_packet_typed("id", "users", MYSQL_TYPE_LONGLONG);
        assert!(pkt.len() > 20);
        // Should contain the table name and column name
        let s = String::from_utf8_lossy(&pkt);
        assert!(s.contains("users"));
        assert!(s.contains("id"));
    }

    // ── SHA-1 / auth helpers ───────────────────────────────────────────────

    #[test]
    fn test_sha1_basic() {
        let hash = sha1(b"hello");
        assert_eq!(hash.len(), 20);
        assert_eq!(
            hex_encode(&hash),
            "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"
        );
    }

    #[test]
    fn test_hex_encode_roundtrip() {
        let data: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];
        assert_eq!(hex_encode(&data), "deadbeef");
    }

    #[test]
    fn test_hex_decode_20_valid() {
        let hex = "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d";
        let decoded = hex_decode_20(hex).unwrap();
        assert_eq!(hex_encode(&decoded), hex);
    }

    #[test]
    fn test_hex_decode_20_invalid_length() {
        assert!(hex_decode_20("abcd").is_none());
    }

    #[test]
    fn test_hex_decode_20_invalid_chars() {
        let bad = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
        assert!(hex_decode_20(bad).is_none());
    }

    #[test]
    fn test_mysql_double_sha1() {
        let hash = mysql_double_sha1("password");
        assert_eq!(hash.len(), 40);
        // Known value: SHA1(SHA1("password"))
        let sha1_pass = sha1(b"password");
        let sha1_sha1 = sha1(&sha1_pass);
        assert_eq!(hash, hex_encode(&sha1_sha1));
    }

    #[test]
    fn test_verify_native_password_correct() {
        let password = "test_password";
        let stored = mysql_double_sha1(password);

        let mut scramble = [0u8; 20];
        for i in 0..20 {
            scramble[i] = (i as u8) + 1;
        }

        // Compute what the client would send
        let sha1_pass = sha1(password.as_bytes());
        let stored_bytes = hex_decode_20(&stored).unwrap();
        let mut hash_scramble = Sha1::new();
        hash_scramble.update(&scramble);
        hash_scramble.update(&stored_bytes);
        let hash_result: [u8; 20] = hash_scramble.finalize().into();

        let mut client_response = [0u8; 20];
        for i in 0..20 {
            client_response[i] = sha1_pass[i] ^ hash_result[i];
        }

        assert!(verify_native_password(
            &scramble,
            &client_response,
            &stored
        ));
    }

    #[test]
    fn test_verify_native_password_wrong() {
        let stored = mysql_double_sha1("correct_password");
        let scramble = [42u8; 20];
        let bad_response = [0u8; 20];
        assert!(!verify_native_password(&scramble, &bad_response, &stored));
    }

    #[test]
    fn test_verify_native_password_short_response() {
        let stored = mysql_double_sha1("password");
        let scramble = [0u8; 20];
        assert!(!verify_native_password(&scramble, &[1, 2, 3], &stored));
    }

    #[test]
    fn test_verify_native_password_bad_hex() {
        let scramble = [0u8; 20];
        let response = [0u8; 20];
        assert!(!verify_native_password(&scramble, &response, "not_hex"));
    }

    // ── Introspection interceptor tests ────────────────────────────────────

    #[test]
    fn test_introspection_set() {
        let r = handle_client_introspection("SET NAMES utf8mb4");
        assert!(r.is_some());
        let (cols, rows) = r.unwrap();
        assert!(cols.is_empty());
        assert!(rows.is_empty());
    }

    #[test]
    fn test_introspection_select_version() {
        let r = handle_client_introspection("SELECT VERSION()").unwrap();
        assert_eq!(r.0, vec!["version()"]);
        assert_eq!(r.1[0][0], Some("8.0.33-kkdb".into()));
    }

    #[test]
    fn test_introspection_select_database() {
        let r = handle_client_introspection("SELECT DATABASE()").unwrap();
        assert_eq!(r.0, vec!["DATABASE()"]);
    }

    #[test]
    fn test_introspection_select_1() {
        let r = handle_client_introspection("SELECT 1").unwrap();
        assert_eq!(r.1[0][0], Some("1".into()));
    }

    #[test]
    fn test_introspection_show_databases() {
        let r = handle_client_introspection("SHOW DATABASES").unwrap();
        assert_eq!(r.0, vec!["Database"]);
        assert!(r.1.len() >= 2);
    }

    #[test]
    fn test_introspection_show_variables() {
        let r = handle_client_introspection("SHOW VARIABLES").unwrap();
        assert_eq!(r.0[0], "Variable_name");
        assert!(!r.1.is_empty());
    }

    #[test]
    fn test_introspection_show_collation() {
        let r = handle_client_introspection("SHOW COLLATION").unwrap();
        assert_eq!(r.0[0], "Collation");
    }

    #[test]
    fn test_introspection_show_tables() {
        let r = handle_client_introspection("SHOW TABLES").unwrap();
        assert_eq!(r.0, vec!["Tables_in_kkdb"]);
    }

    #[test]
    fn test_introspection_show_table_status() {
        let r = handle_client_introspection("SHOW TABLE STATUS").unwrap();
        assert!(r.0.contains(&"Name".to_string()));
    }

    #[test]
    fn test_introspection_select_sysvar() {
        let r = handle_client_introspection("SELECT @@max_allowed_packet").unwrap();
        assert_eq!(r.0[0], "@@max_allowed_packet");
        assert_eq!(r.1[0][0], Some("67108864".into()));
    }

    #[test]
    fn test_introspection_select_sysvar_version() {
        let r = handle_client_introspection("SELECT @@version").unwrap();
        assert_eq!(r.1[0][0], Some("8.0.33-kkdb".into()));
    }

    #[test]
    fn test_introspection_select_sysvar_autocommit() {
        let r = handle_client_introspection("SELECT @@autocommit").unwrap();
        assert_eq!(r.1[0][0], Some("1".into()));
    }

    #[test]
    fn test_introspection_do() {
        let r = handle_client_introspection("DO 1").unwrap();
        assert!(r.0.is_empty());
    }

    // ── New introspection: SHOW WARNINGS / ERRORS / ENGINES / etc. ─────────

    #[test]
    fn test_introspection_show_warnings() {
        let r = handle_client_introspection("SHOW WARNINGS").unwrap();
        assert_eq!(r.0, vec!["Level", "Code", "Message"]);
        assert!(r.1.is_empty()); // no warnings
    }

    #[test]
    fn test_introspection_show_errors() {
        let r = handle_client_introspection("SHOW ERRORS").unwrap();
        assert_eq!(r.0, vec!["Level", "Code", "Message"]);
    }

    #[test]
    fn test_introspection_show_engines() {
        let r = handle_client_introspection("SHOW ENGINES").unwrap();
        assert_eq!(r.0[0], "Engine");
        assert_eq!(r.1[0][0], Some("KKDB".into()));
        assert_eq!(r.1[0][1], Some("DEFAULT".into()));
    }

    #[test]
    fn test_introspection_show_charset() {
        let r = handle_client_introspection("SHOW CHARACTER SET").unwrap();
        assert_eq!(r.0[0], "Charset");
        assert_eq!(r.1[0][0], Some("utf8mb4".into()));

        // Also test alias
        let r2 = handle_client_introspection("SHOW CHARSET").unwrap();
        assert_eq!(r2.0[0], "Charset");
    }

    #[test]
    fn test_introspection_show_processlist() {
        let r = handle_client_introspection("SHOW PROCESSLIST").unwrap();
        assert_eq!(r.0[0], "Id");
        assert_eq!(r.0[1], "User");
        assert!(!r.1.is_empty());
    }

    #[test]
    fn test_introspection_show_full_processlist() {
        let r = handle_client_introspection("SHOW FULL PROCESSLIST").unwrap();
        assert!(r.0.contains(&"Command".to_string()));
    }

    #[test]
    fn test_introspection_show_status() {
        let r = handle_client_introspection("SHOW STATUS").unwrap();
        assert_eq!(r.0[0], "Variable_name");
        assert!(!r.1.is_empty());
    }

    #[test]
    fn test_introspection_show_global_status() {
        let r = handle_client_introspection("SHOW GLOBAL STATUS").unwrap();
        assert_eq!(r.0[0], "Variable_name");
    }

    #[test]
    fn test_introspection_show_global_variables() {
        let r = handle_client_introspection("SHOW GLOBAL VARIABLES").unwrap();
        assert_eq!(r.0[0], "Variable_name");
        // Should contain version info
        let version_row = r.1.iter().find(|row| row[0] == Some("version".into()));
        assert!(version_row.is_some());
        assert_eq!(version_row.unwrap()[1], Some("8.0.33-kkdb".into()));
    }

    #[test]
    fn test_introspection_show_index() {
        let r = handle_client_introspection("SHOW INDEX FROM users").unwrap();
        assert_eq!(r.0[0], "Table");
        assert!(r.1.is_empty()); // no index info available
    }

    #[test]
    fn test_introspection_show_keys() {
        let r = handle_client_introspection("SHOW KEYS FROM users").unwrap();
        assert_eq!(r.0[0], "Table");
    }

    #[test]
    fn test_introspection_show_grants() {
        let r = handle_client_introspection("SHOW GRANTS").unwrap();
        assert!(!r.1.is_empty());
        assert!(r.1[0][0]
            .as_ref()
            .unwrap()
            .contains("GRANT ALL PRIVILEGES"));
    }

    #[test]
    fn test_introspection_kill() {
        let r = handle_client_introspection("KILL 42").unwrap();
        assert!(r.0.is_empty()); // no-op OK
    }

    #[test]
    fn test_introspection_info_schema_schemata() {
        let r =
            handle_client_introspection("SELECT * FROM INFORMATION_SCHEMA.SCHEMATA").unwrap();
        assert_eq!(r.0[0], "CATALOG_NAME");
        assert!(r.1.len() >= 2);
    }

    #[test]
    fn test_introspection_describe_fallthrough() {
        // DESCRIBE should fall through to VM
        let r = handle_client_introspection("DESCRIBE users");
        assert!(r.is_none());
    }

    #[test]
    fn test_introspection_show_create_table_fallthrough() {
        let r = handle_client_introspection("SHOW CREATE TABLE users");
        assert!(r.is_none());
    }

    #[test]
    fn test_introspection_select_sysvar_session() {
        let r = handle_client_introspection("SELECT @@SESSION.transaction_isolation").unwrap();
        assert_eq!(r.1[0][0], Some("REPEATABLE-READ".into()));
    }

    #[test]
    fn test_introspection_normal_query_no_intercept() {
        // Normal SQL should NOT be intercepted
        let r = handle_client_introspection("SELECT * FROM users");
        assert!(r.is_none());
    }

    #[test]
    fn test_introspection_insert_no_intercept() {
        let r = handle_client_introspection("INSERT INTO users VALUES (1, 'a')");
        assert!(r.is_none());
    }

    // ── MySQL connection ID uniqueness ─────────────────────────────────────

    #[test]
    fn test_mysql_conn_id_increments() {
        let id1 = MYSQL_CONN_ID.fetch_add(1, Ordering::Relaxed);
        let id2 = MYSQL_CONN_ID.fetch_add(1, Ordering::Relaxed);
        assert!(id2 > id1);
    }
}
