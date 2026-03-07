# KKDB 高阶 SQL 分析特性指南

除了标准的被广泛使用的基础 CRUD 语句，KKDB 引擎内部同样支持一套完备且强大的高阶数据分析 SQL 语法。本文档将详细介绍这些语法特性，并辅以代码示例，帮助你在 KKDB 中完成复杂的数据统计与分析工作。

---

## 1. 窗口函数 (Window Functions)

KKDB 支持基础的窗口函数计算，目前主要支持 `ROW_NUMBER()`（配合 `OVER` 子句实现分组内排序与 Top-N 提取）。更强大的窗口函数如 `RANK()` 与 `DENSE_RANK()` 也在演进之中。

### 分组 Top-N 查询示例

获取每个商品类别下总销量最高的商品（即分组 Top 1）：

```sql
SELECT * FROM (
    SELECT a.category, a.name, a.total_qty,
           ROW_NUMBER() OVER (PARTITION BY a.category ORDER BY a.total_qty DESC) AS rn
    FROM (
        SELECT p.category, p.name, SUM(oi.qty) AS total_qty
        FROM products p
        JOIN order_items oi ON p.id = oi.product_id
        GROUP BY p.id, p.name, p.category
    ) a
)
WHERE rn <= 1
ORDER BY category;
```

> **注意：** KKDB 完全支持 `PARTITION BY` 子句中指向子查询别名的列（如上方的 `a.category`）。

---

## 2. 通用表表达式 (CTE / WITH 语句)

通过 `WITH` 语句，您可以提前定义一个或多个临时结果集，这有助于将超长的复杂查询分解成易于消化的模块。

### 示例：筛选高于平均消费的客户

```sql
WITH customer_totals AS (
    SELECT o.cust_id, SUM(oi.qty * oi.unit_price) AS total_spent
    FROM orders o
    JOIN order_items oi ON o.id = oi.order_id
    GROUP BY o.cust_id
),
global_average AS (
    SELECT AVG(total_spent) AS global_avg FROM customer_totals
)
SELECT ct.cust_id, ct.total_spent
FROM customer_totals ct
CROSS JOIN global_average ga
WHERE ct.total_spent > ga.global_avg
ORDER BY ct.total_spent DESC;
```

---

## 3. 相关子查询 (Correlated Subqueries)

KKDB 的虚拟机支持上下文感知的表达式求值环境，这意味着 **子查询内部能够完全引用外部查询的列**。KKDB 支持关联的 `EXISTS`, `IN`, `ANY/ALL` 以及标量子查询。

### 示例 1: EXISTS 查询关联

查询所有存在过被取消订单的客户名字：

```sql
SELECT c.name 
FROM customers c
WHERE EXISTS (
    SELECT 1 FROM orders o
    WHERE o.cust_id = c.id AND o.status = 'cancelled'
);
```
这里的 `c.id` 是外部查询传递入当前上下文的关联变量。

### 示例 2: 标量子查询关联

在单一查询中直接将每位顾客的最高单次订单项金额作为一列输出：

```sql
SELECT c.name, (
    SELECT MAX(oi.qty * oi.unit_price)
    FROM orders o JOIN order_items oi ON o.id = oi.order_id
    WHERE o.cust_id = c.id
) AS max_item_value
FROM customers c
ORDER BY c.name;
```

---

## 4. 集合操作 (Set Operations)

KKDB 完美支持多表的集合运算，将两个或多个 `SELECT` 的结果集合并。

- `UNION`：取并集并去重。
- `UNION ALL`：取并集不去重。
- `INTERSECT`：取交集。
- `EXCEPT`：取差集。

### 示例：高等级客户但没有发生过退款

```sql
SELECT id, name FROM customers WHERE tier IN ('gold', 'silver')
EXCEPT
SELECT c.id, c.name FROM customers c
JOIN orders o ON c.id = o.cust_id
WHERE o.status = 'refunded';
```

---

## 5. 复杂条件聚合 (Pivot / Case When)

通过在聚合函数（如 `SUM`）内嵌 `CASE WHEN` 表达式，可以在不借助专门扩展的情况下实现数据透视表（Pivot Table）功能。

### 示例：按国家统计会员等级分布

```sql
SELECT country,
       SUM(CASE WHEN tier='gold'   THEN 1 ELSE 0 END) AS gold_cnt,
       SUM(CASE WHEN tier='silver' THEN 1 ELSE 0 END) AS silver_cnt,
       SUM(CASE WHEN tier='bronze' THEN 1 ELSE 0 END) AS bronze_cnt
FROM customers
GROUP BY country
ORDER BY country;
```

结果产出与常见数据看板的维度切分是一致的结构，便于 BI 工具直接渲染。

---

## 6. 多表与多维聚合 (Multi-table & Multi-dimensional Aggregation)

KKDB 支持跨三表及以上的稳定 `INNER JOIN` 与 `LEFT JOIN`，并且具备完善的 `GROUP BY` 与 `HAVING` 子句的串联机制。

### 示例：各国家和各个品类的消费矩阵

检索所有已完成的订单，计算各个国家每一个产品维度的总销售额：

```sql
SELECT c.country, p.category,
       SUM(oi.qty * oi.unit_price) AS revenue,
       COUNT(DISTINCT o.id) AS order_count
FROM customers c
JOIN orders o ON c.id = o.cust_id AND o.status = 'completed'
JOIN order_items oi ON o.id = oi.order_id
JOIN products p ON oi.product_id = p.id
GROUP BY c.country, p.category
HAVING SUM(oi.qty * oi.unit_price) > 100
ORDER BY revenue DESC;
```

这展示了 KKDB 处理传统星型 / 雪花型模型宽表聚合查询的坚实基础。

---

如果需要在代码中或 CLI 环境尝试以上查询示例，建议先使用相关的 DDL 建表并导入一些模拟数据。所有特性的测试都可以参考代码仓库中的 `tests/analytics_tests.rs` 以获得更多细节！
