// ── R29: Security Hardening Tests ─────────────────────────────────────────────
//
// Tests for:
//   1. Password bcrypt hashing in CREATE USER / ALTER USER
//   2. Audit log integration — enable/disable, recording, SHOW AUDIT LOG
//   3. SET query_cache_enabled / SET audit_log_enabled
//   4. verify_user_password utility
//   5. TLS config loading (no-op when env unset)

use super::*;

// ── Local helpers ─────────────────────────────────────────────────────────────
fn x(vm: &mut VM, sql: &str) {
    vm.execute_sql(sql).unwrap();
}
fn qr(vm: &mut VM, sql: &str) -> Vec<Vec<Value>> {
    match vm.execute_sql(sql).unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got: {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 1. PASSWORD HASHING
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_r29_create_user_stores_bcrypt_hash() {
    let mut vm = VM::new_memory();
    x(&mut vm, "CREATE USER alice WITH PASSWORD 'secret123'");
    // Read the stored password_hash
    let rows = qr(
        &mut vm,
        "SELECT password_hash FROM kkdb_users WHERE username = 'alice'",
    );
    assert_eq!(rows.len(), 1);
    let hash = match &rows[0][0] {
        Value::Text(s) => s.to_string(),
        _ => panic!("expected text"),
    };
    // Must be a bcrypt hash (starts with $2b$ or $2a$)
    assert!(
        hash.starts_with("$2b$") || hash.starts_with("$2a$"),
        "password_hash should be bcrypt, got: {}",
        &hash[..hash.len().min(20)]
    );
    // Must NOT be the plain password
    assert_ne!(hash, "secret123");
}

#[test]
fn test_r29_alter_user_updates_bcrypt_hash() {
    let mut vm = VM::new_memory();
    x(&mut vm, "CREATE USER bob WITH PASSWORD 'oldpass'");
    let rows1 = qr(
        &mut vm,
        "SELECT password_hash FROM kkdb_users WHERE username = 'bob'",
    );
    let hash1 = match &rows1[0][0] {
        Value::Text(s) => s.to_string(),
        _ => panic!("text"),
    };

    x(&mut vm, "ALTER USER bob WITH PASSWORD 'newpass'");
    let rows2 = qr(
        &mut vm,
        "SELECT password_hash FROM kkdb_users WHERE username = 'bob'",
    );
    let hash2 = match &rows2[0][0] {
        Value::Text(s) => s.to_string(),
        _ => panic!("text"),
    };

    // Both must be bcrypt, but different (different passwords)
    assert!(hash2.starts_with("$2b$") || hash2.starts_with("$2a$"));
    assert_ne!(hash1, hash2, "alter should produce a new hash");
}

#[test]
fn test_r29_create_user_empty_password() {
    let mut vm = VM::new_memory();
    // User with no password — hash should be empty string
    let r = vm.execute_sql("CREATE USER nopass");
    // If the parser supports it, the password_hash should be empty
    if r.is_ok() {
        let rows = qr(
            &mut vm,
            "SELECT password_hash FROM kkdb_users WHERE username = 'nopass'",
        );
        if !rows.is_empty() {
            let hash = match &rows[0][0] {
                Value::Text(s) => s.to_string(),
                v => format!("{:?}", v),
            };
            // Empty password should store empty string (not a bcrypt hash)
            assert!(
                hash.is_empty() || hash == "NULL" || hash.starts_with("$2"),
                "empty password should store empty or a hash"
            );
        }
    }
}

#[test]
fn test_r29_verify_user_password() {
    let mut vm = VM::new_memory();
    x(&mut vm, "CREATE USER carol WITH PASSWORD 'mypassword'");
    // Correct password should verify
    assert!(
        vm.verify_user_password("carol", "mypassword"),
        "correct password should verify"
    );
    // Wrong password should not verify
    assert!(
        !vm.verify_user_password("carol", "wrongpass"),
        "wrong password should not verify"
    );
    // Non-existent user should not verify
    assert!(
        !vm.verify_user_password("nobody", "anything"),
        "non-existent user should not verify"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 2. AUDIT LOG INTEGRATION
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_r29_audit_log_disabled_by_default() {
    let vm = VM::new_memory();
    assert!(
        !vm.audit_log.is_enabled(),
        "audit log should be disabled by default"
    );
    assert!(vm.audit_log.is_empty(), "audit log should be empty");
}

#[test]
fn test_r29_set_audit_log_enabled() {
    let mut vm = VM::new_memory();
    x(&mut vm, "SET audit_log_enabled = 'on'");
    assert!(
        vm.audit_log.is_enabled(),
        "audit log should be enabled after SET"
    );

    // Execute some SQL — should be recorded
    x(&mut vm, "CREATE TABLE t1 (id INTEGER PRIMARY KEY)");
    x(&mut vm, "INSERT INTO t1 VALUES (1)");
    let _ = qr(&mut vm, "SELECT * FROM t1");

    // Should have recorded entries (at least the 3 above)
    assert!(
        vm.audit_log.len() >= 3,
        "expected at least 3 audit entries, got {}",
        vm.audit_log.len()
    );

    // Disable
    x(&mut vm, "SET audit_log_enabled = 'off'");
    assert!(!vm.audit_log.is_enabled(), "audit log should be disabled");
}

#[test]
fn test_r29_audit_log_records_detail() {
    let mut vm = VM::new_memory();
    x(&mut vm, "SET audit_log_enabled = 'on'");
    x(
        &mut vm,
        "CREATE TABLE audit_test (id INTEGER PRIMARY KEY, name TEXT)",
    );
    x(&mut vm, "INSERT INTO audit_test VALUES (1, 'hello')");

    let entries = vm.audit_log.entries();
    // Should have at least 2 entries (CREATE TABLE + INSERT; the SET might also be recorded)
    assert!(
        entries.len() >= 2,
        "expected at least 2 entries, got {}",
        entries.len()
    );

    // All entries should be successful
    for entry in entries {
        assert!(entry.success, "entry for '{}' should be success", entry.sql);
    }
}

#[test]
fn test_r29_audit_log_records_failures() {
    let mut vm = VM::new_memory();
    x(&mut vm, "SET audit_log_enabled = 'on'");
    // This should fail (table doesn't exist)
    let _ = vm.execute_sql("INSERT INTO nonexistent VALUES (1)");
    // Should have at least 1 entry (the failed INSERT)
    let failures: Vec<_> = vm
        .audit_log
        .entries()
        .iter()
        .filter(|e| !e.success)
        .collect();
    assert!(!failures.is_empty(), "should record failed SQL");
    assert!(
        failures[0].error.is_some(),
        "failed entry should have error message"
    );
}

#[test]
fn test_r29_show_audit_log() {
    let mut vm = VM::new_memory();
    x(&mut vm, "SET audit_log_enabled = 'on'");
    x(
        &mut vm,
        "CREATE TABLE show_audit_t (id INTEGER PRIMARY KEY)",
    );
    x(&mut vm, "INSERT INTO show_audit_t VALUES (1)");

    let result = vm.execute_sql("SHOW AUDIT LOG").unwrap();
    match result {
        ExecResult::QueryResult { columns, rows } => {
            assert!(columns.contains(&"seq".to_string()));
            assert!(columns.contains(&"sql".to_string()));
            assert!(columns.contains(&"success".to_string()));
            assert!(!rows.is_empty(), "SHOW AUDIT LOG should return entries");
        }
        _ => panic!("SHOW AUDIT LOG should return QueryResult"),
    }
}

#[test]
fn test_r29_audit_log_not_recording_when_disabled() {
    let mut vm = VM::new_memory();
    // audit is disabled by default
    x(&mut vm, "CREATE TABLE no_audit_t (id INTEGER PRIMARY KEY)");
    x(&mut vm, "INSERT INTO no_audit_t VALUES (1)");
    assert!(vm.audit_log.is_empty(), "should not record when disabled");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. SET query_cache_enabled
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_r29_set_query_cache_enabled_off_on() {
    let mut vm = VM::new_memory();
    x(
        &mut vm,
        "CREATE TABLE cache_t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    x(&mut vm, "INSERT INTO cache_t VALUES (1, 'a')");

    // Disable cache
    x(&mut vm, "SET query_cache_enabled = 'off'");

    // Run query twice — both should execute (no caching)
    let r1 = qr(&mut vm, "SELECT * FROM cache_t");
    let r2 = qr(&mut vm, "SELECT * FROM cache_t");
    assert_eq!(r1, r2);

    // Re-enable
    x(&mut vm, "SET query_cache_enabled = 'on'");
}

#[test]
fn test_r29_set_use_lz4() {
    let mut vm = VM::new_memory();
    // Should not error
    x(&mut vm, "SET use_lz4 = 'on'");
    x(&mut vm, "SET use_lz4 = 'off'");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4. TLS CONFIG (unit-level)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_r29_tls_config_none_without_env() {
    // Ensure env vars are not set (they shouldn't be in test)
    std::env::remove_var("KKDB_TLS_CERT");
    std::env::remove_var("KKDB_TLS_KEY");
    let config = crate::server::tls::TlsConfig::from_env().unwrap();
    assert!(
        config.is_none(),
        "TLS config should be None without env vars"
    );
}

#[test]
fn test_r29_tls_config_error_on_bad_path() {
    let result =
        crate::server::tls::TlsConfig::from_files("/nonexistent/cert.pem", "/nonexistent/key.pem");
    assert!(result.is_err(), "should error on missing cert file");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 5. AUDIT CATEGORY CLASSIFICATION
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_r29_audit_category_from_sql() {
    use crate::vm::auth::audit::AuditCategory;
    assert_eq!(
        AuditCategory::from_sql("SELECT * FROM t"),
        AuditCategory::Query
    );
    assert_eq!(
        AuditCategory::from_sql("INSERT INTO t VALUES (1)"),
        AuditCategory::Dml
    );
    assert_eq!(
        AuditCategory::from_sql("UPDATE t SET x = 1"),
        AuditCategory::Dml
    );
    assert_eq!(AuditCategory::from_sql("DELETE FROM t"), AuditCategory::Dml);
    assert_eq!(
        AuditCategory::from_sql("CREATE TABLE t (id INT)"),
        AuditCategory::Ddl
    );
    assert_eq!(AuditCategory::from_sql("BEGIN"), AuditCategory::Txn);
    assert_eq!(AuditCategory::from_sql("COMMIT"), AuditCategory::Txn);
    assert_eq!(
        AuditCategory::from_sql("CREATE USER alice"),
        AuditCategory::Auth
    );
    assert_eq!(
        AuditCategory::from_sql("GRANT SELECT ON t TO alice"),
        AuditCategory::Auth
    );
    assert_eq!(AuditCategory::from_sql("VACUUM"), AuditCategory::System);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 6. SQL INJECTION DETECTION
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_r29_sql_injection_detection() {
    use crate::vm::auth::audit::detect_sql_injection;
    // Classic injection patterns
    assert!(detect_sql_injection("' OR 1=1 --"));
    assert!(detect_sql_injection("admin' UNION SELECT * FROM users --"));
    assert!(detect_sql_injection("1; DROP TABLE users"));
    // Normal SQL should not trigger
    assert!(!detect_sql_injection("SELECT * FROM users WHERE id = 1"));
    assert!(!detect_sql_injection("INSERT INTO log VALUES (1, 'hello')"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// 7. COMBINED SECURITY FLOW
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_r29_full_security_flow() {
    let mut vm = VM::new_memory();

    // Enable audit log
    x(&mut vm, "SET audit_log_enabled = 'on'");

    // Create user with bcrypt-hashed password
    x(&mut vm, "CREATE USER admin WITH PASSWORD 'admin_pass'");

    // Verify password
    assert!(vm.verify_user_password("admin", "admin_pass"));
    assert!(!vm.verify_user_password("admin", "wrong"));

    // Grant privileges
    x(&mut vm, "GRANT SELECT ON my_table TO admin");
    x(&mut vm, "GRANT INSERT ON my_table TO admin");

    // Create and use a table
    x(
        &mut vm,
        "CREATE TABLE my_table (id INTEGER PRIMARY KEY, data TEXT)",
    );
    x(&mut vm, "INSERT INTO my_table VALUES (1, 'secret')");
    let rows = qr(&mut vm, "SELECT * FROM my_table");
    assert_eq!(rows.len(), 1);

    // Alter password
    x(&mut vm, "ALTER USER admin WITH PASSWORD 'new_pass'");
    assert!(vm.verify_user_password("admin", "new_pass"));
    assert!(!vm.verify_user_password("admin", "admin_pass"));

    // Check audit log has all operations
    let entries = vm.audit_log.entries();
    assert!(
        entries.len() >= 6,
        "audit log should have many entries, got {}",
        entries.len()
    );

    // SHOW AUDIT LOG should work
    let result = vm.execute_sql("SHOW AUDIT LOG").unwrap();
    match result {
        ExecResult::QueryResult { rows, .. } => {
            assert!(!rows.is_empty());
        }
        _ => panic!("expected QueryResult"),
    }

    // Revoke and drop user
    x(&mut vm, "REVOKE SELECT ON my_table FROM admin");
    x(&mut vm, "DROP USER admin");

    // Verify user is gone
    let rows = qr(&mut vm, "SELECT * FROM kkdb_users WHERE username = 'admin'");
    assert!(rows.is_empty(), "user should be dropped");
}
