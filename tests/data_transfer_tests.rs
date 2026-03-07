use kkdb::vm::execute::{ExecResult, VM};
use std::fs;

#[test]
fn test_backup_and_restore() {
    let db_path = "test_backup.db";
    let backup_path = "test_backup.sql";
    let restore_path = "test_restore.db";
    let _ = fs::remove_file(db_path);
    let _ = fs::remove_file(backup_path);
    let _ = fs::remove_file(restore_path);

    // Initial Database
    {
        let mut vm = VM::open(db_path).unwrap();
        vm.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);").unwrap();
        vm.execute_sql("INSERT INTO users VALUES (1, 'Alice');").unwrap();
        vm.execute_sql("INSERT INTO users VALUES (2, 'Bob, the builder');").unwrap();
        vm.execute_sql("INSERT INTO users VALUES (3, 'O''Connor');").unwrap(); // test quote escaping
        vm.backup(backup_path).unwrap();
    }

    // Restore Database
    {
        let mut vm2 = VM::open(restore_path).unwrap();
        vm2.restore(backup_path).unwrap();

        // Verify Data
        let res = vm2.execute_sql("SELECT name FROM users ORDER BY id;").unwrap();
        if let ExecResult::QueryResult { rows, .. } = res {
            assert_eq!(rows.len(), 3);
            assert!(matches!(&rows[0][0], kkdb::types::Value::Text(t) if t.as_ref() == "Alice"));
            assert!(matches!(&rows[1][0], kkdb::types::Value::Text(t) if t.as_ref() == "Bob, the builder"));
            assert!(matches!(&rows[2][0], kkdb::types::Value::Text(t) if t.as_ref() == "O'Connor"));
        } else {
            panic!("Expected query result");
        }
    }

    // Cleanup
    let _ = fs::remove_file(db_path);
    let _ = fs::remove_file(backup_path);
    let _ = fs::remove_file(restore_path);
}

#[test]
fn test_export_and_import() {
    let db_path1 = "test_export.db";
    let csv_path = "test_export.csv";
    let db_path2 = "test_import.db";
    let _ = fs::remove_file(db_path1);
    let _ = fs::remove_file(csv_path);
    let _ = fs::remove_file(db_path2);

    {
        let mut vm = VM::open(db_path1).unwrap();
        vm.execute_sql("CREATE TABLE items (id INTEGER PRIMARY KEY, desc TEXT, val REAL);").unwrap();
        vm.execute_sql("INSERT INTO items VALUES (1, 'Item 1', 10.5);").unwrap();
        vm.execute_sql("INSERT INTO items VALUES (2, 'Item, with comma', 20.0);").unwrap();
        vm.execute_sql("INSERT INTO items VALUES (3, 'Item \"with quotes\"', 30.0);").unwrap();
        vm.execute_sql("INSERT INTO items VALUES (4, NULL, NULL);").unwrap();
        vm.export_csv("items", csv_path).unwrap();
    }

    // Check exported file content roughly
    let csv_content = fs::read_to_string(csv_path).unwrap();
    assert!(csv_content.contains("Item, with comma")); // inside quotes conceptually
    
    {
        let mut vm2 = VM::open(db_path2).unwrap();
        // create empty schema manually for importing
        vm2.execute_sql("CREATE TABLE items (id INTEGER PRIMARY KEY, desc TEXT, val REAL);").unwrap();
        vm2.import_csv(csv_path, "items").unwrap();

        // Verify Data
        let res = vm2.execute_sql("SELECT desc, val FROM items ORDER BY id;").unwrap();
        if let ExecResult::QueryResult { rows, .. } = res {
            assert_eq!(rows.len(), 4);
            assert!(matches!(&rows[0][0], kkdb::types::Value::Text(t) if t.as_ref() == "Item 1"));
            assert!(matches!(&rows[1][0], kkdb::types::Value::Text(t) if t.as_ref() == "Item, with comma"));
            assert!(matches!(&rows[2][0], kkdb::types::Value::Text(t) if t.as_ref() == "Item \"with quotes\""));
            assert!(matches!(&rows[3][0], kkdb::types::Value::Null));
        } else {
            panic!("Expected query result");
        }
    }

    // Cleanup
    let _ = fs::remove_file(db_path1);
    let _ = fs::remove_file(csv_path);
    let _ = fs::remove_file(db_path2);
}
