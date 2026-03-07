use kkdb::vm::execute::VM;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use mysql_async::prelude::*;
use mysql_async::Conn;

#[tokio::test]
async fn test_mysql_server_integration() {
    let port: u16 = 33306;
    let vm = VM::new_memory();
    let shared_vm = Arc::new(Mutex::new(vm));
    
    // Start server in background thread
    let server_vm = Arc::clone(&shared_vm);
    std::thread::spawn(move || {
        let _ = kkdb::server::start_server(server_vm, port);
    });

    // Give server a moment to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Connect using mysql_async
    let opts = mysql_async::Opts::from_url(&format!("mysql://root:password@127.0.0.1:{}/test", port)).unwrap();
    let mut conn = Conn::new(opts).await.unwrap();

    // 1. DDL
    conn.query_drop("CREATE TABLE test_users (id INTEGER PRIMARY KEY, name TEXT)").await.unwrap();

    // 2. DML
    conn.query_drop("INSERT INTO test_users VALUES (1, 'Alice'), (2, 'Bob')").await.unwrap();

    // 3. SELECT
    let result: Vec<(i64, String)> = conn.query("SELECT id, name FROM test_users ORDER BY id").await.unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0], (1, "Alice".to_string()));
    assert_eq!(result[1], (2, "Bob".to_string()));

    // 4. Verification with Subquery
    let result2: Vec<String> = conn.query("SELECT name FROM test_users WHERE id = (SELECT MAX(id) FROM test_users)").await.unwrap();
    assert_eq!(result2.len(), 1);
    assert_eq!(result2[0], "Bob");
}
