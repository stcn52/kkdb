//! Integration tests for the KKDB HTTP REST API.
//!
//! Tests the Supabase-style auth flow:
//!   signup -> signin -> query (JWT) -> refresh -> apikey -> query (API key)
//!
//! Run with: cargo test http_api --test http_api_tests

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use kkdb::server::http_api::{build_router, AppState};
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`

// ─── Helper utilities ─────────────────────────────────────────────────────────

/// Create a shared in-memory AppState for tests.
/// All router instances built from it share the same auth_vm and user_vms map.
fn test_state() -> AppState {
    AppState::in_memory()
}

/// Build a router from a SHARED AppState.
/// Multiple calls to this with the same `state` all see the same user VMs.
fn make_router(state: &AppState) -> axum::Router {
    build_router(state.clone())
}

async fn post_json(
    router: &axum::Router,
    path: &str,
    body: Value,
    auth_header: Option<(&str, &str)>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("Content-Type", "application/json");
    if let Some((name, val)) = auth_header {
        builder = builder.header(name, val);
    }
    let req = builder.body(Body::from(body.to_string())).unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        // Show the raw body in test failures for debugging
        json!({"__raw": String::from_utf8_lossy(&bytes).to_string()})
    });
    (status, json)
}

// 鈹€鈹€鈹€ Tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[tokio::test]
async fn test_health() {
    let state = test_state();
    let router = make_router(&state);
    let req = Request::builder()
        .method("GET")
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_signup_and_signin() {
    let state = test_state();
    let router = make_router(&state);

    // Signup
    let (status, body) = post_json(
        &router,
        "/auth/signup",
        json!({"email": "alice@test.com", "password": "p@ssw0rd"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup failed: {body}");
    assert!(body["token"].is_string(), "no token in signup response");
    let token = body["token"].as_str().unwrap().to_string();

    // Inline router rebuild with same VM so state is shared
    let router2 = make_router(&state);

    // Signin
    let (status2, body2) = post_json(
        &router2,
        "/auth/signin",
        json!({"email": "alice@test.com", "password": "p@ssw0rd"}),
        None,
    )
    .await;
    assert_eq!(status2, StatusCode::OK, "signin failed: {body2}");
    assert!(body2["token"].is_string());

    // Signin with wrong password
    let router3 = make_router(&state);
    let (status3, _) = post_json(
        &router3,
        "/auth/signin",
        json!({"email": "alice@test.com", "password": "wrong"}),
        None,
    )
    .await;
    assert_eq!(status3, StatusCode::UNAUTHORIZED);

    // Duplicate signup should return 409
    let router4 = make_router(&state);
    let (status4, _) = post_json(
        &router4,
        "/auth/signup",
        json!({"email": "alice@test.com", "password": "another"}),
        None,
    )
    .await;
    assert_eq!(status4, StatusCode::CONFLICT);

    // Refresh
    let router5 = make_router(&state);
    let req = Request::builder()
        .method("POST")
        .uri("/auth/refresh")
        .header("Authorization", format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();
    let resp = router5.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_query_with_jwt() {
    let state = test_state();

    // Signup to get a token
    let router = make_router(&state);
    let (_, signup_body) = post_json(
        &router,
        "/auth/signup",
        json!({"email": "bob@test.com", "password": "secret"}),
        None,
    )
    .await;
    let token = signup_body["token"].as_str().unwrap().to_string();
    let auth = ("Authorization", format!("Bearer {}", token));

    // Create a table + INSERT via query endpoint (all go to user's VM)
    let router2 = make_router(&state);
    let (s, _) = post_json(
        &router2,
        "/rest/query",
        json!({"sql": "CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, content TEXT)"}),
        Some((&auth.0, &auth.1)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let router3 = make_router(&state);
    let (s2, _) = post_json(
        &router3,
        "/rest/execute",
        json!({"sql": "INSERT INTO notes (id, content) VALUES (1, 'hello')"}),
        Some((&auth.0, &auth.1)),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);

    // SELECT
    let router4 = make_router(&state);
    let (s3, qbody) = post_json(
        &router4,
        "/rest/query",
        json!({"sql": "SELECT * FROM notes"}),
        Some((&auth.0, &auth.1)),
    )
    .await;
    assert_eq!(s3, StatusCode::OK);
    assert!(qbody["columns"].is_array());
    assert!(!qbody["rows"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_rls_isolation() {
    let state = test_state();

    // Register user1
    let r = make_router(&state);
    let (_, b1) = post_json(
        &r,
        "/auth/signup",
        json!({"email": "user1@test.com", "password": "pw1"}),
        None,
    )
    .await;
    let token1 = b1["token"].as_str().unwrap().to_string();
    let auth1 = ("Authorization", format!("Bearer {}", token1));
    let uid1 = b1["user_id"].as_str().unwrap().to_string();

    // Register user2
    let r2 = make_router(&state);
    let (_, b2) = post_json(
        &r2,
        "/auth/signup",
        json!({"email": "user2@test.com", "password": "pw2"}),
        None,
    )
    .await;
    let token2 = b2["token"].as_str().unwrap().to_string();
    let auth2 = ("Authorization", format!("Bearer {}", token2));
    let uid2 = b2["user_id"].as_str().unwrap().to_string();

    // user1 creates their own items table + inserts their data via HTTP API
    // (goes into user1's isolated VM)
    let r3 = make_router(&state);
    let _ = post_json(
        &r3,
        "/rest/batch",
        json!({
            "statements": [
                "CREATE TABLE items (id INTEGER, owner_id TEXT, name TEXT)",
                "ALTER TABLE items ENABLE ROW LEVEL SECURITY",
                "CREATE POLICY owner_only ON items USING (owner_id = auth.uid())",
                format!("INSERT INTO items VALUES (1, '{}', 'item_of_user1')", uid1),
            ]
        }),
        Some((&auth1.0, &auth1.1)),
    )
    .await;

    // user2 creates their own items table + inserts their data via HTTP API
    // (goes into user2's isolated VM — completely separate DB)
    let r4 = make_router(&state);
    let _ = post_json(
        &r4,
        "/rest/batch",
        json!({
            "statements": [
                "CREATE TABLE items (id INTEGER, owner_id TEXT, name TEXT)",
                format!("INSERT INTO items VALUES (2, '{}', 'item_of_user2')", uid2),
            ]
        }),
        Some((&auth2.0, &auth2.1)),
    )
    .await;

    // user1 query -> can only see their own item
    let r5 = make_router(&state);
    let (_, qb1) = post_json(
        &r5,
        "/rest/query",
        json!({"sql": "SELECT * FROM items"}),
        Some((&auth1.0, &auth1.1)),
    )
    .await;
    let rows1 = qb1["rows"].as_array().unwrap();
    assert_eq!(rows1.len(), 1, "user1 should see only their own row");
    assert!(rows1[0][2].as_str().unwrap().contains("user1"));

    // user2 query -> can only see their own item (DB isolation)
    let r6 = make_router(&state);
    let (_, qb2) = post_json(
        &r6,
        "/rest/query",
        json!({"sql": "SELECT * FROM items"}),
        Some((&auth2.0, &auth2.1)),
    )
    .await;
    let rows2 = qb2["rows"].as_array().unwrap();
    assert_eq!(rows2.len(), 1, "user2 should see only their own row");
    assert!(rows2[0][2].as_str().unwrap().contains("user2"));
}

#[tokio::test]
async fn test_missing_auth() {
    let state = test_state();
    let router = make_router(&state);
    let (status, _) = post_json(
        &router,
        "/rest/query",
        json!({"sql": "SELECT 1"}),
        None, // no auth header
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// 鈹€鈹€鈹€ Batch tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[tokio::test]
async fn test_batch_basic() {
    let state = test_state();

    // Signup to get a token
    let r = make_router(&state);
    let (_, sb) = post_json(
        &r,
        "/auth/signup",
        json!({"email": "batch@test.com", "password": "pw"}),
        None,
    )
    .await;
    let token = sb["token"].as_str().unwrap().to_string();
    let auth = ("Authorization", format!("Bearer {}", token));

    // Run a batch: CREATE + INSERT 脳2 + SELECT (all in user's isolated VM)
    let r2 = make_router(&state);
    let (status, body) = post_json(
        &r2,
        "/rest/batch",
        json!({
            "statements": [
                "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price REAL)",
                "INSERT INTO products VALUES (1, 'apple', 0.5)",
                "INSERT INTO products VALUES (2, 'banana', 0.3)",
                "SELECT * FROM products ORDER BY id"
            ]
        }),
        Some((&auth.0, &auth.1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "batch failed: {body}");

    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 4);
    assert_eq!(results[0]["status"], "ok");
    assert_eq!(results[1]["status"], "ok");
    assert_eq!(results[2]["status"], "ok");
    assert_eq!(results[3]["status"], "ok");

    // Last result is SELECT 鈥?should have 2 rows
    let rows = results[3]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(results[3]["columns"].as_array().unwrap()[1], "name");

    assert_eq!(body["count"], 4);
    assert_eq!(body["succeeded"], 4);
    assert_eq!(body["failed_at"], serde_json::Value::Null);
}

#[tokio::test]
async fn test_batch_transaction_rollback() {
    let state = test_state();

    let r = make_router(&state);
    let (_, sb) = post_json(
        &r,
        "/auth/signup",
        json!({"email": "tx@test.com", "password": "pw"}),
        None,
    )
    .await;
    let token = sb["token"].as_str().unwrap().to_string();
    let auth = ("Authorization", format!("Bearer {}", token));

    // Set up table + initial row via HTTP API (goes to user's VM)
    let r2 = make_router(&state);
    let _ = post_json(
        &r2,
        "/rest/batch",
        json!({
            "statements": [
                "CREATE TABLE tx_test (id INTEGER PRIMARY KEY, val TEXT)",
                "INSERT INTO tx_test VALUES (1, 'original')"
            ]
        }),
        Some((&auth.0, &auth.1)),
    )
    .await;

    // Batch with transaction: INSERT ok 鈫?bad SQL 鈫?should rollback
    let r3 = make_router(&state);
    let (status, body) = post_json(
        &r3,
        "/rest/batch",
        json!({
            "statements": [
                "INSERT INTO tx_test VALUES (2, 'new')",
                "THIS IS NOT SQL",
                "INSERT INTO tx_test VALUES (3, 'also_new')"
            ],
            "transaction": true
        }),
        Some((&auth.0, &auth.1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let results = body["results"].as_array().unwrap();
    // Statement 0 ok, statement 1 error, statement 2 skipped
    assert_eq!(results[0]["status"], "ok");
    assert_eq!(results[1]["status"], "error");
    assert_eq!(results[2]["status"], "error");
    assert!(results[2]["error"].as_str().unwrap().contains("skipped"));
    assert_eq!(body["failed_at"], 1);
    assert_eq!(body["succeeded"], 1);

    // Verify rollback: table should still have only row 1
    let r4 = make_router(&state);
    let (_, qb) = post_json(
        &r4,
        "/rest/query",
        json!({"sql": "SELECT * FROM tx_test"}),
        Some((&auth.0, &auth.1)),
    )
    .await;
    // After rollback, id=2 should not exist (only original row 1)
    let rows = qb["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "rollback should have reverted the INSERT");
}

#[tokio::test]
async fn test_batch_missing_auth() {
    let state = test_state();
    let router = make_router(&state);
    let (status, _) = post_json(
        &router,
        "/rest/batch",
        json!({"statements": ["SELECT 1"]}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// 鈹€鈹€鈹€ Bulk write tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[tokio::test]
async fn test_bulk_write_basic() {
    let state = test_state();

    let r = make_router(&state);
    let (_, sb) = post_json(
        &r,
        "/auth/signup",
        json!({"email": "bulk@test.com", "password": "pw"}),
        None,
    )
    .await;
    let token = sb["token"].as_str().unwrap().to_string();
    let auth = ("Authorization", format!("Bearer {}", token));

    // Pre-create table via HTTP API (lands in user's isolated VM)
    let r2 = make_router(&state);
    let _ = post_json(
        &r2,
        "/rest/execute",
        json!({"sql": "CREATE TABLE events (id INTEGER PRIMARY KEY, name TEXT, score REAL)"}),
        Some((&auth.0, &auth.1)),
    )
    .await;

    // Bulk insert 3 rows in one multi-row INSERT (default: transaction=true, bulk_insert=true)
    let r3 = make_router(&state);
    let (status, body) = post_json(
        &r3,
        "/rest/bulk",
        json!({
            "table": "events",
            "rows": [
                {"id": 1, "name": "alpha", "score": 9.5},
                {"id": 2, "name": "beta",  "score": 8.0},
                {"id": 3, "name": "gamma", "score": 7.2}
            ]
        }),
        Some((&auth.0, &auth.1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bulk failed: {body}");
    assert_eq!(body["rows_written"], 3);
    assert_eq!(body["error"], serde_json::Value::Null);
    assert!(body["transaction"].as_bool().unwrap());

    // Verify all rows exist
    let r4 = make_router(&state);
    let (_, qb) = post_json(
        &r4,
        "/rest/query",
        json!({"sql": "SELECT * FROM events ORDER BY id"}),
        Some((&auth.0, &auth.1)),
    )
    .await;
    let rows = qb["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][1].as_str().unwrap(), "alpha");
    assert_eq!(rows[2][1].as_str().unwrap(), "gamma");
}

#[tokio::test]
async fn test_bulk_write_rollback_on_dup() {
    let state = test_state();

    let r = make_router(&state);
    let (_, sb) = post_json(
        &r,
        "/auth/signup",
        json!({"email": "bulkrb@test.com", "password": "pw"}),
        None,
    )
    .await;
    let token = sb["token"].as_str().unwrap().to_string();
    let auth = ("Authorization", format!("Bearer {}", token));

    // Pre-create table + seed data via HTTP API (goes to user's VM)
    let r2 = make_router(&state);
    let _ = post_json(
        &r2,
        "/rest/batch",
        json!({
            "statements": [
                "CREATE TABLE catalog (id INTEGER PRIMARY KEY, sku TEXT)",
                "INSERT INTO catalog VALUES (2, 'existing')"
            ]
        }),
        Some((&auth.0, &auth.1)),
    )
    .await;

    // Bulk with transaction=true, individual mode so we can get a partial-write scenario
    let r3 = make_router(&state);
    let (status, body) = post_json(
        &r3,
        "/rest/bulk",
        json!({
            "table":       "catalog",
            "bulk_insert": false,
            "transaction": true,
            "rows": [
                {"id": 1,  "sku": "new"},
                {"id": 2,  "sku": "dup"},
                {"id": 3,  "sku": "also"}
            ]
        }),
        Some((&auth.0, &auth.1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["error"].is_string(), "expected an error field");

    // Verify rollback: table should still have only the original row (id=2)
    let r4 = make_router(&state);
    let (_, qb) = post_json(
        &r4,
        "/rest/query",
        json!({"sql": "SELECT * FROM catalog"}),
        Some((&auth.0, &auth.1)),
    )
    .await;
    let rows = qb["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "rollback must revert all bulk writes");
    assert_eq!(rows[0][0], 2);
}

#[tokio::test]
async fn test_bulk_write_missing_auth() {
    let state = test_state();
    let router = make_router(&state);
    let (status, _) = post_json(
        &router,
        "/rest/bulk",
        json!({"table": "t", "rows": [{"id": 1}]}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
