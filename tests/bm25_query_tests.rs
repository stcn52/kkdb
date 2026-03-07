/// Integration tests for Phase 5: BM25 FULLTEXT INDEX query path.
/// Uses CREATE FULLTEXT INDEX (not the old FTS5 virtual table) to verify
/// the real BM25 scoring query via exec_fts_bm25_query.
use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};

fn rows_from(result: ExecResult) -> Vec<Vec<Value>> {
    if let ExecResult::QueryResult { rows, .. } = result {
        rows
    } else {
        panic!("Expected QueryResult, got {:?}", result)
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

/// Creates a fresh in-memory VM (no file IO needed for tests).
fn vm() -> VM {
    VM::new_memory()
}

// ─────────────────────────────────────────────────────────────────────────────
// Basic CREATE FULLTEXT INDEX + MATCH query
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_bm25_create_and_match_basic() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE articles (id INTEGER PRIMARY KEY, title TEXT, body TEXT);").unwrap();
    v.execute_sql("INSERT INTO articles VALUES (1, 'Rust Programming', 'Rust is a systems programming language');").unwrap();
    v.execute_sql("INSERT INTO articles VALUES (2, 'Python Scripting', 'Python is great for scripting');").unwrap();
    v.execute_sql("INSERT INTO articles VALUES (3, 'Rust vs Go', 'Comparing Rust and Go performance');").unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_articles ON articles (title, body);").unwrap();

    // 'rust' appears in rows 1 and 3 (and row 3's title has it twice)
    let rows = rows_from(v.execute_sql("SELECT title FROM articles WHERE articles MATCH 'rust';").unwrap());
    assert_eq!(rows.len(), 2, "Expected 2 rows matching 'rust', got: {:?}", rows);
    let titles: Vec<_> = rows.iter().map(|r| r[0].clone()).collect();
    assert!(titles.contains(&text("Rust Programming")));
    assert!(titles.contains(&text("Rust vs Go")));
}

#[test]
fn test_bm25_no_match() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, content TEXT);").unwrap();
    v.execute_sql("INSERT INTO docs VALUES (1, 'Hello world');").unwrap();
    v.execute_sql("INSERT INTO docs VALUES (2, 'Goodbye world');").unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_docs ON docs (content);").unwrap();

    let rows = rows_from(v.execute_sql("SELECT content FROM docs WHERE docs MATCH 'kkdb';").unwrap());
    assert_eq!(rows.len(), 0, "No match expected");
}

#[test]
fn test_bm25_unicode_query() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT);").unwrap();
    // Use unsegmented Chinese — jieba-rs will perform word segmentation.
    // Row 1: contains 数据库 (database), 引擎 (engine), 设计 (design)
    // Row 2: English only — no overlap
    // Row 3: 分布式 (distributed), 存储 (storage) — no 数据库 token
    v.execute_sql("INSERT INTO books VALUES (1, '数据库引擎设计');").unwrap();
    v.execute_sql("INSERT INTO books VALUES (2, 'Database Engine Design');").unwrap();
    v.execute_sql("INSERT INTO books VALUES (3, '分布式存储系统');").unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_books ON books (title);").unwrap();

    // jieba cuts '数据库引擎设计' → includes '数据库', '引擎', '设计' etc.
    // Only row 1 should contain the '数据库' token.
    let rows = rows_from(v.execute_sql("SELECT title FROM books WHERE books MATCH '数据库';").unwrap());
    assert_eq!(rows.len(), 1, "Expected 1 row with '数据库' token, got: {:?}", rows);
    assert_eq!(rows[0][0], text("数据库引擎设计"));
}

#[test]
fn test_bm25_insert_after_create_index() {
    let mut v = vm();
    // Create table and index FIRST, then insert (DML write path)
    v.execute_sql("CREATE TABLE news (id INTEGER PRIMARY KEY, headline TEXT);").unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_news ON news (headline);").unwrap();

    v.execute_sql("INSERT INTO news VALUES (1, 'KKDB launches BM25 search');").unwrap();
    v.execute_sql("INSERT INTO news VALUES (2, 'PostgreSQL adds vector search');").unwrap();
    v.execute_sql("INSERT INTO news VALUES (3, 'BM25 is better than TF-IDF');").unwrap();

    // 'bm25' should match rows 1 & 3
    let rows = rows_from(v.execute_sql("SELECT headline FROM news WHERE news MATCH 'bm25';").unwrap());
    assert_eq!(rows.len(), 2, "Expected 2 rows for 'bm25', got: {:?}", rows);

    // 'search' should match rows 1 & 2
    let rows2 = rows_from(v.execute_sql("SELECT headline FROM news WHERE news MATCH 'search';").unwrap());
    assert_eq!(rows2.len(), 2, "Expected 2 rows for 'search', got: {:?}", rows2);
}

#[test]
fn test_bm25_delete_removes_from_index() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE posts (id INTEGER PRIMARY KEY, body TEXT);").unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_posts ON posts (body);").unwrap();

    v.execute_sql("INSERT INTO posts VALUES (1, 'Learning Rust is fun');").unwrap();
    v.execute_sql("INSERT INTO posts VALUES (2, 'Python is easy to learn');").unwrap();

    // Both match 'learn'/'learning'
    let before = rows_from(v.execute_sql("SELECT body FROM posts WHERE posts MATCH 'learning';").unwrap());
    assert_eq!(before.len(), 1);

    // Delete the Rust post
    v.execute_sql("DELETE FROM posts WHERE id = 1;").unwrap();

    // 'learning' should no longer match
    let after = rows_from(v.execute_sql("SELECT body FROM posts WHERE posts MATCH 'learning';").unwrap());
    assert_eq!(after.len(), 0, "Deleted row should not appear, got: {:?}", after);
}
