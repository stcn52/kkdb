//! End-to-end MySQL Wire Protocol integration tests.
//!
//! These tests start a real KKDB MySQL server on a random free TCP port and
//! connect from the test process using the `mysql_async` client — exactly the
//! same library a Rust application would use against a real MySQL server.
//!
//! Test coverage:
//!   - TCP handshake + unauthenticated root login (dev mode)
//!   - COM_PING keepalive
//!   - DDL: CREATE TABLE
//!   - DML: INSERT / UPDATE
//!   - DQL: SELECT with typed columns
//!   - Multiple sequential queries on the same connection
//!   - SELECT VERSION() introspection query
//!   - SHOW DATABASES introspection query
//!   - COM_QUIT clean disconnect

use mysql_async::prelude::*;
use mysql_async::{Conn, Opts, OptsBuilder, Pool};
use std::net::TcpListener;
use std::time::Duration;
use tokio::time::sleep;

use kkdb::server::http_api::AppState;
use kkdb::server::mysql::serve_mysql;

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Pick a free OS-assigned TCP port.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Start a KKDB MySQL server on a random port and return the connection URL.
/// The server runs until the test process exits.
async fn start_server() -> String {
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let state = AppState::in_memory();
    let addr_clone = addr.clone();

    tokio::spawn(async move {
        serve_mysql(&addr_clone, state).await.ok();
    });

    // Give the server a moment to bind
    sleep(Duration::from_millis(100)).await;
    format!("mysql://root@{addr}/kkdb")
}

/// Build a mysql_async connection to the given URL.
async fn connect(url: &str) -> Conn {
    let opts = Opts::from_url(url).expect("bad URL");
    Conn::new(opts).await.expect("connect failed")
}

// ─── Test 1: connect and ping ─────────────────────────────────────────────────

#[tokio::test]
async fn test_mysql_connect_and_ping() {
    let url = start_server().await;
    let mut conn = connect(&url).await;
    // mysql_async sends a ping during connection setup; explicit ping
    conn.ping().await.expect("ping failed");
    conn.disconnect().await.ok();
}

// ─── Test 2: SELECT VERSION() ─────────────────────────────────────────────────

#[tokio::test]
async fn test_mysql_select_version() {
    let url = start_server().await;
    let mut conn = connect(&url).await;

    let row: Option<String> = conn
        .query_first("SELECT VERSION()")
        .await
        .expect("SELECT VERSION() failed");

    let version = row.expect("must return a row");
    assert!(version.contains("kkdb"), "version must mention kkdb, got: {version}");
    conn.disconnect().await.ok();
}

// ─── Test 3: DDL + INSERT + SELECT ───────────────────────────────────────────

#[tokio::test]
async fn test_mysql_ddl_dml_select() {
    let url = start_server().await;
    let mut conn = connect(&url).await;

    // CREATE TABLE
    conn.query_drop("CREATE TABLE IF NOT EXISTS products (id INT, name TEXT, price REAL)")
        .await
        .expect("CREATE TABLE failed");

    // INSERT rows
    conn.query_drop("INSERT INTO products VALUES (1, 'Widget', 9.99)")
        .await
        .expect("INSERT 1 failed");
    conn.query_drop("INSERT INTO products VALUES (2, 'Gadget', 19.99)")
        .await
        .expect("INSERT 2 failed");

    // SELECT and verify
    let rows: Vec<(i32, String, f64)> = conn
        .query("SELECT id, name, price FROM products ORDER BY id")
        .await
        .expect("SELECT failed");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[0].1, "Widget");
    assert!((rows[0].2 - 9.99).abs() < 0.001);
    assert_eq!(rows[1].0, 2);
    assert_eq!(rows[1].1, "Gadget");

    conn.disconnect().await.ok();
}

// ─── Test 4: multiple sequential queries ─────────────────────────────────────

#[tokio::test]
async fn test_mysql_sequential_queries() {
    let url = start_server().await;
    let mut conn = connect(&url).await;

    conn.query_drop("CREATE TABLE seq_test (n INT)").await.unwrap();

    for i in 1..=5 {
        conn.query_drop(format!("INSERT INTO seq_test VALUES ({i})")).await.unwrap();
    }

    let rows: Vec<i32> = conn.query("SELECT n FROM seq_test ORDER BY n").await.unwrap();
    assert_eq!(rows, vec![1, 2, 3, 4, 5], "all inserted rows must be retrievable");

    conn.disconnect().await.ok();
}

// ─── Test 5: SHOW DATABASES introspection ────────────────────────────────────

#[tokio::test]
async fn test_mysql_show_databases() {
    let url = start_server().await;
    let mut conn = connect(&url).await;

    let rows: Vec<String> = conn
        .query("SHOW DATABASES")
        .await
        .expect("SHOW DATABASES failed");

    assert!(!rows.is_empty(), "SHOW DATABASES must return at least one row");
    assert!(
        rows.iter().any(|db| db == "kkdb"),
        "kkdb must appear in SHOW DATABASES, got: {rows:?}"
    );

    conn.disconnect().await.ok();
}

// ─── Test 6: empty SELECT (no rows) ──────────────────────────────────────────

#[tokio::test]
async fn test_mysql_select_empty() {
    let url = start_server().await;
    let mut conn = connect(&url).await;

    conn.query_drop("CREATE TABLE empty_tbl (x INT)").await.unwrap();
    let rows: Vec<i32> = conn.query("SELECT x FROM empty_tbl").await.unwrap();
    assert!(rows.is_empty(), "select from empty table must return zero rows");

    conn.disconnect().await.ok();
}

// ─── Test 7: connection pool — concurrent queries ────────────────────────────

#[tokio::test]
async fn test_mysql_pool_concurrent() {
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let addr_clone = addr.clone();

    tokio::spawn(async move {
        serve_mysql(&addr_clone, AppState::in_memory()).await.ok();
    });
    sleep(Duration::from_millis(100)).await;

    let url = format!("mysql://root@{addr}/kkdb");
    let pool = Pool::new(url.as_str());

    // Fire 3 concurrent connections
    let handles: Vec<_> = (0..3)
        .map(|_| {
            let pool = pool.clone();
            tokio::spawn(async move {
                let mut conn = pool.get_conn().await.expect("pool conn");
                let v: Option<String> = conn.query_first("SELECT VERSION()").await.unwrap();
                v.unwrap()
            })
        })
        .collect();

    for h in handles {
        let v = h.await.expect("task panicked");
        assert!(v.contains("kkdb"), "version must mention kkdb");
    }

    pool.disconnect().await.ok();
}

// ─── Test 8: UPDATE + re-SELECT ──────────────────────────────────────────────

#[tokio::test]
async fn test_mysql_update() {
    let url = start_server().await;
    let mut conn = connect(&url).await;

    conn.query_drop("CREATE TABLE scores (player TEXT, score INT)").await.unwrap();
    conn.query_drop("INSERT INTO scores VALUES ('Alice', 100)").await.unwrap();
    conn.query_drop("UPDATE scores SET score = 200 WHERE player = 'Alice'").await.unwrap();

    let row: Option<i32> = conn
        .query_first("SELECT score FROM scores WHERE player = 'Alice'")
        .await
        .unwrap();

    assert_eq!(row, Some(200), "updated value must be 200");
    conn.disconnect().await.ok();
}
