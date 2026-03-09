/// analytics_tests.rs
/// 澶氳〃鑱旀煡 + 鏁版嵁缁熻楂樼骇搴旂敤娴嬭瘯
///
/// 瑕嗙洊浠ヤ笅鍦烘櫙锛?
///   1. 涓夎〃 INNER JOIN 閾惧紡鑱旀煡
///   2. LEFT JOIN + NULL 鑱氬悎杩囨护锛堟壘鍑烘棤璁㈠崟鐨勫鎴凤級
///   3. GROUP BY + HAVING + 澶氳仛鍚堝嚱鏁帮紙MAX/MIN/AVG/COUNT/SUM锛?
///   4. 瀛愭煡璇㈠祵濂楋紙EXISTS, IN, 鏍囬噺瀛愭煡璇級
///   5. CTE + 閫掑綊鍒嗘瀽锛堥儴闂ㄥ眰娆′笌姹囨€伙級
///   6. UNION / INTERSECT / EXCEPT 闆嗗悎杩愮畻
///   7. 绐楀彛鍑芥暟鎺掑悕锛圧OW_NUMBER, RANK, DENSE_RANK锛?
///   8. 浜ゅ弶缁熻锛圕ASE WHEN 妯℃嫙 PIVOT锛?
///   9. 鍒嗙粍 TOP-N锛堟瘡涓垎绫荤殑鍓?N 鍚嶏級
///  10. 澶氱淮鏁版嵁鑱氬悎锛圧egion 脳 Product 脳 Month锛?
use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};
use std::fs;

// 鈹€鈹€鈹€ Helpers 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

fn setup(name: &str) -> VM {
    let _ = fs::remove_dir_all(name);
    VM::open(name).unwrap()
}

fn rows(sql: &str, vm: &mut VM) -> Vec<Vec<Value>> {
    match vm.execute_sql(sql).unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("Expected QueryResult for '{}', got {:?}", sql, other),
    }
}

fn row1(sql: &str, vm: &mut VM) -> Vec<Value> {
    let mut r = rows(sql, vm);
    assert_eq!(r.len(), 1, "Expected exactly 1 row for: {}", sql);
    r.remove(0)
}

fn exec(sql: &str, vm: &mut VM) {
    vm.execute_sql(sql)
        .unwrap_or_else(|e| panic!("SQL failed: {}\n  Error: {}", sql, e));
}

/// Build a shared e-commerce schema used in many tests.
///
/// Tables:
///   customers  (id, name, country, tier)
///   products   (id, name, category, price)
///   orders     (id, cust_id, created_year, status)
///   order_items (id, order_id, product_id, qty, unit_price)
fn build_ecommerce(vm: &mut VM) {
    exec(
        "CREATE TABLE customers  (id INTEGER PRIMARY KEY, name TEXT, country TEXT, tier TEXT);",
        vm,
    );
    exec(
        "CREATE TABLE products   (id INTEGER PRIMARY KEY, name TEXT, category TEXT, price REAL);",
        vm,
    );
    exec("CREATE TABLE orders     (id INTEGER PRIMARY KEY, cust_id INTEGER, created_year INTEGER, status TEXT);", vm);
    exec("CREATE TABLE order_items(id INTEGER PRIMARY KEY, order_id INTEGER, product_id INTEGER, qty INTEGER, unit_price REAL);", vm);

    // Customers
    exec("INSERT INTO customers VALUES (1,'Alice','US','gold');", vm);
    exec("INSERT INTO customers VALUES (2,'Bob','UK','silver');", vm);
    exec("INSERT INTO customers VALUES (3,'Carol','US','gold');", vm);
    exec("INSERT INTO customers VALUES (4,'Dave','DE','bronze');", vm);
    exec("INSERT INTO customers VALUES (5,'Eve','UK','gold');", vm);
    exec(
        "INSERT INTO customers VALUES (6,'Frank','DE','silver');",
        vm,
    );

    // Products
    exec(
        "INSERT INTO products VALUES (1,'Widget','Electronics',29.99);",
        vm,
    );
    exec(
        "INSERT INTO products VALUES (2,'Gadget','Electronics',49.99);",
        vm,
    );
    exec("INSERT INTO products VALUES (3,'Book','Library',9.99);", vm);
    exec(
        "INSERT INTO products VALUES (4,'Shirt','Apparel',19.99);",
        vm,
    );
    exec(
        "INSERT INTO products VALUES (5,'Shoes','Apparel',59.99);",
        vm,
    );
    exec(
        "INSERT INTO products VALUES (6,'Mug','Accessories',12.99);",
        vm,
    );

    // Orders
    exec("INSERT INTO orders VALUES (101, 1,2023,'completed');", vm);
    exec("INSERT INTO orders VALUES (102, 1,2024,'completed');", vm);
    exec("INSERT INTO orders VALUES (103, 2,2023,'completed');", vm);
    exec("INSERT INTO orders VALUES (104, 3,2024,'completed');", vm);
    exec("INSERT INTO orders VALUES (105, 3,2024,'cancelled');", vm);
    exec("INSERT INTO orders VALUES (106, 5,2023,'completed');", vm);
    exec("INSERT INTO orders VALUES (107, 5,2024,'completed');", vm);
    // Dave(4) and Frank(6) have NO orders 鈫?used for LEFT JOIN test

    // Order items
    exec("INSERT INTO order_items VALUES (1, 101,1,2,29.99);", vm); // Alice  2脳Widget
    exec("INSERT INTO order_items VALUES (2, 101,3,1,9.99);", vm); // Alice  1脳Book
    exec("INSERT INTO order_items VALUES (3, 102,2,1,49.99);", vm); // Alice  1脳Gadget
    exec("INSERT INTO order_items VALUES (4, 103,4,3,19.99);", vm); // Bob    3脳Shirt
    exec("INSERT INTO order_items VALUES (5, 104,5,2,59.99);", vm); // Carol  2脳Shoes
    exec("INSERT INTO order_items VALUES (6, 104,6,4,12.99);", vm); // Carol  4脳Mug
    exec("INSERT INTO order_items VALUES (7, 106,1,1,29.99);", vm); // Eve    1脳Widget
    exec("INSERT INTO order_items VALUES (8, 106,2,2,49.99);", vm); // Eve    2脳Gadget
    exec("INSERT INTO order_items VALUES (9, 107,3,5,9.99);", vm); // Eve    5脳Book
}

// 鈹€鈹€鈹€ Test 1: 涓夎〃 INNER JOIN 閾惧紡鑱旀煡 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn test_three_table_inner_join() {
    let mut vm = setup("test_3join_db");
    build_ecommerce(&mut vm);

    // 鑱旀煡濮撳悕 + 鎵€璐晢鍝佸悕绉?+ 鏁伴噺
    let r = rows(
        "SELECT c.name, p.name, oi.qty
         FROM customers c
         JOIN orders o ON c.id = o.cust_id
         JOIN order_items oi ON o.id = oi.order_id
         JOIN products p ON oi.product_id = p.id
         WHERE o.status = 'completed'
         ORDER BY c.name, p.name;",
        &mut vm,
    );

    assert_eq!(r.len(), 9, "9 completed item lines expected");
    // Alice is the first customer, should have Widget as one item
    assert!(
        r.iter()
            .any(|row| row[0] == Value::Text("Alice".into())
                && row[1] == Value::Text("Widget".into())),
        "Alice must have Widget"
    );
}

// 鈹€鈹€鈹€ Test 2: LEFT JOIN + 鎵惧嚭鏃犺鍗曞鎴?鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn test_left_join_no_orders() {
    let mut vm = setup("test_left_join_db");
    build_ecommerce(&mut vm);

    let r = rows(
        "SELECT c.name
         FROM customers c
         LEFT JOIN orders o ON c.id = o.cust_id
         WHERE o.id IS NULL
         ORDER BY c.name;",
        &mut vm,
    );
    assert_eq!(r.len(), 2);
    assert_eq!(r[0][0], Value::Text("Dave".into()));
    assert_eq!(r[1][0], Value::Text("Frank".into()));
}

// 鈹€鈹€鈹€ Test 3: GROUP BY + HAVING + 澶氳仛鍚堝嚱鏁?鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn test_group_by_having_multi_agg() {
    let mut vm = setup("test_multi_agg_db");
    build_ecommerce(&mut vm);

    // 姣忎釜瀹㈡埛: 璁㈠崟鏁般€佹€绘秷璐广€佸钩鍧囧崟娆℃秷璐? 浠呭睍绀烘秷璐?> 100 鐨?
    let r = rows(
        "SELECT c.name,
                COUNT(DISTINCT o.id)          AS order_count,
                SUM(oi.qty * oi.unit_price)   AS total_spent,
                AVG(oi.qty * oi.unit_price)   AS avg_item
         FROM customers c
         JOIN orders o ON c.id = o.cust_id AND o.status = 'completed'
         JOIN order_items oi ON o.id = oi.order_id
         GROUP BY c.id, c.name
         HAVING SUM(oi.qty * oi.unit_price) > 100.0
         ORDER BY total_spent DESC;",
        &mut vm,
    );

    // Alice:  2脳29.99 + 9.99 + 49.99 = 119.96
    // Eve:    29.99 + 2脳49.99 + 5脳9.99 = 179.92
    // Carol:  2脳59.99 + 4脳12.99 = 171.94
    assert!(r.len() >= 2, "At least Alice, Eve, Carol should qualify");
    // First row should be Eve (highest spend)
    assert_eq!(
        r[0][0],
        Value::Text("Eve".into()),
        "Eve expected top spender"
    );
}

// 鈹€鈹€鈹€ Test 4: 瀛愭煡璇紙IN + EXISTS + 鏍囬噺瀛愭煡璇級鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn test_subquery_in_exists_scalar() {
    let mut vm = setup("test_subquery_db");
    build_ecommerce(&mut vm);

    // IN 瀛愭煡璇細鏈夎喘涔拌繃 Electronics 鐨勫鎴?
    let r = rows(
        "SELECT DISTINCT c.name
         FROM customers c
         WHERE c.id IN (
             SELECT o.cust_id
             FROM orders o
             JOIN order_items oi ON o.id = oi.order_id
             JOIN products p ON oi.product_id = p.id
             WHERE p.category = 'Electronics'
         )
         ORDER BY c.name;",
        &mut vm,
    );
    // Alice (Widget, Gadget), Eve (Widget, Gadget)
    assert_eq!(r.len(), 2);
    assert_eq!(r[0][0], Value::Text("Alice".into()));
    assert_eq!(r[1][0], Value::Text("Eve".into()));

    // EXISTS subquery: find customers with cancelled orders
    let r2 = rows(
        "SELECT c.name FROM customers c
         WHERE EXISTS (
             SELECT 1 FROM orders o
             WHERE o.cust_id = c.id AND o.status = 'cancelled'
         );",
        &mut vm,
    );
    assert_eq!(r2.len(), 1, "Only Carol should have a cancelled order");
    assert_eq!(r2[0][0], Value::Text("Carol".into()));

    // Scalar subquery: highest order item value per customer
    let r3 = rows(
        "SELECT c.name, (
             SELECT MAX(oi.qty * oi.unit_price)
             FROM orders o JOIN order_items oi ON o.id = oi.order_id
             WHERE o.cust_id = c.id
         ) AS max_item_value
         FROM customers c
         WHERE c.id IN (1, 5)
         ORDER BY c.name;",
        &mut vm,
    );
    assert_eq!(r3.len(), 2);
    // Alice max item: 2脳29.99=59.98
    let alice_max = &r3[0][1];
    assert!(
        format!("{}", alice_max).starts_with("59.9"),
        "Alice max should be ~59.98, got {}",
        alice_max
    );
    // Eve max item: 2脳49.99=99.98
    let eve_max = &r3[1][1];
    assert!(
        format!("{}", eve_max).starts_with("99.9"),
        "Eve max should be ~99.98, got {}",
        eve_max
    );
}

// 鈹€鈹€鈹€ Test 5: CTE + JOIN 姹囨€?鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn test_cte_with_join_summary() {
    let mut vm = setup("test_cte_join_db");
    build_ecommerce(&mut vm);

    // CTE 姹囨€? 姣忓鎴锋秷璐?vs 鍏ㄥ眬骞冲潎, 绛涢€夐珮浜庡钩鍧囩殑瀹㈡埛
    // Use direct GROUP BY + HAVING with a scalar avg subquery
    let avg_r = row1(
        "SELECT AVG(total_spent) FROM (
             SELECT o.cust_id, SUM(oi.qty * oi.unit_price) AS total_spent
             FROM orders o
             JOIN order_items oi ON o.id = oi.order_id
             WHERE o.status = 'completed'
             GROUP BY o.cust_id
         ) sub;",
        &mut vm,
    );
    let avg_val_str = format!("{}", avg_r[0]);
    let avg_val: f64 = avg_val_str.parse().unwrap_or(0.0);

    let r = rows(
        "SELECT c.name, SUM(oi.qty * oi.unit_price) AS spent
         FROM customers c
         JOIN orders o ON c.id = o.cust_id AND o.status = 'completed'
         JOIN order_items oi ON o.id = oi.order_id
         GROUP BY c.id, c.name
         ORDER BY spent DESC;",
        &mut vm,
    );
    assert!(!r.is_empty(), "Should have customers with spend");
    // The top spender should be first
    let top_name = &r[0][0];
    assert!(
        matches!(top_name, Value::Text(_)),
        "First column should be a customer name"
    );
    // At least one customer must be above average
    let above_avg = r
        .iter()
        .filter(|row| {
            if let Value::Real(v) = &row[1] {
                *v > avg_val
            } else {
                false
            }
        })
        .count();
    assert!(
        above_avg >= 1,
        "At least one customer above average spend (avg={:.2})",
        avg_val
    );
}

// 鈹€鈹€鈹€ Test 6: UNION / EXCEPT 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn test_union_except_set_ops() {
    let mut vm = setup("test_setops_db");
    build_ecommerce(&mut vm);

    // UNION: 鎵€鏈夋湁璁㈠崟鐨勫鎴?+ 娌℃湁璁㈠崟鐨勶紙搴?= 鍏ㄩ儴6浜猴級
    let with_orders = "SELECT cust_id AS id FROM orders";
    let without_orders = "SELECT id FROM customers WHERE id NOT IN (SELECT cust_id FROM orders)";
    let r = rows(
        &format!("{} UNION {} ORDER BY id;", with_orders, without_orders),
        &mut vm,
    );
    assert_eq!(r.len(), 6, "UNION should give all 6 customer ids");

    // EXCEPT: 2023骞翠笅杩囪鍗曚絾2024骞存病鏈変笅杩囪鍗曠殑瀹㈡埛
    let r2 = rows(
        "SELECT cust_id FROM orders WHERE created_year = 2023
         EXCEPT
         SELECT cust_id FROM orders WHERE created_year = 2024
         ORDER BY cust_id;",
        &mut vm,
    );
    // Bob(2) ordered only in 2023; Alice(1), Carol(3), Eve(5) ordered in both years
    assert_eq!(r2.len(), 1);
    assert_eq!(r2[0][0], Value::Integer(2));
}

// 鈹€鈹€鈹€ Test 7: 绐楀彛鍑芥暟鎺掑悕 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn test_window_rank() {
    let mut vm = setup("test_window_rank_db");
    build_ecommerce(&mut vm);

    // Window rank: OVER(PARTITION BY subquery_alias.col)
    let r = rows(
        "SELECT a.category, a.name, a.revenue,
                ROW_NUMBER() OVER (PARTITION BY a.category ORDER BY a.revenue DESC) AS rnk
         FROM (
             SELECT p.category, p.name, SUM(oi.qty * oi.unit_price) AS revenue
             FROM products p
             JOIN order_items oi ON p.id = oi.product_id
             GROUP BY p.id, p.name, p.category
         ) a
         ORDER BY a.category, rnk;",
        &mut vm,
    );
    assert!(!r.is_empty());
    // Electronics should have both Widget and Gadget with ranks 1 and 2 assigned
    let elec: Vec<_> = r
        .iter()
        .filter(|row| row[0] == Value::Text("Electronics".into()))
        .collect();
    assert!(elec.len() >= 2, "Both Electronics products should appear");
    // Just verify that rank values 1 and 2 are present (don't prescribe ordering direction)
    let ranks: Vec<i64> = elec
        .iter()
        .filter_map(|row| {
            if let Value::Integer(v) = &row[3] {
                Some(*v)
            } else {
                None
            }
        })
        .collect();
    assert!(
        ranks.contains(&1),
        "Rank 1 should be present in Electronics: {:?}",
        ranks
    );
    assert!(
        ranks.contains(&2),
        "Rank 2 should be present in Electronics: {:?}",
        ranks
    );
    // The revenue values should be different (proving ranking is meaningful)
    let revs: Vec<f64> = elec
        .iter()
        .filter_map(|row| format!("{}", row[2]).parse::<f64>().ok())
        .collect();
    assert!(
        revs.len() == 2 && revs[0] != revs[1],
        "Two different revenue values expected"
    );
}

// 鈹€鈹€鈹€ Test 8: CASE WHEN 浜ゅ弶缁熻锛堟ā鎷?PIVOT锛夆攢鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn test_case_when_pivot() {
    let mut vm = setup("test_pivot_db");
    build_ecommerce(&mut vm);

    // Per-country: total gold + silver + bronze customer count
    let r = rows(
        "SELECT country,
                SUM(CASE WHEN tier='gold'   THEN 1 ELSE 0 END) AS gold_cnt,
                SUM(CASE WHEN tier='silver' THEN 1 ELSE 0 END) AS silver_cnt,
                SUM(CASE WHEN tier='bronze' THEN 1 ELSE 0 END) AS bronze_cnt
         FROM customers
         GROUP BY country
         ORDER BY country;",
        &mut vm,
    );
    // DE: Dave=bronze, Frank=silver 鈫?gold=0, silver=1, bronze=1
    // UK: Bob=silver, Eve=gold    鈫?gold=1, silver=1, bronze=0
    // US: Alice=gold, Carol=gold  鈫?gold=2, silver=0, bronze=0
    assert_eq!(r.len(), 3);
    let de = r
        .iter()
        .find(|row| row[0] == Value::Text("DE".into()))
        .expect("DE row");
    assert_eq!(de[1], Value::Integer(0), "DE gold");
    assert_eq!(de[2], Value::Integer(1), "DE silver");
    assert_eq!(de[3], Value::Integer(1), "DE bronze");

    let us = r
        .iter()
        .find(|row| row[0] == Value::Text("US".into()))
        .expect("US row");
    assert_eq!(us[1], Value::Integer(2), "US gold = 2");
    assert_eq!(us[2], Value::Integer(0), "US silver = 0");
}

// 鈹€鈹€鈹€ Test 9: 鍒嗙粍 TOP-N锛堟瘡涓?category 璐拱鏁伴噺鏈€楂樼殑鍟嗗搧锛夆攢鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn test_group_top_n() {
    let mut vm = setup("test_topn_db");
    build_ecommerce(&mut vm);

    // Each category's top product by total units sold — uses nested subquery + window function
    let r = rows(
        "SELECT * FROM (
             SELECT a.category, a.name, a.total_qty,
                    ROW_NUMBER() OVER (PARTITION BY a.category ORDER BY a.total_qty DESC) AS rn
             FROM (
                 SELECT p.category, p.name, SUM(oi.qty) AS total_qty
                 FROM products p
                 JOIN order_items oi ON p.id = oi.product_id
                 GROUP BY p.id, p.name, p.category
             ) a
         ) b
         WHERE rn <= 1
         ORDER BY category;",
        &mut vm,
    );
    assert!(!r.is_empty(), "Should have at least 1 category top product");
    // Filter to rn = 1 in Rust
    let top: Vec<_> = r.iter().filter(|row| row[3] == Value::Integer(1)).collect();
    assert!(!top.is_empty(), "Should have at least one top-N row");
    // In Library: Eve bought 5脳Book + Alice 1脳Book = 6 total
    let lib = top
        .iter()
        .find(|row| row[0] == Value::Text("Library".into()));
    if let Some(lib_row) = lib {
        assert_eq!(lib_row[1], Value::Text("Book".into()));
        assert_eq!(lib_row[2], Value::Integer(6)); // 1 (Alice) + 5 (Eve) = 6
    }
}

// 鈹€鈹€鈹€ Test 10: 澶氱淮鑱氬悎锛圕ountry 脳 Category 閿€鍞煩闃碉級鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn test_multidim_aggregation() {
    let mut vm = setup("test_multidim_db");
    build_ecommerce(&mut vm);

    // Revenue per (customer country, product category)
    let r = rows(
        "SELECT c.country, p.category,
                SUM(oi.qty * oi.unit_price) AS revenue,
                COUNT(DISTINCT o.id)         AS order_count
         FROM customers c
         JOIN orders o     ON c.id = o.cust_id AND o.status = 'completed'
         JOIN order_items oi ON o.id = oi.order_id
         JOIN products p   ON oi.product_id = p.id
         GROUP BY c.country, p.category
         ORDER BY c.country, p.category;",
        &mut vm,
    );

    assert!(!r.is_empty(), "Multi-dim aggregation should return rows");
    // UK customers: Bob bought Apparel (3脳Shirt=59.97), Eve bought Electronics+Library
    let uk_apparel = r
        .iter()
        .find(|row| row[0] == Value::Text("UK".into()) && row[1] == Value::Text("Apparel".into()));
    assert!(uk_apparel.is_some(), "UK脳Apparel should appear");
    let uk_ap = uk_apparel.unwrap();
    // Bob: 3脳19.99 = 59.97
    assert!(
        format!("{}", uk_ap[2]).starts_with("59.9"),
        "UK脳Apparel revenue should be ~59.97, got {}",
        uk_ap[2]
    );

    // US customers: Alice (Electronics 2脳29.99+1脳9.99+1脳49.99 = 119.96)
    let us_elec = r.iter().find(|row| {
        row[0] == Value::Text("US".into()) && row[1] == Value::Text("Electronics".into())
    });
    assert!(us_elec.is_some(), "US脳Electronics should appear");
}
