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
| `kkdb::sql` | 词法器、解析器、AST |
| `kkdb::schema` | 表/索引元数据（偏底层） |
| `kkdb::storage` | Pager + B-Tree（偏底层） |

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
    println!("{:?}", columns);
    println!("{:?}", rows);
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
  binlog.bin     ← Binlog
```

---

## 3. `vm::execute`

### `struct VM`

核心数据库执行器，负责解析并运行 SQL 语句。

#### 构造方法

| 方法 | 说明 |
|------|------|
| `VM::new_memory() -> VM` | 创建纯内存数据库（不落盘） |
| `VM::open(path: &str) -> Result<VM>` | 打开或创建文件库（目录模式） |
| `VM::open_legacy(path: &str) -> Result<VM>` | 打开旧版单文件格式（向后兼容） |

**`VM::open` 行为：**
- 若 `path` 为已有普通文件 → 等价 `open_legacy`（兼容旧格式）
- 若 `path` 为目录或不存在 → 创建目录，在其中创建 `catalog.kkdb` + 按需创建每个表的 `.kkdb` 文件

#### 执行方法

```rust
fn execute_sql(&mut self, sql: &str) -> Result<ExecResult>
```

一次执行单条 SQL 语句（可带结尾 `;`）。支持：

- DDL：`CREATE TABLE / DROP TABLE / ALTER TABLE / CREATE INDEX / DROP INDEX / CREATE VIEW`
- DML：`INSERT / UPDATE / DELETE`
- 查询：`SELECT`（含 `JOIN / WHERE / GROUP BY / HAVING / ORDER BY / LIMIT / OFFSET / DISTINCT / EXPLAIN`）
- 事务：`BEGIN / COMMIT / ROLLBACK / SAVEPOINT / RELEASE / ROLLBACK TO`

#### 自动刷盘

VM 实现了 `Drop`，析构时自动将 catalog pager 与所有 table pager 的脏页刷入磁盘（best-effort）。在 auto-commit 模式下，每条 DML / DDL 语句也会触发 `auto_flush`。

---

### `enum ExecResult`

```rust
pub enum ExecResult {
    Ok { message: String },
    RowsAffected { count: usize, message: String },
    QueryResult { columns: Vec<String>, rows: Vec<Vec<Value>> },
    Explain { plan: String },
}
```

| 变体 | 触发场景 |
|------|---------|
| `Ok` | DDL、事务语句 |
| `RowsAffected` | INSERT / UPDATE / DELETE |
| `QueryResult` | SELECT |
| `Explain` | EXPLAIN \<stmt\> |

---

## 4. `error`

```rust
pub type Result<T> = std::result::Result<T, KkdbError>;

pub enum KkdbError {
    SyntaxError(String),
    ParseError(String),
    TableNotFound(String),
    TableAlreadyExists(String),
    ColumnNotFound(String),
    ColumnCountMismatch { expected: usize, got: usize },
    ConstraintViolation(String),
    TypeMismatch(String),
    RuntimeError(String),
    BTreeError(String),
    PageOutOfRange(u32),
    CorruptDatabase(String),
    DatabaseFull,
    Io(std::io::Error),
    Internal(String),
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
    Text(Rc<str>),
    Blob(Vec<u8>),
}
```

常用方法：

| 方法 | 说明 |
|------|------|
| `Value::to_i64()` | 转整数（尽力转换） |
| `Value::to_f64()` | 转浮点 |
| `Value::is_truthy()` | SQL 真值判断 |

### `type Row = Vec<Value>`

序列化函数：

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
| `sql::tokenizer` | `Tokenizer` / `Token` 词法器 |
| `sql::parser` | `Parser` / `parse_sql` 语法分析器 |

```rust
use kkdb::sql::parser::parse_sql;
let stmt = parse_sql("SELECT 1 + 2")?;
```

---

## 7. `schema`

主要类型：

| 类型 | 说明 |
|------|------|
| `Schema` | 元数据管理器（内存缓存） |
| `TableSchema` | 表信息（列定义、root_page、next_rowid） |
| `ColumnInfo` | 列信息（name/data_type/pk/autoincrement 等） |
| `IndexSchema` | 索引信息（name/table_name/root_page/unique） |

主要方法：

```rust
// schema 加载（从 catalog pager 读取）
Schema::load_from_pager(pager: &mut Pager) -> Result<()>

// DDL（需分别传 catalog_pager 和 table_pager）
Schema::create_table(
    catalog_pager: &mut Pager,
    table_pager: &mut Pager,
    name: &str,
    column_defs: &[ColumnDef],
    if_not_exists: bool,
    original_sql: &str,
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
Schema::has_indexes_for_table(table: &str) -> bool
Schema::indexes_for_table(table: &str) -> Vec<&IndexSchema>

// ALTER TABLE
Schema::alter_add_column(pager, table_name, col)
Schema::alter_drop_column(pager, table_name, col_name)
Schema::alter_rename_table(pager, old_name, new_name)
Schema::alter_rename_column(pager, table_name, old_col, new_col)
```

> **注意**：`create_table` / `create_index` 现在需要两个 Pager 参数：
> - `catalog_pager`：schema B-Tree 所在的 pager（单文件或 `catalog.kkdb`）
> - `table_pager`：表数据 B-Tree 所在的 pager（多文件模式下为各自的 `.kkdb` 文件）
>
> 在单文件 / 内存模式下两者指向同一个 Pager。

---

## 8. `storage`

### `Pager`

页缓存与文件 I/O，V2 格式使用 COW 双超块保障原子刷盘。

| 方法 | 说明 |
|------|------|
| `Pager::open(path)` | 打开或创建文件 pager（V2 COW 格式） |
| `Pager::open_memory()` | 内存 pager |
| `get_page(page_num)` | 读取页（惰性加载） |
| `get_page_mut(page_num)` | 获取可写页（自动 COW 快照） |
| `allocate_page()` | 分配新页 |
| `flush()` | 将脏页刷盘（autocommit 或事务模式） |
| `begin_transaction()` | 开启事务快照 |
| `commit_transaction()` | 提交并刷盘 |
| `rollback_transaction()` | 回滚（恢复快照） |
| `savepoint(name)` | 创建保存点 |
| `schema_root_page()` | 获取 schema B-Tree 根页号 |

**V2 格式页布局：**
- Page 1, 2：COW 双超块（generation 奇偶轮换保障原子写）
- Page 3+：用户数据（schema、表数据、索引）

### `BTree`

基于 Pager 的 B-Tree 操作。

| 方法 | 说明 |
|------|------|
| `create_table()` | 分配新 B-Tree 根页，返回页号 |
| `insert(root, rowid, row)` | 插入行，返回（可能新的）根页号 |
| `insert_with_buf(...)` | 带重用 buffer 的插入（高性能批量写） |
| `scan_all(root)` | 全表扫描，返回 `Vec<(rowid, Row)>` |
| `scan_rows(root)` | 全表扫描，仅返回 `Vec<Row>` |
| `scan_rows_limit(root, limit)` | 带 limit 的提前终止扫描 |
| `find_by_rowid(root, rowid)` | 按 rowid 点查 |
| `update_row(root, rowid, row)` | 原地更新行 |
| `delete_by_rowid(root, rowid)` | 删除行，返回新根页号 |
| `max_rowid(root)` | 返回最大 rowid（AUTOINCREMENT 用） |
| `count_rows(root)` | 统计行数 |

---

## 9. 分层使用建议

```
┌────────────────────────────────────────────────┐
│ 应用层     VM + ExecResult + KkdbError          │  ← 推荐
├────────────────────────────────────────────────┤
│ 工具层     sql::parser（仅解析 SQL）             │  ← 按需
├────────────────────────────────────────────────┤
│ 内核扩展   schema / storage（与内部耦合较高）    │  ← 谨慎
└────────────────────────────────────────────────┘
```

---

## 10. 版本与兼容性

当前版本：`0.1.0`

- `VM::execute_sql` 接口稳定，是最安全的调用边界。
- 底层 `schema::create_table` / `create_index` 签名已在 v0.1 中调整（新增 `table_pager` 参数），旧单文件代码需同步更新。
- `storage` 层（Pager / BTree）现为 V2 COW 格式，旧 V1 格式文件不兼容。
