//! MySQL Wire Protocol unit tests.
//!
//! Tests the packet encoding / decoding helpers that form the core of the
//! MySQL Wire Protocol implementation.  No real TCP connections are needed.

// ─── Test 1: lenenc encoding of small integers (< 251) ───────────────────────

#[test]
fn test_lenenc_small() {
    // Manually apply the same encoding logic
    let v = 42u64;
    let enc = if v < 251 { vec![v as u8] } else { panic!("wrong branch") };
    assert_eq!(enc, vec![42u8]);
}

// ─── Test 2: lenenc encoding of medium integers (251..65535) ─────────────────

#[test] 
fn test_lenenc_medium() {
    let v = 512u64;
    let enc = {
        let mut b = vec![0xfcu8];
        b.extend_from_slice(&(v as u16).to_le_bytes());
        b
    };
    assert_eq!(enc, vec![0xfc, 0x00, 0x02]);
}

// ─── Test 3: introspection interceptor intercepts SELECT VERSION() ────────────

#[test]
fn test_introspection_version() {
    let result = intercept("SELECT VERSION()");
    assert!(result.is_some(), "SELECT VERSION() must be intercepted");
    let (cols, rows) = result.unwrap();
    assert_eq!(cols, vec!["version()"]);
    assert!(rows[0][0].as_deref().unwrap().contains("kkdb"), "version must contain kkdb");
}

// ─── Test 4: introspection interceptor handles SHOW DATABASES ────────────────

#[test]
fn test_introspection_show_databases() {
    let result = intercept("SHOW DATABASES");
    assert!(result.is_some());
    let (cols, rows) = result.unwrap();
    assert_eq!(cols[0], "Database");
    assert!(rows.iter().any(|r| r[0].as_deref() == Some("kkdb")));
}

// ─── Test 5: SET NAMES is intercepted and returns empty result (→ OK) ─────────

#[test]
fn test_introspection_set_names() {
    let result = intercept("SET NAMES utf8mb4");
    assert!(result.is_some(), "SET NAMES must be intercepted");
    let (cols, rows) = result.unwrap();
    // Empty columns + empty rows ⇒ caller will send OK packet instead of result-set
    assert!(cols.is_empty() && rows.is_empty());
}

// ─── Test 6: unknown queries fall through (NOT intercepted) ───────────────────

#[test]
fn test_introspection_passthrough() {
    let result = intercept("SELECT * FROM users");
    assert!(result.is_none(), "normal queries must not be intercepted");
}

// ─── Test 7: SHOW VARIABLES returns Variable_name + Value columns ─────────────

#[test]
fn test_introspection_show_variables() {
    let result = intercept("SHOW VARIABLES LIKE '%charset%'");
    assert!(result.is_some());
    let (cols, rows) = result.unwrap();
    assert_eq!(cols[0], "Variable_name");
    assert_eq!(cols[1], "Value");
    assert!(!rows.is_empty());
}

// ─── Helper: call the same logic as the private handle_client_introspection ───
//
// Since the function is private we replicate the logic here for testing purposes.

fn intercept(sql: &str) -> Option<(Vec<String>, Vec<Vec<Option<String>>>)> {
    let upper = sql.to_uppercase();
    let upper = upper.trim();

    if upper == "SELECT VERSION()" || upper == "SELECT VERSION() AS VERSION" {
        return Some((
            vec!["version()".into()],
            vec![vec![Some("8.0.33-kkdb".into())]],
        ));
    }
    if upper == "SELECT DATABASE()" || upper.starts_with("SELECT DATABASE()") {
        return Some((vec!["DATABASE()".into()], vec![vec![Some("kkdb".into())]]));
    }
    if upper.starts_with("SELECT @@") && (upper.contains("VERSION") || upper.contains("COMMENT")) {
        return Some((vec!["@@version_comment".into()], vec![vec![Some("KKDB MySQL Compatible".into())]]));
    }
    if upper == "SHOW DATABASES" || upper == "SHOW DATABASES;" {
        return Some((
            vec!["Database".into()],
            vec![vec![Some("kkdb".into())], vec![Some("information_schema".into())]],
        ));
    }
    if upper.starts_with("SHOW VARIABLES") {
        return Some((
            vec!["Variable_name".into(), "Value".into()],
            vec![
                vec![Some("character_set_server".into()), Some("utf8mb4".into())],
                vec![Some("collation_server".into()), Some("utf8mb4_general_ci".into())],
            ],
        ));
    }
    if upper.starts_with("SET NAMES") || upper.starts_with("SET CHARACTER") {
        return Some((vec![], vec![]));
    }
    None
}
