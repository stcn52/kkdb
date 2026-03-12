# KKDB

<p align="center">
  <strong>使用 Rust 实现的功能完备的关系型数据库引擎</strong>
</p>

<p align="center">
  <em>~57,000 行 Rust · 91 模块 · 4,700+ 测试 · 零外部数据库依赖</em>
</p>

---

## 核心亮点

| 分类 | 特性 |
|------|------|
| **SQL** | 完整 DDL/DML/SELECT · 70+ 内置函数 · 窗口函数 · CTE（含递归） · 子查询 · 集合操作 · JSON 函数 |
| **存储** | COW 双超块 Pager · B-Tree（SQLite 兼容格式）· WAL · Buffer Pool（LRU-K） · Bloom Filter · LZ4/Zstd 压缩 |
| **事务** | MVCC 快照隔离 · 表/行级锁 · 死锁检测 · SAVEPOINT · 2PC/3PC 分布式事务 |
| **全文检索** | BM25 倒排索引 · jieba-rs 中文分词 · 模糊搜索 · 同义词扩展 · 分面搜索 |
| **向量搜索** | HNSW 近似最近邻 · Cosine/L2 距离 · 多索引管理 · 量化压缩 |
| **安全** | RBAC 权限 · 行级安全策略（RLS）· 列级加密（AES/ChaCha20）· 审计日志 · 数据脱敏 |
| **网络** | MySQL Wire Protocol v10 · Supabase 风格 HTTP REST API（JWT 认证 + 多租户） |
| **分布式** | openraft v0.9 Raft 共识 · 自动故障转移 · 一致性哈希分片 · 节点发现 · 服务网格 |

## 快速开始

```bash
# 编译
cargo build --release

# 内存模式 REPL
cargo run --release

# 文件持久化模式
cargo run --release -- mydb

# 启动 MySQL + HTTP 服务器
cargo run --release -- --mysql-port 3307 --http-port 8080
```

### 最小 Rust API 示例

```rust
use kkdb::vm::execute::{ExecResult, VM};

let mut vm = VM::new_memory();
vm.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")?;
vm.execute_sql("INSERT INTO users VALUES (1, 'Alice')")?;

if let ExecResult::QueryResult { columns, rows } =
    vm.execute_sql("SELECT * FROM users")?
{
    println!("{columns:?}"); // ["id", "name"]
    for row in &rows { println!("{row:?}"); }
}
```

### 最小 SQL 示例

```sql
CREATE TABLE products (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    price REAL DEFAULT 0.0,
    category TEXT
);

INSERT INTO products (name, price, category) VALUES ('Widget', 29.99, 'tools');

SELECT category, COUNT(*), AVG(price)
FROM products
GROUP BY category
HAVING COUNT(*) > 0;
```

### MySQL 客户端连接

```bash
mysql -h 127.0.0.1 -P 3307
```

### HTTP REST API

```bash
# 执行 SQL
curl -X POST http://localhost:8080/rest/query \
  -H 'Authorization: Bearer <token>' \
  -H 'Content-Type: application/json' \
  -d '{"sql": "SELECT * FROM users"}'
```

## 文档

### 📖 用户文档

| 文档 | 说明 |
|------|------|
| **[完全使用手册](docs/USAGE.md)** | **全部功能的完整参考（31 章节，1800 行）** |
| [高阶 SQL 特性](docs/ADVANCED_SQL.md) | 窗口函数、CTE、子查询、RLS、全文检索 深入指南 |
| [内置函数参考](docs/FUNCTIONS.md) | 70+ 函数逐一列出：聚合/字符串/数学/日期/JSON |
| [Rust API 参考](docs/API.md) | Crate 公开接口、VM 用法、类型系统 |
| [HTTP REST API](docs/HTTP_API.md) | Supabase 风格端点、JWT 认证、多租户 |
| [MySQL 协议服务器](docs/MYSQL_SERVER.md) | Wire Protocol v10、COM 命令、兼容性说明 |
| [分布式集群（Raft）](docs/DISTRIBUTED.md) | 集群部署、Raft HTTP API、快照、成员变更 |

### 🏗️ 设计与架构文档

| 文档 | 说明 |
|------|------|
| [项目总览](docs/PROJECT.md) | 架构概述、模块结构、设计决策 |
| [COW 双超块设计](docs/COW_DOUBLE_SUPERBLOCK_DESIGN.md) | 崩溃安全存储引擎设计方案 |
| [向量搜索设计](docs/VECTOR_SEARCH_DESIGN.md) | HNSW 存储模型与 SQL 接口设计 |
| [Binlog 设计](docs/BINLOG_DESIGN.md) | PITR / 复制 / 审计 日志格式 |
| [SQL 解析器重构](docs/SQLPARSER_REFACTOR_ANALYSIS.md) | sqlparser-rs 迁移分析与进度 |

### 🗺️ 开发路线图

| 文档 | 说明 |
|------|------|
| [升级计划](docs/UPGRADE_PLAN.md) | CoW + 双超块 + Binlog 升级阶段 |
| [优化路线图](docs/optimization_roadmap.md) | 性能优化清单与完成状态 |
| [任务清单](docs/task.md) | 待开发 / 已完成功能 Checklist |

## 测试

```bash
# 运行全部测试（~4700 tests）
cargo test

# 仅库内测试
cargo test --lib

# Windows
.\scripts\check.ps1
```

## 项目结构

```
src/
├── main.rs                   # 交互式 REPL 入口
├── lib.rs                    # Crate 根
├── types.rs                  # DataType / Value / Row
├── schema.rs                 # TableSchema / ColumnInfo
├── error.rs                  # KkdbError（17 种变体）
├── varint.rs                 # LEB128 / ZigZag 编码
├── sql/                      # SQL 解析器
│   ├── ast.rs                #   AST 节点定义
│   ├── parser.rs             #   parse_sql() 入口
│   └── sqlparser_adapter/    #   sqlparser crate 适配层
├── storage/                  # 存储引擎
│   ├── pager.rs              #   COW v2 双超块 Pager
│   ├── btree.rs              #   B-Tree（SQLite 格式）
│   ├── wal.rs                #   WAL 预写日志
│   ├── cursor.rs             #   B-Tree 游标
│   ├── buffer_pool.rs        #   LRU-K(2) 缓冲池
│   ├── bloom.rs              #   Bloom Filter
│   └── ext/                  #   存储扩展模块
├── vm/                       # 虚拟机
│   ├── execute.rs            #   VM 核心（new_memory / open）
│   ├── exec_ddl.rs           #   DDL 执行器
│   ├── exec_dml.rs           #   DML 执行器（+ FK + MVCC）
│   ├── exec_select.rs        #   SELECT 管道（JOIN/CTE/Window）
│   ├── eval_expr.rs          #   表达式求值 + 70+ 函数
│   ├── mvcc.rs               #   MVCC Undo Log
│   ├── optimizer/            #   查询优化器
│   ├── engine/               #   执行引擎扩展
│   ├── auth/                 #   RBAC / 审计 / 安全
│   └── monitor/              #   监控 / 可观测性
├── fulltext/                 # 全文检索（BM25 + jieba-rs）
├── vector/                   # 向量搜索（HNSW）
├── raft/                     # Raft 分布式共识
│   ├── node.rs               #   KkdbNode 封装
│   ├── log_store.rs          #   WAL 持久化 Raft 日志
│   └── features/             #   HA / 2PC / 分片 / 服务网格
├── server/                   # 网络服务器
│   ├── mysql.rs              #   MySQL Wire Protocol v10
│   └── http_api.rs           #   HTTP REST API（axum）
├── binlog/                   # Binlog 复制日志
└── bin/                      # CLI 工具
    ├── kkdb-cli.rs           #   备份/恢复/导入/导出
    └── big_data_bench.rs     #   基准测试
tests/                        # 集成测试
docs/                         # 文档
scripts/                      # 构建/检查脚本
```

## 文件存储结构

```
mydb/
  catalog.kkdb   ← Schema 元数据（所有表的 CREATE 语句与根页记录）
  users.kkdb     ← users 表的数据 B-Tree
  binlog.bin     ← Binlog 复制日志
```

- 页大小 4096 字节（可配置 512 ~ 65536）
- 每表独占一个 `.kkdb` 文件，Schema 存于 `catalog.kkdb`
- 单文件旧格式（`.db`）向后兼容，`VM::open` 自动检测
- 支持 LZ4/Zstd 页面压缩

## 许可

MIT
