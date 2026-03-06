use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};


fn setup_vm(db_dir: &str) -> VM {
    // Clean up any previous test run artifacts
    let _ = std::fs::remove_dir_all(db_dir);
    VM::open(db_dir).unwrap()
}

fn rows_from(result: ExecResult) -> Vec<Vec<Value>> {
    if let ExecResult::QueryResult { rows, .. } = result {
        rows
    } else {
        panic!("Expected QueryResult, got {:?}", result)
    }
}

#[test]
fn test_l4_fts_create_table() {
    let mut vm = setup_vm("test_l4_fts_create_table_db");

    vm.execute_sql("CREATE VIRTUAL TABLE docs USING fts5(title, body);").unwrap();

    // Verify it exists in schema and is_fts is true
    let table = vm.schema.tables.get("docs").expect("Table docs should exist");
    assert!(table.is_fts, "Table docs should be marked as FTS");
    assert_eq!(table.col_names, vec!["title", "body"]);

    // Verify hidden index table exists
    let idx_table = vm.schema.tables.get("docs_fts_idx").expect("Hidden FTS index table should exist");
    assert!(!idx_table.is_fts, "Hidden FTS table should not be FTS recursively");

    // Drop table should cascade
    vm.execute_sql("DROP TABLE docs;").unwrap();
    assert!(!vm.schema.tables.contains_key("docs"), "Table docs should be dropped");
    assert!(!vm.schema.tables.contains_key("docs_fts_idx"), "Hidden FTS index table should be dropped");
}

#[test]
fn test_l4_fts_insert_and_match() {
    let mut vm = setup_vm("test_l4_fts_insert_and_match_db");

    vm.execute_sql("CREATE VIRTUAL TABLE docs USING fts5(title, body);").unwrap();
    
    // Insert rows - rowid is assigned automatically
    vm.execute_sql("INSERT INTO docs (title, body) VALUES ('Apple Mac', 'Core i9 MBP');").unwrap();
    vm.execute_sql("INSERT INTO docs (title, body) VALUES ('Banana', 'Yellow fruit');").unwrap();
    vm.execute_sql("INSERT INTO docs (title, body) VALUES ('Apple iPhone', 'A15 Bionic');").unwrap();

    // MATCH query for 'apple' (case-insensitive token)
    let rows = rows_from(vm.execute_sql("SELECT title FROM docs WHERE docs MATCH 'apple';").unwrap());
    assert_eq!(rows.len(), 2, "Expected 2 rows for 'apple' match, got: {:?}", rows);
    assert!(rows.contains(&vec![Value::Text("Apple Mac".into())]));
    assert!(rows.contains(&vec![Value::Text("Apple iPhone".into())]));

    // MATCH query for 'fruit'
    let rows2 = rows_from(vm.execute_sql("SELECT title FROM docs WHERE docs MATCH 'fruit';").unwrap());
    assert_eq!(rows2.len(), 1);
    assert_eq!(rows2[0], vec![Value::Text("Banana".into())]);

    // MATCH query for non-existent token
    let rows3 = rows_from(vm.execute_sql("SELECT title FROM docs WHERE docs MATCH 'orange';").unwrap());
    assert_eq!(rows3.len(), 0);
}

#[test]
fn test_l4_fts_update_delete() {
    let mut vm = setup_vm("test_l4_fts_update_delete_db");

    vm.execute_sql("CREATE VIRTUAL TABLE books USING fts5(title);").unwrap();
    vm.execute_sql("INSERT INTO books (title) VALUES ('The Lord of the Rings');").unwrap();
    vm.execute_sql("INSERT INTO books (title) VALUES ('Harry Potter');").unwrap();

    // Initial MATCH
    let r1 = rows_from(vm.execute_sql("SELECT title FROM books WHERE books MATCH 'lord';").unwrap());
    assert_eq!(r1.len(), 1, "Expected 1 row before update, got: {:?}", r1);

    // Update the first matching row
    vm.execute_sql("UPDATE books SET title = 'The Hobbit' WHERE title = 'The Lord of the Rings';").unwrap();

    // Old token should be removed
    let r2 = rows_from(vm.execute_sql("SELECT title FROM books WHERE books MATCH 'lord';").unwrap());
    assert_eq!(r2.len(), 0, "Old token 'lord' should not match anymore, got: {:?}", r2);

    // New token should match
    let r3 = rows_from(vm.execute_sql("SELECT title FROM books WHERE books MATCH 'hobbit';").unwrap());
    assert_eq!(r3.len(), 1, "New token 'hobbit' should match, got: {:?}", r3);

    // Delete Harry Potter row
    vm.execute_sql("DELETE FROM books WHERE title = 'Harry Potter';").unwrap();
    let r4 = rows_from(vm.execute_sql("SELECT title FROM books WHERE books MATCH 'harry';").unwrap());
    assert_eq!(r4.len(), 0, "Deleted row token 'harry' should not match, got: {:?}", r4);
}
