# KKDB 完全使用手册

KKDB 是一个使用 Rust 实现的轻量级、功能完备的关系型数据库引擎。本文档覆盖 KKDB 引擎的**全部功能**，包括存储引擎、SQL 语法、事务控制、安全权限、全文检索、向量搜索、分布式集群、HTTP REST API、MySQL 协议兼容等。

---

## 目录

1. [快速入门](#1-快速入门)
2. [数据类型](#2-数据类型)
3. [DDL — 数据定义语言](#3-ddl--数据定义语言)
4. [DML — 数据操作语言](#4-dml--数据操作语言)
5. [SELECT 查询](#5-select-查询)
6. [表达式与运算符](#6-表达式与运算符)
7. [内置函数](#7-内置函数)
8. [窗口函数](#8-窗口函数)
9. [集合操作](#9-集合操作)
10. [通用表表达式（CTE）](#10-通用表表达式cte)
11. [子查询](#11-子查询)
12. [事务与并发控制](#12-事务与并发控制)
13. [视图与触发器](#13-视图与触发器)
14. [索引](#14-索引)
15. [全文检索（BM25）](#15-全文检索bm25)
16. [向量搜索（HNSW）](#16-向量搜索hnsw)
17. [用户与权限管理（RBAC）](#17-用户与权限管理rbac)
18. [行级安全策略（RLS）](#18-行级安全策略rls)
19. [预编译语句](#19-预编译语句)
20. [JSON 函数](#20-json-函数)
21. [数据导入导出](#21-数据导入导出)
22. [EXPLAIN 查询计划](#22-explain-查询计划)
23. [系统命令](#23-系统命令)
24. [存储引擎详解](#24-存储引擎详解)
25. [MySQL 协议服务器](#25-mysql-协议服务器)
26. [HTTP REST API](#26-http-rest-api)
27. [分布式集群（Raft）](#27-分布式集群raft)
28. [Binlog 与复制](#28-binlog-与复制)
29. [Rust API 编程接口](#29-rust-api-编程接口)
30. [命令行工具](#30-命令行工具)
31. [模块架构全景](#31-模块架构全景)

---

## 1. 快速入门

### 1.1 编译与运行

```bash
# 编译
cargo build --release

# 以内存模式启动交互式 REPL
cargo run --release

# 以文件模式启动（持久化）
cargo run --release -- my_database.kkdb

# 启动 MySQL 协议服务器（端口 3307）
cargo run --release -- --mysql-port 3307

# 启动 HTTP REST API 服务器（端口 8080）
cargo run --release -- --http-port 8080
```

### 1.2 交互式 REPL 命令

| 命令 | 功能 |
|------|------|
| `.help` | 显示帮助信息 |
| `.quit` / `.exit` | 退出 REPL |
| `.tables` | 列出所有表 |
| `.schema [table]` | 显示表的 CREATE 语句 |
| `.open <path>` | 打开数据库文件 |
| `.memory` | 切换到内存模式 |
| `SHOW TABLES;` | 列出所有表 |
| `SHOW ENGINE STATUS;` | 显示存储引擎状态 |

### 1.3 最小示例

```sql
-- 创建表
CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    email TEXT UNIQUE,
    age INTEGER CHECK(age >= 0)
);

-- 插入数据
INSERT INTO users (name, email, age) VALUES ('Alice', 'alice@example.com', 30);
INSERT INTO users (name, email, age) VALUES ('Bob', 'bob@example.com', 25);

-- 查询
SELECT * FROM users WHERE age > 20 ORDER BY name;

-- 更新
UPDATE users SET age = 31 WHERE name = 'Alice';

-- 删除
DELETE FROM users WHERE name = 'Bob';
```

---

## 2. 数据类型

KKDB 支持以下基础数据类型（`DataType` 枚举）：

| 类型 | 说明 | 运行时值 |
|------|------|----------|
| `NULL` | 空值 | `Value::Null` |
| `INTEGER` | 64 位有符号整数 | `Value::Integer(i64)` |
| `REAL` | 64 位浮点数 | `Value::Real(f64)` |
| `TEXT` | UTF-8 文本字符串 | `Value::Text(Cow<str>)` |
| `BLOB` | 原始二进制数据 | `Value::Blob(Vec<u8>)` |
| `TIMESTAMP` | 时间戳（存储为 TEXT） | `Value::Text(...)` |

### 2.1 类型转换（CAST）

```sql
SELECT CAST(42 AS TEXT);                -- '42'
SELECT CAST('3.14' AS REAL);            -- 3.14
SELECT CAST('2024-01-01' AS TIMESTAMP); -- '2024-01-01'
SELECT TRY_CAST('abc' AS INTEGER);      -- NULL（不抛错）
```

支持的目标类型：`INTEGER`、`REAL`、`TEXT`、`BLOB`、`NUMERIC`、`DATE`、`TIME`、`TIMESTAMP`、`JSON`。

---

## 3. DDL — 数据定义语言

### 3.1 CREATE TABLE

```sql
-- 基础建表
CREATE TABLE products (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    price REAL DEFAULT 0.0,
    category TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- IF NOT EXISTS
CREATE TABLE IF NOT EXISTS logs (
    id INTEGER PRIMARY KEY,
    message TEXT
);

-- 列级约束：外键
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE ON UPDATE CASCADE,
    product_id INTEGER REFERENCES products(id) ON DELETE SET NULL,
    quantity INTEGER CHECK(quantity > 0)
);

-- 表级 CHECK 约束
CREATE TABLE inventory (
    id INTEGER PRIMARY KEY,
    min_qty INTEGER,
    max_qty INTEGER,
    CHECK(min_qty <= max_qty)
);

-- CREATE TABLE AS SELECT
CREATE TABLE active_users AS
SELECT * FROM users WHERE age >= 18;
```

#### 列约束一览

| 约束 | 说明 |
|------|------|
| `PRIMARY KEY` | 主键 |
| `AUTOINCREMENT` | 自增（仅 INTEGER PRIMARY KEY） |
| `NOT NULL` | 非空 |
| `UNIQUE` | 唯一 |
| `DEFAULT expr` | 默认值 |
| `CHECK(expr)` | 检查约束 |
| `REFERENCES table(col)` | 外键引用 |
| `ON DELETE CASCADE\|SET NULL\|RESTRICT` | 外键删除动作 |
| `ON UPDATE CASCADE\|SET NULL\|RESTRICT` | 外键更新动作 |

### 3.2 ALTER TABLE

```sql
-- 添加列
ALTER TABLE users ADD COLUMN phone TEXT;

-- 删除列
ALTER TABLE users DROP COLUMN phone;

-- 重命名表
ALTER TABLE users RENAME TO members;

-- 重命名列
ALTER TABLE users RENAME COLUMN email TO email_address;

-- 启用行级安全
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
```

### 3.3 DROP TABLE

```sql
DROP TABLE users;
DROP TABLE IF EXISTS temp_data;
```

### 3.4 VACUUM

```sql
-- 回收未使用的数据库页面空间
VACUUM;
```

### 3.5 ANALYZE

```sql
-- 收集表的统计信息，供查询优化器使用
ANALYZE TABLE users;
```

---

## 4. DML — 数据操作语言

### 4.1 INSERT

```sql
-- 基础插入
INSERT INTO users (name, email) VALUES ('Charlie', 'charlie@test.com');

-- 多行插入
INSERT INTO users (name, email) VALUES
    ('Dave', 'dave@test.com'),
    ('Eve', 'eve@test.com');

-- INSERT OR REPLACE（冲突时替换）
INSERT OR REPLACE INTO users (id, name, email) VALUES (1, 'Alice Updated', 'alice@new.com');

-- INSERT OR IGNORE（冲突时忽略）
INSERT OR IGNORE INTO users (name, email) VALUES ('Alice', 'alice@example.com');

-- ON CONFLICT DO UPDATE（UPSERT）
INSERT INTO users (id, name, email)
VALUES (1, 'Alice', 'alice@updated.com')
ON CONFLICT DO UPDATE SET email = 'alice@updated.com';

-- INSERT ... SELECT
INSERT INTO archive_users SELECT * FROM users WHERE age > 60;

-- RETURNING 子句
INSERT INTO users (name, email) VALUES ('Frank', 'frank@test.com')
RETURNING id, name;
```

### 4.2 UPDATE

```sql
-- 基础更新
UPDATE users SET age = 31 WHERE name = 'Alice';

-- 多列更新
UPDATE users SET name = 'Alice B', age = 32 WHERE id = 1;

-- RETURNING 子句
UPDATE users SET age = age + 1 WHERE age < 30 RETURNING id, name, age;
```

### 4.3 DELETE

```sql
-- 条件删除
DELETE FROM users WHERE age < 18;

-- RETURNING 子句
DELETE FROM users WHERE name = 'Bob' RETURNING *;
```

---

## 5. SELECT 查询

### 5.1 基础查询

```sql
-- 选择所有列
SELECT * FROM users;

-- 选择特定列
SELECT name, email FROM users;

-- 别名
SELECT name AS user_name, age * 2 AS double_age FROM users;

-- DISTINCT
SELECT DISTINCT category FROM products;

-- WHERE 过滤
SELECT * FROM users WHERE age > 25 AND name LIKE 'A%';

-- ORDER BY（支持 NULLS FIRST / NULLS LAST）
SELECT * FROM users ORDER BY age DESC NULLS LAST, name ASC;

-- LIMIT / OFFSET
SELECT * FROM users ORDER BY id LIMIT 10 OFFSET 20;
```

### 5.2 JOIN

```sql
-- INNER JOIN
SELECT u.name, o.quantity
FROM users u
INNER JOIN orders o ON u.id = o.user_id;

-- LEFT JOIN
SELECT u.name, o.quantity
FROM users u
LEFT JOIN orders o ON u.id = o.user_id;

-- RIGHT JOIN
SELECT u.name, o.quantity
FROM users u
RIGHT JOIN orders o ON u.id = o.user_id;

-- FULL OUTER JOIN
SELECT u.name, o.quantity
FROM users u
FULL JOIN orders o ON u.id = o.user_id;

-- CROSS JOIN
SELECT * FROM colors CROSS JOIN sizes;

-- NATURAL JOIN
SELECT * FROM users NATURAL JOIN profiles;

-- 多表 JOIN
SELECT u.name, o.id, p.name AS product
FROM users u
JOIN orders o ON u.id = o.user_id
JOIN products p ON o.product_id = p.id;
```

JOIN 算法选择（自动由优化器决定）：
- **Nested Loop Join**：小表 or 索引高选择率
- **Hash Join**：等值连接，构建端较小
- **Sort-Merge Join**：两端已排序或可廉价排序

### 5.3 GROUP BY / HAVING

```sql
-- 分组统计
SELECT category, COUNT(*) as cnt, AVG(price) as avg_price
FROM products
GROUP BY category
HAVING COUNT(*) > 5;
```

### 5.4 表值函数

```sql
-- UNNEST：展开数组
SELECT * FROM UNNEST(1, 2, 3) AS t(val);

-- generate_series：生成序列
SELECT * FROM generate_series(1, 10);
SELECT * FROM generate_series(1, 100, 5);  -- 步长 = 5
```

### 5.5 SELECT FOR UPDATE

```sql
-- 行级排他锁
SELECT * FROM accounts WHERE id = 1 FOR UPDATE;
```

---

## 6. 表达式与运算符

### 6.1 算术运算符

| 运算符 | 说明 | 示例 |
|--------|------|------|
| `+` | 加法 | `SELECT 1 + 2` |
| `-` | 减法 | `SELECT 10 - 3` |
| `*` | 乘法 | `SELECT 4 * 5` |
| `/` | 除法 | `SELECT 10 / 3` |
| `%` | 取模 | `SELECT 10 % 3` |

### 6.2 比较运算符

| 运算符 | 说明 |
|--------|------|
| `=` | 相等 |
| `!=` / `<>` | 不等 |
| `<` | 小于 |
| `<=` | 小于等于 |
| `>` | 大于 |
| `>=` | 大于等于 |

### 6.3 逻辑运算符

| 运算符 | 说明 |
|--------|------|
| `AND` | 逻辑与 |
| `OR` | 逻辑或 |
| `NOT` | 逻辑非 |
| `XOR` | 逻辑异或 |

### 6.4 位运算符

| 运算符 | 说明 |
|--------|------|
| `\|` | 按位或 |
| `&` | 按位与 |
| `^` | 按位异或 |
| `<<` | 左移 |
| `>>` | 右移 |

### 6.5 特殊运算符

```sql
-- IS NULL / IS NOT NULL
SELECT * FROM users WHERE email IS NOT NULL;

-- IN
SELECT * FROM users WHERE age IN (25, 30, 35);

-- BETWEEN
SELECT * FROM users WHERE age BETWEEN 20 AND 30;

-- LIKE（支持 % 和 _）
SELECT * FROM users WHERE name LIKE 'A%';

-- ILIKE（不区分大小写）
SELECT * FROM users WHERE name ILIKE 'alice';

-- LIKE ... ESCAPE
SELECT * FROM logs WHERE msg LIKE '%100\%%' ESCAPE '\';

-- || 字符串连接
SELECT first_name || ' ' || last_name AS full_name FROM users;

-- CASE WHEN
SELECT name,
    CASE
        WHEN age < 18 THEN '未成年'
        WHEN age < 60 THEN '成年'
        ELSE '老年'
    END AS age_group
FROM users;

-- Simple CASE
SELECT name,
    CASE status
        WHEN 1 THEN '活跃'
        WHEN 0 THEN '禁用'
        ELSE '未知'
    END
FROM users;

-- EXISTS
SELECT * FROM users u
WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id);

-- ANY / ALL
SELECT * FROM products WHERE price > ANY (SELECT price FROM products WHERE category = 'cheap');
SELECT * FROM products WHERE price > ALL (SELECT price FROM products WHERE category = 'cheap');
```

---

## 7. 内置函数

### 7.1 聚合函数

| 函数 | 说明 | 示例 |
|------|------|------|
| `COUNT(*)` | 行数统计 | `SELECT COUNT(*) FROM users` |
| `COUNT(col)` | 非空值计数 | `SELECT COUNT(email) FROM users` |
| `COUNT(DISTINCT col)` | 去重计数 | `SELECT COUNT(DISTINCT category) FROM products` |
| `SUM(col)` | 求和 | `SELECT SUM(price) FROM products` |
| `AVG(col)` | 平均值 | `SELECT AVG(age) FROM users` |
| `MIN(col)` | 最小值 | `SELECT MIN(price) FROM products` |
| `MAX(col)` | 最大值 | `SELECT MAX(price) FROM products` |
| `GROUP_CONCAT(col)` | 拼接字符串 | `SELECT GROUP_CONCAT(name) FROM users` |
| `GROUP_CONCAT(col, sep)` | 带分隔符拼接 | `SELECT GROUP_CONCAT(name, '; ') FROM users` |

### 7.2 字符串函数

| 函数 | 说明 | 示例 |
|------|------|------|
| `LENGTH(s)` | 字符串长度 | `SELECT LENGTH('hello')` → 5 |
| `UPPER(s)` | 转大写 | `SELECT UPPER('hello')` → 'HELLO' |
| `LOWER(s)` | 转小写 | `SELECT LOWER('HELLO')` → 'hello' |
| `TRIM(s)` | 去除两端空白 | `SELECT TRIM('  hi  ')` → 'hi' |
| `LTRIM(s)` | 去除左侧空白 | `SELECT LTRIM('  hi')` → 'hi' |
| `RTRIM(s)` | 去除右侧空白 | `SELECT RTRIM('hi  ')` → 'hi' |
| `SUBSTR(s, start, len)` | 子串 | `SELECT SUBSTR('hello', 2, 3)` → 'ell' |
| `SUBSTRING(s, start, len)` | 同 SUBSTR | `SELECT SUBSTRING('hello', 1, 3)` → 'hel' |
| `REPLACE(s, from, to)` | 替换 | `SELECT REPLACE('hello', 'l', 'r')` → 'herro' |
| `INSTR(s, sub)` | 查找位置 | `SELECT INSTR('hello', 'll')` → 3 |
| `REVERSE(s)` | 反转 | `SELECT REVERSE('hello')` → 'olleh' |
| `LPAD(s, len, pad)` | 左填充 | `SELECT LPAD('hi', 5, '*')` → '***hi' |
| `RPAD(s, len, pad)` | 右填充 | `SELECT RPAD('hi', 5, '*')` → 'hi***' |
| `LEFT(s, n)` | 左截取 | `SELECT LEFT('hello', 3)` → 'hel' |
| `RIGHT(s, n)` | 右截取 | `SELECT RIGHT('hello', 3)` → 'llo' |
| `REPEAT(s, n)` | 重复 | `SELECT REPEAT('ab', 3)` → 'ababab' |
| `CONCAT(s1, s2, ...)` | 连接 | `SELECT CONCAT('a', 'b', 'c')` → 'abc' |
| `CONCAT_WS(sep, s1, ...)` | 带分隔符连接 | `SELECT CONCAT_WS('-', 'a', 'b')` → 'a-b' |
| `FORMAT(n, decimals)` | 格式化数字 | `SELECT FORMAT(1234.5, 2)` → '1234.50' |
| `HEX(n)` | 转十六进制 | `SELECT HEX(255)` → 'FF' |

### 7.3 数学函数

| 函数 | 说明 | 示例 |
|------|------|------|
| `ABS(x)` | 绝对值 | `SELECT ABS(-5)` → 5 |
| `ROUND(x [, n])` | 四舍五入 | `SELECT ROUND(3.14159, 2)` → 3.14 |
| `CEIL(x)` / `CEILING(x)` | 上取整 | `SELECT CEIL(3.2)` → 4 |
| `FLOOR(x)` | 下取整 | `SELECT FLOOR(3.8)` → 3 |
| `SQRT(x)` | 平方根 | `SELECT SQRT(16)` → 4.0 |
| `POWER(x, y)` / `POW(x, y)` | 幂运算 | `SELECT POWER(2, 10)` → 1024 |
| `LOG(x)` / `LN(x)` | 自然对数 | `SELECT LN(2.718)` → ~1.0 |
| `LOG2(x)` | 以 2 为底对数 | `SELECT LOG2(8)` → 3.0 |
| `LOG10(x)` | 以 10 为底对数 | `SELECT LOG10(100)` → 2.0 |
| `EXP(x)` | e 的 x 次方 | `SELECT EXP(1)` → ~2.718 |
| `MOD(x, y)` | 取模 | `SELECT MOD(10, 3)` → 1 |
| `SIGN(x)` | 符号函数 | `SELECT SIGN(-5)` → -1 |
| `PI()` | π 常量 | `SELECT PI()` → 3.14159... |
| `RANDOM()` | 随机整数 | `SELECT RANDOM()` |
| `SIN(x)` | 正弦 | `SELECT SIN(0)` → 0 |
| `COS(x)` | 余弦 | `SELECT COS(0)` → 1 |
| `TAN(x)` | 正切 | `SELECT TAN(0)` → 0 |
| `ASIN(x)` | 反正弦 | `SELECT ASIN(1)` → ~1.57 |
| `ACOS(x)` | 反余弦 | `SELECT ACOS(1)` → 0 |
| `ATAN(x)` | 反正切 | `SELECT ATAN(1)` → ~0.785 |
| `ATAN2(y, x)` | 二参反正切 | `SELECT ATAN2(1, 1)` → ~0.785 |
| `RADIANS(deg)` | 度转弧度 | `SELECT RADIANS(180)` → ~3.14 |
| `DEGREES(rad)` | 弧度转度 | `SELECT DEGREES(PI())` → 180 |
| `TRUNCATE(x, n)` | 截断小数 | `SELECT TRUNCATE(3.14159, 2)` → 3.14 |
| `GREATEST(a, b, ...)` | 最大值 | `SELECT GREATEST(1, 5, 3)` → 5 |
| `LEAST(a, b, ...)` | 最小值 | `SELECT LEAST(1, 5, 3)` → 1 |

### 7.4 日期时间函数

| 函数 | 说明 | 示例 |
|------|------|------|
| `DATE('now')` | 当前日期 | `SELECT DATE('now')` |
| `TIME('now')` | 当前时间 | `SELECT TIME('now')` |
| `DATETIME('now')` | 当前日期时间 | `SELECT DATETIME('now')` |
| `STRFTIME(fmt, dt)` | 格式化日期 | `SELECT STRFTIME('%Y-%m-%d', '2024-01-01')` |
| `JULIANDAY(dt)` | 儒略日 | `SELECT JULIANDAY('2024-01-01')` |
| `NOW()` | 当前时间戳 | `SELECT NOW()` |
| `CURRENT_TIMESTAMP` | 当前时间戳 | `SELECT CURRENT_TIMESTAMP` |
| `CURRENT_DATE` | 当前日期 | `SELECT CURRENT_DATE` |
| `CURRENT_TIME` | 当前时间 | `SELECT CURRENT_TIME` |
| `EXTRACT(field FROM expr)` | 提取日期部分 | `SELECT EXTRACT(YEAR FROM '2024-01-01')` |
| `DATE_ADD(dt, INTERVAL n unit)` | 日期加法 | `SELECT DATE_ADD('2024-01-01', INTERVAL 1 DAY)` |
| `DATE_SUB(dt, INTERVAL n unit)` | 日期减法 | `SELECT DATE_SUB('2024-01-01', INTERVAL 1 MONTH)` |
| `DATEDIFF(d1, d2)` | 日期差（天数） | `SELECT DATEDIFF('2024-12-31', '2024-01-01')` |
| `DATE_TRUNC(field, dt)` | 日期截断 | `SELECT DATE_TRUNC('month', '2024-06-15')` |
| `YEAR(dt)` | 提取年份 | `SELECT YEAR('2024-06-15')` → 2024 |
| `MONTH(dt)` | 提取月份 | `SELECT MONTH('2024-06-15')` → 6 |
| `DAY(dt)` | 提取日期 | `SELECT DAY('2024-06-15')` → 15 |
| `HOUR(dt)` | 提取小时 | `SELECT HOUR('10:30:00')` → 10 |
| `MINUTE(dt)` | 提取分钟 | `SELECT MINUTE('10:30:00')` → 30 |
| `SECOND(dt)` | 提取秒 | `SELECT SECOND('10:30:45')` → 45 |

### 7.5 其它函数

| 函数 | 说明 | 示例 |
|------|------|------|
| `TYPEOF(x)` | 返回类型名 | `SELECT TYPEOF(42)` → 'integer' |
| `COALESCE(a, b, ...)` | 第一个非空值 | `SELECT COALESCE(NULL, 42)` → 42 |
| `NULLIF(a, b)` | 相等则返回 NULL | `SELECT NULLIF(1, 1)` → NULL |
| `IIF(cond, t, f)` | 条件表达式 | `SELECT IIF(1>0, 'yes', 'no')` → 'yes' |
| `IF(cond, t, f)` | 条件表达式 | `SELECT IF(age>18, '成年', '未成年') FROM users` |
| `IFNULL(a, b)` | 空值替换 | `SELECT IFNULL(email, '无') FROM users` |
| `GLOB(pattern, s)` | 通配符匹配 | `SELECT GLOB('*.txt', 'file.txt')` |
| `UNICODE(s)` | 首字符 Unicode 码点 | `SELECT UNICODE('A')` → 65 |
| `CHAR(n1, n2, ...)` | Unicode 码点→字符 | `SELECT CHAR(72, 105)` → 'Hi' |
| `ZEROBLOB(n)` | n 字节零 BLOB | `SELECT ZEROBLOB(4)` |
| `LAST_INSERT_ROWID()` | 最后插入的 rowid | `SELECT LAST_INSERT_ROWID()` |
| `CHANGES()` | 上一条语句影响行数 | `SELECT CHANGES()` |
| `TOTAL_CHANGES()` | 连接总影响行数 | `SELECT TOTAL_CHANGES()` |
| `QUOTE(x)` | SQL 引用转义 | `SELECT QUOTE('it''s')` |
| `SOUNDEX(s)` | Soundex 编码 | `SELECT SOUNDEX('Robert')` |

---

## 8. 窗口函数

### 8.1 排名函数

```sql
SELECT name, department, salary,
    ROW_NUMBER() OVER (ORDER BY salary DESC) AS row_num,
    RANK()       OVER (ORDER BY salary DESC) AS rank,
    DENSE_RANK() OVER (ORDER BY salary DESC) AS dense_rank,
    PERCENT_RANK() OVER (ORDER BY salary DESC) AS pct_rank,
    CUME_DIST()  OVER (ORDER BY salary DESC) AS cume_dist,
    NTILE(4)     OVER (ORDER BY salary DESC) AS quartile
FROM employees;
```

### 8.2 偏移函数

```sql
SELECT name, salary,
    LAG(salary, 1, 0)  OVER (ORDER BY hire_date) AS prev_salary,
    LEAD(salary, 1, 0) OVER (ORDER BY hire_date) AS next_salary,
    FIRST_VALUE(salary) OVER (PARTITION BY dept ORDER BY hire_date) AS first_sal,
    LAST_VALUE(salary)  OVER (PARTITION BY dept ORDER BY hire_date
        ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS last_sal,
    NTH_VALUE(salary, 3) OVER (ORDER BY salary DESC) AS third_highest
FROM employees;
```

### 8.3 窗口聚合

```sql
SELECT name, department, salary,
    SUM(salary) OVER (PARTITION BY department) AS dept_total,
    AVG(salary) OVER (PARTITION BY department ORDER BY hire_date
        ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) AS moving_avg,
    COUNT(*)    OVER (PARTITION BY department) AS dept_count
FROM employees;
```

### 8.4 窗口帧规范

支持：`ROWS`、`RANGE`、`GROUPS` 三种模式。

```sql
-- ROWS 帧
ROWS BETWEEN 3 PRECEDING AND CURRENT ROW
ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
ROWS BETWEEN CURRENT ROW AND 2 FOLLOWING
ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING

-- RANGE 帧
RANGE BETWEEN 100 PRECEDING AND 100 FOLLOWING

-- GROUPS 帧
GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING
```

### 8.5 命名窗口定义

```sql
SELECT name,
    SUM(salary) OVER w AS running_total,
    AVG(salary) OVER w AS running_avg
FROM employees
WINDOW w AS (PARTITION BY department ORDER BY hire_date);
```

---

## 9. 集合操作

```sql
-- UNION ALL（保留重复）
SELECT name FROM employees UNION ALL SELECT name FROM contractors;

-- UNION DISTINCT（去重）
SELECT name FROM employees UNION DISTINCT SELECT name FROM contractors;

-- INTERSECT ALL / DISTINCT
SELECT name FROM table_a INTERSECT ALL SELECT name FROM table_b;
SELECT name FROM table_a INTERSECT DISTINCT SELECT name FROM table_b;

-- EXCEPT ALL / DISTINCT
SELECT name FROM table_a EXCEPT ALL SELECT name FROM table_b;
SELECT name FROM table_a EXCEPT DISTINCT SELECT name FROM table_b;

-- 集合操作上的 ORDER BY / LIMIT
(SELECT name FROM employees UNION SELECT name FROM contractors)
ORDER BY name LIMIT 10;
```

---

## 10. 通用表表达式（CTE）

### 10.1 非递归 CTE

```sql
WITH high_earners AS (
    SELECT name, salary FROM employees WHERE salary > 100000
)
SELECT * FROM high_earners ORDER BY salary DESC;
```

### 10.2 多 CTE

```sql
WITH
    dept_stats AS (
        SELECT department, AVG(salary) AS avg_sal FROM employees GROUP BY department
    ),
    top_depts AS (
        SELECT department FROM dept_stats WHERE avg_sal > 80000
    )
SELECT e.name, e.salary
FROM employees e
JOIN top_depts td ON e.department = td.department;
```

### 10.3 递归 CTE

```sql
-- 斐波那契数列
WITH RECURSIVE fib(n, a, b) AS (
    VALUES(0, 0, 1)
    UNION ALL
    SELECT n + 1, b, a + b FROM fib WHERE n < 20
)
SELECT n, a AS fib_value FROM fib;

-- 层次结构遍历
WITH RECURSIVE org_tree AS (
    SELECT id, name, manager_id, 0 AS depth
    FROM employees
    WHERE manager_id IS NULL
    UNION ALL
    SELECT e.id, e.name, e.manager_id, ot.depth + 1
    FROM employees e
    JOIN org_tree ot ON e.manager_id = ot.id
)
SELECT * FROM org_tree ORDER BY depth, name;
```

---

## 11. 子查询

```sql
-- 标量子查询
SELECT name, (SELECT COUNT(*) FROM orders o WHERE o.user_id = u.id) AS order_count
FROM users u;

-- IN 子查询
SELECT * FROM users WHERE id IN (SELECT user_id FROM orders WHERE total > 100);

-- NOT IN 子查询
SELECT * FROM users WHERE id NOT IN (SELECT user_id FROM blacklist);

-- EXISTS 子查询
SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id);

-- ANY / ALL
SELECT * FROM products WHERE price > ANY (SELECT price FROM cheap_products);
SELECT * FROM products WHERE price > ALL (SELECT price FROM cheap_products);

-- FROM 子查询
SELECT avg_price FROM (
    SELECT category, AVG(price) AS avg_price FROM products GROUP BY category
) AS sub
WHERE avg_price > 50;
```

---

## 12. 事务与并发控制

### 12.1 基础事务

```sql
BEGIN;
INSERT INTO accounts (name, balance) VALUES ('Alice', 1000);
UPDATE accounts SET balance = balance - 100 WHERE name = 'Alice';
COMMIT;

-- 回滚
BEGIN;
DELETE FROM users;
ROLLBACK;
```

### 12.2 保存点（Savepoint）

```sql
BEGIN;
INSERT INTO users (name) VALUES ('Alice');
SAVEPOINT sp1;
INSERT INTO users (name) VALUES ('Bob');
ROLLBACK TO SAVEPOINT sp1;  -- 仅撤销 Bob 的插入
RELEASE SAVEPOINT sp1;
COMMIT;
```

### 12.3 MVCC（多版本并发控制）

KKDB 实现了 PostgreSQL 风格的 MVCC：

- **快照隔离（Snapshot Isolation）**：每个事务看到一致性快照
- **版本链**：通过 Undo Log 维护历史版本
- **活跃事务注册表**：追踪所有进行中的事务
- **GC 清理**：自动按水位线清理过期版本

### 12.4 锁管理

- **表级锁**：Shared（共享）/ Exclusive（排他）模式
- **死锁检测**：基于等待图（Wait-for Graph）的环路检测
- **行级锁定**：通过 `SELECT ... FOR UPDATE` 实现

---

## 13. 视图与触发器

### 13.1 视图

```sql
-- 创建视图
CREATE VIEW active_users AS
SELECT id, name, email FROM users WHERE status = 'active';

-- OR REPLACE
CREATE OR REPLACE VIEW active_users AS
SELECT id, name, email, age FROM users WHERE status = 'active';

-- IF NOT EXISTS
CREATE VIEW IF NOT EXISTS v_stats AS
SELECT COUNT(*) AS total FROM users;

-- 查询视图（与表相同）
SELECT * FROM active_users WHERE age > 25;
```

### 13.2 触发器

```sql
-- 创建触发器
CREATE TRIGGER log_insert
AFTER INSERT ON users
FOR EACH ROW
INSERT INTO audit_log (action, table_name, timestamp)
VALUES ('INSERT', 'users', DATETIME('now'));

-- OR REPLACE
CREATE OR REPLACE TRIGGER log_update
BEFORE UPDATE ON users
FOR EACH ROW
INSERT INTO audit_log (action) VALUES ('UPDATE');

-- 删除触发器
DROP TRIGGER log_insert;
DROP TRIGGER IF EXISTS log_update;
```

触发器时机：`BEFORE` / `AFTER`  
触发事件：`INSERT` / `UPDATE` / `DELETE`

---

## 14. 索引

### 14.1 B-Tree 索引

```sql
-- 创建普通索引
CREATE INDEX idx_users_name ON users(name);

-- 创建唯一索引
CREATE UNIQUE INDEX idx_users_email ON users(email);

-- 复合索引
CREATE INDEX idx_orders_user_date ON orders(user_id, created_at);

-- IF NOT EXISTS
CREATE INDEX IF NOT EXISTS idx_price ON products(price);

-- 删除索引
DROP INDEX idx_users_name;
DROP INDEX IF EXISTS idx_old;
```

### 14.2 索引特性

- **前缀压缩**：B-Tree 叶节点上的排序键自动前缀压缩，节省空间
- **Bloom 过滤器**：基于 FNV-1a 的 Bloom Filter 加速 Point Lookup
- **自适应优化**：查询优化器根据统计信息自动选择索引扫描 vs 全表扫描

---

## 15. 全文检索（BM25）

### 15.1 创建全文索引

```sql
-- 在指定列上创建全文索引
CREATE FULLTEXT INDEX idx_ft_articles ON articles(title, body);

-- IF NOT EXISTS
CREATE FULLTEXT INDEX IF NOT EXISTS idx_ft ON posts(content);
```

### 15.2 全文搜索查询

```sql
-- MATCH ... AGAINST 语法
SELECT id, title, MATCH(title, body) AGAINST ('数据库 Rust') AS score
FROM articles
WHERE MATCH(title, body) AGAINST ('数据库 Rust')
ORDER BY score DESC;

-- 在 WHERE 中使用
SELECT * FROM articles
WHERE MATCH(title, body) AGAINST ('关键词')
LIMIT 10;
```

### 15.3 BM25 评分

全文检索使用 BM25 算法评分：

$$\text{BM25}(q, d) = \sum_{t \in q} \text{IDF}(t) \cdot \frac{tf(t, d) \cdot (k_1 + 1)}{tf(t, d) + k_1 \cdot (1 - b + b \cdot \frac{|d|}{avgdl})}$$

- **倒排索引**：存储于 B-Tree，键格式为 `\x00FTS\x01{IndexID}\x02{Token}\x03{RowID}`
- **分词器**：Latin/ASCII 采用空白分割 + 小写；CJK（中日韩）通过 `jieba-rs` 分词
- **停用词**：内置英文和中文停用词表

### 15.4 高级全文特性

- **模糊搜索**：基于编辑距离的近似匹配（`FuzzySearcher`）
- **同义词扩展**：查询时自动扩展同义词（`SynonymExpander`）
- **分面搜索**：按分类维度聚合搜索结果（`FacetSearcher`）
- **实时索引更新**：INSERT/UPDATE/DELETE 自动维护全文索引

---

## 16. 向量搜索（HNSW）

### 16.1 创建向量索引

```sql
-- 创建 HNSW 向量索引
CREATE VECTOR INDEX idx_embeddings ON documents(embedding) DIM 1536 DISTANCE COSINE;

-- L2 距离
CREATE VECTOR INDEX idx_vec ON items(vec_col) DIM 768 DISTANCE L2;

-- IF NOT EXISTS
CREATE VECTOR INDEX IF NOT EXISTS idx_v ON t(col) DIM 128 DISTANCE COSINE;

-- 删除向量索引
DROP VECTOR INDEX idx_embeddings;
DROP VECTOR INDEX IF EXISTS idx_old;
```

### 16.2 向量查询

向量数据以 BLOB 格式存储（f32 数组的二进制编码），通过 `VectorIndexRegistry` 管理多个索引。

### 16.3 HNSW 算法参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `M` | 16 | 每节点最大邻居数（layer > 0） |
| `M_MAX0` | 32 | 第 0 层最大邻居数（= 2×M） |
| `ef_construction` | 200 | 构建时候选集大小 |
| `ef_search` | 50 | 搜索时候选集大小 |
| `REBUILD_THRESHOLD` | 0.2 | 删除率达到 20% 时自动重建图 |

### 16.4 距离度量

| 度量 | SQL 关键字 | 返回值范围 |
|------|------------|------------|
| Cosine 相似度 | `COSINE` / `COS` | [-1, 1]，越大越相似 |
| L2 欧氏距离 | `L2` / `EUCLIDEAN` | [0, +∞)，越小越相似 |

### 16.5 高级向量特性

- **多向量索引管理**：`MultiVectorIndex` 支持不同维度和度量的多索引
- **量化压缩**：减少内存占用
- **批量导入优化**

---

## 17. 用户与权限管理（RBAC）

### 17.1 用户管理

```sql
-- 创建用户
CREATE USER alice WITH PASSWORD 'secret123';

-- 修改密码
ALTER USER alice WITH PASSWORD 'new_password';

-- 删除用户
DROP USER alice;
DROP USER IF EXISTS bob;
```

### 17.2 权限管理

```sql
-- 授权
GRANT SELECT, INSERT ON users TO alice;
GRANT ALL ON products TO bob;
GRANT SELECT ON DATABASE mydb TO readonly_role;

-- 撤销权限
REVOKE INSERT ON users FROM alice;
REVOKE ALL ON products FROM bob;
```

### 17.3 权限类型

| 权限 | 说明 |
|------|------|
| `SELECT` | 查询权限 |
| `INSERT` | 插入权限 |
| `UPDATE` | 更新权限 |
| `DELETE` | 删除权限 |
| `CREATE` | 建表/建索引权限 |
| `DROP` | 删表/删索引权限 |
| `ALTER` | 修改表结构权限 |
| `GRANT` | 授权权限 |
| `EXECUTE` | 执行存储过程权限 |
| `ALL` | 所有权限 |

### 17.4 审计日志

RBAC 模块内置审计系统，记录每条 SQL 操作的：
- 序列号、时间戳
- 执行用户
- SQL 语句
- 成功/失败状态
- 影响行数

### 17.5 高级安全特性

- **列级加密**：支持 AES-128/AES-256/ChaCha20 算法
- **动态数据脱敏**：对敏感列自动脱敏
- **细粒度列级/行级权限**：精确控制每个用户可访问的列和行
- **密码策略**：可配置密码复杂度、过期时间等
- **TLS 配置**：加密传输层

---

## 18. 行级安全策略（RLS）

```sql
-- 启用 RLS
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;

-- 创建策略
CREATE POLICY user_docs ON documents
    FOR ALL
    TO user_role
    USING (owner_id = CURRENT_USER)
    WITH CHECK (owner_id = CURRENT_USER);

-- 删除策略
DROP POLICY user_docs ON documents;
DROP POLICY IF EXISTS old_policy ON documents;
```

### 18.1 策略组成

- **USING 子句**：应用于 SELECT / UPDATE / DELETE — 过滤可见行
- **WITH CHECK 子句**：应用于 INSERT / UPDATE — 验证新/修改行

### 18.2 会话变量

```sql
-- 设置当前用户（用于 RLS 策略中的引用）
SET kkdb.current_user = 'alice';
```

---

## 19. 预编译语句

```sql
-- 准备语句
PREPARE get_user AS SELECT * FROM users WHERE id = ? AND age > ?;

-- 执行（传入参数）
EXECUTE get_user USING 1, 18;

-- 释放
DEALLOCATE PREPARE get_user;
```

参数占位符为 `?`，按位置绑定（0-based）。

---

## 20. JSON 函数

```sql
-- JSON 提取
SELECT JSON_EXTRACT('{"name": "Alice", "age": 30}', '$.name');  -- 'Alice'

-- JSON 类型检查
SELECT JSON_TYPE('{"a": 1}');  -- 'object'

-- JSON 验证
SELECT JSON_VALID('{"a": 1}');  -- 1
SELECT JSON_VALID('invalid');   -- 0

-- JSON 数组/对象构建
SELECT JSON_ARRAY(1, 'hello', NULL);     -- [1,"hello",null]
SELECT JSON_OBJECT('name', 'Alice');     -- {"name":"Alice"}

-- JSON 键
SELECT JSON_KEYS('{"a":1, "b":2}');      -- ["a","b"]

-- JSON 长度
SELECT JSON_LENGTH('{"a":1, "b":2}');    -- 2
SELECT JSON_LENGTH('[1, 2, 3]');         -- 3

-- JSON 成员检查
SELECT JSON_CONTAINS('{"a":1}', '1', '$.a');      -- 1 或 0

-- JSON 引用
SELECT JSON_QUOTE('hello "world"');      -- "hello \"world\""

-- JSON 修改
SELECT JSON_SET('{"a":1}', '$.b', 2);   -- {"a":1,"b":2}
SELECT JSON_REMOVE('{"a":1,"b":2}', '$.b');  -- {"a":1}
```

---

## 21. 数据导入导出

### 21.1 SQL Dump 备份/恢复

```bash
# 备份到 SQL 文件
kkdb-cli backup my_db.kkdb backup.sql

# 从 SQL 文件恢复
kkdb-cli restore my_db.kkdb backup.sql
```

### 21.2 CSV 导入/导出

```bash
# 导出表到 CSV
kkdb-cli export my_db.kkdb users users.csv

# 从 CSV 导入
kkdb-cli import my_db.kkdb users users.csv
```

### 21.3 Rust API

```rust
use kkdb::vm::data_transfer;

// SQL Dump
data_transfer::backup_to_sql(&vm, "backup.sql")?;
data_transfer::restore_from_sql(&mut vm, "backup.sql")?;

// CSV
data_transfer::export_csv(&vm, "users", "users.csv")?;
data_transfer::import_csv(&mut vm, "users", "users.csv")?;
```

---

## 22. EXPLAIN 查询计划

```sql
-- 基础 EXPLAIN
EXPLAIN SELECT * FROM users WHERE age > 25;

-- EXPLAIN ANALYZE（执行并打印实际耗时）
EXPLAIN ANALYZE SELECT * FROM users JOIN orders ON users.id = orders.user_id;

-- EXPLAIN (FORMAT TREE)（树形计划输出）
EXPLAIN (FORMAT TREE) SELECT * FROM users WHERE id = 1;

-- EXPLAIN (FORMAT JSON)（JSON 格式输出）
EXPLAIN (FORMAT JSON) SELECT * FROM users ORDER BY name;
```

查询优化器特性：
- **CBO（基于代价的优化器）**：使用 ANALYZE 收集的统计信息
- **索引选择**：自动选择覆盖索引、前缀索引
- **Join 重排**：DPccp 风格多表 Join 枚举
- **谓词下推**：将 WHERE 条件推送到表扫描层
- **子查询去关联化**：将相关子查询转换为 JOIN

---

## 23. 系统命令

```sql
-- 查看所有表
SHOW TABLES;

-- 查看存储引擎状态
SHOW ENGINE STATUS;

-- 回收空间
VACUUM;

-- 收集统计
ANALYZE TABLE users;

-- 设置会话变量
SET kkdb.current_user = 'alice';
```

---

## 24. 存储引擎详解

### 24.1 COW 双超块 Pager

KKDB 的存储引擎基于 **COW（Copy-on-Write）双超块** 设计：

- **页大小**：可配置 512 ~ 65536 字节，默认 4096
- **最大页数**：1,048,576 页 (约 4GB)
- **校验和**：每页使用 FNV-1a 32-bit 校验
- **崩溃安全**：双超块（A/B 交替写入），任何时刻至少一个超块完整

### 24.2 B-Tree

- **叶子页**：类型标记 0x0D，支持双向链表
- **内部页**：类型标记 0x05
- **溢出链**：大值使用溢出页存储
- **SQLite 兼容格式**

### 24.3 WAL（预写日志）

PostgreSQL 风格的 WAL：

- 基于帧（Frame）的格式
- 每帧带校验和
- 支持 Checkpoint 将 WAL 数据写回主文件
- 崩溃恢复时自动回放

### 24.4 Buffer Pool

LRU-K(2) 页面缓冲池：

- **预读取**（Read-ahead Prefetch）
- **写合并**（Write Coalescing）
- **自适应大小调整**

### 24.5 其他存储特性

- **LSM-Tree 压缩模拟器**：字典压缩、冷热分层
- **Bloom 过滤器**：FNV-1a 双哈希，自动调整大小以保持 ~1% FPR
- **前缀压缩**：B-Tree 叶节点上的排序键前缀压缩
- **增量备份**：基于页级校验和的 `BackupManifest`
- **自适应压缩**：None / LZ4 / Zstd / Snappy / DictZstd 多种算法
- **冷热分层**：自动将热数据和冷数据分开存储
- **空间回收**：碎片整理与空间释放
- **并行 Checkpoint**：WAL Group Commit + 并行写回

---

## 25. MySQL 协议服务器

KKDB 内置完整的 **MySQL Wire Protocol v10** 服务器，兼容所有 MySQL 客户端工具。

### 25.1 启动

```bash
# 启动 MySQL 服务器（默认端口 3307）
cargo run --release -- --mysql-port 3307
```

### 25.2 支持的 MySQL 命令

| 命令类型 | 说明 |
|----------|------|
| `COM_QUERY` | SQL 查询 |
| `COM_PING` | 心跳检测 |
| `COM_QUIT` | 断开连接 |
| `COM_INIT_DB` | 切换数据库（USE db） |
| `COM_FIELD_LIST` | 列出表字段 |
| `COM_STATISTICS` | 服务器统计信息 |
| `COM_STMT_PREPARE` | 预编译语句 |
| `COM_STMT_EXECUTE` | 执行预编译语句 |
| `COM_STMT_CLOSE` | 释放预编译语句 |
| `COM_RESET_CONNECTION` | 重置连接状态 |

### 25.3 客户端连接

```bash
# MySQL CLI
mysql -h 127.0.0.1 -P 3307

# 其他工具：DBeaver、Navicat、DataGrip、JDBC、mysql2 等均可直连
```

---

## 26. HTTP REST API

KKDB 内置 Supabase 风格的 HTTP REST API，基于 **axum** 框架。

### 26.1 启动

```bash
cargo run --release -- --http-port 8080
```

### 26.2 认证

```bash
# 注册
curl -X POST http://localhost:8080/auth/signup \
  -H 'Content-Type: application/json' \
  -d '{"email": "user@example.com", "password": "secret"}'

# 登录（返回 JWT access_token + refresh_token）
curl -X POST http://localhost:8080/auth/signin \
  -H 'Content-Type: application/json' \
  -d '{"email": "user@example.com", "password": "secret"}'

# 刷新 Token
curl -X POST http://localhost:8080/auth/refresh \
  -H 'Content-Type: application/json' \
  -d '{"refresh_token": "..."}'

# API Key 管理
curl -X POST http://localhost:8080/auth/apikeys \
  -H 'Authorization: Bearer <token>' \
  -H 'Content-Type: application/json' \
  -d '{"name": "my-key"}'
```

### 26.3 查询接口

```bash
# 执行 SQL 查询
curl -X POST http://localhost:8080/rest/query \
  -H 'Authorization: Bearer <token>' \
  -H 'Content-Type: application/json' \
  -d '{"sql": "SELECT * FROM users"}'

# 执行 DML（INSERT/UPDATE/DELETE）
curl -X POST http://localhost:8080/rest/execute \
  -H 'Authorization: Bearer <token>' \
  -H 'Content-Type: application/json' \
  -d '{"sql": "INSERT INTO users (name) VALUES ('\''Alice'\'')"}'

# 批量执行
curl -X POST http://localhost:8080/rest/batch \
  -H 'Authorization: Bearer <token>' \
  -H 'Content-Type: application/json' \
  -d '{"statements": ["INSERT INTO t VALUES(1)", "INSERT INTO t VALUES(2)"]}'

# 批量数据操作
curl -X POST http://localhost:8080/rest/bulk \
  -H 'Authorization: Bearer <token>' \
  -H 'Content-Type: application/json' \
  -d '{"table": "users", "rows": [{"name": "A"}, {"name": "B"}]}'

# 健康检查
curl http://localhost:8080/health
```

### 26.4 多租户隔离

每个注册用户拥有独立的 VM 实例和数据库文件，完全隔离。数据库文件存储在 `data_dir/<user_id>/` 目录下。

---

## 27. 分布式集群（Raft）

### 27.1 架构

KKDB 使用 [openraft](https://github.com/datafuselabs/openraft) v0.9 实现 Raft 共识：

- **强一致写入**：所有写请求通过 Raft Leader 提交
- **分布式节点**（`KkdbNode`）：每个节点包装一个 `openraft::Raft` 实例
- **状态机**（`KkdbStateMachine`）：将 Raft 日志条目（SQL 语句）应用到本地 VM
- **WAL 持久化日志**（`wal.log` + `vote.json` + `purge.json`）：CRC32 校验保护

### 27.2 网络传输

两种模式：
- **内存传输**（Phase 1）：通过 `NodeRegistry` 共享内存通信，适合测试
- **HTTP 传输**（Phase 2）：基于 `reqwest` + `axum` 的 JSON-RPC，适合跨进程/跨机器

### 27.3 Raft HTTP 端点

| 端点 | 说明 |
|------|------|
| `POST /raft/append-entries` | 日志复制 |
| `POST /raft/vote` | 投票请求 |
| `POST /raft/install-snapshot` | 快照安装 |
| `POST /raft/init` | 初始化集群 |
| `POST /raft/add-learner` | 添加 Learner 节点 |
| `POST /raft/change-membership` | 变更成员身份 |
| `GET /raft/status` | 集群状态 |
| `GET /raft/metrics` | Raft 指标 |
| `GET /raft/metrics/prometheus` | Prometheus 格式指标 |

### 27.4 集群管理

```bash
# 初始化 3 节点集群
# Node 1: --raft-id 1 --raft-port 9001 --http-port 8001
# Node 2: --raft-id 2 --raft-port 9002 --http-port 8002
# Node 3: --raft-id 3 --raft-port 9003 --http-port 8003
```

### 27.5 高级分布式特性

- **自动故障转移**：Leader 失效后自动选举新 Leader
- **读副本路由**：只读查询自动路由到 Follower
- **2PC/3PC 分布式事务**：跨节点原子事务
- **一致性哈希**：可配置虚拟节点的分片路由
- **分布式快照隔离**：跨分片推送、全局死锁检测
- **日志压缩**：Raft 日志自动压缩
- **跨数据中心复制**：延迟监控、拓扑发现
- **Multi-Raft 组管理**：多 Group 并行共识
- **Saga 编排器**：补偿事务
- **服务网格**：节点发现注册、配置中心、链路加密

---

## 28. Binlog 与复制

### 28.1 Binlog 记录类型

| 类型 | 说明 |
|------|------|
| `Begin` | 事务开始 |
| `Insert` | 插入操作（table, rowid, values） |
| `Update` | 更新操作（table, rowid, old_values, new_values） |
| `Delete` | 删除操作（table, rowid, values） |
| `Prepare` | 两阶段提交：准备 |
| `Commit` | 事务提交 |
| `Rollback` | 事务回滚 |
| `Sql` | 原始 SQL 语句 |

### 28.2 Binlog 广播

`BinlogBroadcaster` 支持订阅 binlog 事件流，可用于：
- 基于语句的复制（Statement-based Replication）
- 变更数据捕获（CDC）
- 与 Raft 集成实现分布式复制

---

## 29. Rust API 编程接口

### 29.1 创建 VM 实例

```rust
use kkdb::vm::execute::{VM, ExecResult};

// 内存模式
let mut vm = VM::new_memory();

// 文件模式（持久化，按表拆分文件）
let mut vm = VM::open("my_database.kkdb")?;
```

### 29.2 执行 SQL

```rust
match vm.execute("SELECT * FROM users WHERE age > 25") {
    Ok(ExecResult::QueryResult { columns, rows }) => {
        println!("Columns: {:?}", columns);
        for row in rows {
            println!("{:?}", row);
        }
    }
    Ok(ExecResult::RowsAffected(n)) => println!("{} rows affected", n),
    Ok(ExecResult::Ok) => println!("OK"),
    Ok(ExecResult::Explain(plan)) => println!("Plan: {}", plan),
    Err(e) => eprintln!("Error: {:?}", e),
}
```

### 29.3 ExecResult 枚举

| 变体 | 说明 |
|------|------|
| `Ok` | DDL 成功，无返回数据 |
| `RowsAffected(usize)` | INSERT/UPDATE/DELETE 影响行数 |
| `QueryResult { columns, rows }` | SELECT 查询结果集 |
| `Explain(String)` | EXPLAIN 输出 |

### 29.4 连接池

```rust
use kkdb::vm::connection_pool::ConnectionPool;

let pool = ConnectionPool::new("my_db.kkdb", 10)?; // 最大 10 连接
let handle = pool.get()?; // 借出一个连接

// handle 实现 Deref<Target=VM>，可直接使用
handle.execute("SELECT 1")?;
// drop(handle) 自动归还连接
```

### 29.5 SQL 解析

```rust
use kkdb::sql::parser::parse_sql;
use kkdb::sql::ast::Statement;

let stmt = parse_sql("SELECT * FROM users WHERE id = 1")?;
match stmt {
    Statement::Select(select) => { /* ... */ }
    Statement::Insert(insert) => { /* ... */ }
    _ => {}
}
```

### 29.6 数据类型

```rust
use kkdb::types::{Value, DataType, Row};

let val = Value::Integer(42);
let text = Value::Text(std::borrow::Cow::Owned("hello".to_string()));
let row: Row = vec![val, text, Value::Null];
```

### 29.7 错误类型

```rust
use kkdb::error::KkdbError;

// 所有错误变体：
// Io, SyntaxError, ParseError, RuntimeError, TypeError,
// TableAlreadyExists, TableNotFound, ColumnNotFound,
// ColumnCountMismatch, PageOutOfRange, BTreeError,
// DatabaseFull, CorruptDatabase, ConstraintViolation,
// Internal, NotImplemented, LockConflict
```

---

## 30. 命令行工具

### 30.1 主程序 (`kkdb`)

```bash
# 交互式 REPL（内存模式）
kkdb

# 打开数据库文件
kkdb my_database.kkdb

# 启动网络服务
kkdb --mysql-port 3307 --http-port 8080
```

### 30.2 CLI 工具 (`kkdb-cli`)

```bash
# 备份
kkdb-cli backup <db_path> <output.sql>

# 恢复
kkdb-cli restore <db_path> <input.sql>

# 导出 CSV
kkdb-cli export <db_path> <table> <output.csv>

# 导入 CSV
kkdb-cli import <db_path> <table> <input.csv>
```

---

## 31. 模块架构全景

以下是 KKDB 全部模块的结构化清单：

### 31.1 基础模块

| 模块 | 文件 | 说明 |
|------|------|------|
| **types** | `src/types.rs` | DataType / Value / Row 定义，序列化/反序列化 |
| **error** | `src/error.rs` | KkdbError 错误枚举（thiserror），17 种错误变体 |
| **schema** | `src/schema.rs` | TableSchema / ColumnInfo / PolicySchema，Schema 目录 B-Tree 管理 |
| **varint** | `src/varint.rs` | LEB128 变长整数编码/解码，ZigZag 有符号转换 |

### 31.2 SQL 解析

| 模块 | 文件 | 说明 |
|------|------|------|
| **sql::ast** | `src/sql/ast.rs` | 全部 SQL 语句的 AST 节点定义（745 行） |
| **sql::parser** | `src/sql/parser.rs` | `parse_sql()` 入口，委托 sqlparser 适配器 |
| **sql::sqlparser_adapter** | `src/sql/sqlparser_adapter/mod.rs` | sqlparser crate (SQLiteDialect) → KKDB AST 转换层 |
| *— common* | `src/sql/sqlparser_adapter/common.rs` | 通用转换辅助 |
| *— expr* | `src/sql/sqlparser_adapter/expr.rs` | 表达式转换 |
| *— query* | `src/sql/sqlparser_adapter/query.rs` | SELECT 查询转换 |
| *— statement* | `src/sql/sqlparser_adapter/statement.rs` | DDL/DML 语句转换 |

### 31.3 存储引擎

| 模块 | 文件 | 说明 |
|------|------|------|
| **storage::pager** | `src/storage/pager.rs` | COW v2 双超块 Pager（1788 行），崩溃安全持久化 |
| **storage::btree** | `src/storage/btree.rs` | B-Tree（1860 行），叶/内部页，溢出链，SQLite 兼容格式 |
| **storage::wal** | `src/storage/wal.rs` | PostgreSQL 风格 WAL（1589 行），帧校验 + Checkpoint |
| **storage::cursor** | `src/storage/cursor.rs` | B-Tree 游标，栈式遍历 |
| **storage::bloom** | `src/storage/bloom.rs` | Bloom Filter（FNV-1a 双哈希），~1% FPR |
| **storage::buffer_pool** | `src/storage/buffer_pool.rs` | LRU-K(2) 缓冲池，预读取 + 写合并 |
| **storage::lsm** | `src/storage/lsm.rs` | LSM-Tree 压缩模拟，字典压缩 |
| **storage::prefix_compress** | `src/storage/prefix_compress.rs` | B-Tree 叶节点前缀压缩 |
| **storage::backup** | `src/storage/backup.rs` | 页级校验 + 增量备份 BackupManifest |
| **storage::ext::advanced** | `src/storage/ext/advanced.rs` | 写放大追踪、分层 Bloom、分区裁剪 |
| **storage::ext::optimizer** | `src/storage/ext/optimizer.rs` | 自适应压缩、IO 调度、页预热 |
| **storage::ext::ultimate** | `src/storage/ext/ultimate.rs` | 自适应页大小、WAL Group Commit、并行 Checkpoint |
| **storage::ext::adv_storage** | `src/storage/ext/adv_storage.rs` | 自适应压缩 v2、页预取、增量合并 |
| **storage::ext::deep_storage** | `src/storage/ext/deep_storage.rs` | 列存引擎、数据分区管理、冷热分层 |

### 31.4 虚拟机（VM）

| 模块 | 文件 | 说明 |
|------|------|------|
| **vm::execute** | `src/vm/execute.rs` | VM 核心（1488 行），`new_memory`/`open`，多文件模式 |
| **vm::exec_ddl** | `src/vm/exec_ddl.rs` | DDL 执行器（2367 行），CREATE/DROP/ALTER/VACUUM/ANALYZE |
| **vm::exec_dml** | `src/vm/exec_dml.rs` | DML 执行器（2201 行），INSERT/UPDATE/DELETE + FK + MVCC |
| **vm::exec_select** | `src/vm/exec_select.rs` | SELECT 管道（4914 行），JOIN/CTE/Window/FTS/子查询 |
| **vm::eval_expr** | `src/vm/eval_expr.rs` | 表达式求值（2647 行），函数、运算符、LIKE、占位符 |
| **vm::mvcc** | `src/vm/mvcc.rs` | MVCC Undo Log（1087 行），快照隔离、保存点 |
| **vm::lock_manager** | `src/vm/lock_manager.rs` | 表级锁管理、死锁检测 |
| **vm::connection_pool** | `src/vm/connection_pool.rs` | 线程安全连接池，`Arc<Mutex<VM>>` |
| **vm::prepared** | `src/vm/prepared.rs` | PREPARE/EXECUTE/DEALLOCATE 预编译语句 |
| **vm::gc** | `src/vm/gc.rs` | MVCC GC、隔离级别验证、FK 级联追踪 |
| **vm::data_transfer** | `src/vm/data_transfer.rs` | SQL Dump / CSV 数据导入导出 |

### 31.5 查询优化器

| 模块 | 文件 | 说明 |
|------|------|------|
| **optimizer::query_compiler** | `src/vm/optimizer/query_compiler.rs` | 查询模板编译、表达式向量化代码生成 |
| **optimizer::query_cache** | `src/vm/optimizer/query_cache.rs` | MySQL 风格查询结果缓存（LRU + DML 失效） |
| **optimizer::query_opt_deep** | `src/vm/optimizer/query_opt_deep.rs` | 代价模型校准、DPccp Join 枚举、子查询去关联 |
| **optimizer::adaptive_join** | `src/vm/optimizer/adaptive_join.rs` | 自适应 Join 算法选择 + 物化视图刷新追踪 |
| **optimizer::vectorized** | `src/vm/optimizer/vectorized.rs` | 列式 ColumnBatch、向量化 filter/project/aggregate |
| **optimizer::exec_engine_v2** | `src/vm/optimizer/exec_engine_v2.rs` | 向量化引擎 2.0、表达式 JIT、并行查询 |
| **optimizer::query_opt_v2** | `src/vm/optimizer/query_opt_v2.rs` | 全局索引优化、规则查询重写、自动索引建议 |

### 31.6 执行引擎扩展

| 模块 | 文件 | 说明 |
|------|------|------|
| **engine::exec_engine** | `src/vm/engine/exec_engine.rs` | 流式窗口函数、排序溢出磁盘、Semi/Anti Join |
| **engine::sql_engine_adv** | `src/vm/engine/sql_engine_adv.rs` | MV 增量刷新、游标分页、异步管道 |
| **engine::dev_tools** | `src/vm/engine/dev_tools.rs` | SQL Lint、计划树可视化、索引建议、Schema 迁移 |
| **engine::adv_query** | `src/vm/engine/adv_query.rs` | MV 自动刷新、分布式查询路由、流式背压 |
| **engine::sql_ext** | `src/vm/engine/sql_ext.rs` | 增强窗口函数、MERGE 语句、批量 UPSERT |
| **engine::sql_pipeline** | `src/vm/engine/sql_pipeline.rs` | 流式查询处理、多阶段聚合、执行计划缓存池 |
| **engine::dev_experience** | `src/vm/engine/dev_experience.rs` | Explain 可视化、查询性能分析、Schema 迁移工具 |

### 31.7 安全 / 认证 / 审计

| 模块 | 文件 | 说明 |
|------|------|------|
| **auth::rbac** | `src/vm/auth/rbac.rs` | RBAC 角色权限、审计持久化 |
| **auth::audit** | `src/vm/auth/audit.rs` | 审计日志（序列号 + 时间戳 + SQL + 结果） |
| **auth::security** | `src/vm/auth/security.rs` | 列级加密（AES/ChaCha20）、数据脱敏、TLS、密码策略 |
| **auth::security_adv** | `src/vm/auth/security_adv.rs` | 细粒度列/行级权限、行策略 |

### 31.8 监控 / 可观测性

| 模块 | 文件 | 说明 |
|------|------|------|
| **monitor::perf_counter** | `src/vm/monitor/perf_counter.rs` | 原子性能计数器、慢查询日志、计划缓存统计 |
| **monitor::diagnostics** | `src/vm/monitor/diagnostics.rs` | EXPLAIN ANALYZE 节点统计、系统目录虚拟元数据 |
| **monitor::observability** | `src/vm/monitor/observability.rs` | 查询追踪、资源配额、在线 DDL 进度 |
| **monitor::observability_ops** | `src/vm/monitor/observability_ops.rs` | 慢查询收集、资源水位告警、连接池监控 |
| **monitor::observability_v2** | `src/vm/monitor/observability_v2.rs` | 分布式追踪、指标聚合、健康检查仪表盘 |
| **monitor::test_catalog** | `src/vm/monitor/test_catalog.rs` | 测试用例分类系统 |
| **monitor::bench_framework** | `src/vm/monitor/bench_framework.rs` | 基准测试框架 |

### 31.9 全文检索

| 模块 | 文件 | 说明 |
|------|------|------|
| **fulltext** | `src/fulltext/mod.rs` | FullTextTokenizer（HuggingFace）、`tokenize_to_tf()` |
| **fulltext::index** | `src/fulltext/index.rs` | BM25 倒排索引存储（B-Tree 键编码，Postings + IDF + Global） |
| **fulltext::tokenizer** | `src/fulltext/tokenizer.rs` | 中英文分词器（jieba-rs CJK + 英文空白分割 + 停用词） |
| **fulltext::fts_advanced** | `src/fulltext/fts_advanced.rs` | 模糊搜索、同义词扩展、分面搜索、实时索引更新 |

### 31.10 向量搜索

| 模块 | 文件 | 说明 |
|------|------|------|
| **vector** | `src/vector/mod.rs` | VectorIndex 元数据 + HNSW 图、VectorIndexRegistry 管理多索引 |
| **vector::hnsw** | `src/vector/hnsw.rs` | HNSW 图实现（996 行），O(log N) ANN 搜索、惰性删除 + 重建 |
| **vector::distance** | `src/vector/distance.rs` | DistanceMetric 枚举（Cosine / L2），距离计算函数 |
| **vector::index** | `src/vector/index.rs` | 向量数据 B-Tree 键/值编码 |
| **vector::vector_advanced** | `src/vector/vector_advanced.rs` | 多向量索引、混合搜索、量化压缩、批量导入优化 |

### 31.11 服务器

| 模块 | 文件 | 说明 |
|------|------|------|
| **server** | `src/server/mod.rs` | `start_server`（MySQL）+ `start_http_server`（REST） |
| **server::http_api** | `src/server/http_api.rs` | Supabase 风格 REST API（1252 行），JWT 认证 + 多租户 |
| **server::kk_backend** | `src/server/kk_backend.rs` | msql_srv MysqlShim 桥接 |
| **server::mysql** | `src/server/mysql.rs` | 原生 MySQL Wire Protocol v10（1699 行） |

### 31.12 分布式 / Raft

| 模块 | 文件 | 说明 |
|------|------|------|
| **raft::types** | `src/raft/types.rs` | KkdbRequest/KkdbResponse/KkdbTypeConfig 类型定义 |
| **raft::node** | `src/raft/node.rs` | KkdbNode — Raft 节点封装，集群初始化 |
| **raft::log_store** | `src/raft/log_store.rs` | WAL 持久化 Raft 日志存储（CRC32 校验） |
| **raft::network** | `src/raft/network.rs` | 内存 Raft 传输（Phase 1） |
| **raft::state_machine** | `src/raft/state_machine.rs` | KkdbStateMachine — SQL 日志回放 + JSON 快照 |
| **raft::http_network** | `src/raft/http_network.rs` | HTTP Raft 网络客户端（Phase 2） |
| **raft::http_transport** | `src/raft/http_transport.rs` | HTTP JSON-RPC 传输层（append/vote/snapshot 端点） |

### 31.13 Raft 高级特性

| 模块 | 文件 | 说明 |
|------|------|------|
| **features::ha** | `src/raft/features/ha.rs` | Leader 选举、自动故障转移、读副本路由 |
| **features::ha_dr** | `src/raft/features/ha_dr.rs` | 故障转移链、副本同步、跨地域 DR |
| **features::dtx** | `src/raft/features/dtx.rs` | 2PC/3PC 分布式事务协调器 |
| **features::dist_txn** | `src/raft/features/dist_txn.rs` | 两阶段锁升级、全局序列化 |
| **features::consistent_hash** | `src/raft/features/consistent_hash.rs` | 一致性哈希环（可配虚拟节点） |
| **features::snapshot_isolation** | `src/raft/features/snapshot_isolation.rs` | 分布式快照隔离、全局死锁检测 |
| **features::cluster_mgmt** | `src/raft/features/cluster_mgmt.rs` | Raft 日志压缩、成员变更、跨 DC 延迟监控 |
| **features::dist_txn_adv** | `src/raft/features/dist_txn_adv.rs` | Saga 编排器、补偿事务、分布式快照 |
| **features::dist_advanced** | `src/raft/features/dist_advanced.rs` | Multi-Raft Group 管理、跨区复制、动态负载均衡 |
| **features::dist_infra** | `src/raft/features/dist_infra.rs` | 节点发现注册、配置中心、服务网格、链路加密 |

### 31.14 Binlog

| 模块 | 文件 | 说明 |
|------|------|------|
| **binlog** | `src/binlog/mod.rs` | LogRecord 枚举、二进制序列化、BinlogBroadcaster |

### 31.15 二进制入口

| 文件 | 说明 |
|------|------|
| `src/main.rs` | 交互式 REPL（rustyline），支持 MySQL/HTTP 服务器启动 |
| `src/bin/kkdb-cli.rs` | 命令行工具：backup / restore / export / import |
| `src/bin/big_data_bench.rs` | 大数据量基准测试工具 |

---

## 附录 A：现有文档索引

| 文档 | 说明 |
|------|------|
| [PROJECT.md](PROJECT.md) | 项目概述与架构 |
| [API.md](API.md) | Rust API 文档 |
| [FUNCTIONS.md](FUNCTIONS.md) | 内置函数参考 |
| [ADVANCED_SQL.md](ADVANCED_SQL.md) | 高阶 SQL 特性指南 |
| [HTTP_API.md](HTTP_API.md) | HTTP REST API 文档 |
| [MYSQL_SERVER.md](MYSQL_SERVER.md) | MySQL 协议服务器文档 |
| [DISTRIBUTED.md](DISTRIBUTED.md) | 分布式集群文档 |
| [VECTOR_SEARCH_DESIGN.md](VECTOR_SEARCH_DESIGN.md) | 向量搜索引擎设计 |
| [BINLOG_DESIGN.md](BINLOG_DESIGN.md) | Binlog 设计文档 |
| [COW_DOUBLE_SUPERBLOCK_DESIGN.md](COW_DOUBLE_SUPERBLOCK_DESIGN.md) | COW 双超块存储设计 |
| [SQLPARSER_REFACTOR_ANALYSIS.md](SQLPARSER_REFACTOR_ANALYSIS.md) | SQL 解析器重构分析 |
| [UPGRADE_PLAN.md](UPGRADE_PLAN.md) | 升级计划 |
| [optimization_roadmap.md](optimization_roadmap.md) | 优化路线图 |

---

## 附录 B：总代码量统计

| 子系统 | 核心文件数 | 估算代码行数 |
|--------|-----------|-------------|
| 存储引擎 | 14 | ~8,000 |
| VM 核心 | 11 | ~15,000 |
| 查询优化器 | 7 | ~3,700 |
| 执行引擎扩展 | 7 | ~4,000 |
| 安全/审计 | 4 | ~2,200 |
| 监控/可观测 | 7 | ~3,300 |
| SQL 解析 | 6 | ~2,800 |
| 全文检索 | 4 | ~1,500 |
| 向量搜索 | 5 | ~2,100 |
| 服务器 | 4 | ~3,200 |
| Raft 核心 | 7 | ~2,100 |
| Raft 高级特性 | 10 | ~5,800 |
| Binlog | 1 | ~900 |
| 基础模块 | 4 | ~2,300 |
| **总计** | **~91** | **~57,000** |
