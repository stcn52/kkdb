# KKDB

KKDB 是一个用 Rust 实现的轻量级 SQLite 风格数据库引擎。

## 文档

- 项目总览：[`docs/PROJECT.md`](docs/PROJECT.md)
- 高阶 SQL 特性：[`docs/ADVANCED_SQL.md`](docs/ADVANCED_SQL.md)
- API 参考：[`docs/API.md`](docs/API.md)
- 存储可靠性设计：[`docs/COW_DOUBLE_SUPERBLOCK_DESIGN.md`](docs/COW_DOUBLE_SUPERBLOCK_DESIGN.md)
- Binlog 设计：[`docs/BINLOG_DESIGN.md`](docs/BINLOG_DESIGN.md)
- **分布式集群（Raft）**：[`docs/DISTRIBUTED.md`](docs/DISTRIBUTED.md)

## 主要特性

- SQL 词法器、解析器、AST
- VM 执行器（DDL / DML / SELECT）
- Pager + B-Tree 存储引擎（COW 双超块）
- **按表拆分文件**存储（类 MySQL InnoDB 风格）
- 内存库与文件库两种模式
- 交互式 REPL 命令行
- 事务支持（BEGIN / COMMIT / ROLLBACK）

## 构建与运行

```bash
# 构建
cargo build

# 运行 REPL（内存库）
cargo run

# 运行 REPL（文件库 — 自动创建目录结构）
cargo run -- mydb
```

## 库快速入门

### 内存模式

```rust
use kkdb::vm::execute::{ExecResult, VM};

let mut vm = VM::new_memory();
vm.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")?;
vm.execute_sql("INSERT INTO users VALUES (1, 'Alice')")?;

if let ExecResult::QueryResult { columns, rows } =
    vm.execute_sql("SELECT id, name FROM users")?
{
    println!("{:?}", columns); // ["id", "name"]
    println!("{:?}", rows);
}
```

### 文件模式（按表分文件）

```rust
use kkdb::vm::execute::VM;

// 首次打开：自动创建 mydb/ 目录
{
    let mut vm = VM::open("mydb")?;
    vm.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")?;
    vm.execute_sql("INSERT INTO users VALUES (1, 'Alice')")?;
    // vm 析构时自动刷盘
}

// 再次打开：加载已有数据
{
    let mut vm = VM::open("mydb")?;
    // SELECT users ...
}
```

生成的目录结构：

```
mydb/
  catalog.kkdb   ← Schema 元数据（所有表的 CREATE 语句与根页记录）
  users.kkdb     ← users 表的数据 B-Tree
  binlog.bin     ← Binlog
```

## 测试

```bash
cargo test
```

Windows 推荐用隔离脚本：

```powershell
.\scripts\check.ps1
```

## 目录结构

```
src/
  sql/         # tokenizer / parser / ast
  storage/     # pager / btree / cursor
  vm/          # execute + ddl/dml/select
  schema.rs    # schema catalog
  types.rs     # runtime values
  error.rs     # error type
  binlog/      # WAL-style binlog
  main.rs      # REPL
tests/         # integration tests
scripts/       # check scripts
docs/          # design & API docs
```

## 说明

- 页大小：4096 字节。
- 文件库模式下，每个表独占一个 `.kkdb` 文件，Schema 存于 `catalog.kkdb`。
- 单文件旧格式（`.db`）向后兼容，通过 `VM::open` 自动检测。
- 事务支持 `BEGIN` / `COMMIT` / `ROLLBACK` / `SAVEPOINT`。
