# KKDB 项目文档

## 1. 项目简介

KKDB 是一个使用 Rust 实现的轻量级 SQLite 风格数据库引擎，包含：

- SQL 分词器（Tokenizer）
- SQL 解析器（Parser）与 AST
- 虚拟机执行器（VM）
- Pager + B-Tree 存储引擎
- 内存库与文件库两种模式
- 交互式 REPL 命令行

项目代码入口：

- 库入口：`src/lib.rs`
- CLI 入口：`src/main.rs`

## 2. 功能范围

已支持的核心 SQL 语句：

- DDL：`CREATE TABLE`、`DROP TABLE`、`ALTER TABLE`、`CREATE INDEX`
- DML：`INSERT`、`UPDATE`、`DELETE`
- 查询：`SELECT`（含 `WHERE`、`JOIN`、`GROUP BY`、`HAVING`、`ORDER BY`、`LIMIT/OFFSET`、`DISTINCT`）
- 事务：`BEGIN`、`COMMIT`、`ROLLBACK`
- 计划：`EXPLAIN <statement>`

表达式能力（主要）：

- 算术：`+ - * / %`
- 比较：`= != <> < <= > >=`
- 逻辑：`AND OR NOT`
- 其他：`IS NULL`、`IN (...)`、`LIKE`、`BETWEEN`
- 子查询：标量子查询、`IN (SELECT ...)`、`EXISTS (SELECT ...)`
- 常见聚合：`COUNT`、`SUM`、`AVG`、`MIN`、`MAX`

## 3. 架构概览

执行链路：

1. `sql/tokenizer` 将 SQL 文本拆分为 Token
2. `sql/parser` 将 Token 构造成 AST（`sql/ast`）
3. `vm/execute` 对 AST 执行，调用 DDL/DML/SELECT 子模块
4. `storage/pager` 与 `storage/btree` 持久化数据与索引
5. `schema` 维护元数据缓存与系统表（第 1 页）

关键模块职责：

- `src/sql`：词法、语法、AST 定义
- `src/vm`：SQL 执行引擎
- `src/storage`：页管理、B-Tree、游标
- `src/schema.rs`：表/索引元数据管理
- `src/types.rs`：值类型系统与序列化
- `src/error.rs`：统一错误类型

## 4. 存储与事务

### 4.1 Pager

- 页面大小：`4096` 字节（`PAGE_SIZE`）
- 页号从 `1` 开始，`page 1` 为 schema 根页
- 文件模式支持脏页刷盘
- 内存模式不落盘

### 4.2 B-Tree

- 表与索引均基于 B-Tree 组织
- 支持插入、更新、按 rowid 查找、扫描、删除

### 4.3 事务语义

- `BEGIN`：建立内存快照
- `COMMIT`：先刷盘，成功后再清理快照
- `ROLLBACK`：恢复快照

## 5. 索引与执行优化

当前已实现的关键优化包括：

- 语句缓存（FIFO 淘汰，容量上限 256）
- `WHERE` 索引下推（`= / IN / < <= > >= / BETWEEN`）
- `ORDER BY + LIMIT` Top-N 优化（支持 `OFFSET`）
- 范围查询有序缓存（首列值有序 + 二分）
- 大候选集批量回表（一次扫描 + 哈希回填）
- UNIQUE 冲突检测候选收窄（避免全索引扫描）

## 6. REPL 使用

启动：

```bash
# 内存数据库
cargo run

# 文件数据库
cargo run -- mydb.db
```

REPL 点命令：

- `.help`
- `.quit` / `.exit`
- `.tables`
- `.schema [TABLE]`
- `.open FILE`
- `.memory`

## 7. 构建与测试

```bash
cargo build
cargo test
```

Windows 推荐：

```powershell
.\scripts\check.ps1
```

该脚本会执行 `fmt + clippy + test`，并隔离 target 目录。

## 8. 目录结构

```text
src/
  sql/         # tokenizer / parser / ast
  storage/     # pager / btree / cursor
  vm/          # execute + ddl/dml/select
  schema.rs    # schema catalog
  types.rs     # runtime values
  error.rs     # error type
  main.rs      # REPL
tests/         # integration/perf tests
scripts/       # check scripts
```

## 9. 已知边界

- 当前主要聚焦 SQLite 风格核心子集，不等同完整 SQLite
- API 公开面较小，`VM` 是最稳定入口
- 低层 `storage` 与 `schema` API 更偏内部实现细节，后续可能调整
