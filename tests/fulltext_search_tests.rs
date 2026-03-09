/// Comprehensive integration tests for Full-Text Search (BM25).
///
/// Covers:
///  1. Single-word match + BM25 score ordering
///  2. Punctuation in documents (stripped by tokenizer)
///  3. Case insensitivity
///  4. Empty result for unknown term
///  5. Multi-word OR semantics (union of matching docs)
///  6. Multi-word AND scoring (docs with ALL terms rank first)
///  7. Insert after CREATE FULLTEXT INDEX (DML write path)
///  8. DELETE removes from index (no phantom hits)
///  9. UPDATE changes indexed text
/// 10. Multi-column FTS index
/// 11. Stop-word IDF suppression (term in every doc → low score)
/// 12. Score ordering: higher TF in same doc → higher rank
use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn vm() -> VM {
    VM::new_memory()
}

fn rows_from(result: ExecResult) -> Vec<Vec<Value>> {
    match result {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("Expected QueryResult, got {:?}", other),
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 1: Single-word match — correct docs returned
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_single_word_basic() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    v.execute_sql("INSERT INTO docs VALUES (1, 'Rust is a systems programming language');")
        .unwrap();
    v.execute_sql("INSERT INTO docs VALUES (2, 'Python is great for scripting');")
        .unwrap();
    v.execute_sql("INSERT INTO docs VALUES (3, 'Go is compiled and fast');")
        .unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_docs ON docs (body);")
        .unwrap();

    let rows = rows_from(
        v.execute_sql("SELECT body FROM docs WHERE docs MATCH 'rust';")
            .unwrap(),
    );
    assert_eq!(rows.len(), 1, "Expected exactly 1 doc matching 'rust'");
    assert_eq!(rows[0][0], text("Rust is a systems programming language"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: BM25 score ordering — doc with higher TF ranks first
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_bm25_score_ordering() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE arts (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    // Row 1: "database" appears 3 times → higher TF → should rank first
    v.execute_sql("INSERT INTO arts VALUES (1, 'database database database optimization');")
        .unwrap();
    // Row 2: "database" appears once
    v.execute_sql("INSERT INTO arts VALUES (2, 'introduction to database systems');")
        .unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_arts ON arts (body);")
        .unwrap();

    let rows = rows_from(
        v.execute_sql("SELECT body FROM arts WHERE arts MATCH 'database';")
            .unwrap(),
    );
    assert_eq!(rows.len(), 2, "Both rows should match 'database'");
    // First result must be the row with higher TF (3 occurrences vs 1)
    assert_eq!(
        rows[0][0],
        text("database database database optimization"),
        "Higher TF row must rank first. Got order: {:?}",
        rows
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: Punctuation stripping — commas, periods, exclamation marks ignored
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_punctuation_stripped() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE articles (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    v.execute_sql("INSERT INTO articles VALUES (1, 'Rust, the language! Systems-level code.');")
        .unwrap();
    v.execute_sql("INSERT INTO articles VALUES (2, 'Python: scripting and automation!');")
        .unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_art ON articles (body);")
        .unwrap();

    // "rust" should match despite being followed by a comma in the original
    let rows = rows_from(
        v.execute_sql("SELECT body FROM articles WHERE articles MATCH 'rust';")
            .unwrap(),
    );
    assert_eq!(
        rows.len(),
        1,
        "Punctuation should be stripped; 'rust' must match row 1"
    );

    // "language" should match (tokenized despite trailing period + exclamation)
    let rows2 = rows_from(
        v.execute_sql("SELECT body FROM articles WHERE articles MATCH 'language';")
            .unwrap(),
    );
    assert_eq!(
        rows2.len(),
        1,
        "'language' must match row 1 despite punctuation"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 4: Case insensitivity
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_case_insensitive() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE news (id INTEGER PRIMARY KEY, headline TEXT);")
        .unwrap();
    v.execute_sql("INSERT INTO news VALUES (1, 'KKDB Launches BM25 Search');")
        .unwrap();
    v.execute_sql("INSERT INTO news VALUES (2, 'Open Source Database News');")
        .unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_news ON news (headline);")
        .unwrap();

    // Query in lower-case — should match upper-case token
    let r1 = rows_from(
        v.execute_sql("SELECT headline FROM news WHERE news MATCH 'kkdb';")
            .unwrap(),
    );
    assert_eq!(r1.len(), 1);

    // Query in upper-case — should also match
    let r2 = rows_from(
        v.execute_sql("SELECT headline FROM news WHERE news MATCH 'KKDB';")
            .unwrap(),
    );
    assert_eq!(r2.len(), 1);

    assert_eq!(r1, r2, "Case should not affect BM25 results");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 5: No match for unknown term
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_no_match() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE posts (id INTEGER PRIMARY KEY, content TEXT);")
        .unwrap();
    v.execute_sql("INSERT INTO posts VALUES (1, 'Hello world');")
        .unwrap();
    v.execute_sql("INSERT INTO posts VALUES (2, 'Goodbye world');")
        .unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_posts ON posts (content);")
        .unwrap();

    let rows = rows_from(
        v.execute_sql("SELECT content FROM posts WHERE posts MATCH 'kkdb';")
            .unwrap(),
    );
    assert_eq!(
        rows.len(),
        0,
        "No docs contain 'kkdb'; result must be empty"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 6: Multi-word query — OR semantics (union of per-token hits)
// Any document containing at least one query token should appear.
// Uses DML write path (CREATE INDEX first, then INSERT) for reliable root tracking.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_multi_word_or_semantics() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    // Create index FIRST, then INSERT (DML write path)
    v.execute_sql("CREATE FULLTEXT INDEX idx_t ON t (body);")
        .unwrap();
    v.execute_sql("INSERT INTO t VALUES (1, 'Rust programming language');")
        .unwrap(); // rust, language
    v.execute_sql("INSERT INTO t VALUES (2, 'Python is a scripting language');")
        .unwrap(); // language
    v.execute_sql("INSERT INTO t VALUES (3, 'Go concurrency model');")
        .unwrap(); // neither
    v.execute_sql("INSERT INTO t VALUES (4, 'Rust and Python comparison');")
        .unwrap(); // rust

    // Multi-word OR: union of ('rust' ∪ 'language') = rows 1, 2, 4
    let rows = rows_from(
        v.execute_sql("SELECT body FROM t WHERE t MATCH 'rust language';")
            .unwrap(),
    );
    assert_eq!(
        rows.len(),
        3,
        "OR semantics: docs with any query token. Got: {:?}",
        rows
    );

    let bodies: Vec<_> = rows.iter().map(|r| r[0].clone()).collect();
    assert!(
        bodies.contains(&text("Rust programming language")),
        "Row 1 (rust+language) must be in results. Got: {:?}",
        bodies
    );
    assert!(
        bodies.contains(&text("Python is a scripting language")),
        "Row 2 (language) must be in results. Got: {:?}",
        bodies
    );
    assert!(
        bodies.contains(&text("Rust and Python comparison")),
        "Row 4 (rust) must be in results. Got: {:?}",
        bodies
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 7: Multi-word AND scoring — docs with ALL terms score higher
// The doc containing both 'rust' AND 'database' accumulates scores from two tokens
// and must rank first compared to docs with only one term.
// Uses DML write path (CREATE INDEX first, then INSERT) for reliable root tracking.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_multi_word_and_scoring() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    // Create index FIRST, then INSERT (DML write path)
    v.execute_sql("CREATE FULLTEXT INDEX idx_t ON t (body);")
        .unwrap();
    v.execute_sql("INSERT INTO t VALUES (1, 'Rust is a fast database language');")
        .unwrap(); // rust + database
    v.execute_sql("INSERT INTO t VALUES (2, 'Rust programming tutorial');")
        .unwrap(); // rust only
    v.execute_sql("INSERT INTO t VALUES (3, 'Introduction to database systems');")
        .unwrap(); // database only

    // Now multi-word OR: rows 1+2 (rust) ∪ rows 1+3 (database) = rows 1, 2, 3
    let rows = rows_from(
        v.execute_sql("SELECT body FROM t WHERE t MATCH 'rust database';")
            .unwrap(),
    );
    assert_eq!(
        rows.len(),
        3,
        "All three rows should match (OR union). Got: {:?}",
        rows
    );
    // Row 1 has BOTH 'rust' and 'database' → cumulative BM25 → should rank first
    assert_eq!(
        rows[0][0],
        text("Rust is a fast database language"),
        "Doc with both tokens must rank first. Order: {:?}",
        rows
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 8: Insert after CREATE FULLTEXT INDEX (DML write path)
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_insert_after_index() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE news (id INTEGER PRIMARY KEY, headline TEXT);")
        .unwrap();
    // Create index FIRST (empty table)
    v.execute_sql("CREATE FULLTEXT INDEX idx_news ON news (headline);")
        .unwrap();
    // Now insert rows — should go through maintain_fts_insert
    v.execute_sql("INSERT INTO news VALUES (1, 'KKDB launches BM25 search');")
        .unwrap();
    v.execute_sql("INSERT INTO news VALUES (2, 'PostgreSQL adds vector search');")
        .unwrap();
    v.execute_sql("INSERT INTO news VALUES (3, 'BM25 outperforms TF-IDF');")
        .unwrap();

    let r1 = rows_from(
        v.execute_sql("SELECT headline FROM news WHERE news MATCH 'bm25';")
            .unwrap(),
    );
    assert_eq!(
        r1.len(),
        2,
        "'bm25' should match rows 1 and 3. Got: {:?}",
        r1
    );

    let r2 = rows_from(
        v.execute_sql("SELECT headline FROM news WHERE news MATCH 'search';")
            .unwrap(),
    );
    assert_eq!(
        r2.len(),
        2,
        "'search' should match rows 1 and 2. Got: {:?}",
        r2
    );

    let r3 = rows_from(
        v.execute_sql("SELECT headline FROM news WHERE news MATCH 'vector';")
            .unwrap(),
    );
    assert_eq!(
        r3.len(),
        1,
        "'vector' should match only row 2. Got: {:?}",
        r3
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 9: DELETE removes a row from the FTS index (no phantom hits)
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_delete_removes_from_index() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE posts (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_posts ON posts (body);")
        .unwrap();
    v.execute_sql("INSERT INTO posts VALUES (1, 'Learning Rust is fun');")
        .unwrap();
    v.execute_sql("INSERT INTO posts VALUES (2, 'Python is easy to learn');")
        .unwrap();

    // Before delete: both should match 'learn'/'learning'
    let before = rows_from(
        v.execute_sql("SELECT body FROM posts WHERE posts MATCH 'learning';")
            .unwrap(),
    );
    assert_eq!(
        before.len(),
        1,
        "Before delete: only row 1 has 'learning'. Got: {:?}",
        before
    );

    v.execute_sql("DELETE FROM posts WHERE id = 1;").unwrap();

    let after = rows_from(
        v.execute_sql("SELECT body FROM posts WHERE posts MATCH 'learning';")
            .unwrap(),
    );
    assert_eq!(
        after.len(),
        0,
        "After delete: 'learning' must return 0 rows. Got: {:?}",
        after
    );

    // Row 2 must still be searchable
    let r2 = rows_from(
        v.execute_sql("SELECT body FROM posts WHERE posts MATCH 'python';")
            .unwrap(),
    );
    assert_eq!(
        r2.len(),
        1,
        "Surviving row 2 must still be searchable. Got: {:?}",
        r2
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 10: UPDATE changes the FTS index
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_update_changes_index() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT);")
        .unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_books ON books (title);")
        .unwrap();
    v.execute_sql("INSERT INTO books VALUES (1, 'The Lord of the Rings');")
        .unwrap();
    v.execute_sql("INSERT INTO books VALUES (2, 'Harry Potter');")
        .unwrap();

    // Old token matches
    let r1 = rows_from(
        v.execute_sql("SELECT title FROM books WHERE books MATCH 'lord';")
            .unwrap(),
    );
    assert_eq!(r1.len(), 1, "Before update: 'lord' must match row 1");

    // Update the row text
    v.execute_sql("UPDATE books SET title = 'The Hobbit' WHERE id = 1;")
        .unwrap();

    // Old token should no longer appear
    let r2 = rows_from(
        v.execute_sql("SELECT title FROM books WHERE books MATCH 'lord';")
            .unwrap(),
    );
    assert_eq!(
        r2.len(),
        0,
        "After update: 'lord' must not match. Got: {:?}",
        r2
    );

    // New token should now be searchable
    let r3 = rows_from(
        v.execute_sql("SELECT title FROM books WHERE books MATCH 'hobbit';")
            .unwrap(),
    );
    assert_eq!(
        r3.len(),
        1,
        "After update: 'hobbit' must match new text. Got: {:?}",
        r3
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 11: Multi-column FTS index — tokens from title AND body both score
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_multi_column_index() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE articles (id INTEGER PRIMARY KEY, title TEXT, body TEXT);")
        .unwrap();
    // 'engine' appears only in title of row 1
    v.execute_sql("INSERT INTO articles VALUES (1, 'Database Engine', 'Architecture overview');")
        .unwrap();
    // 'engine' appears only in body of row 2
    v.execute_sql(
        "INSERT INTO articles VALUES (2, 'Storage Layer', 'The query engine internals');",
    )
    .unwrap();
    // 'engine' not in row 3 at all
    v.execute_sql("INSERT INTO articles VALUES (3, 'Introduction', 'Getting started guide');")
        .unwrap();
    // Index BOTH title and body
    v.execute_sql("CREATE FULLTEXT INDEX idx_articles ON articles (title, body);")
        .unwrap();

    let rows = rows_from(
        v.execute_sql("SELECT title FROM articles WHERE articles MATCH 'engine';")
            .unwrap(),
    );
    assert_eq!(
        rows.len(),
        2,
        "Both rows 1 and 2 contain 'engine' across different columns"
    );

    let titles: Vec<_> = rows.iter().map(|r| r[0].clone()).collect();
    assert!(titles.contains(&text("Database Engine")));
    assert!(titles.contains(&text("Storage Layer")));
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 12: Stop-word IDF suppression — term appearing in ALL docs gets low score
// The stop word 'the' appears in every document.
// Even when searched, it should return results but rank by other signals.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_stop_word_low_idf() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    // 'the' appears in every row
    v.execute_sql("INSERT INTO t VALUES (1, 'the quick brown fox');")
        .unwrap();
    v.execute_sql("INSERT INTO t VALUES (2, 'the lazy dog sleeps');")
        .unwrap();
    v.execute_sql("INSERT INTO t VALUES (3, 'the cat sat on the mat');")
        .unwrap(); // 'the' twice
    v.execute_sql("CREATE FULLTEXT INDEX idx_t ON t (body);")
        .unwrap();

    // 'the' matches all docs; row 3 has it twice (higher TF), so it should rank first
    let rows = rows_from(
        v.execute_sql("SELECT body FROM t WHERE t MATCH 'the';")
            .unwrap(),
    );
    assert_eq!(rows.len(), 3, "All 3 rows contain 'the'");
    // Row 3 has tf=2 for 'the' → should be first despite low IDF
    assert_eq!(
        rows[0][0],
        text("the cat sat on the mat"),
        "Row with tf=2 for 'the' should rank first. Order: {:?}",
        rows
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 13: Mixed English and Chinese tokens in same document
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_mixed_language() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    // Mixed Chinese (space-separated words) and English
    v.execute_sql("INSERT INTO t VALUES (1, '数据库 engine design');")
        .unwrap();
    v.execute_sql("INSERT INTO t VALUES (2, 'database 引擎 architecture');")
        .unwrap();
    v.execute_sql("INSERT INTO t VALUES (3, 'distributed 分布式 systems');")
        .unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_t ON t (body);")
        .unwrap();

    // English query matches row 1
    let r1 = rows_from(
        v.execute_sql("SELECT body FROM t WHERE t MATCH 'engine';")
            .unwrap(),
    );
    assert_eq!(r1.len(), 1);
    assert_eq!(r1[0][0], text("数据库 engine design"));

    // Chinese query matches row 2
    let r2 = rows_from(
        v.execute_sql("SELECT body FROM t WHERE t MATCH '引擎';")
            .unwrap(),
    );
    assert_eq!(r2.len(), 1);
    assert_eq!(r2[0][0], text("database 引擎 architecture"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 14: Empty table MATCH returns empty result (not an error)
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_empty_table() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_t ON t (body);")
        .unwrap();
    // No rows inserted

    let rows = rows_from(
        v.execute_sql("SELECT body FROM t WHERE t MATCH 'anything';")
            .unwrap(),
    );
    assert_eq!(
        rows.len(),
        0,
        "Empty table must return 0 rows, not an error"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 15: Backfill path — CREATE FULLTEXT INDEX on pre-existing rows
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_backfill_existing_rows() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    // Insert rows FIRST, then create the index → triggers backfill
    v.execute_sql("INSERT INTO t VALUES (1, 'Rust systems programming');")
        .unwrap();
    v.execute_sql("INSERT INTO t VALUES (2, 'Python scripting basics');")
        .unwrap();
    v.execute_sql("INSERT INTO t VALUES (3, 'Go concurrency patterns');")
        .unwrap();
    v.execute_sql("CREATE FULLTEXT INDEX idx_t ON t (body);")
        .unwrap();

    // All pre-existing rows must be indexed
    let r = rows_from(
        v.execute_sql("SELECT body FROM t WHERE t MATCH 'rust';")
            .unwrap(),
    );
    assert_eq!(r.len(), 1);

    let r2 = rows_from(
        v.execute_sql("SELECT body FROM t WHERE t MATCH 'scripting';")
            .unwrap(),
    );
    assert_eq!(r2.len(), 1);
    assert_eq!(r2[0][0], text("Python scripting basics"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 16: Fastsearch Compatibility Mock Test
// Tests Chinese word segmentation, text intersection scoring, and query
// deduplication similar to fastsearch's index_test.go and word_test.go.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn test_fts_fastsearch_mock_data() {
    let mut v = vm();
    v.execute_sql("CREATE TABLE toutiao (id INTEGER PRIMARY KEY, title TEXT);")
        .unwrap();

    // Insert fastsearch test strings
    v.execute_sql(
        "INSERT INTO toutiao VALUES (1, '想在西安买房投资，哪个区域比较好，最好有具体楼盘？');",
    )
    .unwrap();
    v.execute_sql(
        "INSERT INTO toutiao VALUES (2, '西安本地有哪些特色美食，肉夹馍和羊肉泡馍哪家强？');",
    )
    .unwrap();
    v.execute_sql("INSERT INTO toutiao VALUES (3, '最新A股行情分析，投资哪类股票比较好？');")
        .unwrap();
    v.execute_sql("INSERT INTO toutiao VALUES (4, '深圳北站附近的区域和博物馆，值得投资吗？');")
        .unwrap();

    v.execute_sql("CREATE FULLTEXT INDEX idx_toutiao ON toutiao (title);")
        .unwrap();

    // Query 1: "西安 买房" -> Matches rows 1 and 2. Row 1 has both tokens ("西安", "买房"), row 2 only has "西安".
    let r1 = rows_from(
        v.execute_sql("SELECT title FROM toutiao WHERE toutiao MATCH '西安 买房';")
            .unwrap(),
    );
    assert_eq!(
        r1.len(),
        2,
        "Expected 2 matches for '西安 买房' due to OR semantics"
    );
    assert_eq!(
        r1[0][0],
        text("想在西安买房投资，哪个区域比较好，最好有具体楼盘？"),
        "Row with BOTH terms must rank first"
    );

    // Query 2: "投资 比较好" -> Matches rows 1, 3, 4.
    // Row 1 and 3 have both terms. Row 4 only has "投资".
    let r2 = rows_from(
        v.execute_sql("SELECT title FROM toutiao WHERE toutiao MATCH '投资 比较好';")
            .unwrap(),
    );
    assert_eq!(r2.len(), 3, "Expected 3 matches for '投资 比较好'");

    // We expect docs that contain both tokens to score higher than docs with only one.
    let first_result = r2[0][0].clone();
    assert!(
        first_result == text("想在西安买房投资，哪个区域比较好，最好有具体楼盘？")
            || first_result == text("最新A股行情分析，投资哪类股票比较好？"),
        "A row with both '投资' and '比较好' should be ranked first"
    );

    // The union of multiple tokens works identically to fastsearch's MultiSearch intersection.
    // Query 3: "博物馆 深圳北" -> Matches row 4. Uses terms directly from fastsearch's word_test.go.
    let r3 = rows_from(
        v.execute_sql("SELECT title FROM toutiao WHERE toutiao MATCH '博物馆 深圳北';")
            .unwrap(),
    );
    assert_eq!(r3.len(), 1, "Expected 1 match for '博物馆 深圳北'");
    assert_eq!(r3[0][0], text("深圳北站附近的区域和博物馆，值得投资吗？"));
}
