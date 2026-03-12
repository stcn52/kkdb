pub mod http_api;
pub mod kk_backend;
pub mod mysql;
pub mod tls;

use crate::vm::execute::VM;
use kk_backend::KkdbBackend;
use msql_srv::MysqlIntermediary;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

pub fn start_server(vm: Arc<Mutex<VM>>, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    println!("[KKDB] MySQL server listening on :{}", port);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let vm_clone = Arc::clone(&vm);
                thread::spawn(move || {
                    let backend = KkdbBackend {
                        vm: vm_clone,
                        current_user: None,
                    };
                    match MysqlIntermediary::run_on_tcp(backend, stream) {
                        Ok(_) => {}
                        Err(e) => eprintln!("Client connection error: {}", e),
                    }
                });
            }
            Err(e) => {
                eprintln!("Error accepting connection: {}", e);
            }
        }
    }

    Ok(())
}

/// Start the HTTP REST API server (Supabase-style) on the given port.
///
/// - `data_dir` — optional root directory for per-user databases.
///   `None` → in-memory mode (each user gets their own in-memory VM).
///   `Some(path)` → per-user data is persisted under `{path}/{user_id}/`.
pub fn start_http_server(
    _vm: Arc<Mutex<VM>>, // kept for backward compat; auth VM is opened from data_dir
    port: u16,
    data_dir: Option<PathBuf>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let state = match data_dir {
            Some(dir) => match http_api::AppState::with_dir(dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[KKDB] Failed to open HTTP API data directory: {e}");
                    std::process::exit(1);
                }
            },
            None => http_api::AppState::in_memory(),
        };

        let router = http_api::build_router(state);
        let addr = format!("0.0.0.0:{}", port);
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .expect("Failed to bind HTTP API port");
        println!("[KKDB] HTTP API listening on http://{}", addr);
        axum::serve(listener, router)
            .await
            .expect("HTTP API server error");
    })
}
