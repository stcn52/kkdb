# KKDB 项目文档

## 1. 项目简介

KKDB 是一个使用 Rust 实现的轻量级 SQLite 风格数据库引擎，包含：

- SQL 词法器（Tokenizer）、解析器（Parser）与 AST
- 虚拟机执行器（VM）支持 DDL / DML / SELECT / 事务
- Pager + B-Tree 存储引擎（COW 双超块 V2 格式）
- **按表拆分文件**（MySQL InnoDB 风格）
- 内存库与文件库两种模式
- 交互式 REPL 命令行
- Binlog（WAL 风格，用于崩溃恢复基础）

代码入口：

- 库入口：`src/lib.rs`
- CLI 入口：`src/main.rs`

---

## 2. 功能范围

### SQL 支持

**DDL：**
`CREATE TABLE`、`DROP TABLE`、`ALTER TABLE`（ADD/DROP/RENAME 列、RENAME 表）、`CREATE INDEX`、`DROP INDEX`、`CREATE VIEW`、`DROP VIEW`

**DML：**
`INSERT`（含 `INSERT OR REPLACE / IGNORE / NOTHING`）、`UPDATE`、`DELETE`

**查询：**
`SELECT`：`WHERE`、`JOIN`（内连接/左连接）、`GROUP BY`、`HAVING`、`ORDER BY`、`LIMIT/OFFSET`、`DISTINCT`、子查询、窗口函数（基础）

**事务：**
`BEGIN`、`COMMIT`、`ROLLBACK`、`SAVEPOINT`、`RELEASE`、`ROLLBACK TO`

**表达式：**
- 算术：`+ - * / %`
- 比较：`= != <> < <= > >=`
- 逻辑：`AND OR NOT`
- 其他：`IS NULL`、`IN (...)`、`LIKE`、`BETWEEN`、`CASE WHEN`
- 子查询：标量子查询、`IN (SELECT ...)`、`EXISTS (SELECT ...)`
- 聚合：`COUNT`、`SUM`、`AVG`、`MIN`、`MAX`

---

## 3. 架构概览

### 执行链路

```
SQL 文本
  ↓ sql/tokenizer
Token 流
  ↓ sql/parser
AST（Statement / Expr）
  ↓ vm/execute
  ├── exec_ddl.rs  (CREATE/DROP/ALTER)
  ├── exec_dml.rs  (INSERT/UPDATE/DELETE)
  └── exec_select.rs (SELECT)
        ↓
  schema.rs（元数据缓存）
        ↓
  storage/pager  ←→  storage/btree
        ↓
  磁盘文件（catalog.kkdb / <table>.kkdb）
```

### 关键模块职责

| 模块 | 职责 |
|------|------|
| `src/sql` | 词法、语法、AST 定义 |
| `src/vm/execute.rs` | VM 核心：路由、事务、pager 管理 |
| `src/vm/exec_ddl.rs` | CREATE / DROP / ALTER / INDEX |
| `src/vm/exec_dml.rs` | INSERT / UPDATE / DELETE |
| `src/vm/exec_select.rs` | SELECT、JOIN、聚合、子查询 |
| `src/schema.rs` | 表/索引/视图元数据管理 |
| `src/storage/pager.rs` | 页缓存、COW 双超块、事务快照 |
| `src/storage/btree.rs` | B-Tree 插入/扫描/删除 |
| `src/types.rs` | 值类型系统与序列化 |
| `src/error.rs` | 统一错误类型 |
| `src/binlog/` | Binlog 记录与读取 |

---

## 4. 存储引擎

### 4.1 按表分文件（Multi-file 模式）

`VM::open("mydb")` 使用目录模式：

```
mydb/
  catalog.kkdb   ← Schema B-Tree（所有表的元数据、根页记录）
  users.kkdb     ← users 表的数据 B-Tree
  orders.kkdb    ← orders 表的数据 B-Tree
  binlog.bin
```

- `catalog.kkdb` 只存储 Schema：相当于 MySQL 的 `information_schema` + frm 文件
- 每个 `.kkdb` 文件是独立的 COW V2-format Pager
- 表数据的根页号（`root_page`）存于 catalog，实际数据在对应表文件中
- `VM` 持有 `table_pagers: HashMap<String, Pager>`，通过 `get_table_pager_mut(table_name)` 路由

### 4.2 Pager（COW V2 格式）

- 页大小：`4096` 字节
- 页号从 `1` 开始（0 无效，类 SQLite）
- **Page 1, 2**：COW 双超块（generation 轮换，保障原子写）
- **Page 3**：Schema 根页（叶节点 B-Tree）
- **Page 4+**：用户数据页

**COW 原理：**
- `flush_v2_autocommit`：写脏页 → 写 inactive 超块（新 generation）→ 轮换为 active
- `begin_transaction`：O(1)，无需克隆，脏页首次写时按需 COW
- `rollback_transaction`：仅恢复被修改过的页

### 4.3 B-Tree

- 表与索引均使用 B-Tree
- Leaf 页直接存行数据
- Interior 页存键（rowid）与子页指针
- 支持：插入（含分裂）、更新（原地/重插）、删除（物理删除）、全表扫描、按 rowid 点查

### 4.4 自动刷盘

- **auto-commit 模式**：每条 DML / DDL 执行后调用 `auto_flush`，同时 flush catalog pager 和所有 table pager
- **事务模式**（`BEGIN/COMMIT`）：COMMIT 时统一刷盘
- **VM Drop**：析构时 best-effort 刷盘（兜底保障，防止数据丢失）

---

## 5. 执行优化

| 优化 | 说明 |
|------|------|
| 语句缓存 | FIFO 淘汰，上限 256 条，复用 AST 避免重复解析 |
| 索引下推 | `WHERE col = val / IN / < / <= / > / >= / BETWEEN` 走索引 |
| LIMIT 推送 | 全表扫描带 early-exit limit，减少不必要行反序列化 |
| 范围查询有序缓存 | 索引首列有序缓存 + 二分查找 |
| 大候选集批量回表 | 候选数 ≥ 96 时一次全扫描 + HashMap 回填，避免大量点查 |
| UNIQUE 冲突收窄 | 仅扫描冲突候选，不做全索引扫描 |
| 批量插入 buffer | `insert_with_buf` 复用序列化 buffer |

---

## 6. REPL 使用

```bash
# 内存数据库
cargo run

# 文件数据库（目录模式）
cargo run -- mydb
```

点命令：

| 命令 | 说明 |
|------|------|
| `.help` | 帮助 |
| `.quit` / `.exit` | 退出 |
| `.tables` | 列出所有表 |
| `.schema [TABLE]` | 打印表定义 |
| `.open PATH` | 切换数据库 |
| `.memory` | 切换为内存库 |

---

## 7. 构建与测试

```bash
cargo build
cargo test
```

Windows 推荐用隔离脚本（避免文件锁问题）：

```powershell
.\scripts\check.ps1
```

该脚本执行 `fmt + clippy + test`。

---

## 8. 目录结构

```
src/
  sql/           # tokenizer / parser / ast
  storage/       # pager / btree / cursor
  vm/            # execute + ddl/dml/select/eval
  schema.rs      # schema catalog
  schema_tests.rs
  types.rs       # runtime values
  error.rs       # error type
  binlog/        # binlog record
  varint.rs      # variable-length int codec
  main.rs        # REPL
tests/
  integration_test.rs
scripts/
  check.ps1
docs/
  API.md
  PROJECT.md
  COW_DOUBLE_SUPERBLOCK_DESIGN.md
  BINLOG_DESIGN.md
```

---

## 9. 已知边界与后续规划

- 当前聚焦 SQLite 风格核心子集，并非完整 SQLite 兼容
- 多文件目录模式只对安全文件名的表生效（仅字母/数字/下划线）
- 旧单文件格式（`.db`）通过 `open_legacy` 向后兼容
- 后续规划：WAL 完整实现、并发读写、更多 SQL 函数支持
