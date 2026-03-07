# KKDB 项目文档

## 1. 项目简介

KKDB 是一个使用 Rust 实现的轻量级、功能完备的关系型数据库引擎，具备：

- SQL 词法器（Tokenizer）、解析器（sqlparser-rs 适配器）与 AST
- 虚拟机执行器（VM）支持完整 DDL / DML / SELECT / 事务
- Pager + B-Tree 存储引擎（COW 双超块 V2 格式，崩溃安全）
- **按表拆分文件**（MySQL InnoDB 风格）
- 内存库与文件库两种模式
- 交互式 REPL 命令行
- Binlog（操作日志，支持 PITR 基础）
- **Raft 分布式共识**（HTTP 网络层 + WAL 日志存储）
- **HTTP REST API**（Supabase 风格）+ **MySQL 有线协议**服务器
- **全文检索**（BM25 倒排索引）
- **行级安全（RLS）**、**触发器**、**外键**、**CHECK/UNIQUE 约束**
- **MVCC 事务**（COW 快照 + undo log）+ **全局死锁检测**

代码入口：

- 库入口：`src/lib.rs`
- CLI 入口：`src/main.rs`

---

## 2. 功能范围

### SQL 支持

**DDL：**
`CREATE TABLE`、`DROP TABLE`、`ALTER TABLE`（ADD/DROP/RENAME 列、RENAME 表、ENABLE ROW LEVEL SECURITY）、`CREATE INDEX`、`DROP INDEX`、`CREATE VIEW`、`CREATE TRIGGER`、`DROP TRIGGER`、`CREATE FULLTEXT INDEX`、`CREATE POLICY`、`DROP POLICY`、`ANALYZE TABLE`、`VACUUM`

**DML：**
`INSERT`（含 `INSERT OR REPLACE / IGNORE / NOTHING / ON CONFLICT DO UPDATE SET`）、`UPDATE`、`DELETE`；所有 DML 支持 `RETURNING` 子句

**查询：**
`SELECT`：`WHERE`、`JOIN`（内连接/左连接/右连接/交叉连接/LEFT SEMI）、`GROUP BY`、`HAVING`、`ORDER BY`（含 `NULLS FIRST/LAST`）、`LIMIT/OFFSET`、`DISTINCT`、子查询（相关 + 非相关）、`CTE (WITH)`、`WITH RECURSIVE`、集合操作（`UNION / UNION ALL / INTERSECT / EXCEPT`）、窗口函数（`ROW_NUMBER / RANK / DENSE_RANK / LEAD / LAG / SUM / AVG` 等 `OVER(PARTITION BY ... ORDER BY ...)` 框架）、`EXPLAIN`

**事务：**
`BEGIN`、`COMMIT`、`ROLLBACK`、`SAVEPOINT`、`RELEASE`、`ROLLBACK TO`

**表达式：**
- 算术：`+ - * / %`
- 比较：`= != <> < <= > >=`
- 逻辑：`AND OR NOT`
- 其他：`IS NULL`、`IN`、`LIKE`（含通配符 `%` / `_`）、`BETWEEN`、`CASE WHEN ... THEN ... ELSE ... END`
- 子查询：标量、`IN (SELECT ...)`、`EXISTS (SELECT ...)`、`ANY / ALL`（完全支持相关子查询）
- 聚合：`COUNT`、`SUM`、`AVG`、`MIN`、`MAX`、`COUNT(DISTINCT ...)`
- 内置函数：`COALESCE`、`NULLIF`、`ABS`、`UPPER`、`LOWER`、`LENGTH`、`TRIM`、`SUBSTR`、`REPLACE`、`ROUND`、`CAST`、`TYPEOF`、`STRFTIME`、`DATE`、`NOW`
- 全文检索：`FTS_MATCH(table, index, query)` 使用 BM25 排序

**用户与权限：**
`CREATE USER`、`ALTER USER`（修改密码）、`DROP USER`、`GRANT`、`REVOKE`

---

## 3. 架构概览

### 执行链路

```
SQL 文本
  ↓ sql/sqlparser_adapter（sqlparser-rs 0.61）
AST（Statement / Expr）
  ↓ vm/execute（路由 + 事务 + 语句缓存）
  ├── exec_ddl.rs  (CREATE/DROP/ALTER/INDEX/VIEW/TRIGGER)
  ├── exec_dml.rs  (INSERT/UPDATE/DELETE + RETURNING)
  ├── exec_select.rs (SELECT + JOIN + CTE + 窗口函数 + FTS)
  └── eval_expr.rs  (表达式/函数求值)
        ↓
  schema.rs（表/索引/视图/触发器/策略 元数据缓存）
        ↓
  storage/pager.rs  ←→  storage/btree.rs
        ↓
  磁盘文件（catalog.kkdb / <table>.kkdb）+ binlog.bin
```

### 关键模块职责

| 模块 | 职责 |
|------|------|
| `src/sql/` | 词法、语法、AST 定义、sqlparser-rs 适配器 |
| `src/vm/execute.rs` | VM 核心：路由、事务管理、语句缓存、自适应索引 |
| `src/vm/exec_ddl.rs` | CREATE / DROP / ALTER / INDEX / VIEW / TRIGGER |
| `src/vm/exec_dml.rs` | INSERT / UPDATE / DELETE + 约束检查 + FTS 维护 |
| `src/vm/exec_select.rs` | SELECT、JOIN（哈希/嵌套循环）、聚合、子查询、RLS |
| `src/vm/eval_expr.rs` | 表达式求值、内置函数、FTS 打分 |
| `src/vm/lock_manager.rs` | 全局表级锁 + 死锁检测（等待图 DFS） |
| `src/vm/mvcc.rs` | MVCC undo log 条目定义 |
| `src/vm/data_transfer.rs` | 备份/恢复/导入/导出（含 CSV、JSON） |
| `src/schema.rs` | 表/索引/视图/触发器/RLS策略 元数据管理 |
| `src/storage/pager.rs` | 页缓存、COW 双超块、事务快照、Savepoint |
| `src/storage/btree.rs` | B-Tree 插入/扫描/删除、VarInt 编码、前缀压缩 |
| `src/fulltext/` | BM25 tokenizer + 倒排索引读写 |
| `src/types.rs` | 值类型系统（Integer/Real/Text/Blob/Null）与序列化 |
| `src/error.rs` | 统一错误类型 `KkdbError` |
| `src/binlog/` | Binlog 记录（Begin/Insert/Update/Delete/Commit/Rollback）|
| `src/raft/` | Raft 共识（openraft）+ HTTP 网络 + WAL 日志存储 |
| `src/server/` | HTTP REST API（axum）+ MySQL 有线协议 + TCP 服务器 |

---

## 4. 存储引擎

### 4.1 按表分文件（Multi-file 模式）

`VM::open("mydb")` 使用目录模式：

```
mydb/
  catalog.kkdb   ← Schema B-Tree（所有表的元数据、根页记录）
  users.kkdb     ← users 表的数据 B-Tree
  orders.kkdb    ← orders 表的数据 B-Tree
  binlog.bin     ← Binlog（操作日志）
  raft/          ← Raft WAL（仅集群模式）
    wal.log
    vote.json
    purge.json
```

- `catalog.kkdb` 只存储 Schema（相当于 MySQL 的 information_schema）
- 每个 `.kkdb` 是独立的 COW V2-format Pager
- `VM` 持有 `table_pagers: HashMap<String, Pager>`，通过 `get_table_pager_mut(name)` 路由

### 4.2 Pager（COW V2 格式）

- 页大小：`4096` 字节（编译期可配置 8KB/16KB）
- 页号从 `1` 开始（0 无效，类 SQLite）
- **Page 1, 2**：COW 双超块（generation 轮换，保障原子写）
- **Page 3**：Schema 根页
- **Page 4+**：用户数据页

**COW 原理：**
- 写入永不覆盖旧页，事务内按需 COW 新页
- 提交时：写脏页 → fsync → 写 inactive 超块（含新 generation）→ fsync
- 回滚时：仅恢复被修改过的页，O(dirty_pages) 代价

### 4.3 B-Tree

- 表与索引均使用 B-Tree（类 SQLite B+Tree 格式）
- **VarInt 编码**：key 和 value 长度使用变长整数，平均压缩 30-50%
- **右边界追加优化**：单调递增 rowid 插入跳过不必要分裂，批量写性能提升 40%+
- **叶节点键前缀压缩**：字符串索引体积减少 40-70%
- **LZ4 页压缩**：页数据可选 LZ4 压缩，冷存储体积减半

### 4.4 自动刷盘

- **auto-commit 模式**：每条 DML / DDL 执行后调用 `auto_flush`
- **事务模式**（`BEGIN/COMMIT`）：COMMIT 时统一刷盘
- **VM Drop**：析构时 best-effort 刷盘（兜底保障）
- **Savepoint**：支持嵌套式部分回滚

---

## 5. 高级特性

### 5.1 全文检索（BM25）

```sql
-- 创建全文索引
CREATE FULLTEXT INDEX idx_ft ON articles (title, content);

-- BM25 相关性查询（返回按分数排序的结果）
SELECT title, FTS_MATCH('articles', 'idx_ft', 'rust database') AS score
FROM articles
WHERE FTS_MATCH('articles', 'idx_ft', 'rust database') > 0
ORDER BY score DESC
LIMIT 10;
```

- Unicode 分词器（支持中英文）
- 实时维护（INSERT/UPDATE/DELETE 自动更新倒排索引）
- BM25 打分：`IDF × TF × (k1+1) / (TF + k1 × (1-b+b×dl/avgdl))`

### 5.2 行级安全（RLS）

```sql
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;

CREATE POLICY orders_owner ON orders
  FOR ALL
  TO 'user_role'
  USING (owner_id = current_user_id());

SET kkdb.current_user = 'alice';
SELECT * FROM orders; -- 只返回 alice 的数据
```

### 5.3 触发器

```sql
CREATE TRIGGER audit_update
  AFTER UPDATE ON employees
  FOR EACH ROW
  BEGIN
    INSERT INTO audit_log (table_name, action, ts)
    VALUES ('employees', 'UPDATE', NOW());
  END;
```

### 5.4 外键约束

```sql
CREATE TABLE orders (
  id INTEGER PRIMARY KEY,
  cust_id INTEGER REFERENCES customers(id)
    ON DELETE CASCADE
    ON UPDATE RESTRICT
);
```

支持 `ON DELETE / ON UPDATE` 的 `CASCADE`、`SET NULL`、`RESTRICT` 动作。

### 5.5 死锁检测

VM 内置全局锁管理器，使用等待图（Wait-for Graph）DFS 算法实时检测死锁：

```
Lock conflict: table `orders` is Exclusive-locked by txn 2, txn 3 cannot acquire
Deadlock detected: txn 3 and txn 2 form a cycle on table `orders`
```

### 5.6 自适应索引

统计全表扫描频率，超过阈值自动为高频访问列创建索引（可关闭）：

```rust
vm.adaptive_threshold = 10; // 10 次全扫描后自动建索引
```

---

## 6. 服务器模式

```bash
# 启动 TCP + HTTP + MySQL 服务
cargo run -- mydb --server \
  --port 3306 \          # 原生 TCP 协议
  --http-port 6543 \     # HTTP REST API (Supabase 风格)
  --mysql-port 3307 \    # MySQL 有线协议（兼容 mysql2/DBeaver）
  --data-dir ./data
```

| 协议 | 默认端口 | 说明 |
|------|---------|------|
| 原生 TCP | 3306 | KKDB 自有文本协议 |
| HTTP REST | 6543 | `POST /query` 执行 SQL，JSON 返回 |
| MySQL 协议 | 3307 | 兼容标准 MySQL 客户端（DBeaver、mysql2 等）|

### Raft 集群模式

```bash
# 节点 1
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

---

## 7. 执行优化

| 优化 | 说明 |
|------|------|
| 语句缓存 | FIFO 淘汰，上限 256 条，复用 AST 避免重复解析 |
| 索引下推 | `WHERE col = val / IN / < / <= / > / >= / BETWEEN` 走索引 |
| 哈希 JOIN | Equi-join 使用哈希表，O(n+m) 代替 O(n×m) |
| Top-N 排序 | `ORDER BY + LIMIT` 使用 `select_nth_unstable` 避免全排序 |
| 大候选集批量回表 | 候选数 ≥ 96 时一次全扫 + HashMap 回填，避免大量点查 |
| 预计算 ORDER BY 键 | 每行仅对 ORDER BY 表达式求值一次，避免比较期重复计算 |
| 批量插入 buffer | `insert_with_buf` 复用序列化 buffer，降低内存分配压力 |
| 自适应索引 | 热列自动建议/创建索引 |
| 不相关子查询缓存 | 检测到不含外部列的 IN 子查询，一次执行后转换为列表 |
| RLS 短路 | FTS 索引扫描后跳过 WHERE 重复过滤，避免误杀 OR 匹配行 |

---

## 8. REPL 使用

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

## 9. 构建与测试

```bash
cargo build
cargo test          # 单线程，避免 Windows 文件锁冲突
```

Windows 推荐用隔离脚本：

```powershell
.\scripts\check.ps1   # fmt + clippy + test
```

---

## 10. 目录结构

```
src/
  sql/              # tokenizer / parser / ast / sqlparser_adapter
  storage/          # pager / btree / cursor / compression
  vm/               # execute + ddl/dml/select/eval + lock + mvcc + data_transfer
  fulltext/         # BM25 tokenizer + 倒排索引
  raft/             # openraft + HTTP 网络层 + WAL 日志存储
  server/           # HTTP API (axum) + MySQL 协议 + TCP 服务器
  binlog/           # Binlog 记录与读取
  schema.rs         # 元数据管理（表/索引/视图/触发器/策略）
  types.rs          # 运行时值类型 + 序列化
  error.rs          # 统一错误类型
  varint.rs         # 变长整数编解码
  main.rs           # REPL + 服务器入口
tests/              # 集成测试（200+ 测试用例）
scripts/
  check.ps1         # fmt + clippy + test 一键检查
docs/
  PROJECT.md                     # 本文档
  API.md                         # 公开 API 参考（Rust crate）
  ADVANCED_SQL.md                # 高级 SQL 特性指南
  HTTP_API.md                    # HTTP REST API 服务器文档
  MYSQL_SERVER.md                # MySQL 有线协议服务器文档
  DISTRIBUTED.md                 # 分布式集群（Raft）部署与 API
  VECTOR_SEARCH_DESIGN.md        # 向量搜索引擎设计（HNSW + B-Tree 集成）
  FUNCTIONS.md                   # 内置函数完整参考手册
  COW_DOUBLE_SUPERBLOCK_DESIGN.md
  BINLOG_DESIGN.md
  SQLPARSER_REFACTOR_ANALYSIS.md
  optimization_roadmap.md
```

---

## 11. 已知边界与后续规划

- 当前聚焦 SQLite 风格核心子集，并非完整 PostgreSQL 兼容
- 多文件目录模式只对安全文件名的表生效（仅字母/数字/下划线）
- 旧单文件格式（`.db`）通过 `open_legacy` 向后兼容
- 后续规划：
  - WAL 完整实现（写放大优化、并发读）
  - 代价优化器（CBO）
  - 溢出页（超 4KB 的行）
  - B+ Tree 叶页双向链表（范围扫描优化）
  - LRU Buffer Pool
