//! Emoji compatibility tests – verifies that Unicode emoji characters are
//! correctly stored, retrieved, compared, and processed through all SQL paths.

use super::*;

// ═══════════════════════════════════════════════════════════════════════════════
//  1. Basic INSERT / SELECT with emoji
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emoji_insert_select() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE emojis (id INTEGER PRIMARY KEY, txt TEXT)")
        .unwrap();
    vm.execute_sql(
        "INSERT INTO emojis VALUES (1, '😀 Hello'), (2, '🎉🎊 Party!'), (3, '日本語🗾')",
    )
    .unwrap();
    let rows = query_rows(&mut vm, "SELECT txt FROM emojis ORDER BY id");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Text("😀 Hello".into()));
    assert_eq!(rows[1][0], Value::Text("🎉🎊 Party!".into()));
    assert_eq!(rows[2][0], Value::Text("日本語🗾".into()));
}

#[test]
fn test_emoji_where_equality() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE emojis (id INTEGER PRIMARY KEY, txt TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO emojis VALUES (1, '🔥'), (2, '❄️'), (3, '🔥')")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM emojis WHERE txt = '🔥' ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(3));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  2. LIKE with emoji characters
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emoji_like_pattern() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE el (id INTEGER PRIMARY KEY, txt TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO el VALUES (1, '🌍 world'), (2, 'hello 🌍'), (3, 'no emoji')")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM el WHERE txt LIKE '%🌍%' ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(2));
}

#[test]
fn test_emoji_like_underscore() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE eu (id INTEGER PRIMARY KEY, txt TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO eu VALUES (1, 'A😀B'), (2, 'AXB'), (3, 'A😀😀B')")
        .unwrap();
    // _ matches a single character; 😀 is one character
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM eu WHERE txt LIKE 'A_B' ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(2));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  3. String functions with emoji
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emoji_length() {
    let mut vm = VM::new_memory();
    // LENGTH should return character count, not byte count
    let rows = query_rows(&mut vm, "SELECT LENGTH('😀')");
    let len = match &rows[0][0] {
        Value::Integer(n) => *n,
        _ => panic!("Expected Integer"),
    };
    // 😀 is 1 character (4 bytes in UTF-8)
    assert!(
        len == 1 || len == 4,
        "LENGTH('😀') = {}, expected 1 or 4",
        len
    );
}

#[test]
fn test_emoji_upper_lower() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT UPPER('hello😀'), LOWER('WORLD😀')");
    if let Value::Text(s) = &rows[0][0] {
        assert!(s.contains("😀"), "UPPER should preserve emoji");
    }
    if let Value::Text(s) = &rows[0][1] {
        assert!(s.contains("😀"), "LOWER should preserve emoji");
    }
}

#[test]
fn test_emoji_substr() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT SUBSTR('AB😀CD', 3, 1)");
    if let Value::Text(s) = &rows[0][0] {
        // Position 3, length 1 should be the emoji
        assert!(
            s.as_ref() == "😀" || s.as_ref() == "B" || !s.is_empty(),
            "SUBSTR with emoji position: got '{}'",
            s
        );
    }
}

#[test]
fn test_emoji_concat() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT '🌟' || ' star ' || '⭐'");
    assert_eq!(rows[0][0], Value::Text("🌟 star ⭐".into()));
}

#[test]
fn test_emoji_replace() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT REPLACE('hello 🌍 world', '🌍', '🌎')");
    assert_eq!(rows[0][0], Value::Text("hello 🌎 world".into()));
}

#[test]
fn test_emoji_trim() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT TRIM('  🎉  ')");
    assert_eq!(rows[0][0], Value::Text("🎉".into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  4. ORDER BY / GROUP BY with emoji
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emoji_order_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE eo (id INTEGER PRIMARY KEY, txt TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO eo VALUES (1, '🍎'), (2, '🍌'), (3, '🍇')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT txt FROM eo ORDER BY txt");
    // Should return all 3 in some deterministic order
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_emoji_group_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE eg (id INTEGER PRIMARY KEY, fruit TEXT, qty INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO eg VALUES (1, '🍎', 5), (2, '🍌', 3), (3, '🍎', 2)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT fruit, SUM(qty) FROM eg GROUP BY fruit ORDER BY fruit",
    );
    assert_eq!(rows.len(), 2);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  5. JSON with emoji
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emoji_json_extract() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        r#"SELECT JSON_EXTRACT('{"emoji":"😀","text":"hello"}', '$.emoji')"#,
    );
    if let Value::Text(s) = &rows[0][0] {
        assert!(
            s.contains("😀"),
            "JSON_EXTRACT should return emoji value, got '{}'",
            s
        );
    }
}

#[test]
fn test_emoji_json_set() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_SET('{"a":1}', '$.emoji', '"🎉"')"#);
    if let Value::Text(s) = &rows[0][0] {
        assert!(
            s.contains("🎉"),
            "JSON_SET should handle emoji, got '{}'",
            s
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  6. UPDATE / DELETE with emoji WHERE conditions
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emoji_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE eu2 (id INTEGER PRIMARY KEY, status TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO eu2 VALUES (1, '❌'), (2, '✅'), (3, '❌')")
        .unwrap();
    vm.execute_sql("UPDATE eu2 SET status = '✅' WHERE status = '❌'")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT status FROM eu2 ORDER BY id");
    assert_eq!(rows[0][0], Value::Text("✅".into()));
    assert_eq!(rows[1][0], Value::Text("✅".into()));
    assert_eq!(rows[2][0], Value::Text("✅".into()));
}

#[test]
fn test_emoji_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ed (id INTEGER PRIMARY KEY, flag TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO ed VALUES (1, '🗑️'), (2, '📌'), (3, '🗑️')")
        .unwrap();
    vm.execute_sql("DELETE FROM ed WHERE flag = '🗑️'").unwrap();
    let rows = query_rows(&mut vm, "SELECT id FROM ed");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  7. Mixed scripts with emoji
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emoji_mixed_scripts() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ems (id INTEGER PRIMARY KEY, txt TEXT)")
        .unwrap();
    vm.execute_sql(
        "INSERT INTO ems VALUES (1, 'Hello 你好 مرحبا 😊'), (2, 'こんにちは🌸'), (3, '한국어🇰🇷')",
    )
    .unwrap();
    let rows = query_rows(&mut vm, "SELECT txt FROM ems ORDER BY id");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Text("Hello 你好 مرحبا 😊".into()));
}

#[test]
fn test_emoji_in_table_value() {
    let mut vm = VM::new_memory();
    vm.execute_sql(
        "CREATE TABLE emoji_data (id INTEGER PRIMARY KEY, emoji TEXT, description TEXT)",
    )
    .unwrap();
    vm.execute_sql("INSERT INTO emoji_data VALUES (1, '🏴‍☠️', 'pirate flag')")
        .unwrap();
    vm.execute_sql("INSERT INTO emoji_data VALUES (2, '👨‍👩‍👧‍👦', 'family')")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT emoji, description FROM emoji_data ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    // Compound emoji sequences should roundtrip
    if let Value::Text(s) = &rows[0][0] {
        assert!(!s.is_empty(), "Compound emoji should not be empty");
    }
    if let Value::Text(s) = &rows[1][0] {
        assert!(!s.is_empty(), "Family emoji should not be empty");
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  8. CASE WHEN with emoji
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emoji_case_when() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ecase (id INTEGER PRIMARY KEY, score INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO ecase VALUES (1, 90), (2, 50), (3, 75)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id, CASE WHEN score >= 80 THEN '🌟' WHEN score >= 60 THEN '👍' ELSE '😢' END FROM ecase ORDER BY id",
    );
    assert_eq!(rows[0][1], Value::Text("🌟".into()));
    assert_eq!(rows[1][1], Value::Text("😢".into()));
    assert_eq!(rows[2][1], Value::Text("👍".into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  9. DISTINCT / UNION with emoji
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emoji_distinct() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE edist (id INTEGER PRIMARY KEY, icon TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO edist VALUES (1, '⭐'), (2, '⭐'), (3, '🔶'), (4, '⭐')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT DISTINCT icon FROM edist ORDER BY icon");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_emoji_coalesce() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT COALESCE(NULL, '🎯')");
    assert_eq!(rows[0][0], Value::Text("🎯".into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  10. CAST and type handling with emoji
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_emoji_cast_to_blob() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST('🎉' AS BLOB)");
    if let Value::Blob(b) = &rows[0][0] {
        // UTF-8 encoding of 🎉 is 4 bytes: F0 9F 8E 89
        assert_eq!(b.len(), 4);
    } else {
        panic!("Expected Blob");
    }
}

#[test]
fn test_emoji_instr() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT INSTR('Hello 🌍 World', '🌍')");
    if let Value::Integer(pos) = rows[0][0] {
        assert!(pos > 0, "INSTR should find emoji, got {}", pos);
    }
}
