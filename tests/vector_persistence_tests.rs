//! Phase 3 — Vector index persistence tests.
//!
//! These tests verify that vector indexes survive VM restart (open → close → reopen),
//! that DML auto-maintenance (INSERT / UPDATE / DELETE) correctly updates the HNSW graph,
//! and that `VEC_SEARCH` returns correct results in all scenarios.
//!
//! ## VEC_SEARCH pattern
//!
//! VEC_SEARCH is a per-row *scalar* function that returns the similarity score between
//! the row's stored vector and the query vector:
//!
//! ```sql
//! SELECT id, VEC_SEARCH('table', 'index', <query_blob>) AS score
//! FROM table
//! ORDER BY score DESC
//! LIMIT k
//! ```

use kkdb::vm::execute::{ExecResult, VM};
use kkdb::types::Value;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Encode a small f32 slice as a VEC() blob expression the SQL parser can read.
fn vec_expr(v: &[f32]) -> String {
    let inner: Vec<String> = v.iter().map(|x| x.to_string()).collect();
    format!("VEC('[{}]')", inner.join(","))
}

/// Run SQL and unwrap the result.
fn run(vm: &mut VM, sql: &str) -> ExecResult {
    vm.execute_sql(sql)
        .unwrap_or_else(|e| panic!("SQL failed:\n  {sql}\nError: {e}"))
}

/// Run a SELECT and return all rows.
fn query_rows(vm: &mut VM, sql: &str) -> Vec<Vec<Value>> {
    match run(vm, sql) {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("Expected QueryResult for:\n  {sql}\nGot: {other:?}"),
    }
}

/// Return the id (column 0) of the top VEC_SEARCH result for `query` in `table`/`index`.
fn top_id(vm: &mut VM, table: &str, index: &str, query: &[f32]) -> i64 {
    let qv = vec_expr(query);
    let vs = format!("VEC_SEARCH('{table}', '{index}', {qv})");
    // Use the full expression in ORDER BY (not the alias 'score') because ORDER BY aliases
    // are not resolved in the pre-projection sort path when no GROUP BY exists.
    let sql = format!("SELECT id, {vs} AS score FROM {table} ORDER BY {vs} DESC LIMIT 1");
    let rows = query_rows(vm, &sql);
    assert!(!rows.is_empty(), "VEC_SEARCH returned no rows for query {query:?}");
    match &rows[0][0] {
        Value::Integer(id) => *id,
        other => panic!("Expected Integer id in col[0], got: {other:?}"),
    }
}

/// Temporary directory for an on-disk DB, wiped on construction and on drop.
struct TmpDb {
    path: std::path::PathBuf,
}

impl TmpDb {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("kkdb_vectest_{name}"));
        if path.exists() {
            std::fs::remove_dir_all(&path).unwrap();
        }
        Self { path }
    }

    fn path_str(&self) -> &str {
        self.path.to_str().unwrap()
    }
}

impl Drop for TmpDb {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

// ── In-memory tests ───────────────────────────────────────────────────────────

/// CREATE VECTOR INDEX + VEC_SEARCH works correctly in memory.
#[test]
fn test_create_vector_index_basic() {
    let mut vm = VM::new_memory();
    run(&mut vm, "CREATE TABLE docs (id INTEGER PRIMARY KEY, emb BLOB)");
    run(&mut vm, &format!(
        "INSERT INTO docs VALUES (1, {e1}), (2, {e2}), (3, {e3})",
        e1 = vec_expr(&[1.0, 0.0, 0.0]),
        e2 = vec_expr(&[0.0, 1.0, 0.0]),
        e3 = vec_expr(&[0.0, 0.0, 1.0]),
    ));
    run(&mut vm, "CREATE VECTOR INDEX idx_emb ON docs(emb) DIM 3 DISTANCE COSINE");

    assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[1.0, 0.0, 0.0]), 1);
    assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[0.0, 1.0, 0.0]), 2);
    assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[0.0, 0.0, 1.0]), 3);
}

/// INSERT after CREATE VECTOR INDEX automatically updates the HNSW graph.
#[test]
fn test_insert_auto_maintenance() {
    let mut vm = VM::new_memory();
    run(&mut vm, "CREATE TABLE docs (id INTEGER PRIMARY KEY, emb BLOB)");
    run(&mut vm, "CREATE VECTOR INDEX idx_emb ON docs(emb) DIM 3 DISTANCE COSINE");

    // Insert AFTER index creation — maintenance hook must add them.
    run(&mut vm, &format!("INSERT INTO docs VALUES (1, {})", vec_expr(&[1.0, 0.0, 0.0])));
    run(&mut vm, &format!("INSERT INTO docs VALUES (2, {})", vec_expr(&[0.0, 1.0, 0.0])));

    assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[1.0, 0.01, 0.0]), 1);
    assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[0.01, 1.0, 0.0]), 2);
}

/// UPDATE auto-maintenance: old vector removed, new one inserted.
#[test]
fn test_update_auto_maintenance() {
    let mut vm = VM::new_memory();
    run(&mut vm, "CREATE TABLE docs (id INTEGER PRIMARY KEY, emb BLOB)");
    run(&mut vm, &format!(
        "INSERT INTO docs VALUES (1, {e1}), (2, {e2})",
        e1 = vec_expr(&[1.0, 0.0, 0.0]),
        e2 = vec_expr(&[0.0, 1.0, 0.0]),
    ));
    run(&mut vm, "CREATE VECTOR INDEX idx_emb ON docs(emb) DIM 3 DISTANCE COSINE");

    // Verify baseline: rowid 1 closest to [1,0,0]
    assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[1.0, 0.0, 0.0]), 1);

    // Move rowid 1's embedding to [0,0,1] direction
    run(&mut vm, &format!(
        "UPDATE docs SET emb = {} WHERE id = 1",
        vec_expr(&[0.0, 0.0, 1.0])
    ));

    // After update: rowid 2 ([0,1,0]) should be *closer* to [1,0,0] than rowid 1 ([0,0,1])
    // because cosine([1,0,0],[0,1,0]) = 0 > cosine([1,0,0],[0,0,1]) = 0 — they're equal here,
    // so just check that rowid 1 is NOT the only result (it may tie but shouldn't dominate).
    // A more decisive test: query along the new direction [0,0,1] → rowid 1 should win.
    assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[0.0, 0.0, 1.0]), 1,
        "After update rowid 1 should be closest to its new direction [0,0,1]");
}

/// DELETE auto-maintenance: deleted rows must not appear in VEC_SEARCH results.
#[test]
fn test_delete_auto_maintenance() {
    let mut vm = VM::new_memory();
    run(&mut vm, "CREATE TABLE docs (id INTEGER PRIMARY KEY, emb BLOB)");
    run(&mut vm, &format!(
        "INSERT INTO docs VALUES (1, {e1}), (2, {e2})",
        e1 = vec_expr(&[1.0, 0.0, 0.0]),
        e2 = vec_expr(&[0.0, 1.0, 0.0]),
    ));
    run(&mut vm, "CREATE VECTOR INDEX idx_emb ON docs(emb) DIM 3 DISTANCE COSINE");

    // rowid 1 is best for [1,0,0]
    assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[1.0, 0.0, 0.0]), 1);

    run(&mut vm, "DELETE FROM docs WHERE id = 1");

    // After delete, querying [1,0,0] should NOT return rowid 1 (only rowid 2 remains)
    let id = top_id(&mut vm, "docs", "idx_emb", &[1.0, 0.0, 0.0]);
    assert_ne!(id, 1, "Deleted rowid 1 must not appear in VEC_SEARCH results");
    assert_eq!(id, 2, "Only rowid 2 should remain");
}

/// IF NOT EXISTS handles duplicate index gracefully.
#[test]
fn test_create_vector_index_if_not_exists() {
    let mut vm = VM::new_memory();
    run(&mut vm, "CREATE TABLE t (id INTEGER PRIMARY KEY, v BLOB)");
    run(&mut vm, "CREATE VECTOR INDEX IF NOT EXISTS idx_v ON t(v) DIM 2");
    // Second call with IF NOT EXISTS — must not error
    run(&mut vm, "CREATE VECTOR INDEX IF NOT EXISTS idx_v ON t(v) DIM 2");
}

/// L2 distance: nearest Euclidean neighbour is returned.
#[test]
fn test_l2_distance_index() {
    let mut vm = VM::new_memory();
    run(&mut vm, "CREATE TABLE pts (id INTEGER PRIMARY KEY, v BLOB)");
    run(&mut vm, &format!(
        "INSERT INTO pts VALUES (1, {e1}), (2, {e2}), (3, {e3})",
        e1 = vec_expr(&[0.0, 0.0]),
        e2 = vec_expr(&[1.0, 0.0]),
        e3 = vec_expr(&[10.0, 0.0]),
    ));
    run(&mut vm, "CREATE VECTOR INDEX idx_l2 ON pts(v) DIM 2 DISTANCE L2");

    // [0.9, 0.0] → nearest L2 is rowid 2 ([1.0, 0.0], distance=0.1)
    assert_eq!(top_id(&mut vm, "pts", "idx_l2", &[0.9, 0.0]), 2);
}

// ── On-disk persistence tests ─────────────────────────────────────────────────

/// Index survives VM restart: open → insert + create index → close → reopen → query.
#[test]
fn test_vector_index_survives_restart() {
    let tmp = TmpDb::new("restart");

    // Session 1: create table, insert rows, create vector index
    {
        let mut vm = VM::open(tmp.path_str()).unwrap();
        run(&mut vm, "CREATE TABLE docs (id INTEGER PRIMARY KEY, emb BLOB)");
        run(&mut vm, &format!(
            "INSERT INTO docs VALUES (1, {e1}), (2, {e2}), (3, {e3})",
            e1 = vec_expr(&[1.0, 0.0, 0.0]),
            e2 = vec_expr(&[0.0, 1.0, 0.0]),
            e3 = vec_expr(&[0.0, 0.0, 1.0]),
        ));
        run(&mut vm, "CREATE VECTOR INDEX idx_emb ON docs(emb) DIM 3 DISTANCE COSINE");
    } // VM dropped → auto_flush writes all pages

    // Session 2: reopen and verify VEC_SEARCH works via the rebuilt HNSW
    {
        let mut vm = VM::open(tmp.path_str()).unwrap();
        assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[1.0, 0.0, 0.0]), 1,
            "After restart, closest to [1,0,0] should be rowid 1");
        assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[0.0, 0.0, 1.0]), 3,
            "After restart, closest to [0,0,1] should be rowid 3");
    }
}

/// New rows inserted after restart are automatically added to the rebuilt HNSW.
#[test]
fn test_vector_index_insert_after_restart() {
    let tmp = TmpDb::new("insert_after_restart");

    // Session 1: table + one row + index
    {
        let mut vm = VM::open(tmp.path_str()).unwrap();
        run(&mut vm, "CREATE TABLE docs (id INTEGER PRIMARY KEY, emb BLOB)");
        run(&mut vm, &format!("INSERT INTO docs VALUES (1, {})", vec_expr(&[1.0, 0.0, 0.0])));
        run(&mut vm, "CREATE VECTOR INDEX idx_emb ON docs(emb) DIM 3 DISTANCE COSINE");
    }

    // Session 2: insert a second row and verify both are searchable
    {
        let mut vm = VM::open(tmp.path_str()).unwrap();
        run(&mut vm, &format!("INSERT INTO docs VALUES (2, {})", vec_expr(&[0.0, 1.0, 0.0])));

        assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[1.0, 0.0, 0.0]), 1);
        assert_eq!(top_id(&mut vm, "docs", "idx_emb", &[0.0, 1.0, 0.0]), 2,
            "Newly inserted rowid 2 should be closest to [0,1,0]");
    }
}
