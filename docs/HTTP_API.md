# KKDB HTTP REST API 文档

KKDB 内置一个 Supabase 风格的 HTTP REST API 服务器，基于 **axum** 框架构建，支持 JWT 认证、API Key、多租户数据库隔离和 Raft 集群写路由。

---

## 目录

1. [启动 HTTP 服务器](#1-启动-http-服务器)
2. [多租户架构](#2-多租户架构)
3. [认证方式](#3-认证方式)
4. [端点参考](#4-端点参考)
   - [GET /health](#get-health)
   - [POST /auth/signup](#post-authsignup)
   - [POST /auth/signin](#post-authsignin)
   - [POST /auth/refresh](#post-authrefresh)
   - [POST /auth/apikeys](#post-authapikeys)
   - [POST /rest/query](#post-restquery)
   - [POST /rest/execute](#post-restexecute)
   - [POST /rest/batch](#post-restbatch)
   - [POST /rest/bulk](#post-restbulk)
5. [错误响应格式](#5-错误响应格式)
6. [Raft 集群模式](#6-raft-集群模式)
7. [环境变量配置](#7-环境变量配置)
8. [行级安全（RLS）与 HTTP](#8-rls-与-http)
9. [完整示例：前后端集成](#9-完整示例)

---

## 1. 启动 HTTP 服务器

```bash
# 单机模式，数据持久化到 ./data 目录
cargo run -- --server --http-port 6543 --data-dir ./data

# 仅内存（测试用，重启后数据丢失）
cargo run -- --server --http-port 6543
```

服务就绪后输出：

```
[KKDB] HTTP API listening on http://0.0.0.0:6543
```

---

## 2. 多租户架构

每个注册用户拥有一个**完全隔离的数据库**：

```
data/
  _auth/          ← 全局认证数据库（kkdb_auth_users、kkdb_api_keys）
    catalog.kkdb
  <user_id_1>/    ← 用户 1 的私有数据库
    catalog.kkdb
    mytable.kkdb
    binlog.bin
  <user_id_2>/    ← 用户 2 的私有数据库
    catalog.kkdb
    ...
```

- **`_auth`** 数据库：全局唯一，存储用户账号和 API Key，采用 bcrypt 哈希密码
- **用户数据库**：每个用户有独立的 `data/<user_id>/` 目录，首次查询时按需创建
- **`root` / `admin` 用户**：映射到 `auth_vm`（全局数据库）
- 用户 ID 格式限制：仅允许字母数字、`-`、`_`、`@`、`.`，不允许 `..` 路径穿透

---

## 3. 认证方式

HTTP API 支持两种认证方式，二选一：

### 3.1 JWT Bearer Token

由 `/auth/signup` 或 `/auth/signin` 颁发，有效期默认 1 小时。

```http
Authorization: Bearer <jwt_token>
```

JWT Payload 结构：

```json
{
  "sub": "3f8a1c2d-...",   // user_id (UUID)
  "email": "alice@example.com",
  "role": "authenticated",
  "exp": 1741234567
}
```

### 3.2 API Key

由 `/auth/apikeys` 创建，适合后端服务调用：

```http
X-API-Key: kkdb_a1b2c3d4e5f6...
```

API Key 以 bcrypt 哈希存储（cost=4），每次验证需扫描全部 key。建议 key 数量不超过 1000 个。

---

## 4. 端点参考

### GET /health

健康检查，无需认证。

**响应：**

```json
{ "status": "ok", "engine": "kkdb" }
```

---

### POST /auth/signup

注册新用户。

**请求：**

```json
{
  "email": "alice@example.com",
  "password": "my-secure-password"
}
```

**响应 200：**

```json
{
  "user_id": "3f8a1c2d-...",
  "email": "alice@example.com",
  "token": "eyJhbGci..."
}
```

**错误：**

| HTTP | 说明 |
|------|------|
| 400 | email 或 password 为空 |
| 409 | 该 email 已注册 |

**注意：** 注册时同时计算 bcrypt 密码哈希（HTTP 认证用）和 `SHA1(SHA1(password))` 哈希（MySQL 协议认证用），两种协议共享同一账号体系。

---

### POST /auth/signin

登录并获取 JWT Token。

**请求：**

```json
{
  "email": "alice@example.com",
  "password": "my-secure-password"
}
```

**响应 200：**

```json
{
  "user_id": "3f8a1c2d-...",
  "email": "alice@example.com",
  "token": "eyJhbGci..."
}
```

**错误：**

| HTTP | 说明 |
|------|------|
| 401 | 邮箱不存在或密码错误 |

---

### POST /auth/refresh

使用当前 JWT 换取新 Token（续期），即使 Token 接近过期也可使用。

**请求头：**

```http
Authorization: Bearer <current_token>
```

**请求体：** 空

**响应 200：** 同 `/auth/signin`

---

### POST /auth/apikeys

为当前登录用户创建一个 API Key。

**请求头：**

```http
Authorization: Bearer <jwt_token>
```

**请求体：** 空

**响应 200：**

```json
{
  "key": "kkdb_a1b2c3d4e5f6...",
  "key_id": "7c9f2e1a-..."
}
```

> ⚠️ **key 只返回一次，请立即保存。** 服务端仅存储 bcrypt 哈希，无法恢复明文。

---

### POST /rest/query

执行单条 SQL 语句（SELECT / DDL / DML 均可）。写操作在集群模式下自动路由到 Leader。

**请求头：**

```http
Authorization: Bearer <jwt_token>
# 或
X-API-Key: kkdb_...
```

**请求体：**

```json
{
  "sql": "SELECT id, name FROM users WHERE active = 1 ORDER BY name LIMIT 20"
}
```

**响应 200（SELECT）：**

```json
{
  "columns": ["id", "name"],
  "rows": [
    [1, "Alice"],
    [2, "Bob"]
  ]
}
```

**响应 200（DML / DDL）：**

```json
{
  "columns": ["message"],
  "rows": [["1 row(s) inserted"]]
}
```

**值类型映射：**

| KKDB 类型 | JSON 类型 |
|---------|---------|
| `NULL` | `null` |
| `Integer(i64)` | `number` |
| `Real(f64)` | `number` |
| `Text(str)` | `string` |
| `Blob(bytes)` | `string`（Base64 编码）|

**读写分类规则：**

- 以 `SELECT`、`EXPLAIN`、`SHOW`、`ANALYZE`、`DESCRIBE` 开头 → 读操作（不路由 Raft）
- 其余 → 写操作（集群模式路由到 Leader）
- `WITH ... SELECT` 被识别为读；`WITH ... INSERT` 被识别为写

---

### POST /rest/execute

与 `/rest/query` 完全相同，路由到同一处理函数。别名，方便语义区分。

---

### POST /rest/batch

在单次 HTTP 调用中执行多条 SQL，可选包裹在事务中。

**请求体：**

```json
{
  "statements": [
    "CREATE TABLE IF NOT EXISTS logs (id INTEGER PRIMARY KEY, msg TEXT)",
    "INSERT INTO logs (msg) VALUES ('hello')",
    "SELECT COUNT(*) FROM logs"
  ],
  "transaction": true
}
```

- `transaction: true`：自动 `BEGIN` / `COMMIT`；任一语句失败则 `ROLLBACK`
- `transaction: false`（默认）：逐条执行，失败继续执行后续语句

**响应 200：**

```json
{
  "results": [
    {
      "status": "ok",
      "statement": "CREATE TABLE ...",
      "columns": ["message"],
      "rows": [["Table 'logs' created"]],
      "rows_affected": null
    },
    {
      "status": "ok",
      "statement": "INSERT INTO ...",
      "columns": [],
      "rows": [],
      "rows_affected": 1
    },
    {
      "status": "ok",
      "statement": "SELECT COUNT(*) ...",
      "columns": ["COUNT(*)"],
      "rows": [[1]],
      "rows_affected": null
    }
  ],
  "count": 3,
  "succeeded": 3,
  "transaction": true,
  "failed_at": null
}
```

如果开启事务且某条语句失败：

```json
{
  "results": [
    { "status": "ok", "statement": "...", ... },
    { "status": "error", "statement": "...", "error": "ConstraintViolation: ..." }
  ],
  "count": 2,
  "succeeded": 1,
  "transaction": true,
  "failed_at": 1
}
```

---

### POST /rest/bulk

高效批量写入单张表。自动构造多行 `INSERT VALUES (r1),(r2),...`。

**请求体：**

```json
{
  "table": "events",
  "rows": [
    { "user_id": 1, "action": "login",  "ts": "2026-03-08T00:00:00Z" },
    { "user_id": 2, "action": "logout", "ts": "2026-03-08T00:01:00Z" },
    { "user_id": 1, "action": "click",  "ts": "2026-03-08T00:02:00Z" }
  ],
  "transaction": true,
  "bulk_insert": true
}
```

- `transaction`（默认 `true`）：将全部行包裹在一个事务中
- `bulk_insert`（默认 `true`）：拼接为单条多值 INSERT；`false` 则逐行 INSERT

**列顺序说明：** 使用第一行的键作为列列表，所有行必须拥有相同的键。

**响应 200：**

```json
{
  "table": "events",
  "rows_written": 3,
  "transaction": true,
  "error": null
}
```

如果出错：

```json
{
  "table": "events",
  "rows_written": 0,
  "transaction": true,
  "error": "TableNotFound: events"
}
```

---

## 5. 错误响应格式

所有错误均返回以下格式的 JSON：

```json
{ "error": "错误描述字符串" }
```

常见 HTTP 状态码：

| 状态码 | 场景 |
|-------|------|
| 400 | SQL 语法错误、约束违反、参数缺失 |
| 401 | 未提供认证信息或认证失败 |
| 403 | RLS 策略拒绝 |
| 409 | 用户已存在（注册冲突）|
| 503 | 集群无 Leader 可用 |
| 500 | 引擎内部错误 |

---

## 6. Raft 集群模式

在集群部署中，写请求自动路由：

```
              ┌─────────────────────────────────────────┐
              │           客户端                          │
              └─────────────┬───────────────────────────┘
                            │ POST /rest/query (写操作)
                 ┌──────────▼──────────┐
                 │   Follower 节点 2   │
                 │  is_leader() = false│
                 └──────────┬──────────┘
                            │ HTTP 透明转发 (X-Raft-Forward: 1)
                 ┌──────────▼──────────┐
                 │   Leader 节点 1     │
                 │  client_write()     │
                 │  → Raft 日志复制    │
                 └─────────────────────┘
```

- **写操作**（INSERT/UPDATE/DELETE/DDL 等）：若当前节点为 Follower，透明代理到 Leader，客户端无感知
- **读操作**（SELECT）：Follower 先 `ensure_linearizable()`（ReadIndex 机制），确保已应用所有 committed 日志后本地执行
- 若 Leader 未知（选举中）：返回 `503 Service Unavailable`

启动集群：

```bash
# 节点 1 (Leader 候选)
cargo run -- --server \
  --http-port 6543 \
  --node-id 1 \
  --raft-addr 127.0.0.1:7001 \
  --rest-addr http://127.0.0.1:6543 \
  --peers "2=127.0.0.1:7002,3=127.0.0.1:7003" \
  --peer-rest-addrs "2=http://127.0.0.1:6544,3=http://127.0.0.1:6545" \
  --data-dir ./node1
```

---

## 7. 环境变量配置

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `KKDB_JWT_SECRET` | `kkdb-super-secret-jwt-key-change-in-production` | JWT 签名密钥（**生产环境必须设置！**）|
| `KKDB_JWT_EXPIRY` | `3600` | JWT 有效期（秒）|

> ⚠️ **安全警告**：生产环境必须设置 `KKDB_JWT_SECRET` 为一个随机的 256-bit 字符串：
> ```bash
> export KKDB_JWT_SECRET=$(openssl rand -hex 32)
> ```

---

## 8. RLS 与 HTTP

每次认证成功后，服务器自动向该用户的 VM 注入会话变量：

```
kkdb.current_user   = <user_id>
request.jwt.sub     = <user_id>
request.jwt.email   = <email>
request.jwt.role    = "authenticated"
```

RLS 策略可以直接引用这些变量：

```sql
-- 创建只允许用户查看自己数据的策略
ALTER TABLE messages ENABLE ROW LEVEL SECURITY;
CREATE POLICY my_messages ON messages
  USING (sender_id = (SELECT id FROM users
                      WHERE email = (SELECT value FROM session_vars
                                     WHERE key = 'request.jwt.email')));
```

---

## 9. 完整示例

### 使用 curl

```bash
# 注册
curl -X POST http://localhost:6543/auth/signup \
  -H "Content-Type: application/json" \
  -d '{"email":"alice@example.com","password":"secret123"}'

# 登录（获取 Token）
TOKEN=$(curl -s -X POST http://localhost:6543/auth/signin \
  -H "Content-Type: application/json" \
  -d '{"email":"alice@example.com","password":"secret123"}' \
  | jq -r .token)

# 建表
curl -X POST http://localhost:6543/rest/query \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"sql":"CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, content TEXT, created_at TEXT)"}'

# 插入
curl -X POST http://localhost:6543/rest/bulk \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "table": "notes",
    "rows": [
      {"id": 1, "content": "Hello KKDB", "created_at": "2026-03-08"},
      {"id": 2, "content": "REST API rocks", "created_at": "2026-03-08"}
    ]
  }'

# 查询
curl -X POST http://localhost:6543/rest/query \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"sql":"SELECT * FROM notes ORDER BY id"}'
```

### 使用 JavaScript (fetch)

```javascript
const BASE = 'http://localhost:6543';

// 登录
const auth = await fetch(`${BASE}/auth/signin`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ email: 'alice@example.com', password: 'secret123' })
}).then(r => r.json());

const token = auth.token;
const headers = {
  'Authorization': `Bearer ${token}`,
  'Content-Type': 'application/json'
};

// SELECT 查询
const result = await fetch(`${BASE}/rest/query`, {
  method: 'POST',
  headers,
  body: JSON.stringify({ sql: 'SELECT * FROM notes' })
}).then(r => r.json());

console.log(result.columns); // ["id", "content", "created_at"]
console.log(result.rows);    // [[1, "Hello KKDB", "2026-03-08"], ...]

// 批量事务
const batch = await fetch(`${BASE}/rest/batch`, {
  method: 'POST',
  headers,
  body: JSON.stringify({
    statements: [
      "BEGIN",
      "UPDATE notes SET content = 'Updated' WHERE id = 1",
      "INSERT INTO notes VALUES (3, 'New note', '2026-03-08')",
      "COMMIT"
    ],
    transaction: false  // 手动 BEGIN/COMMIT
  })
}).then(r => r.json());
```

---

## 相关文档

- [完全使用手册](USAGE.md) — 全部功能的综合参考
- [MySQL 协议服务器](MYSQL_SERVER.md) — MySQL Wire Protocol 接口
- [Rust API 参考](API.md) — 嵌入式 Rust 接口
- [分布式集群](DISTRIBUTED.md) — Raft HTTP 端点与集群管理
- [高阶 SQL 特性](ADVANCED_SQL.md) — SQL 语法深入指南
