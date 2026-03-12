# KKDB 应用案例文档

本文档通过完整的真实场景案例，演示如何使用 KKDB 构建各类应用。

---

## 目录

1. [电商订单系统](#1-电商订单系统)
2. [内容管理系统（CMS）](#2-内容管理系统cms)
3. [实时日志分析平台](#3-实时日志分析平台)
4. [AI 向量检索应用](#4-ai-向量检索应用)
5. [多租户 SaaS 平台](#5-多租户-saas-平台)
6. [IoT 时序数据管理](#6-iot-时序数据管理)

---

## 1. 电商订单系统

### 1.1 数据库设计

```sql
-- 用户表
CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE,
    email TEXT UNIQUE,
    password_hash TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    status TEXT DEFAULT 'active' CHECK(status IN ('active', 'suspended', 'deleted'))
);

-- 商品表
CREATE TABLE products (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    description TEXT,
    price REAL NOT NULL CHECK(price >= 0),
    stock INTEGER NOT NULL DEFAULT 0 CHECK(stock >= 0),
    category TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 订单表
CREATE TABLE orders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    total_amount REAL NOT NULL CHECK(total_amount >= 0),
    status TEXT DEFAULT 'pending' CHECK(status IN ('pending', 'paid', 'shipped', 'delivered', 'cancelled')),
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP
);

-- 订单明细表
CREATE TABLE order_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id INTEGER NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
    product_id INTEGER NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    quantity INTEGER NOT NULL CHECK(quantity > 0),
    unit_price REAL NOT NULL CHECK(unit_price >= 0)
);

-- 索引
CREATE INDEX idx_orders_user_id ON orders(user_id);
CREATE INDEX idx_orders_status ON orders(status);
CREATE INDEX idx_order_items_order_id ON order_items(order_id);
CREATE INDEX idx_products_category ON products(category);
```

### 1.2 业务查询

```sql
-- 用户购买历史
SELECT o.id AS order_id, o.status, o.total_amount, o.created_at,
       p.name AS product_name, oi.quantity, oi.unit_price
FROM orders o
JOIN order_items oi ON o.id = oi.order_id
JOIN products p ON oi.product_id = p.id
WHERE o.user_id = 1
ORDER BY o.created_at DESC;

-- 商品销售排行（Top 10）
SELECT p.name, p.category, SUM(oi.quantity) AS total_sold,
       SUM(oi.quantity * oi.unit_price) AS total_revenue
FROM order_items oi
JOIN products p ON oi.product_id = p.id
JOIN orders o ON oi.order_id = o.id
WHERE o.status IN ('paid', 'shipped', 'delivered')
GROUP BY p.id, p.name, p.category
ORDER BY total_sold DESC
LIMIT 10;

-- 月度销售报表
SELECT STRFTIME('%Y-%m', o.created_at) AS month,
       COUNT(DISTINCT o.id) AS order_count,
       COUNT(DISTINCT o.user_id) AS customer_count,
       SUM(o.total_amount) AS total_revenue,
       AVG(o.total_amount) AS avg_order_value
FROM orders o
WHERE o.status != 'cancelled'
GROUP BY STRFTIME('%Y-%m', o.created_at)
ORDER BY month DESC;

-- 库存预警（低库存商品）
SELECT name, category, stock
FROM products
WHERE stock < 10 AND stock > 0
ORDER BY stock ASC;
```

### 1.3 事务：下单流程

```sql
BEGIN;

-- 1. 检查库存
SELECT stock FROM products WHERE id = 42 FOR UPDATE;
-- 假设 stock = 50，用户要买 3 件

-- 2. 扣减库存
UPDATE products SET stock = stock - 3 WHERE id = 42;

-- 3. 创建订单
INSERT INTO orders (user_id, total_amount, status)
VALUES (1, 89.97, 'pending');

-- 4. 创建订单明细
INSERT INTO order_items (order_id, product_id, quantity, unit_price)
VALUES (LAST_INSERT_ROWID(), 42, 3, 29.99);

COMMIT;
```

### 1.4 数据分析：窗口函数

```sql
-- 每个用户的消费排名
SELECT u.username, o.total_amount,
       RANK() OVER (ORDER BY o.total_amount DESC) AS spending_rank,
       SUM(o.total_amount) OVER (PARTITION BY o.user_id) AS user_total
FROM users u
JOIN orders o ON u.id = o.user_id
WHERE o.status != 'cancelled';

-- 环比增长率
WITH monthly AS (
    SELECT STRFTIME('%Y-%m', created_at) AS month,
           SUM(total_amount) AS revenue
    FROM orders
    WHERE status != 'cancelled'
    GROUP BY month
)
SELECT month, revenue,
       LAG(revenue) OVER (ORDER BY month) AS prev_revenue,
       ROUND((revenue - LAG(revenue) OVER (ORDER BY month)) * 100.0
             / LAG(revenue) OVER (ORDER BY month), 2) AS growth_pct
FROM monthly;
```

---

## 2. 内容管理系统（CMS）

### 2.1 数据库设计

```sql
CREATE TABLE articles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    author_id INTEGER NOT NULL REFERENCES users(id),
    status TEXT DEFAULT 'draft' CHECK(status IN ('draft', 'published', 'archived')),
    tags TEXT,  -- JSON 数组
    published_at TIMESTAMP,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP
);

-- 全文索引
CREATE FULLTEXT INDEX idx_ft_articles ON articles(title, body);

-- 标签索引
CREATE INDEX idx_articles_status ON articles(status);
```

### 2.2 全文检索查询

```sql
-- 中文全文搜索
SELECT id, title,
       MATCH(title, body) AGAINST ('Rust 数据库 性能') AS relevance
FROM articles
WHERE MATCH(title, body) AGAINST ('Rust 数据库 性能')
  AND status = 'published'
ORDER BY relevance DESC
LIMIT 20;

-- 按标签+全文组合检索
SELECT a.id, a.title, a.tags,
       MATCH(a.title, a.body) AGAINST ('分布式') AS score
FROM articles a
WHERE MATCH(a.title, a.body) AGAINST ('分布式')
  AND JSON_CONTAINS(a.tags, '"distributed"')
ORDER BY score DESC;
```

### 2.3 CTE：文章统计仪表盘

```sql
WITH
  monthly_stats AS (
    SELECT STRFTIME('%Y-%m', published_at) AS month,
           COUNT(*) AS article_count
    FROM articles
    WHERE status = 'published'
    GROUP BY month
  ),
  author_stats AS (
    SELECT u.username, COUNT(a.id) AS articles
    FROM users u
    JOIN articles a ON u.id = a.author_id
    WHERE a.status = 'published'
    GROUP BY u.id, u.username
  )
SELECT 'monthly' AS type, month AS label, article_count AS value
FROM monthly_stats
UNION ALL
SELECT 'author', username, articles
FROM author_stats
ORDER BY type, value DESC;
```

---

## 3. 实时日志分析平台

### 3.1 数据库设计

```sql
CREATE TABLE logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    level TEXT NOT NULL CHECK(level IN ('DEBUG', 'INFO', 'WARN', 'ERROR', 'FATAL')),
    service TEXT NOT NULL,
    message TEXT NOT NULL,
    context TEXT  -- JSON 格式的上下文信息
);

CREATE INDEX idx_logs_timestamp ON logs(timestamp);
CREATE INDEX idx_logs_level ON logs(level);
CREATE INDEX idx_logs_service ON logs(service);
CREATE FULLTEXT INDEX idx_ft_logs ON logs(message);
```

### 3.2 日志分析查询

```sql
-- 最近 1 小时的错误日志
SELECT timestamp, service, level, message
FROM logs
WHERE level IN ('ERROR', 'FATAL')
ORDER BY timestamp DESC
LIMIT 100;

-- 各服务错误率统计
SELECT service,
       COUNT(*) AS total_logs,
       SUM(CASE WHEN level IN ('ERROR', 'FATAL') THEN 1 ELSE 0 END) AS error_count,
       ROUND(SUM(CASE WHEN level IN ('ERROR', 'FATAL') THEN 1 ELSE 0 END) * 100.0
             / COUNT(*), 2) AS error_rate_pct
FROM logs
GROUP BY service
HAVING COUNT(*) > 10
ORDER BY error_rate_pct DESC;

-- 错误趋势（按小时）
SELECT STRFTIME('%Y-%m-%d %H:00', timestamp) AS hour,
       COUNT(*) AS error_count
FROM logs
WHERE level IN ('ERROR', 'FATAL')
GROUP BY hour
ORDER BY hour DESC
LIMIT 24;

-- 全文搜索错误日志
SELECT timestamp, service, message,
       MATCH(message) AGAINST ('connection timeout') AS relevance
FROM logs
WHERE MATCH(message) AGAINST ('connection timeout')
ORDER BY relevance DESC
LIMIT 20;

-- JSON 上下文提取
SELECT timestamp, message,
       JSON_EXTRACT(context, '$.user_id') AS user_id,
       JSON_EXTRACT(context, '$.request_id') AS request_id,
       JSON_EXTRACT(context, '$.duration_ms') AS duration
FROM logs
WHERE level = 'ERROR'
  AND JSON_EXTRACT(context, '$.duration_ms') > 5000
ORDER BY timestamp DESC;
```

---

## 4. AI 向量检索应用

### 4.1 数据库设计

```sql
-- 文档表（含嵌入向量）
CREATE TABLE documents (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    embedding BLOB,  -- f32 数组的二进制编码
    source TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 创建向量索引（1536 维，适配 OpenAI text-embedding-3-small）
CREATE VECTOR INDEX idx_doc_embedding ON documents(embedding) DIM 1536 DISTANCE COSINE;

-- 全文索引（混合搜索）
CREATE FULLTEXT INDEX idx_ft_docs ON documents(title, content);
```

### 4.2 向量 + 全文混合搜索（Rust API）

```rust
use kkdb::vm::execute::{VM, ExecResult};

let mut vm = VM::new_memory();

// 1. 创建表和索引
vm.execute_sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, content TEXT, embedding BLOB)")?;
vm.execute_sql("CREATE VECTOR INDEX idx_emb ON docs(embedding) DIM 4 DISTANCE COSINE")?;
vm.execute_sql("CREATE FULLTEXT INDEX idx_ft ON docs(title, content)")?;

// 2. 插入数据（向量以 BLOB 存储）
//    实际应用中由 ML 模型生成 embedding

// 3. 全文检索
if let ExecResult::QueryResult { columns, rows } = vm.execute_sql(
    "SELECT id, title, MATCH(title, content) AGAINST ('数据库') AS score \
     FROM docs WHERE MATCH(title, content) AGAINST ('数据库') ORDER BY score DESC LIMIT 5"
)? {
    for row in &rows {
        println!("标题: {:?}, 相关度: {:?}", row[1], row[2]);
    }
}
```

### 4.3 RAG（检索增强生成）架构

```
用户问题
    ↓
1. 文本 → Embedding（调用外部 ML API）
    ↓
2. KKDB 向量搜索（HNSW 近似最近邻）
    ↓
3. 检索到 Top-K 相关文档
    ↓
4. 文档 + 问题 → LLM 生成答案
    ↓
5. 返回增强答案
```

关键 SQL：
```sql
-- 向量相似度搜索（通过 Rust API 调用 VectorIndexRegistry）
-- 全文补充搜索
SELECT id, title, MATCH(title, content) AGAINST ('关键词') AS score
FROM documents
WHERE MATCH(title, content) AGAINST ('关键词')
ORDER BY score DESC
LIMIT 10;
```

---

## 5. 多租户 SaaS 平台

### 5.1 方案 A：HTTP REST API 多租户（推荐）

KKDB HTTP REST API 天然支持多租户——每个注册用户自动获得独立的 VM 实例和数据库文件。

```bash
# 注册租户
curl -X POST http://localhost:8080/auth/signup \
  -H 'Content-Type: application/json' \
  -d '{"email": "tenant1@example.com", "password": "secret"}'

# 登录获取 Token
curl -X POST http://localhost:8080/auth/signin \
  -H 'Content-Type: application/json' \
  -d '{"email": "tenant1@example.com", "password": "secret"}'
# 返回: {"access_token": "eyJ...", "refresh_token": "..."}

# 每个租户操作自己的数据库
curl -X POST http://localhost:8080/rest/execute \
  -H 'Authorization: Bearer eyJ...' \
  -H 'Content-Type: application/json' \
  -d '{"sql": "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)"}'
```

### 5.2 方案 B：行级安全策略（RLS）

在同一张表中通过 RLS 策略隔离不同租户的数据。

```sql
-- 创建共享表
CREATE TABLE tenant_data (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tenant_id TEXT NOT NULL,
    data TEXT
);

-- 启用 RLS
ALTER TABLE tenant_data ENABLE ROW LEVEL SECURITY;

-- 创建策略：每个用户只能看到自己的数据
CREATE POLICY tenant_isolation ON tenant_data
    FOR ALL
    USING (tenant_id = CURRENT_USER)
    WITH CHECK (tenant_id = CURRENT_USER);

-- 设置当前租户
SET kkdb.current_user = 'tenant_A';

-- 只能看到 tenant_A 的数据
SELECT * FROM tenant_data;
```

### 5.3 方案 C：RBAC 权限组合

```sql
-- 创建租户用户
CREATE USER tenant_a WITH PASSWORD 'pass_a';
CREATE USER tenant_b WITH PASSWORD 'pass_b';

-- 各自只能访问自己的表
GRANT SELECT, INSERT, UPDATE, DELETE ON tenant_a_data TO tenant_a;
GRANT SELECT, INSERT, UPDATE, DELETE ON tenant_b_data TO tenant_b;
```

---

## 6. IoT 时序数据管理

### 6.1 数据库设计

```sql
CREATE TABLE sensor_data (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    device_id TEXT NOT NULL,
    metric_name TEXT NOT NULL,
    value REAL NOT NULL,
    timestamp TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    tags TEXT  -- JSON: {"location": "room1", "unit": "celsius"}
);

CREATE INDEX idx_sensor_device_time ON sensor_data(device_id, timestamp);
CREATE INDEX idx_sensor_metric ON sensor_data(metric_name);
```

### 6.2 时序分析查询

```sql
-- 设备最新读数
SELECT DISTINCT device_id, metric_name,
       FIRST_VALUE(value) OVER (
           PARTITION BY device_id, metric_name
           ORDER BY timestamp DESC
       ) AS latest_value,
       FIRST_VALUE(timestamp) OVER (
           PARTITION BY device_id, metric_name
           ORDER BY timestamp DESC
       ) AS latest_time
FROM sensor_data;

-- 温度传感器小时平均值
SELECT device_id,
       STRFTIME('%Y-%m-%d %H:00', timestamp) AS hour,
       AVG(value) AS avg_temp,
       MIN(value) AS min_temp,
       MAX(value) AS max_temp
FROM sensor_data
WHERE metric_name = 'temperature'
GROUP BY device_id, hour
ORDER BY device_id, hour DESC;

-- 异常检测：偏离均值 2 个标准差
WITH stats AS (
    SELECT device_id, metric_name,
           AVG(value) AS avg_val,
           AVG(value * value) - AVG(value) * AVG(value) AS variance
    FROM sensor_data
    GROUP BY device_id, metric_name
)
SELECT s.device_id, s.metric_name, s.value, s.timestamp,
       st.avg_val, SQRT(st.variance) AS stddev
FROM sensor_data s
JOIN stats st ON s.device_id = st.device_id AND s.metric_name = st.metric_name
WHERE ABS(s.value - st.avg_val) > 2 * SQRT(st.variance)
ORDER BY s.timestamp DESC;

-- 批量写入（高吞吐）
INSERT INTO sensor_data (device_id, metric_name, value) VALUES
    ('sensor-001', 'temperature', 22.5),
    ('sensor-001', 'humidity', 65.3),
    ('sensor-002', 'temperature', 23.1),
    ('sensor-002', 'humidity', 58.7);
```

---

## 相关文档

- [完全使用手册](USAGE.md) — 全部功能的综合参考
- [高阶 SQL 特性](ADVANCED_SQL.md) — 窗口函数、CTE、子查询深入指南
- [内置函数参考](FUNCTIONS.md) — 70+ 函数详细说明
- [HTTP REST API](HTTP_API.md) — HTTP 接口使用指南
- [部署指南](DEPLOYMENT.md) — 生产环境部署方案
