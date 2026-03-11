use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};

fn setup_vm(db_dir: &str) -> VM {
    // Clean up any previous test run artifacts
    let path = format!("testdata/{}", db_dir);
    std::fs::create_dir_all("testdata").ok();
    let _ = std::fs::remove_dir_all(&path);
    VM::open(&path).unwrap()
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

    vm.execute_sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, body TEXT);")
        .unwrap();
    vm.execute_sql("CREATE FULLTEXT INDEX idx_docs_fts ON docs (title, body);")
        .unwrap();

    // Verify it exists in schema
    let table = vm
        .schema
        .tables
        .get("docs")
        .expect("Table docs should exist");
    assert_eq!(table.col_names, vec!["id", "title", "body"]);

    // Verify index table exists
    let idx = vm
        .schema
        .indexes
        .get("idx_docs_fts")
        .expect("FTS index should exist");
    assert!(idx.is_fts, "Index should be marked as FTS");

    // Drop index
    vm.execute_sql("DROP INDEX idx_docs_fts;").unwrap();
    assert!(
        !vm.schema.indexes.contains_key("idx_docs_fts"),
        "FTS index should be dropped"
    );
}

#[test]
fn test_l4_fts_insert_and_match() {
    let mut vm = setup_vm("test_l4_fts_insert_and_match_db");

    vm.execute_sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, body TEXT);")
        .unwrap();
    vm.execute_sql("CREATE FULLTEXT INDEX idx_docs_fts ON docs (title, body);")
        .unwrap();

    // Insert rows explicitly
    eprintln!("--- FIRST INSERT ---");
    vm.execute_sql("INSERT INTO docs (id, title, body) VALUES (1, 'Apple Mac', 'Core i9 MBP');")
        .unwrap();
    eprintln!("--- SECOND INSERT ---");
    vm.execute_sql("INSERT INTO docs (id, title, body) VALUES (3, 'Banana', 'Yellow fruit');")
        .unwrap();
    vm.execute_sql("INSERT INTO docs (id, title, body) VALUES (5, 'Apple iPhone', 'A15 Bionic');")
        .unwrap();

    // MATCH query for 'apple' (case-insensitive token)
    let rows = rows_from(
        vm.execute_sql("SELECT title FROM docs WHERE docs MATCH 'apple';")
            .unwrap(),
    );
    assert_eq!(
        rows.len(),
        2,
        "Expected 2 rows for 'apple' match, got: {:?}",
        rows
    );
    assert!(rows.contains(&vec![Value::Text("Apple Mac".into())]));
    assert!(rows.contains(&vec![Value::Text("Apple iPhone".into())]));

    // MATCH query for 'fruit'
    let rows2 = rows_from(
        vm.execute_sql("SELECT title FROM docs WHERE docs MATCH 'fruit';")
            .unwrap(),
    );
    assert_eq!(rows2.len(), 1);
    assert_eq!(rows2[0], vec![Value::Text("Banana".into())]);

    // MATCH query for non-existent token
    let rows3 = rows_from(
        vm.execute_sql("SELECT title FROM docs WHERE docs MATCH 'orange';")
            .unwrap(),
    );
    assert_eq!(rows3.len(), 0);
}

#[test]
fn test_l4_fts_update_delete() {
    let mut vm = setup_vm("test_l4_fts_update_delete_db");

    vm.execute_sql("CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT);")
        .unwrap();
    vm.execute_sql("CREATE FULLTEXT INDEX idx_books_fts ON books (title);")
        .unwrap();
    eprintln!("--- FIRST BOOKS INSERT ---");
    vm.execute_sql("INSERT INTO books (id, title) VALUES (1, 'The Lord of the Rings');")
        .unwrap();
    eprintln!("--- SECOND BOOKS INSERT ---");
    vm.execute_sql("INSERT INTO books (id, title) VALUES (3, 'Harry Potter');")
        .unwrap();

    // Initial MATCH
    let r1 = rows_from(
        vm.execute_sql("SELECT title FROM books WHERE books MATCH 'lord';")
            .unwrap(),
    );
    assert_eq!(r1.len(), 1, "Expected 1 row before update, got: {:?}", r1);

    // Update the first matching row
    vm.execute_sql("UPDATE books SET title = 'The Hobbit' WHERE title = 'The Lord of the Rings';")
        .unwrap();

    // Old token should be removed
    let r2 = rows_from(
        vm.execute_sql("SELECT title FROM books WHERE books MATCH 'lord';")
            .unwrap(),
    );
    assert_eq!(
        r2.len(),
        0,
        "Old token 'lord' should not match anymore, got: {:?}",
        r2
    );

    // New token should match
    let r3 = rows_from(
        vm.execute_sql("SELECT title FROM books WHERE books MATCH 'hobbit';")
            .unwrap(),
    );
    assert_eq!(
        r3.len(),
        1,
        "New token 'hobbit' should match, got: {:?}",
        r3
    );

    // Delete Harry Potter row
    vm.execute_sql("DELETE FROM books WHERE title = 'Harry Potter';")
        .unwrap();
    let r4 = rows_from(
        vm.execute_sql("SELECT title FROM books WHERE books MATCH 'harry';")
            .unwrap(),
    );
    assert_eq!(
        r4.len(),
        0,
        "Deleted row token 'harry' should not match, got: {:?}",
        r4
    );
}
