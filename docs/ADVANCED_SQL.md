# KKDB 高阶 SQL 特性完全指南

本指南覆盖 KKDB 引擎所支持的全部高阶 SQL 特性，包括数据分析、事务控制、安全权限、全文检索等。

---

## 目录

1. [窗口函数](#1-窗口函数)
2. [通用表表达式（CTE / WITH）](#2-通用表表达式)
3. [相关子查询](#3-相关子查询)
4. [集合操作](#4-集合操作)
5. [复杂聚合与透视](#5-复杂聚合与透视)
6. [多表多维聚合](#6-多表多维聚合)
7. [事务与并发控制](#7-事务与并发控制)
8. [外键约束](#8-外键约束)
9. [CHECK 约束与唯一约束](#9-check-约束与唯一约束)
10. [触发器](#10-触发器)
11. [行级安全（RLS）](#11-行级安全)
12. [全文检索（BM25）](#12-全文检索)
13. [视图](#13-视图)
14. [RETURNING 子句](#14-returning-子句)
15. [ON CONFLICT（Upsert）](#15-on-conflict)
16. [ANALYZE 与自适应索引](#16-analyze-与自适应索引)

---

## 1. 窗口函数

KKDB 支持完整的窗口函数框架，包括 `ROW_NUMBER`、`RANK`、`DENSE_RANK`、`LEAD`、`LAG`、`NTILE`，以及聚合函数作为窗口函数（`SUM OVER`、`AVG OVER` 等）。

```sql
-- ROW_NUMBER: 每个部门内按薪资排名
SELECT
    name, department, salary,
    ROW_NUMBER() OVER (PARTITION BY department ORDER BY salary DESC) AS rn
FROM employees;

-- RANK / DENSE_RANK: 同薪同名次
SELECT
    name, salary,
    RANK()       OVER (ORDER BY salary DESC) AS rank,
    DENSE_RANK() OVER (ORDER BY salary DESC) AS dense_rank
FROM employees;

-- 分组 Top-N: 每个类别销量最高的商品
SELECT * FROM (
    SELECT p.category, p.name, SUM(oi.qty) AS total_qty,
           ROW_NUMBER() OVER (PARTITION BY p.category ORDER BY SUM(oi.qty) DESC) AS rn
    FROM products p
    JOIN order_items oi ON p.id = oi.product_id
    GROUP BY p.id, p.name, p.category
)
WHERE rn <= 3
ORDER BY category, rn;

-- LEAD / LAG: 访问前后行
SELECT
    month, revenue,
    LAG(revenue, 1)  OVER (ORDER BY month) AS prev_revenue,
    LEAD(revenue, 1) OVER (ORDER BY month) AS next_revenue,
    revenue - LAG(revenue, 1) OVER (ORDER BY month) AS mom_change
FROM monthly_revenue;

-- 累计聚合
SELECT
    order_date,
    amount,
    SUM(amount)  OVER (ORDER BY order_date ROWS UNBOUNDED PRECEDING) AS cumulative,
    AVG(amount)  OVER (ORDER BY order_date ROWS BETWEEN 6 PRECEDING AND CURRENT ROW) AS ma7
FROM orders;
```

---

## 2. 通用表表达式

### 普通 CTE

将复杂查询拆解为可读的命名子查询：

```sql
WITH
  customer_totals AS (
      SELECT o.cust_id, SUM(oi.qty * oi.unit_price) AS total_spent
      FROM orders o
      JOIN order_items oi ON o.id = oi.order_id
      GROUP BY o.cust_id
  ),
  global_avg AS (
      SELECT AVG(total_spent) AS avg_spend FROM customer_totals
  )
SELECT ct.cust_id, ct.total_spent,
       ROUND(ct.total_spent / ga.avg_spend, 2) AS ratio_to_avg
FROM customer_totals ct
CROSS JOIN global_avg ga
WHERE ct.total_spent > ga.avg_spend
ORDER BY ct.total_spent DESC;
```

### 递归 CTE（WITH RECURSIVE）

生成序列、展开树形结构：

```sql
-- 生成 1-100 的整数序列
WITH RECURSIVE nums AS (
    SELECT 1 AS n
    UNION ALL
    SELECT n + 1 FROM nums WHERE n < 100
)
SELECT n FROM nums;

-- 展开组织层级（员工-上级关系）
WITH RECURSIVE org_tree AS (
    SELECT id, name, manager_id, 0 AS depth
    FROM employees WHERE manager_id IS NULL
    UNION ALL
    SELECT e.id, e.name, e.manager_id, ot.depth + 1
    FROM employees e
    JOIN org_tree ot ON e.manager_id = ot.id
)
SELECT depth, name FROM org_tree ORDER BY depth, name;
```

---

## 3. 相关子查询

子查询内可完整引用外部查询的列：

```sql
-- EXISTS: 查找有过取消订单的客户
SELECT c.name
FROM customers c
WHERE EXISTS (
    SELECT 1 FROM orders o
    WHERE o.cust_id = c.id AND o.status = 'cancelled'
);

-- NOT EXISTS: 从未下单的客户
SELECT c.name
FROM customers c
WHERE NOT EXISTS (SELECT 1 FROM orders o WHERE o.cust_id = c.id);

-- IN + 子查询关联
SELECT p.name
FROM products p
WHERE p.id IN (
    SELECT oi.product_id FROM order_items oi
    JOIN orders o ON oi.order_id = o.id
    WHERE o.status = 'completed'
);

-- 标量子查询: 每位客户最大单笔订单金额
SELECT c.name, (
    SELECT MAX(oi.qty * oi.unit_price)
    FROM orders o JOIN order_items oi ON o.id = oi.order_id
    WHERE o.cust_id = c.id
) AS max_item_value
FROM customers c
ORDER BY max_item_value DESC;

-- ANY / ALL
SELECT name, salary
FROM employees
WHERE salary > ALL (
    SELECT AVG(salary) FROM employees GROUP BY department
);
```

---

## 4. 集合操作

```sql
-- UNION: 合并并去重
SELECT id, name, 'customer' AS type FROM customers
UNION
SELECT id, name, 'supplier' AS type FROM suppliers;

-- UNION ALL: 合并不去重（保留全部行）
SELECT product_id FROM order_items WHERE qty > 10
UNION ALL
SELECT product_id FROM returns;

-- INTERSECT: 取交集
SELECT cust_id FROM orders WHERE status = 'completed'
INTERSECT
SELECT cust_id FROM orders WHERE amount > 1000;

-- EXCEPT: 差集（高等级客户中无退款记录的）
SELECT id, name FROM customers WHERE tier IN ('gold', 'silver')
EXCEPT
SELECT c.id, c.name FROM customers c
JOIN orders o ON c.id = o.cust_id
WHERE o.status = 'refunded';
```

---

## 5. 复杂聚合与透视

### CASE WHEN 透视

```sql
-- 按国家统计会员等级分布（Pivot）
SELECT country,
       SUM(CASE WHEN tier = 'gold'   THEN 1 ELSE 0 END) AS gold_cnt,
       SUM(CASE WHEN tier = 'silver' THEN 1 ELSE 0 END) AS silver_cnt,
       SUM(CASE WHEN tier = 'bronze' THEN 1 ELSE 0 END) AS bronze_cnt,
       COUNT(*) AS total
FROM customers
GROUP BY country
ORDER BY total DESC;

-- 条件聚合：仅统计已完成订单的金额
SELECT
    cust_id,
    COUNT(*) AS total_orders,
    SUM(CASE WHEN status = 'completed' THEN amount ELSE 0 END) AS completed_revenue,
    SUM(CASE WHEN status = 'refunded'  THEN amount ELSE 0 END) AS refunded_amount
FROM orders
GROUP BY cust_id
HAVING completed_revenue > 0;
```

---

## 6. 多表多维聚合

```sql
-- 各国家 × 各品类的收入矩阵（四表 JOIN）
SELECT
    c.country, p.category,
    SUM(oi.qty * oi.unit_price) AS revenue,
    COUNT(DISTINCT o.id)         AS order_count,
    COUNT(DISTINCT o.cust_id)    AS unique_customers
FROM customers c
JOIN orders o      ON c.id = o.cust_id AND o.status = 'completed'
JOIN order_items oi ON o.id = oi.order_id
JOIN products p    ON oi.product_id = p.id
GROUP BY c.country, p.category
HAVING SUM(oi.qty * oi.unit_price) > 100
ORDER BY revenue DESC;
```

---

## 7. 事务与并发控制

### 标准事务

```sql
BEGIN;
INSERT INTO accounts (id, balance) VALUES (1, 1000);
UPDATE accounts SET balance = balance - 100 WHERE id = 1;
UPDATE accounts SET balance = balance + 100 WHERE id = 2;
COMMIT;
```

### 事务回滚

```sql
BEGIN;
UPDATE inventory SET qty = qty - 5 WHERE product_id = 42;
-- 发现库存不足，回滚
ROLLBACK;
```

### 保存点（嵌套事务）

```sql
BEGIN;
INSERT INTO orders (cust_id, amount) VALUES (1, 500);
SAVEPOINT sp1;

INSERT INTO order_items (order_id, product_id, qty) VALUES (LAST_INSERT_ROWID(), 10, 2);

-- 只回滚到 sp1，保留 orders 的插入
ROLLBACK TO SAVEPOINT sp1;

-- 改插其他商品，再提交
INSERT INTO order_items (order_id, product_id, qty) VALUES (LAST_INSERT_ROWID(), 20, 1);
COMMIT;
```

### 锁与死锁

KKDB 使用全局表级锁（Shared / Exclusive）和等待图死锁检测：

```sql
-- 会话 A: 锁定 orders 表
BEGIN;
UPDATE orders SET status = 'processing' WHERE id = 1;

-- 会话 B: 尝试锁定同一表（等待）
BEGIN;
UPDATE orders SET status = 'cancelled' WHERE id = 2;
-- 若两会话形成循环等待，KKDB 将报告 Deadlock detected 并回滚代价较小的事务
```

---

## 8. 外键约束

```sql
CREATE TABLE customers (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL
);

CREATE TABLE orders (
    id      INTEGER PRIMARY KEY,
    cust_id INTEGER NOT NULL
        REFERENCES customers(id)
            ON DELETE CASCADE    -- 删除客户时级联删除订单
            ON UPDATE RESTRICT,  -- 禁止更新被引用的客户 ID
    amount  REAL
);

-- 插入时校验外键
INSERT INTO orders (cust_id, amount) VALUES (999, 100.0);
-- Error: Foreign key constraint failed: orders.cust_id references customers.id

-- 级联删除
DELETE FROM customers WHERE id = 1;
-- 自动删除所有 cust_id = 1 的 orders 行
```

---

## 9. CHECK 约束与唯一约束

```sql
CREATE TABLE products (
    id       INTEGER PRIMARY KEY,
    name     TEXT    NOT NULL,
    price    REAL    NOT NULL CHECK (price >= 0),
    stock    INTEGER NOT NULL CHECK (stock >= 0),
    sku      TEXT    UNIQUE,
    category TEXT    CHECK (category IN ('electronics', 'clothing', 'food'))
);

-- CHECK 约束在 INSERT/UPDATE 时自动验证
INSERT INTO products (name, price, stock, sku) VALUES ('Widget', -5.0, 10, 'W001');
-- Error: CHECK constraint failed: products (price >= 0)
```

---

## 10. 触发器

```sql
-- AFTER INSERT: 自动记录审计日志
CREATE TRIGGER trg_audit_insert
  AFTER INSERT ON orders
  FOR EACH ROW
  BEGIN
    INSERT INTO audit_log (table_name, action, record_id, ts)
    VALUES ('orders', 'INSERT', NEW.id, NOW());
  END;

-- BEFORE UPDATE: 防止已完成订单被修改
CREATE TRIGGER trg_protect_completed
  BEFORE UPDATE ON orders
  FOR EACH ROW
  WHEN OLD.status = 'completed'
  BEGIN
    SELECT RAISE(ABORT, 'Cannot modify a completed order');
  END;

-- AFTER DELETE: 更新库存统计
CREATE TRIGGER trg_restore_stock
  AFTER DELETE ON order_items
  FOR EACH ROW
  BEGIN
    UPDATE products SET stock = stock + OLD.qty WHERE id = OLD.product_id;
  END;

-- 查看触发器（通过 .schema 命令）
-- DROP TRIGGER trg_audit_insert;
```

---

## 11. 行级安全

对同一张表的不同用户只显示其有权查看的行：

```sql
-- 开启 RLS
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;

-- 为普通用户创建策略（只能看自己的订单）
CREATE POLICY orders_own ON orders
  USING (cust_id = (SELECT id FROM customers WHERE name = CURRENT_USER));

-- 为管理员创建无限制策略
CREATE POLICY orders_admin ON orders
  TO 'admin'
  USING (1 = 1);

-- 在会话中设置当前用户
SET kkdb.current_user = 'alice';

-- alice 只能看到自己的订单
SELECT * FROM orders;  -- 自动注入 WHERE cust_id = alice.id

-- 删除策略
DROP POLICY orders_own ON orders;
```

---

## 12. 全文检索

KKDB 内置 BM25 全文检索引擎，支持中英文混合文档：

```sql
-- 建表
CREATE TABLE articles (
    id      INTEGER PRIMARY KEY,
    title   TEXT NOT NULL,
    content TEXT
);

-- 创建全文索引（可对多列）
CREATE FULLTEXT INDEX idx_ft ON articles (title, content);

-- 插入数据
INSERT INTO articles VALUES
    (1, 'Rust Programming', 'Rust is a systems programming language...'),
    (2, 'Database Design',  'B-Tree is fundamental to database storage...'),
    (3, 'Rust Database',    'Building a database in Rust using B-Tree...');

-- BM25 相关性检索（支持多关键词自动 OR 合并）
SELECT id, title,
       FTS_MATCH('articles', 'idx_ft', 'rust database') AS score
FROM articles
WHERE FTS_MATCH('articles', 'idx_ft', 'rust database') > 0
ORDER BY score DESC
LIMIT 10;
```

> **中文分词说明：** KKDB 已集成 `jieba-rs`（结巴中文分词，`Cargo.toml: jieba-rs = "0.7"`），可对中文内容自动分词。查询时关键词之间以空格分隔即可，例如：`FTS_MATCH('t', 'idx', '数据库 性能')`。

---

## 13. 视图

```sql
-- 创建视图
CREATE VIEW active_customers AS
    SELECT c.id, c.name, c.country, COUNT(o.id) AS order_count
    FROM customers c
    LEFT JOIN orders o ON c.id = o.cust_id AND o.status = 'completed'
    GROUP BY c.id, c.name, c.country
    HAVING COUNT(o.id) > 0;

-- 使用视图（透明展开）
SELECT * FROM active_customers WHERE country = 'US' ORDER BY order_count DESC;

-- 替换视图
CREATE OR REPLACE VIEW active_customers AS
    SELECT c.id, c.name FROM customers c WHERE c.active = 1;

-- 删除视图
DROP VIEW IF EXISTS active_customers;
```

---

## 14. RETURNING 子句

在 INSERT / UPDATE / DELETE 后立即返回受影响行的数据（无需额外 SELECT）：

```sql
-- INSERT RETURNING: 获取自动生成的主键
INSERT INTO orders (cust_id, amount)
VALUES (1, 500.00)
RETURNING id, created_at;

-- UPDATE RETURNING: 获取更新后的新值
UPDATE accounts
SET balance = balance - 100
WHERE id = 1
RETURNING id, balance AS new_balance;

-- DELETE RETURNING: 获取已删除行的内容
DELETE FROM cart_items WHERE session_id = 'abc123'
RETURNING product_id, qty;
```

---

## 15. ON CONFLICT（Upsert）

```sql
-- IGNORE: 主键冲突时跳过（不抛错）
INSERT OR IGNORE INTO settings (key, value) VALUES ('theme', 'dark');

-- REPLACE: 冲突时先删除再插入（完整替换）
INSERT OR REPLACE INTO cache (key, value, expires_at)
VALUES ('session:42', '{"user":"alice"}', NOW() + 3600);

-- ON CONFLICT DO UPDATE SET（PostgreSQL 风格 Upsert）
INSERT INTO product_stats (product_id, view_count)
VALUES (101, 1)
ON CONFLICT (product_id) DO UPDATE SET
    view_count = view_count + 1,
    last_viewed = NOW();
```

---

## 16. ANALYZE 与自适应索引

### 手动收集统计信息

```sql
-- 扫描全表，更新列统计（NDV、min、max、null_count）
ANALYZE TABLE orders;
ANALYZE TABLE products;
```

### 自适应索引（运行时）

KKDB 在运行时统计全表扫描频率。当某列触发全扫描次数超过阈值时，自动创建索引：

```sql
-- 重复执行会触发自适应索引创建
SELECT * FROM orders WHERE status = 'pending';
SELECT * FROM orders WHERE status = 'completed';
-- ... （达到阈值后自动执行 CREATE INDEX idx_orders_status_auto ON orders (status)）

-- 通过 API 调整阈值（默认：5 次）
-- vm.adaptive_threshold = 10;
```

### VACUUM

```sql
-- 合并 pending-free 页到活动 freelist 并刷盘（释放逻辑空间）
VACUUM;
```

---

## 综合示例：电商分析查询

```sql
-- 找出每个国家消费额前 3 名的客户，包括其最近的订单状态
WITH ranked AS (
    SELECT
        c.country, c.name, c.id AS cust_id,
        SUM(o.amount) AS total_spent,
        MAX(o.created_at) AS last_order_date,
        (SELECT status FROM orders WHERE cust_id = c.id ORDER BY created_at DESC LIMIT 1) AS last_status,
        ROW_NUMBER() OVER (PARTITION BY c.country ORDER BY SUM(o.amount) DESC) AS rank
    FROM customers c
    JOIN orders o ON c.id = o.cust_id AND o.status != 'cancelled'
    GROUP BY c.country, c.id, c.name
)
SELECT country, rank, name, total_spent, last_order_date, last_status
FROM ranked
WHERE rank <= 3
ORDER BY country, rank;
```
