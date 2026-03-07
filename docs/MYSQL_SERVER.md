# KKDB MySQL 协议服务器文档

KKDB 内置一个完整的 **MySQL 有线协议（Wire Protocol）** 服务器，允许任何标准 MySQL 客户端（DBeaver、Navicat、DataGrip、`mysql` CLI、JDBC、mysql2、mysqlclient、Sequel Pro 等）无需任何修改直接连接 KKDB。

---

## 目录

1. [启动 MySQL 服务器](#1-启动-mysql-服务器)
2. [认证机制](#2-认证机制)
3. [连接示例](#3-连接示例)
4. [支持的命令](#4-支持的命令)
5. [兼容性处理](#5-兼容性处理)
6. [用户管理](#6-用户管理)
7. [多租户数据隔离](#7-多租户数据隔离)
8. [RLS 与 MySQL 连接](#8-rls-与-mysql-连接)
9. [已知限制](#9-已知限制)

---

## 1. 启动 MySQL 服务器

```bash
# MySQL 协议监听 3307 端口（避免与本地 MySQL 冲突）
cargo run -- --server --mysql-port 3307 --data-dir ./data

# 同时开启 HTTP API + MySQL 协议
cargo run -- --server \
  --http-port 6543 \
  --mysql-port 3307 \
  --data-dir ./data
```

服务就绪后输出：

```
[MySQL] Listening on 0.0.0.0:3307
```

---

## 2. 认证机制

### 协议握手流程

KKDB 实现了完整的 MySQL Protocol 4.1 握手：

```
客户端          KKDB MySQL 服务器
  |                    |
  |←── Handshake v10 ──|  (版本: 8.0.33-kkdb, 20字节 scramble, 能力标志)
  |                    |
  |── HandshakeResponse→|  (用户名, auth_response, 可选数据库名)
  |                    |
  |←── OK / ERR ───────|  (认证成功或失败)
  |                    |
  |── COM_QUERY ───────→|  (执行 SQL)
  |←── ResultSet ───────|  (列定义 + 数据行)
```

### 密码认证

使用 **`mysql_native_password`** 插件（标准 SHA1 挑战-响应）：

```
client_auth = SHA1(password) XOR SHA1(scramble ‖ SHA1(SHA1(password)))
```

服务端存储 `hex(SHA1(SHA1(password)))` ——仅存双重哈希，无法还原明文。

### 账号来源

MySQL 协议账号与 HTTP API 账号**共享同一套用户体系**：

1. 通过 HTTP API `/auth/signup` 注册 → 同时计算 bcrypt 哈希（HTTP 用）和 `SHA1(SHA1(password))`（MySQL 用）
2. 通过 HTTP API `/auth/signin` 登录 → 同步更新 `mysql_auth_hash`（懒迁移）

> 首次通过 HTTP 登录后，该账号才能用于 MySQL 协议连接。直接跳过 HTTP 登录只注册的账号也可以立即使用 MySQL 协议（注册时已计算 MySQL 哈希）。

---

## 3. 连接示例

### mysql CLI

```bash
mysql -h 127.0.0.1 -P 3307 -u alice@example.com -p
# 输入密码后回车
```

### DBeaver / DataGrip / Navicat

| 字段 | 值 |
|------|-----|
| 连接类型 | MySQL |
| 服务器 | `127.0.0.1` |
| 端口 | `3307` |
| 用户名 | 注册时的 email（如 `alice@example.com`）|
| 密码 | 注册时的密码 |
| 数据库 | 可留空 |

### JDBC (Java)

```java
String url = "jdbc:mysql://127.0.0.1:3307/?user=alice%40example.com&password=secret";
Connection conn = DriverManager.getConnection(url);
```

### Node.js (mysql2)

```javascript
import mysql from 'mysql2/promise';

const conn = await mysql.createConnection({
  host: '127.0.0.1',
  port: 3307,
  user: 'alice@example.com',
  password: 'secret123',
});

const [rows] = await conn.execute('SELECT * FROM notes WHERE id = ?', [1]);
console.log(rows);

await conn.end();
```

### Python (mysql-connector-python)

```python
import mysql.connector

conn = mysql.connector.connect(
    host='127.0.0.1',
    port=3307,
    user='alice@example.com',
    password='secret123',
)
cursor = conn.cursor()
cursor.execute('SELECT id, content FROM notes ORDER BY id LIMIT 10')
for row in cursor:
    print(row)
conn.close()
```

### Rust (sqlx)

```rust
use sqlx::mysql::MySqlPoolOptions;

let pool = MySqlPoolOptions::new()
    .max_connections(5)
    .connect("mysql://alice%40example.com:secret123@127.0.0.1:3307/")
    .await?;

let rows = sqlx::query!("SELECT id, content FROM notes")
    .fetch_all(&pool)
    .await?;
```

---

## 4. 支持的命令

| COM 字节 | 命令 | 说明 |
|---------|------|------|
| `0x03` | `COM_QUERY` | 执行任意 SQL 语句（主要命令）|
| `0x0e` | `COM_PING` | 保活心跳（`mysql -e "SELECT 1"` 用）|
| `0x01` | `COM_QUIT` | 断开连接 |
| `0x02` | `COM_INIT_DB` | `USE database` 语句 |

所有 `COM_QUERY` 语句均传递给该用户的 KKDB VM 执行，支持：

- 完整 DDL / DML / SELECT（见 [ADVANCED_SQL.md](ADVANCED_SQL.md)）
- `BEGIN` / `COMMIT` / `ROLLBACK` / `SAVEPOINT`
- 所有 KKDB 扩展语法（FTS、RLS、触发器等）

---

## 5. 兼容性处理

MySQL 客户端连接时会自动发送一系列内省查询，KKDB 对这些查询在协议层进行拦截并返回静态兼容响应，**无需 KKDB 内核支持这些特定的 MySQL 语法**：

| 拦截的查询 / 命令 | 返回 |
|------------------|------|
| `SET @@session.*` | OK（静默接受所有 SET 会话变量）|
| `SELECT @@version` | `8.0.33-kkdb` |
| `SELECT @@max_allowed_packet` | `67108864` |
| `SELECT @@sql_mode` | `ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES` |
| `SELECT @@autocommit` | `1` |
| `SELECT @@transaction_isolation` | `REPEATABLE-READ` |
| `SELECT @@character_set_server` | `utf8mb4` |
| `SELECT @@collation_server` | `utf8mb4_general_ci` |
| `SELECT VERSION()` | `8.0.33-kkdb` |
| `SELECT DATABASE()` | `kkdb` |
| `SELECT 1` | `1` |
| `SHOW DATABASES` | `kkdb`, `information_schema` |
| `SHOW VARIABLES` | max_allowed_packet 等基础变量 |
| `SHOW COLLATION` | `utf8mb4_general_ci` |
| `SHOW TABLES` | 空表（DBeaver 连接时用）|
| `SHOW TABLE STATUS` | 空集 |
| `DO 1` | OK |

> 结果：DBeaver、DataGrip、Navicat、mysql CLI 均可开箱即用，不需要任何额外配置。

---

## 6. 用户管理

### 创建用户

通过 HTTP API 注册（推荐，同时创建完整账号）：

```bash
curl -X POST http://localhost:6543/auth/signup \
  -H "Content-Type: application/json" \
  -d '{"email":"dev@company.com","password":"devpass123"}'
```

之后即可用 MySQL 协议连接：

```bash
mysql -h 127.0.0.1 -P 3307 -u dev@company.com -p
```

### root / admin 账号

- 首次启动时若 `kkdb_auth_users` 表为空，**不允许任何用户连接**（安全设计）
- 要创建初始 root 用户，使用 HTTP API signup 端点（email 填 `root`，密码自定义）

### 密码修改

```bash
# 通过 HTTP API 修改密码（登录后重新颁发 mysql_auth_hash）
curl -X POST http://localhost:6543/auth/signin \
  -H "Content-Type: application/json" \
  -d '{"email":"root","password":"new-password"}'
```

---

## 7. 多租户数据隔离

MySQL 协议连接后，SQL 语句在该**登录用户的私有 VM** 中执行：

```
mysql -u alice@example.com -p   →  执行在 data/alice@example.com/ 数据库
mysql -u bob@example.com   -p   →  执行在 data/bob@example.com/ 数据库
mysql -u root              -p   →  执行在全局 auth 数据库
```

用户之间完全隔离：
- 每个用户的 `CREATE TABLE` 只在自己的数据库中可见
- 用户数据库在首次 SQL 执行时按需创建，无需预初始化
- `root` / `admin` 用户使用全局 auth VM（包含 `kkdb_auth_users` 等系统表）

---

## 8. RLS 与 MySQL 连接

认证成功后，服务器自动向用户 VM 注入会话变量（在首次 `COM_QUERY` 前）：

```sql
SET kkdb.current_user = '<email>';
SET request.jwt.sub   = '<user_uuid>';
```

因此 RLS 策略在 MySQL 连接中同样生效：

```sql
-- RLS 策略（按 email 过滤）
CREATE POLICY own_notes ON notes
  USING (author = (SELECT current_user_email()));  -- 自定义函数或直接用 session_vars

-- 通过 MySQL DBeaver 连接 alice@example.com 后查询
SELECT * FROM notes;  -- 自动只返回 alice 的数据
```

---

## 9. 已知限制

| 限制 | 说明 |
|------|------|
| 预处理语句 (`COM_STMT_*`) | 暂不支持；请使用文本协议 (`COM_QUERY`) |
| 游标 / 流式结果集 | 暂不支持（结果集全量返回后发送）|
| `LOAD DATA INFILE` | 暂不支持；使用 HTTP `/rest/bulk` 代替 |
| `INFORMATION_SCHEMA` 查询 | 部分 DBeaver 高级功能（如自动补全表/列）依赖 `INFORMATION_SCHEMA`，目前仅返回空集 |
| 连接池 keep-alive | 支持 `COM_PING`（心跳保活），可正常使用连接池 |
| TLS/SSL | 暂不支持；建议在内网或 VPN 环境使用 |
| 字符集 | 服务端统一声明 `utf8mb4`，客户端字符集协商静默接受 |
| 并发连接 | 每连接一个独立 tokio task，连接数无硬限制（受系统文件描述符限制）|

---

## 协议包格式参考

KKDB 实现了标准 MySQL 4.1 文本协议：

```
[3 bytes LE: payload_len][1 byte: seq_id][payload_bytes...]
```

结果集编码：

```
ColCount (lenenc_int)
→ ColDef × N (def / schema / table / name / ...)
→ EOF
→ Row × M (每列为 lenenc_string 或 0xFB = NULL)
→ EOF
```

服务端能力标志（Capability Flags）：

| 标志 | 值 | 说明 |
|------|----|------|
| `CLIENT_PROTOCOL_41` | `0x200` | Protocol 4.1 |
| `CLIENT_SECURE_CONNECTION` | `0x8000` | 安全连接 |
| `CLIENT_PLUGIN_AUTH` | `0x80000` | 插件认证 |
| `CLIENT_CONNECT_WITH_DB` | `0x8` | 连接时选择数据库 |
| `CLIENT_TRANSACTIONS` | `0x2000` | 事务状态标志 |
