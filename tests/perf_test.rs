//! Performance benchmarks for kkdb
//!
//! Run with: cargo test --test perf_test --release -- --nocapture

use kkdb::storage::btree::BTree;
use kkdb::storage::pager::Pager;
use kkdb::types::Value;
use kkdb::vm::execute::VM;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn setup_vm_with_table(rows: usize) -> VM {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE bench (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)")
        .unwrap();
    for i in 1..=rows {
        vm.execute_sql(&format!(
            "INSERT INTO bench VALUES ({}, 'name_{}', {})",
            i,
            i,
            i * 10
        ))
        .unwrap();
    }
    vm
}

/// Run a closure `iterations` times and return (total_ms, avg_us)
fn measure<F: FnMut()>(label: &str, iterations: usize, mut f: F) -> f64 {
    // Warm up
    f();

    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    let elapsed = start.elapsed();
    let total_ms = elapsed.as_secs_f64() * 1000.0;
    let avg_us = total_ms * 1000.0 / iterations as f64;
    println!(
        "  {:<40} {:>8.2} ms total  |  {:>10.2} µs/iter  ({} iters)",
        label, total_ms, avg_us, iterations
    );
    avg_us
}

// ---------------------------------------------------------------------------
// INSERT benchmarks
// ---------------------------------------------------------------------------

#[test]
fn perf_insert() {
    println!("\n{}", "=".repeat(70));
    println!("  INSERT PERFORMANCE");
    println!("{}", "=".repeat(70));

    for &count in &[100, 500, 1000] {
        measure(&format!("INSERT {} rows", count), 5, || {
            let mut vm = VM::new_memory();
            vm.execute_sql("CREATE TABLE bench (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)")
                .unwrap();
            for i in 1..=count {
                vm.execute_sql(&format!(
                    "INSERT INTO bench VALUES ({}, 'name_{}', {})",
                    i,
                    i,
                    i * 10
                ))
                .unwrap();
            }
        });
    }
}

// ---------------------------------------------------------------------------
// SELECT benchmarks
// ---------------------------------------------------------------------------

#[test]
fn perf_select() {
    println!("\n{}", "=".repeat(70));
    println!("  SELECT PERFORMANCE");
    println!("{}", "=".repeat(70));

    for &count in &[100, 500, 1000] {
        let mut vm = setup_vm_with_table(count);

        measure(&format!("SELECT * ({} rows)", count), 20, || {
            vm.execute_sql("SELECT * FROM bench").unwrap();
        });

        measure(&format!("SELECT WHERE ({} rows)", count), 20, || {
            vm.execute_sql("SELECT * FROM bench WHERE value > 500")
                .unwrap();
        });

        measure(&format!("SELECT ORDER BY ({} rows)", count), 20, || {
            vm.execute_sql("SELECT * FROM bench ORDER BY value DESC")
                .unwrap();
        });

        measure(&format!("SELECT LIMIT 10 ({} rows)", count), 20, || {
            vm.execute_sql("SELECT * FROM bench ORDER BY id LIMIT 10")
                .unwrap();
        });
    }
}

// ---------------------------------------------------------------------------
// UPDATE benchmarks
// ---------------------------------------------------------------------------

#[test]
fn perf_update() {
    println!("\n{}", "=".repeat(70));
    println!("  UPDATE PERFORMANCE");
    println!("{}", "=".repeat(70));

    for &count in &[100, 500] {
        measure(&format!("UPDATE 50 of {} rows", count), 5, || {
            let mut vm = setup_vm_with_table(count);
            vm.execute_sql("UPDATE bench SET value = value + 1 WHERE id <= 50")
                .unwrap();
        });

        measure(&format!("UPDATE ALL {} rows", count), 5, || {
            let mut vm = setup_vm_with_table(count);
            vm.execute_sql("UPDATE bench SET value = 0").unwrap();
        });
    }
}

// ---------------------------------------------------------------------------
// DELETE benchmarks
// ---------------------------------------------------------------------------

#[test]
fn perf_delete() {
    println!("\n{}", "=".repeat(70));
    println!("  DELETE PERFORMANCE");
    println!("{}", "=".repeat(70));

    for &count in &[100, 500] {
        measure(&format!("DELETE 50 of {} rows", count), 5, || {
            let mut vm = setup_vm_with_table(count);
            vm.execute_sql("DELETE FROM bench WHERE id <= 50").unwrap();
        });

        measure(&format!("DELETE ALL {} rows", count), 5, || {
            let mut vm = setup_vm_with_table(count);
            vm.execute_sql("DELETE FROM bench").unwrap();
        });
    }
}

// ---------------------------------------------------------------------------
// Aggregate benchmarks
// ---------------------------------------------------------------------------

#[test]
fn perf_aggregate() {
    println!("\n{}", "=".repeat(70));
    println!("  AGGREGATE PERFORMANCE");
    println!("{}", "=".repeat(70));

    for &count in &[100, 500, 1000] {
        let mut vm = VM::new_memory();
        vm.execute_sql("CREATE TABLE bench (id INTEGER PRIMARY KEY, cat TEXT, value INTEGER)")
            .unwrap();
        for i in 1..=count {
            let cat = format!("cat_{}", i % 10);
            vm.execute_sql(&format!(
                "INSERT INTO bench VALUES ({}, '{}', {})",
                i,
                cat,
                i * 10
            ))
            .unwrap();
        }

        measure(&format!("GROUP BY + HAVING ({} rows)", count), 20, || {
            vm.execute_sql(
                "SELECT cat FROM bench GROUP BY cat HAVING SUM(value) > 100 ORDER BY cat",
            )
            .unwrap();
        });
    }
}

// ---------------------------------------------------------------------------
// JOIN benchmarks
// ---------------------------------------------------------------------------

#[test]
fn perf_join() {
    println!("\n{}", "=".repeat(70));
    println!("  JOIN PERFORMANCE");
    println!("{}", "=".repeat(70));

    for &count in &[50, 100, 200] {
        let mut vm = VM::new_memory();
        vm.execute_sql("CREATE TABLE t1 (a INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        vm.execute_sql("CREATE TABLE t2 (b INTEGER PRIMARY KEY, a_ref INTEGER, data TEXT)")
            .unwrap();
        for i in 1..=count {
            vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, 'name_{}')", i, i))
                .unwrap();
            vm.execute_sql(&format!(
                "INSERT INTO t2 VALUES ({}, {}, 'data_{}')",
                i, i, i
            ))
            .unwrap();
        }

        measure(&format!("INNER JOIN {}x{} rows", count, count), 10, || {
            vm.execute_sql("SELECT * FROM t1 JOIN t2 ON t1.a = t2.a_ref")
                .unwrap();
        });

        measure(&format!("LEFT JOIN {}x{} rows", count, count), 10, || {
            vm.execute_sql("SELECT * FROM t1 LEFT JOIN t2 ON t1.a = t2.a_ref")
                .unwrap();
        });
    }
}

// ---------------------------------------------------------------------------
// B-tree raw operation benchmarks
// ---------------------------------------------------------------------------

#[test]
fn perf_btree_insert() {
    println!("\n{}", "=".repeat(70));
    println!("  B-TREE INSERT PERFORMANCE");
    println!("{}", "=".repeat(70));

    for &count in &[100, 500, 1000] {
        measure(&format!("btree insert {} rows", count), 10, || {
            let mut pager = Pager::open_memory();
            let mut btree = BTree::new(&mut pager);
            let root = btree.create_table().unwrap();

            let mut current_root = root;
            for i in 1..=count {
                let row = vec![
                    Value::Integer(i as i64),
                    Value::Text(format!("name_{}", i).into()),
                    Value::Integer(i as i64 * 10),
                ];
                let mut btree = BTree::new(&mut pager);
                current_root = btree.insert(current_root, i as i64, &row).unwrap();
            }
        });
    }
}

#[test]
fn perf_btree_scan() {
    println!("\n{}", "=".repeat(70));
    println!("  B-TREE SCAN PERFORMANCE");
    println!("{}", "=".repeat(70));

    for &count in &[100, 500, 1000] {
        let mut pager = Pager::open_memory();
        let mut btree = BTree::new(&mut pager);
        let root = btree.create_table().unwrap();

        let mut current_root = root;
        for i in 1..=count {
            let row = vec![
                Value::Integer(i as i64),
                Value::Text(format!("name_{}", i).into()),
                Value::Integer(i as i64 * 10),
            ];
            let mut btree = BTree::new(&mut pager);
            current_root = btree.insert(current_root, i as i64, &row).unwrap();
        }

        measure(&format!("btree scan_all {} rows", count), 50, || {
            let mut btree = BTree::new(&mut pager);
            let _rows = btree.scan_all(current_root).unwrap();
        });
    }
}

#[test]
fn perf_btree_find() {
    println!("\n{}", "=".repeat(70));
    println!("  B-TREE FIND PERFORMANCE");
    println!("{}", "=".repeat(70));

    for &count in &[100, 500, 1000] {
        let mut pager = Pager::open_memory();
        let mut btree = BTree::new(&mut pager);
        let root = btree.create_table().unwrap();

        let mut current_root = root;
        for i in 1..=count {
            let row = vec![
                Value::Integer(i as i64),
                Value::Text(format!("name_{}", i).into()),
                Value::Integer(i as i64 * 10),
            ];
            let mut btree = BTree::new(&mut pager);
            current_root = btree.insert(current_root, i as i64, &row).unwrap();
        }

        measure(&format!("btree find in {} rows", count), 100, || {
            let mut btree = BTree::new(&mut pager);
            let target = 1_i64; // always find first row (guaranteed to exist)
            let _result = btree.find_by_rowid(current_root, target).unwrap();
        });
    }
}

// ---------------------------------------------------------------------------
// SQL parse + execute round-trip benchmark
// ---------------------------------------------------------------------------

#[test]
fn perf_sql_round_trip() {
    println!("\n{}", "=".repeat(70));
    println!("  SQL ROUND-TRIP PERFORMANCE");
    println!("{}", "=".repeat(70));

    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)")
        .unwrap();
    for i in 1..=100 {
        vm.execute_sql(&format!(
            "INSERT INTO t1 VALUES ({}, 'name_{}', {})",
            i,
            i,
            i * 10
        ))
        .unwrap();
    }

    measure("parse+exec simple SELECT", 500, || {
        vm.execute_sql("SELECT * FROM t1 WHERE id = 50").unwrap();
    });

    measure("parse+exec complex SELECT", 200, || {
        vm.execute_sql(
            "SELECT * FROM t1 WHERE value > 100 AND value < 800 ORDER BY value DESC LIMIT 10",
        )
        .unwrap();
    });

    measure("parse+exec INSERT", 500, || {
        vm.execute_sql("INSERT INTO t1 VALUES (9999, 'test', 42)")
            .unwrap();
        vm.execute_sql("DELETE FROM t1 WHERE id = 9999").unwrap();
    });
}

// ---------------------------------------------------------------------------
// Complex query benchmarks
// ---------------------------------------------------------------------------

#[test]
fn perf_complex_queries() {
    println!("\n{}", "=".repeat(70));
    println!("  COMPLEX QUERY PERFORMANCE");
    println!("{}", "=".repeat(70));

    // Setup: create tables with realistic data
    let mut vm = VM::new_memory();
    vm.execute_sql(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER, city TEXT)",
    )
    .unwrap();
    vm.execute_sql(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, amount INTEGER, status TEXT)",
    )
    .unwrap();
    vm.execute_sql(
        "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price INTEGER, category TEXT)",
    )
    .unwrap();

    for i in 1..=200 {
        let city = match i % 5 {
            0 => "Beijing",
            1 => "Shanghai",
            2 => "Guangzhou",
            3 => "Shenzhen",
            _ => "Hangzhou",
        };
        vm.execute_sql(&format!(
            "INSERT INTO users VALUES ({}, 'user_{}', {}, '{}')",
            i,
            i,
            20 + (i % 40),
            city
        ))
        .unwrap();
    }

    for i in 1..=500 {
        let status = if i % 3 == 0 {
            "completed"
        } else if i % 3 == 1 {
            "pending"
        } else {
            "cancelled"
        };
        vm.execute_sql(&format!(
            "INSERT INTO orders VALUES ({}, {}, {}, '{}')",
            i,
            (i % 200) + 1,
            (i * 17) % 1000,
            status
        ))
        .unwrap();
    }

    for i in 1..=100 {
        let cat = match i % 4 {
            0 => "electronics",
            1 => "clothing",
            2 => "food",
            _ => "books",
        };
        vm.execute_sql(&format!(
            "INSERT INTO products VALUES ({}, 'product_{}', {}, '{}')",
            i,
            i,
            10 + (i * 7) % 500,
            cat
        ))
        .unwrap();
    }

    // Complex WHERE with multiple AND/OR conditions
    measure("multi-cond WHERE (200 rows)", 20, || {
        vm.execute_sql("SELECT * FROM users WHERE age > 30 AND age < 50 AND city = 'Beijing'")
            .unwrap();
    });

    // LIKE pattern matching
    measure("LIKE pattern (200 rows)", 20, || {
        vm.execute_sql("SELECT * FROM users WHERE name LIKE 'user_1%'")
            .unwrap();
    });

    // Expression-heavy SELECT
    measure("expression-heavy SELECT (500 rows)", 20, || {
        vm.execute_sql(
            "SELECT id, amount * 2 + 100, amount FROM orders WHERE amount > 200 AND amount < 800",
        )
        .unwrap();
    });

    // GROUP BY with multiple aggregates
    measure("multi-aggregate GROUP BY (500 rows)", 10, || {
        vm.execute_sql("SELECT status FROM orders GROUP BY status HAVING SUM(amount) > 1000")
            .unwrap();
    });

    // ORDER BY + LIMIT on large result
    measure("ORDER BY + LIMIT (500 rows)", 20, || {
        vm.execute_sql(
            "SELECT * FROM orders WHERE status = 'completed' ORDER BY amount DESC LIMIT 20",
        )
        .unwrap();
    });

    // JOIN + WHERE filter
    measure("JOIN + WHERE (200x500)", 5, || {
        vm.execute_sql(
            "SELECT * FROM users JOIN orders ON users.id = orders.user_id WHERE orders.amount > 500",
        )
        .unwrap();
    });

    // JOIN + GROUP BY
    measure("JOIN + GROUP BY (200x500)", 5, || {
        vm.execute_sql(
            "SELECT city FROM users JOIN orders ON users.id = orders.user_id GROUP BY city HAVING SUM(orders.amount) > 5000",
        )
        .unwrap();
    });

    // LEFT JOIN with NULL check via WHERE
    measure("LEFT JOIN + filter (200x500)", 5, || {
        vm.execute_sql(
            "SELECT * FROM users LEFT JOIN orders ON users.id = orders.user_id WHERE orders.amount > 300",
        )
        .unwrap();
    });

    // Subquery in FROM
    measure("subquery in FROM (200 rows)", 10, || {
        vm.execute_sql(
            "SELECT * FROM (SELECT id, name, age FROM users WHERE age > 25) AS sub WHERE age < 45",
        )
        .unwrap();
    });

    // Complex expression with functions
    measure("scalar functions (200 rows)", 20, || {
        vm.execute_sql(
            "SELECT id, UPPER(name), LENGTH(name), ABS(age - 30) FROM users WHERE LENGTH(name) > 5",
        )
        .unwrap();
    });

    // BETWEEN
    measure("BETWEEN filter (500 rows)", 20, || {
        vm.execute_sql("SELECT * FROM orders WHERE amount BETWEEN 200 AND 600")
            .unwrap();
    });

    // IN list
    measure("IN list filter (200 rows)", 20, || {
        vm.execute_sql("SELECT * FROM users WHERE city IN ('Beijing', 'Shanghai', 'Shenzhen')")
            .unwrap();
    });

    // Multi-column ORDER BY
    measure("multi-col ORDER BY (200 rows)", 20, || {
        vm.execute_sql("SELECT * FROM users ORDER BY city, age DESC LIMIT 50")
            .unwrap();
    });
}

// ---------------------------------------------------------------------------
// Advanced complex query benchmarks
// ---------------------------------------------------------------------------

#[test]
fn perf_advanced_queries() {
    println!("\n{}", "=".repeat(70));
    println!("  ADVANCED QUERY PERFORMANCE");
    println!("{}", "=".repeat(70));

    // Setup: 3 tables for chained JOINs
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE departments (id INTEGER PRIMARY KEY, name TEXT, budget INTEGER)")
        .unwrap();
    vm.execute_sql(
        "CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT, dept_id INTEGER, salary INTEGER)",
    ).unwrap();
    vm.execute_sql(
        "CREATE TABLE projects (id INTEGER PRIMARY KEY, emp_id INTEGER, title TEXT, hours INTEGER)",
    )
    .unwrap();

    for i in 1..=20 {
        vm.execute_sql(&format!(
            "INSERT INTO departments VALUES ({}, 'dept_{}', {})",
            i,
            i,
            i * 50000
        ))
        .unwrap();
    }
    for i in 1..=300 {
        vm.execute_sql(&format!(
            "INSERT INTO employees VALUES ({}, 'emp_{}', {}, {})",
            i,
            i,
            (i % 20) + 1,
            30000 + (i * 137) % 70000
        ))
        .unwrap();
    }
    for i in 1..=600 {
        vm.execute_sql(&format!(
            "INSERT INTO projects VALUES ({}, {}, 'proj_{}', {})",
            i,
            (i % 300) + 1,
            i,
            (i * 31) % 200
        ))
        .unwrap();
    }

    // 3-table JOIN chain
    measure("3-table JOIN (20x300x600)", 3, || {
        vm.execute_sql(
            "SELECT departments.name, employees.name, projects.title \
             FROM departments \
             JOIN employees ON departments.id = employees.dept_id \
             JOIN projects ON employees.id = projects.emp_id \
             WHERE projects.hours > 100",
        )
        .unwrap();
    });

    // 3-table JOIN + GROUP BY + aggregate
    measure("3-table JOIN + GROUP BY", 3, || {
        vm.execute_sql(
            "SELECT departments.name \
             FROM departments \
             JOIN employees ON departments.id = employees.dept_id \
             JOIN projects ON employees.id = projects.emp_id \
             GROUP BY departments.name \
             HAVING SUM(projects.hours) > 500",
        )
        .unwrap();
    });

    // Deeply nested arithmetic expression
    measure("nested expr (300 rows)", 20, || {
        vm.execute_sql(
            "SELECT id, ((salary * 2 + 1000) * 3 - 500) FROM employees WHERE ((salary + 1000) * 2) > 100000",
        ).unwrap();
    });

    // Large IN list
    let in_values: String = (1..=50)
        .map(|i| format!("{}", i))
        .collect::<Vec<_>>()
        .join(", ");
    let in_query = format!("SELECT * FROM employees WHERE dept_id IN ({})", in_values);
    measure("large IN list (50 values, 300 rows)", 10, || {
        vm.execute_sql(&in_query).unwrap();
    });

    // Complex LIKE patterns
    measure("LIKE prefix (300 rows)", 20, || {
        vm.execute_sql("SELECT * FROM employees WHERE name LIKE 'emp_1%'")
            .unwrap();
    });

    measure("LIKE suffix (300 rows)", 20, || {
        vm.execute_sql("SELECT * FROM employees WHERE name LIKE '%_5'")
            .unwrap();
    });

    measure("LIKE contains (600 rows)", 20, || {
        vm.execute_sql("SELECT * FROM projects WHERE title LIKE '%proj_1%'")
            .unwrap();
    });

    // COALESCE / IFNULL
    measure("COALESCE (300 rows)", 20, || {
        vm.execute_sql("SELECT id, COALESCE(name, 'unknown') FROM employees WHERE salary > 50000")
            .unwrap();
    });

    // Multi-function pipeline
    measure("multi-function pipeline (300 rows)", 10, || {
        vm.execute_sql(
            "SELECT id, UPPER(name), LENGTH(name), ABS(salary - 50000), TYPEOF(salary) FROM employees WHERE LENGTH(name) > 4",
        ).unwrap();
    });

    // BETWEEN + ORDER BY + LIMIT
    measure("BETWEEN + ORDER + LIMIT (300 rows)", 20, || {
        vm.execute_sql(
            "SELECT * FROM employees WHERE salary BETWEEN 40000 AND 80000 ORDER BY salary DESC LIMIT 25",
        ).unwrap();
    });

    // NOT IN
    let not_in_query = format!(
        "SELECT * FROM employees WHERE dept_id NOT IN ({})",
        (1..=10)
            .map(|i| format!("{}", i))
            .collect::<Vec<_>>()
            .join(", ")
    );
    measure("NOT IN (10 values, 300 rows)", 20, || {
        vm.execute_sql(&not_in_query).unwrap();
    });

    // Subquery + JOIN
    measure("subquery + ORDER BY", 10, || {
        vm.execute_sql(
            "SELECT * FROM (SELECT id, name, salary FROM employees WHERE salary > 50000) AS high_earners ORDER BY salary DESC LIMIT 20",
        ).unwrap();
    });

    // Realistic analytics: JOIN + filter + aggregate
    measure("analytics: dept salary summary", 3, || {
        vm.execute_sql(
            "SELECT departments.name \
             FROM departments \
             JOIN employees ON departments.id = employees.dept_id \
             WHERE employees.salary > 40000 \
             GROUP BY departments.name \
             HAVING SUM(employees.salary) > 100000",
        )
        .unwrap();
    });
}

// ---------------------------------------------------------------------------
// Stress test with larger datasets
// ---------------------------------------------------------------------------

#[test]
fn perf_stress() {
    println!("\n{}", "=".repeat(70));
    println!("  STRESS TEST PERFORMANCE");
    println!("{}", "=".repeat(70));

    // Large INSERT
    measure("INSERT 2000 rows", 3, || {
        let mut vm = VM::new_memory();
        vm.execute_sql("CREATE TABLE big (id INTEGER PRIMARY KEY, a TEXT, b INTEGER, c TEXT)")
            .unwrap();
        for i in 1..=2000 {
            vm.execute_sql(&format!(
                "INSERT INTO big VALUES ({}, 'val_{}', {}, 'cat_{}')",
                i,
                i,
                i * 7 % 1000,
                i % 20
            ))
            .unwrap();
        }
    });

    // Setup for queries
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE big (id INTEGER PRIMARY KEY, a TEXT, b INTEGER, c TEXT)")
        .unwrap();
    for i in 1..=2000 {
        vm.execute_sql(&format!(
            "INSERT INTO big VALUES ({}, 'val_{}', {}, 'cat_{}')",
            i,
            i,
            i * 7 % 1000,
            i % 20
        ))
        .unwrap();
    }

    measure("SELECT * 2000 rows", 10, || {
        vm.execute_sql("SELECT * FROM big").unwrap();
    });

    measure("SELECT WHERE 2000 rows", 10, || {
        vm.execute_sql("SELECT * FROM big WHERE b > 500 AND b < 800")
            .unwrap();
    });

    measure("GROUP BY 2000 rows (20 groups)", 10, || {
        vm.execute_sql("SELECT c FROM big GROUP BY c HAVING SUM(b) > 10000")
            .unwrap();
    });

    measure("ORDER BY 2000 rows", 10, || {
        vm.execute_sql("SELECT * FROM big ORDER BY b DESC LIMIT 100")
            .unwrap();
    });

    measure("LIKE on 2000 rows", 10, || {
        vm.execute_sql("SELECT * FROM big WHERE a LIKE 'val_1%'")
            .unwrap();
    });

    measure("UPDATE 500 of 2000 rows", 3, || {
        let mut vm2 = VM::new_memory();
        vm2.execute_sql("CREATE TABLE big (id INTEGER PRIMARY KEY, a TEXT, b INTEGER, c TEXT)")
            .unwrap();
        for i in 1..=2000 {
            vm2.execute_sql(&format!(
                "INSERT INTO big VALUES ({}, 'val_{}', {}, 'cat_{}')",
                i,
                i,
                i * 7 % 1000,
                i % 20
            ))
            .unwrap();
        }
        vm2.execute_sql("UPDATE big SET b = b + 1 WHERE b < 250")
            .unwrap();
    });
}

// ---------------------------------------------------------------------------
// End-to-end mixed workload
// ---------------------------------------------------------------------------

#[test]
fn perf_mixed_workload() {
    println!("\n{}", "=".repeat(70));
    println!("  MIXED WORKLOAD PERFORMANCE");
    println!("{}", "=".repeat(70));

    measure("mixed workload (500 ops)", 3, || {
        let mut vm = VM::new_memory();
        vm.execute_sql("CREATE TABLE bench (id INTEGER PRIMARY KEY, name TEXT, value INTEGER)")
            .unwrap();

        // Phase 1: 200 inserts
        for i in 1..=200 {
            vm.execute_sql(&format!(
                "INSERT INTO bench VALUES ({}, 'name_{}', {})",
                i,
                i,
                i * 10
            ))
            .unwrap();
        }

        // Phase 2: 100 selects with various filters
        for i in 0..100 {
            let threshold = i * 20;
            vm.execute_sql(&format!("SELECT * FROM bench WHERE value > {}", threshold))
                .unwrap();
        }

        // Phase 3: 100 updates
        for i in 1..=100 {
            vm.execute_sql(&format!(
                "UPDATE bench SET value = {} WHERE id = {}",
                i * 100,
                i
            ))
            .unwrap();
        }

        // Phase 4: 50 deletes
        for i in 151..=200 {
            vm.execute_sql(&format!("DELETE FROM bench WHERE id = {}", i))
                .unwrap();
        }

        // Phase 5: aggregate query
        vm.execute_sql("SELECT * FROM bench ORDER BY value DESC LIMIT 10")
            .unwrap();
    });
}
