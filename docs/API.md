# KKDB API 文档

本文档描述 `kkdb` crate 的公开 API 与推荐调用方式。

---

## 1. 模块总览

`src/lib.rs` 暴露如下模块：

| 模块 | 说明 |
|------|------|
| `kkdb::vm::execute` | **推荐入口**：`VM` + `ExecResult` |
| `kkdb::error` | 统一错误类型 `KkdbError` |
| `kkdb::types` | 值类型系统 `Value` / `Row` |
| `kkdb::sql` | 词法器、解析器（sqlparser-rs 适配器）、AST |
| `kkdb::schema` | 表/索引/视图/触发器 元数据（偏底层）|
| `kkdb::storage` | Pager + B-Tree（偏底层）|
| `kkdb::fulltext` | BM25 分词器 + 倒排索引 |
| `kkdb::binlog` | Binlog 记录与管理器 |

---

## 2. 快速开始

### 内存数据库

```rust
use kkdb::vm::execute::{ExecResult, VM};

let mut vm = VM::new_memory();
vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")?;
vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")?;

if let ExecResult::QueryResult { columns, rows } =
    vm.execute_sql("SELECT id, name FROM t1")?
{
    println!("{:?}", columns); // ["id", "name"]
    println!("{:?}", rows);    // [[Integer(1), Text("Alice")]]
}
```

### 文件数据库（按表分文件）

```rust
use kkdb::vm::execute::VM;

// 首次打开：自动创建 mydb/ 目录
{
    let mut vm = VM::open("mydb")?;
    vm.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")?;
    vm.execute_sql("INSERT INTO users VALUES (1, 'Alice')")?;
    // VM 析构时自动刷盘（impl Drop）
}

// 重新打开：数据持久化，自动加载 schema
{
    let mut vm = VM::open("mydb")?;
    let result = vm.execute_sql("SELECT * FROM users")?;
    // ...
}
```

目录结构：

```
mydb/
  catalog.kkdb   ← Schema B-Tree（表定义 + 索引定义）
  users.kkdb     ← users 表数据 B-Tree
  orders.kkdb    ← orders 表数据 B-Tree（若存在）
  binlog.bin     ← Binlog（操作日志）
```

---

## 3. `vm::execute`

### `struct VM`

核心数据库执行器，负责解析并运行 SQL 语句。

#### 构造方法

| 方法 | 说明 |
|------|------|
| `VM::new_memory() -> VM` | 创建纯内存数据库（不落盘）|
| `VM::open(path: &str) -> Result<VM>` | 打开或创建文件库（目录模式）|

**`VM::open` 行为：**
- 若 `path` 为已有普通文件 → **报错**（旧单文件格式已不再支持）
- 若 `path` 为目录或不存在 → 创建目录，在其中创建 `catalog.kkdb` + 按需创建每个表的 `.kkdb` 文件

#### 主要字段

| 字段 | 说明 |
|------|------|
| `vm.schema` | 内存中的 Schema（表/索引/视图/触发器/策略）|
| `vm.session_vars` | 会话变量（`kkdb.current_user` 等 RLS 变量）|
| `vm.adaptive_threshold` | 自适应索引触发阈值（默认 5，0 禁用自适应）|
| `vm.query_access_counter` | 全表扫描计数器（按 table+col）|
| `vm.binlog` | Binlog 管理器 |

#### 执行方法

```rust
fn execute_sql(&mut self, sql: &str) -> Result<ExecResult>
```

一次执行单条 SQL 语句（可带结尾 `;`）。支持：

- DDL：`CREATE TABLE / DROP TABLE / ALTER TABLE / CREATE INDEX / DROP INDEX / CREATE VIEW / CREATE TRIGGER / CREATE FULLTEXT INDEX / CREATE POLICY / ANALYZE TABLE / VACUUM`
- DML：`INSERT / UPDATE / DELETE`（含 `RETURNING`）
- 查询：`SELECT`（含 JOIN / WHERE / GROUP BY / HAVING / ORDER BY / LIMIT / OFFSET / DISTINCT / WITH CTE / WITH RECURSIVE / UNION / EXCEPT / INTERSECT / 窗口函数 / 相关子查询 / EXPLAIN）
- 事务：`BEGIN / COMMIT / ROLLBACK / SAVEPOINT / RELEASE / ROLLBACK TO`
- 安全：`CREATE USER / ALTER USER / DROP USER / GRANT / REVOKE`
- 会话：`SET kkdb.key = 'value'`

#### 批量插入（跳过 SQL 解析）

```rust
/// 直接插入已求值的行，绕过 SQL 解析/AST 阶段，高性能批量写入
pub fn insert_batch_raw(
    &mut self,
    table_name: &str,
    value_rows: Vec<Vec<Value>>,
) -> Result<ExecResult>
```

#### 自动刷盘

VM 实现了 `Drop`，析构时自动将 catalog pager 与所有 table pager 的脏页刷入磁盘（best-effort）。在 auto-commit 模式下，每条 DML / DDL 语句也会触发 `auto_flush`。

---

### `enum ExecResult`

```rust
pub enum ExecResult {
    /// DDL、事务语句成功（包含消息字符串）
    Ok { message: String },
    /// INSERT / UPDATE / DELETE 完成：受影响行数
    RowsAffected { count: usize, message: String },
    /// SELECT 完成：列名 + 数据行
    QueryResult {
        columns: Vec<String>,
        rows:    Vec<Vec<Value>>,
    },
    /// EXPLAIN <stmt>：查询计划文本
    Explain { plan: String },
}
```

| 变体 | 触发场景 |
|------|---------|
| `Ok` | DDL、事务语句、SET |
| `RowsAffected` | INSERT / UPDATE / DELETE（无 RETURNING）|
| `QueryResult` | SELECT，以及带 RETURNING 的 DML |
| `Explain` | `EXPLAIN <stmt>` |

---

## 4. `error`

```rust
pub type Result<T> = std::result::Result<T, KkdbError>;

pub enum KkdbError {
    SyntaxError(String),          // 词法/语法错误
    ParseError(String),           // AST 转换错误
    TableNotFound(String),        // 表不存在
    TableAlreadyExists(String),   // 表已存在
    ColumnNotFound(String),       // 列不存在
    ColumnCountMismatch { expected: usize, got: usize },
    ConstraintViolation(String),  // NOT NULL / UNIQUE / FK / CHECK 失败
    TypeMismatch(String),
    RuntimeError(String),         // 运行时错误（含 RLS 权限拒绝）
    BTreeError(String),
    PageOutOfRange(u32),
    CorruptDatabase(String),
    DatabaseFull,
    Io(std::io::Error),
    Internal(String),             // 引擎内部断言失败
}
```

---

## 5. `types`

### `enum Value`

```rust
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(Rc<str>),    // 引用计数字符串，克隆廉价
    Blob(Vec<u8>),
}
```

常用方法：

| 方法 | 说明 |
|------|------|
| `Value::to_i64() -> Option<i64>` | 转整数（尽力转换）|
| `Value::to_f64() -> Option<f64>` | 转浮点 |
| `Value::is_truthy() -> bool` | SQL 真值（非 NULL、非 0、非空串）|
| `Value::type_name() -> &str` | 返回 `"integer"` / `"real"` / `"text"` / `"blob"` / `"null"` |

### `type Row = Vec<Value>`

序列化函数（内部使用 VarInt 编码）：

```rust
fn serialize_row(row: &Row) -> Vec<u8>
fn deserialize_row(data: &[u8]) -> Result<Row>
```

---

## 6. `sql`

子模块：

| 模块 | 说明 |
|------|------|
| `sql::ast` | `Statement` / `Expr` 等 AST 节点 |
| `sql::tokenizer` | `Tokenizer` / `Token` 词法器（保留，向后兼容）|
| `sql::parser` | `parse_sql` 入口（路由到 sqlparser-rs 适配器）|
| `sql::sqlparser_adapter` | sqlparser-rs 0.61 → KKDB AST 转换器（分文件模块化）|

```rust
use kkdb::sql::parser::parse_sql;
let stmt = parse_sql("SELECT 1 + 2")?;
```

**解析器特性：**
- 底层使用 `sqlparser-rs 0.61`，`SQLiteDialect`
- 单语句解析（多语句报错）
- `COUNT(*)` 自动 remapped 为 `COUNT(1)`
- `JOIN USING(col)` 自动展开为等值 ON 条件

---

## 7. `schema`

主要类型：

| 类型 | 说明 |
|------|------|
| `Schema` | 元数据管理器（内存缓存）|
| `TableSchema` | 表信息（列定义、root_page、next_rowid、triggers、policies）|
| `ColumnInfo` | 列信息（name/data_type/pk/autoincrement/not_null/unique/references）|
| `ColumnStats` | 列统计（total_count/null_count/ndv/min/max，由 ANALYZE 填充）|
| `IndexSchema` | 索引信息（name/table_name/root_page/unique/columns）|
| `TriggerSchema` | 触发器（name/timing/event/table_name/body_sql）|
| `PolicySchema` | RLS 策略（name/table/role/using_expr）|

主要方法：

```rust
// Schema 加载（从 catalog pager 读取）
Schema::load_from_pager(pager: &mut Pager) -> Result<()>

// DDL（需分别传 catalog_pager 和 table_pager）
Schema::create_table(
    catalog_pager: &mut Pager,
    table_pager: &mut Pager,
    name: &str,
    column_defs: &[ColumnDef],
    if_not_exists: bool,
    original_sql: &str,
    check_exprs: &[Expr],       // CHECK 约束
    is_fts: bool,               // 是否 FTS 辅助表
) -> Result<()>

Schema::create_index(
    catalog_pager: &mut Pager,
    table_pager: &mut Pager,
    index_name: &str,
    table_name: &str,
    columns: &[String],
    unique: bool,
    if_not_exists: bool,
    original_sql: &str,
) -> Result<()>

Schema::drop_table(pager: &mut Pager, name: &str, if_exists: bool) -> Result<()>

// 查询
Schema::get_table(name: &str) -> Result<&TableSchema>
Schema::get_table_mut(name: &str) -> Result<&mut TableSchema>
Schema::find_column(table: &str, col: &str) -> Result<usize>
Schema::indexes_for_table(table: &str) -> Vec<&IndexSchema>

// ALTER TABLE
Schema::alter_add_column(pager, table_name, col_def)
Schema::alter_drop_column(pager, table_name, col_name)
Schema::alter_rename_table(pager, old_name, new_name)
Schema::alter_rename_column(pager, table_name, old_col, new_col)

// 触发器
Schema::drop_trigger_by_name(pager, name, if_exists)

// RLS 策略
Schema::register_fts_index(name, table, columns, root_page)
```

> **注意**：`create_table` / `create_index` 需要两个 Pager 参数：
> - `catalog_pager`：schema B-Tree 所在的 pager
> - `table_pager`：表数据 B-Tree 所在的 pager（多文件模式下各自的 `.kkdb` 文件）
>
> 在单文件 / 内存模式下两者指向同一个 Pager（通过原始指针别名安全访问）。

---

## 8. `storage`

### `Pager`

页缓存与文件 I/O，V2 格式使用 COW 双超块保障原子刷盘。

| 方法 | 说明 |
|------|------|
| `Pager::open(path)` | 打开或创建文件 pager（V2 COW 格式）|
| `Pager::open_memory()` | 内存 pager（不落盘）|
| `get_page(page_num)` | 读取页（惰性加载）|
| `get_page_mut(page_num)` | 获取可写页（自动 COW 快照）|
| `allocate_page()` | 分配新页（高水位 + freelist）|
| `flush()` | 将脏页刷盘 |
| `begin_transaction()` | 开启 COW 事务快照 |
| `commit_transaction()` | 提交并双 fsync 刷盘 |
| `rollback_transaction()` | 回滚（恢复快照）|
| `savepoint(name)` | 创建保存点 |
| `release_savepoint(name)` | 释放保存点 |
| `rollback_to_savepoint(name)` | 回滚到保存点 |
| `schema_root_page()` | 获取 schema B-Tree 根页号 |
| `set_schema_root_page(page)` | 更新 schema 根页（COW 分裂后）|
| `active_txid()` | 当前活动事务 ID |
| `in_transaction()` | 是否在事务中 |

**V2 格式页布局：**

- Page 1, 2：COW 双超块（generation 奇偶轮换保障原子写）
- Page 3：Schema B-Tree 根页
- Page 4+：用户数据（表数据、索引）

### `BTree`

基于 Pager 的 B-Tree 操作。

| 方法 | 说明 |
|------|------|
| `create_table()` | 分配新 B-Tree 根页，返回页号 |
| `insert(root, rowid, row)` | 插入行，返回（可能新的）根页号 |
| `insert_with_buf(...)` | 带重用 buffer 的插入（高性能批量写）|
| `scan_all(root)` | 全表扫描，返回 `Vec<(rowid, Row)>` |
| `scan_rows(root)` | 全表扫描，仅返回 `Vec<Row>` |
| `scan_rows_limit(root, limit)` | 带 limit 的提前终止扫描 |
| `find_by_rowid(root, rowid)` | 按 rowid 点查 |
| `update_row(root, rowid, row)` | 原地更新行，返回新根页号 |
| `update_row_with_buf(...)` | 带重用 buffer 的更新 |
| `delete_by_rowid(root, rowid)` | 删除行，返回（已删除行, 新根页号）|
| `max_rowid(root)` | 返回最大 rowid（AUTOINCREMENT 用）|
| `count_rows(root)` | 统计行数 |

---

## 9. `fulltext`

BM25 全文检索子系统。

```rust
use kkdb::fulltext::tokenizer::simple_tokenize;

// 分词（Unicode 感知）
let tokens = simple_tokenize("Hello World 数据库");
// => ["hello", "world", "数据库"]（英文小写，CJK 视作字母数字）
```

> **中文使用建议**：CJK 字符被视为字母数字字符，不会被空格以外的字符分割。
> 建议在输入时手动用空格分隔中文词，例如 `"数据库 性能 优化"`。

BM25 评分公式：

```
score(d, q) = Σ_t IDF(t) × TF(t,d) × (k1+1) / (TF(t,d) + k1 × (1 - b + b × dl/avgdl))
```

默认参数：`k1 = 1.2`，`b = 0.75`

---

## 10. 服务器模式

### 启动选项

```bash
# 启动所有协议服务器
cargo run -- mydb --server
  --port 3306          # 原生 TCP 协议（KKDB 自有协议）
  --http-port 6543     # HTTP REST API（Supabase 风格）
  --mysql-port 3307    # MySQL 有线协议（兼容 DBeaver/mysql2）
```

### HTTP REST API（完整端点列表）

认证方式：`Authorization: Bearer <JWT>` 或 `X-API-Key: <key>`。

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/health` | 健康检测 → `{"status":"ok","engine":"kkdb"}` |
| POST | `/auth/signup` | 注册用户，返回 JWT |
| POST | `/auth/signin` | 登录，返回 JWT |
| POST | `/auth/refresh` | 刷新 JWT（现有 token 仍有效时可调用） |
| POST | `/auth/apikeys` | 创建 API 密钥（返回密钥原文，只显示一次） |
| POST | `/rest/query` | 执行单条 SQL 语句（也可用 `/rest/execute` 或 `/rest/sql`） |
| POST | `/rest/batch` | **批量执行**：一次调用运行多条语句，可选事务模式 |
| POST | `/rest/bulk` | **批量写入**：将 JSON 行数组高效插入单张表 |

#### POST /auth/signup

```json
{"email": "alice@example.com", "password": "secret"}
```

响应：`{"user_id": "uuid", "email": "...", "token": "eyJ..."}`

#### POST /auth/signin

```json
{"email": "alice@example.com", "password": "secret"}
```

响应：同 signup。登录时会自动更新 `mysql_auth_hash`（MySQL 客户端连接需要）。

#### POST /auth/refresh

请求头：`Authorization: Bearer <旧 token>` 即可，无请求体。  
响应：`{"user_id": "...", "email": "...", "token": "<新 token>"}`

#### POST /auth/apikeys

请求头：`Authorization: Bearer <JWT>`，无请求体。  
响应：`{"key": "kkdb_xxxxxxxxxxxxxxxx", "key_id": "uuid"}`

> **注意**：原始 API 密钥只返回一次，请立即保存。后续请求使用 `X-API-Key: kkdb_xxx`。

#### POST /rest/query（= /rest/execute = /rest/sql）

```json
{"sql": "SELECT * FROM orders WHERE user_id = 1"}
```

响应：
```json
{"columns": ["id", "amount"], "rows": [[1, 100.0]]}
```

- 写操作（INSERT/UPDATE/DELETE/DDL）在集群模式下自动路由到 Leader。
- 读操作在 Follower 上执行（linearizable 模式：先等待 ReadIndex fence）。
- JWT 的 `sub` 自动注入为 `request.jwt.sub` 会话变量，供 RLS 使用。

#### POST /rest/batch

一次调用执行多条 SQL 语句：

```json
{
  "statements": [
    "INSERT INTO orders (user_id, amount) VALUES (1, 100)",
    "UPDATE stats SET total = total + 100 WHERE user_id = 1"
  ],
  "transaction": true
}
```

响应：
```json
{
  "results": [
    {"status": "ok", "statement": "...", "columns": ["message"], "rows": [...], "rows_affected": 1},
    {"status": "ok", "statement": "...", "columns": ["message"], "rows": [...], "rows_affected": 1}
  ],
  "count": 2,
  "succeeded": 2,
  "transaction": true,
  "failed_at": null
}
```

- `transaction: true`：自动包裹 BEGIN/COMMIT，任意语句失败则整体 ROLLBACK。
- `failed_at`：若事务失败，记录首个出错语句的 0-based 索引。

#### POST /rest/bulk

高效批量插入同一张表中的多行：

```json
{
  "table": "orders",
  "rows": [
    {"user_id": 1, "amount": 100.0},
    {"user_id": 2, "amount": 200.0}
  ],
  "transaction": true,
  "bulk_insert": true
}
```

响应：
```json
{"table": "orders", "rows_written": 2, "transaction": true, "error": null}
```

- `bulk_insert: true`（默认）：生成单条多值 `INSERT INTO t VALUES (r1),(r2),...` 语句，比逐行插入快。
- `bulk_insert: false`：每行一条独立 INSERT，共用一个事务。
- 列顺序由第一行的键决定，所有行须具有相同的键集。

#### 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `KKDB_JWT_SECRET` | JWT 签名密钥（生产环境必须设置） | 内置默认值（不安全）|
| `KKDB_JWT_EXPIRY` | JWT 有效期（秒）| `3600`（1 小时）|
```

### MySQL 有线协议

兼容标准 MySQL 客户端，可直接使用 DBeaver、DataGrip、`mysql` 命令行等工具连接：

```bash
mysql -h 127.0.0.1 -P 3307 -u root
```

### Raft 集群模式

```bash
# 节点 1（Leader 候选）
cargo run -- --server --node-id 1 \
  --raft-addr 127.0.0.1:7001 \
  --peers "2=127.0.0.1:7002,3=127.0.0.1:7003" \
  --data-dir ./node1

# 节点 2
cargo run -- --server --node-id 2 \
  --raft-addr 127.0.0.1:7002 \
  --peers "1=127.0.0.1:7001,3=127.0.0.1:7003" \
  --data-dir ./node2
```

- 底层使用 `openraft`
- 写操作由 Leader 处理并通过 Raft WAL 同步到所有 Follower
- Leader 失效后自动重新选举（无需人工干预）

---

## 11. 分层使用建议

```
┌────────────────────────────────────────────────────────┐
│ 应用层     VM + ExecResult + KkdbError                 │  ← 推荐
├────────────────────────────────────────────────────────┤
│ 工具层     sql::parser（仅解析 SQL）                    │  ← 按需
│            fulltext::tokenizer（文本分词）              │  ← 按需
├────────────────────────────────────────────────────────┤
│ 内核扩展   schema / storage（与内部耦合较高）           │  ← 谨慎
└────────────────────────────────────────────────────────┘
```

---

## 12. 版本与兼容性

当前版本：`0.1.0`

- `VM::execute_sql` 接口稳定，是最安全的调用边界。
- `schema::create_table` / `create_index` 签名要求两个 Pager 参数（catalog + table），旧单文件代码需同步更新。
- `storage` 层为 V2 COW 格式，旧 V1 单文件格式已不再支持。
- sqlparser-rs 适配器取代了旧版手写解析器，语法覆盖更广（`sqlparser 0.61`，PostgreSQL 风格方言）。
- 全文检索、RLS、触发器、外键、窗口函数均在 v0.1.0 中实现，API 可能随版本演进。
