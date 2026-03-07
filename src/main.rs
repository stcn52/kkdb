use kkdb::storage::btree::BTree;
use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::env;
use std::sync::{Arc, Mutex};

fn print_query_result(columns: &[String], rows: &[Vec<kkdb::types::Value>]) {
    if columns.is_empty() && rows.is_empty() {
        return;
    }

    // Calculate column widths
    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    for row in rows {
        for (i, val) in row.iter().enumerate() {
            if i < widths.len() {
                let w = format!("{}", val).len();
                if w > widths[i] {
                    widths[i] = w;
                }
            }
        }
    }

    // Print header
    let header: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{:width$}", c, width = widths[i]))
        .collect();
    println!("{}", header.join(" | "));

    // Print separator
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    println!("{}", sep.join("-+-"));

    // Print rows
    for row in rows {
        let vals: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let s = format!("{}", v);
                if i < widths.len() {
                    format!("{:width$}", s, width = widths[i])
                } else {
                    s
                }
            })
            .collect();
        println!("{}", vals.join(" | "));
    }

    println!("({} rows)", rows.len());
}

fn print_help() {
    println!("KKDB - A SQLite implementation in Rust");
    println!();
    println!("Commands:");
    println!("  .help              Show this help message");
    println!("  .quit / .exit      Exit the program");
    println!("  .tables            List all tables");
    println!("  .schema [TABLE]    Show CREATE statement for tables");
    println!("  .open FILE         Open a database file");
    println!("  .memory            Switch to in-memory database");
    println!();
    println!("SQL Statements:");
    println!("  CREATE TABLE name (col1 TYPE, col2 TYPE, ...)");
    println!("  DROP TABLE [IF EXISTS] name");
    println!("  INSERT INTO name [(cols)] VALUES (vals), ...");
    println!("  SELECT cols FROM table [WHERE ...] [ORDER BY ...] [LIMIT n]");
    println!("  UPDATE name SET col=val [WHERE ...]");
    println!("  DELETE FROM name [WHERE ...]");
    println!("  EXPLAIN statement");
}

fn handle_dot_command(vm: &mut VM, cmd: &str) -> Option<VM> {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let command = parts[0].to_lowercase();

    match command.as_str() {
        ".help" => {
            print_help();
            None
        }
        ".quit" | ".exit" => {
            std::process::exit(0);
        }
        ".tables" => {
            let mut names: Vec<&String> = vm.schema.tables.keys().collect();
            names.sort();
            for name in names {
                println!("{}", name);
            }
            None
        }
        ".schema" => {
            let filter = parts.get(1).map(|s| s.trim());
            let mut entries: Vec<(String, String)> = Vec::new();

            // Prefer persisted schema catalog so output includes exact SQL.
            let schema_root = vm.pager.schema_root_page();
            let mut btree = BTree::new(&mut vm.pager);
            match btree.scan_all(schema_root) {
                Ok(rows) => {
                    for (_rowid, row) in rows {
                        if row.len() < 5 {
                            continue;
                        }
                        let name = match &row[1] {
                            Value::Text(v) => v.to_string(),
                            _ => continue,
                        };
                        let sql = match &row[4] {
                            Value::Text(v) => v.to_string(),
                            _ => continue,
                        };
                        if let Some(f) = filter {
                            if !name.eq_ignore_ascii_case(f) {
                                continue;
                            }
                        }
                        entries.push((name, sql));
                    }
                }
                Err(_) => {
                    // Fallback: reconstruct from in-memory schema cache.
                    for (name, table) in &vm.schema.tables {
                        if let Some(f) = filter {
                            if !name.eq_ignore_ascii_case(f) {
                                continue;
                            }
                        }
                        let cols: Vec<String> = table
                            .columns
                            .iter()
                            .map(|c| {
                                let mut s = format!("{} {}", c.name, c.data_type);
                                if c.primary_key {
                                    s.push_str(" PRIMARY KEY");
                                }
                                if c.autoincrement {
                                    s.push_str(" AUTOINCREMENT");
                                }
                                if c.not_null {
                                    s.push_str(" NOT NULL");
                                }
                                if c.unique {
                                    s.push_str(" UNIQUE");
                                }
                                s
                            })
                            .collect();
                        entries.push((
                            name.clone(),
                            format!("CREATE TABLE {} ({})", name, cols.join(", ")),
                        ));
                    }
                }
            }

            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (_name, sql) in entries {
                let text = sql.trim();
                if text.ends_with(';') {
                    println!("{}", text);
                } else {
                    println!("{};", text);
                }
            }
            None
        }
        ".open" => {
            if let Some(path) = parts.get(1) {
                match VM::open(path.trim()) {
                    Ok(new_vm) => {
                        println!("Opened database: {}", path.trim());
                        Some(new_vm)
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        None
                    }
                }
            } else {
                eprintln!("Usage: .open FILENAME");
                None
            }
        }
        ".memory" => {
            println!("Switched to in-memory database");
            Some(VM::new_memory())
        }
        _ => {
            eprintln!("Unknown command: {}. Use .help for help.", command);
            None
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut vm = if args.len() > 1 {
        match VM::open(&args[1]) {
            Ok(vm) => {
                println!("KKDB v0.1.0 - opened '{}'", args[1]);
                vm
            }
            Err(e) => {
                eprintln!("Error opening '{}': {}", args[1], e);
                std::process::exit(1);
            }
        }
    } else {
        println!("KKDB v0.1.0 - in-memory database");
        VM::new_memory()
    };

    println!("Type .help for usage hints, .quit to exit.");
    println!();

    // SERVER MODE
    if args.iter().any(|arg| arg == "--server") {
        let port_str = args
            .iter()
            .skip_while(|arg| *arg != "--port")
            .nth(1)
            .cloned()
            .unwrap_or_else(|| "3306".to_string());
        let port: u16 = port_str.parse().unwrap_or(3306);

        let http_port_str = args
            .iter()
            .skip_while(|arg| *arg != "--http-port")
            .nth(1)
            .cloned()
            .unwrap_or_else(|| "6543".to_string());
        let http_port: u16 = http_port_str.parse().unwrap_or(6543);

        // New async MySQL Wire Protocol port (standard MySQL clients: DBeaver, mysql2 etc.)
        let mysql_port_str = args
            .iter()
            .skip_while(|arg| *arg != "--mysql-port")
            .nth(1)
            .cloned()
            .unwrap_or_else(|| "3307".to_string());
        let mysql_port: u16 = mysql_port_str.parse().unwrap_or(3307);

        // ── Resolve data directory: --data-dir flag > kkdb_config table > None ─
        let data_dir_cli = args
            .iter()
            .skip_while(|arg| *arg != "--data-dir")
            .nth(1)
            .cloned();

        let data_dir: Option<std::path::PathBuf> = if let Some(dir) = data_dir_cli {
            // CLI flag takes precedence
            Some(std::path::PathBuf::from(dir))
        } else {
            // Fallback: read kkdb_config table if it exists
            let config_val = vm.execute_sql(
                "SELECT value FROM kkdb_config WHERE key = 'http.data_dir'"
            ).ok().and_then(|r| match r {
                kkdb::vm::execute::ExecResult::QueryResult { rows, .. } => {
                    rows.into_iter().next()
                        .and_then(|row| row.into_iter().next())
                        .map(|v| v.to_string())
                }
                _ => None,
            });
            config_val.map(std::path::PathBuf::from)
        };

        if let Some(ref dir) = data_dir {
            println!("[KKDB] Per-user data dir: {}", dir.display());
        } else {
            println!("[KKDB] HTTP API running in in-memory mode (no --data-dir set)");
        }

        let shared_vm = Arc::new(Mutex::new(vm));

        // Start HTTP REST API (Supabase-style) in a background OS thread
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(async move {
                let state = match data_dir {
                    Some(dir) => match kkdb::server::http_api::AppState::with_dir(dir) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("[KKDB] Failed to open HTTP data dir: {e}");
                            std::process::exit(1);
                        }
                    },
                    None => kkdb::server::http_api::AppState::in_memory(),
                };
                let router = kkdb::server::http_api::build_router(state);
                let addr = format!("0.0.0.0:{}", http_port);
                let listener = tokio::net::TcpListener::bind(&addr).await
                    .expect("Failed to bind HTTP API port");
                println!("[KKDB] HTTP API  listening on http://{}", addr);
                axum::serve(listener, router).await
                    .expect("HTTP API server error");
            });
        });

        // Start async MySQL Wire Protocol server in a background OS thread
        let mysql_data_dir = args
            .iter()
            .skip_while(|arg| *arg != "--data-dir")
            .nth(1)
            .cloned()
            .map(std::path::PathBuf::from);
        let _ = Arc::clone(&shared_vm); // keep shared_vm alive
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime (mysql)");
            rt.block_on(async move {
                let state = match mysql_data_dir {
                    Some(dir) => match kkdb::server::http_api::AppState::with_dir(dir) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("[MySQL] Failed to open data dir: {e}");
                            return;
                        }
                    },
                    None => kkdb::server::http_api::AppState::in_memory(),
                };
                let addr = format!("0.0.0.0:{mysql_port}");
                if let Err(e) = kkdb::server::mysql::serve_mysql(&addr, state).await {
                    eprintln!("[MySQL] Server error: {e}");
                }
            });
        });

        println!("[KKDB] MySQL API listening on 0.0.0.0:{}", port);

        // ── Optional Raft cluster mode ──────────────────────────────────────
        // Flags: --node-id <u64>  --raft-addr <host:port>  --peers <id=host:port,...>
        let node_id: Option<u64> = args
            .iter()
            .skip_while(|a| *a != "--node-id")
            .nth(1)
            .and_then(|s| s.parse().ok());

        let raft_addr_str: Option<String> = args
            .iter()
            .skip_while(|a| *a != "--raft-addr")
            .nth(1)
            .cloned();

        if let (Some(nid), Some(raft_addr_str)) = (node_id, raft_addr_str) {
            // Parse peers: "2=127.0.0.1:7002,3=127.0.0.1:7003"
            let peers_raw: Option<String> = args
                .iter()
                .skip_while(|a| *a != "--peers")
                .nth(1)
                .cloned();

            let peer_addrs: std::collections::BTreeMap<u64, String> = peers_raw
                .unwrap_or_default()
                .split(',')
                .filter_map(|s| {
                    let mut parts = s.splitn(2, '=');
                    let id: u64 = parts.next()?.trim().parse().ok()?;
                    let addr = format!("http://{}", parts.next()?.trim());
                    Some((id, addr))
                })
                .collect();

            let self_url = format!("http://{}", raft_addr_str);
            let raft_socket: std::net::SocketAddr = raft_addr_str
                .parse()
                .expect("invalid --raft-addr");

            // Build data dir for this raft node (reuse data_dir logic above)
            let data_dir_raft = args
                .iter()
                .skip_while(|a| *a != "--data-dir")
                .nth(1)
                .cloned()
                .map(std::path::PathBuf::from);

            // WAL dir = same as data dir (will create {data_dir}/raft/ subdirectory)
            let wal_dir = data_dir_raft.clone();

            let raft_state = match data_dir_raft {
                Some(dir) => match kkdb::server::http_api::AppState::with_dir(dir) {
                    Ok(s) => s,
                    Err(e) => { eprintln!("[Raft] Failed to open data dir: {e}"); std::process::exit(1); }
                },
                None => kkdb::server::http_api::AppState::in_memory(),
            };

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async move {
                    let binlog = wal_dir.as_ref().map(|dir| {
                        let inner = dir.join("binlog.kkdb");
                        let mgr = kkdb::binlog::BinlogManager::open(&inner).unwrap();
                        kkdb::binlog::BinlogBroadcaster::new(mgr, 1024)
                    });

                    let node = kkdb::raft::node::new_with_http_network(
                        nid, raft_state, self_url, peer_addrs.clone(),
                        wal_dir, binlog,
                    ).await.expect("create raft node");
                    let node = std::sync::Arc::new(node);

                    // Bootstrap single node or if --peers is empty
                    if peer_addrs.is_empty() {
                        println!("[Raft] Single-node bootstrap, node {nid}");
                        node.init_single().await.unwrap_or_default();
                    }
                    // (Multi-node: operator calls POST /raft/init after all nodes start)

                    println!("[Raft] Node {nid} RPC server on {raft_socket}");
                    kkdb::raft::node::start_raft_http_server(
                        std::sync::Arc::clone(&node), raft_socket,
                    ).await;
                });
            });
        }

        if let Err(e) = kkdb::server::start_server(shared_vm, port) {
            eprintln!("Fatal server error: {}", e);
        }
        return;

    }

    // REPL MODE
    let mut rl = DefaultEditor::new().expect("Failed to create line editor");
    let mut multi_line_buf = String::new();

    loop {
        let prompt = if multi_line_buf.is_empty() {
            "kkdb> "
        } else {
            "  ... "
        };

        match rl.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim();

                if trimmed.is_empty() {
                    continue;
                }

                // Dot commands
                if trimmed.starts_with('.') && multi_line_buf.is_empty() {
                    let _ = rl.add_history_entry(trimmed);
                    if let Some(new_vm) = handle_dot_command(&mut vm, trimmed) {
                        vm = new_vm;
                    }
                    continue;
                }

                multi_line_buf.push_str(trimmed);
                multi_line_buf.push(' ');

                // Check if statement is complete (ends with semicolon)
                if !trimmed.ends_with(';') {
                    continue;
                }

                let sql = multi_line_buf.trim().to_string();
                multi_line_buf.clear();

                let _ = rl.add_history_entry(&sql);

                match vm.execute_sql(&sql) {
                    Ok(result) => match result {
                        ExecResult::Ok { message } => println!("{}", message),
                        ExecResult::RowsAffected { message, .. } => println!("{}", message),
                        ExecResult::QueryResult { columns, rows } => {
                            print_query_result(&columns, &rows);
                        }
                        ExecResult::Explain { plan } => print!("{}", plan),
                    },
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
            Err(ReadlineError::Interrupted) => {
                multi_line_buf.clear();
                println!("^C");
            }
            Err(ReadlineError::Eof) => {
                println!("Bye!");
                break;
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        }
    }
}
