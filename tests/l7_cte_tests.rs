// L7 Recursive CTE integration tests
use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};
use std::fs;

fn setup_vm(db_dir: &str) -> VM {
    let _ = fs::remove_dir_all(db_dir);
    VM::open(db_dir).unwrap()
}

fn rows_from(result: ExecResult) -> Vec<Vec<Value>> {
    if let ExecResult::QueryResult { rows, .. } = result {
        rows
    } else {
        panic!("Expected QueryResult, got {:?}", result)
    }
}

fn int(v: i64) -> Value { Value::Integer(v) }

// ──────────────────────────────────────────────────────────
// Plain (non-recursive) CTE
// ──────────────────────────────────────────────────────────

#[test]
fn test_l7_plain_cte() {
    let mut vm = setup_vm("test_l7_plain_cte_db");
    vm.execute_sql("CREATE TABLE t (id INTEGER, val INTEGER);").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 10);").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 20);").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3, 30);").unwrap();

    let rows = rows_from(vm.execute_sql(
        "WITH big AS (SELECT id, val FROM t WHERE val > 15)
         SELECT id, val FROM big ORDER BY id;"
    ).unwrap());

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], int(2));
    assert_eq!(rows[1][0], int(3));
}

#[test]
fn test_l7_plain_cte_multiple() {
    let mut vm = setup_vm("test_l7_multi_cte_db");
    vm.execute_sql("CREATE TABLE nums (n INTEGER);").unwrap();
    for i in 1..=5 {
        vm.execute_sql(&format!("INSERT INTO nums VALUES ({});", i)).unwrap();
    }

    let rows = rows_from(vm.execute_sql(
        "WITH evens AS (SELECT n FROM nums WHERE n % 2 = 0)
         SELECT n FROM evens ORDER BY n;"
    ).unwrap());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], int(2));
    assert_eq!(rows[1][0], int(4));
}

// ──────────────────────────────────────────────────────────
// Recursive CTE: counting 1..5
// ──────────────────────────────────────────────────────────

#[test]
fn test_l7_recursive_cte_counter() {
    let mut vm = setup_vm("test_l7_recursive_cte_db");

    let rows = rows_from(vm.execute_sql(
        "WITH RECURSIVE cnt(n) AS (
           SELECT 1
           UNION ALL
           SELECT n + 1 FROM cnt WHERE n < 5
         )
         SELECT n FROM cnt ORDER BY n;"
    ).unwrap());

    assert_eq!(rows.len(), 5, "should have 5 rows: {:?}", rows);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row[0], int((i + 1) as i64), "row {} should be {}", i, i+1);
    }
}

// ──────────────────────────────────────────────────────────
// Recursive CTE: Fibonacci up to 10 terms
// ──────────────────────────────────────────────────────────

#[test]
fn test_l7_recursive_cte_fibonacci() {
    let mut vm = setup_vm("test_l7_fib_db");

    let rows = rows_from(vm.execute_sql(
        "WITH RECURSIVE fib(a, b) AS (
           SELECT 0, 1
           UNION ALL
           SELECT b, a + b FROM fib WHERE a < 34
         )
         SELECT a FROM fib ORDER BY a;"
    ).unwrap());

    // Fibonacci: 0, 1, 1, 2, 3, 5, 8, 13, 21, 34
    let expected = vec![0i64, 1, 1, 2, 3, 5, 8, 13, 21, 34];
    let got: Vec<i64> = rows.iter().map(|r| match r[0] {
        Value::Integer(v) => v,
        _ => panic!("expected integer")
    }).collect();
    assert_eq!(got, expected, "Fibonacci sequence mismatch");
}

// ──────────────────────────────────────────────────────────
// Recursive CTE: tree/hierarchy traversal
// ──────────────────────────────────────────────────────────

#[test]
fn test_l7_recursive_cte_hierarchy() {
    let mut vm = setup_vm("test_l7_hierarchy_db");
    vm.execute_sql("CREATE TABLE emp (id INTEGER, name TEXT, manager_id INTEGER);").unwrap();
    vm.execute_sql("INSERT INTO emp VALUES (1, 'CEO', NULL);").unwrap();
    vm.execute_sql("INSERT INTO emp VALUES (2, 'VP', 1);").unwrap();
    vm.execute_sql("INSERT INTO emp VALUES (3, 'Manager', 2);").unwrap();
    vm.execute_sql("INSERT INTO emp VALUES (4, 'Engineer', 3);").unwrap();

    // Get chain starting from CEO (id=1), depth-first
    let rows = rows_from(vm.execute_sql(
        "WITH RECURSIVE org AS (
           SELECT id, name, 0 as depth FROM emp WHERE manager_id IS NULL
           UNION ALL
           SELECT e.id, e.name, org.depth + 1 FROM emp e JOIN org ON e.manager_id = org.id
         )
         SELECT id, depth FROM org ORDER BY id;"
    ).unwrap());

    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0][0], int(1)); // CEO depth 0
    assert_eq!(rows[0][1], int(0));
    assert_eq!(rows[1][0], int(2)); // VP depth 1
    assert_eq!(rows[1][1], int(1));
    assert_eq!(rows[3][0], int(4)); // Engineer depth 3
    assert_eq!(rows[3][1], int(3));
}
