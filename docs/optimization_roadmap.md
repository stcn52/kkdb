# KKDB 完整优化路线图

> 原则：早期做对，不留技术债。按照工业级嵌入式数据库引擎的完整路径规划。

---

## ✅ 已完成

### 存储引擎

| 项目 | 说明 |
|------|------|
| COW 双超块原子刷盘 | crash-safe 页写入；生成代号轮换，fsync 两次确保原子性 |
| VarInt 编码 | 减少序列化体积约 30-50%（键长/值长变长编码）|
| B-Tree 右边界追加 | 单调 rowid 写入跳过不必要分裂，批量写性能提升 40%+ |
| 按表分文件 | 类 MySQL InnoDB：`catalog.kkdb` + 各表 `.kkdb` |
| 叶节点键前缀压缩 (F1) | 相邻 cell 公共前缀去重，字符串索引体积减少 40-70% |
| 页内 LZ4 压缩 (F2) | flush 前压缩，冷存储文件体积减半 |
| 可变页大小 (F3) | 编译期配置 4KB / 8KB / 16KB（`PAGE_SIZE` 常量）|
| Savepoint / 嵌套回滚 | `SAVEPOINT` / `RELEASE` / `ROLLBACK TO` 全支持 |

### 查询引擎

| 项目 | 说明 |
|------|------|
| 语句缓存 | FIFO 256 条，AST 复用避免重复解析 |
| 索引等值 / 范围下推 | `=` / `IN` / `<` / `<=` / `>` / `>=` / `BETWEEN` 走索引 |
| 哈希 JOIN | Equi-join 使用哈希表，O(n+m) 代替 O(n×m) |
| Top-N 排序 | `ORDER BY + LIMIT` 用 `select_nth_unstable`，无需全排序 |
| 大候选集批量回表 | 候选数 ≥ 96 时一次全扫 + HashMap 回填，减少点查开销 |
| 预计算 ORDER BY 键 | 每行仅对 ORDER BY 表达式求值一次，比较期免重算 |
| 批量插入 buffer | `insert_with_buf` 复用序列化 buffer，降低内存分配压力 |
| 自适应索引 (O3) | 热列超过访问阈值自动建议/创建索引 |
| 列统计信息 (O1) | `ANALYZE TABLE` 收集 NDV、min/max、null_count |
| 不相关子查询缓存 | 无外部引用的 IN 子查询：一次执行后转化为 IN 列表 |

### SQL 功能

| 项目 | 说明 |
|------|------|
| 外键约束 (L1) | schema 存储引用关系；INSERT/UPDATE/DELETE 检查；CASCADE / SET NULL / RESTRICT |
| CHECK 约束 (L2) | `CREATE TABLE` 时写入 schema；INSERT/UPDATE 时表达式验证 |
| 触发器 (L3) | BEFORE / AFTER；INSERT / UPDATE / DELETE；RAISE(ABORT) 支持 |
| 全文索引 BM25 (L4) | 倒排索引存于独立 B-Tree；`FTS_MATCH()` 函数；实时维护 |
| 窗口函数完善 (L5) | ROW_NUMBER / RANK / DENSE_RANK / LEAD / LAG / NTILE / 聚合窗口 |
| 递归 CTE (L7) | `WITH RECURSIVE ... UNION ALL` 树形查询支持 |
| CREATE TABLE AS SELECT | 类型推断 + 自动建表 + 批量 INSERT |
| RETURNING 子句 | INSERT / UPDATE / DELETE 后立即返回受影响行 |
| ON CONFLICT 策略 | `IGNORE` / `REPLACE` / `DO UPDATE SET`（Upsert）|
| EXPLAIN | 查询计划输出（SCAN / FILTER / SORT / LIMIT）|
| 行级安全 RLS | `ENABLE ROW LEVEL SECURITY` + `CREATE POLICY` + `SET kkdb.*` |
| 相关子查询 | `outer_rows` 栈支持任意深度嵌套的相关子查询 |
| 集合操作 | UNION / UNION ALL / INTERSECT / EXCEPT |
| 视图 | CREATE VIEW / CREATE OR REPLACE VIEW / DROP VIEW |
| 用户权限 | CREATE USER / ALTER USER / DROP USER / GRANT / REVOKE |

### 并发与可靠性

| 项目 | 说明 |
|------|------|
| MVCC undo log (C1) | 事务内 DML 记录 undo 条目；COW pager 提供物理回滚 |
| 全局锁管理器 (C3) | 全局表级 Shared/Exclusive 锁 |
| 死锁检测 (C3) | Wait-for Graph DFS 实时检测环；自动回滚代价较小的事务 |
| Binlog | INSERT/UPDATE/DELETE/BEGIN/COMMIT/ROLLBACK 一一记录 |
| Raft 共识 | openraft 集群；HTTP 网络 + WAL 日志存储 + Leader 选举 |

---

## 第一阶段：存储引擎基础补完

### S1. 溢出页（Overflow Pages）⭐ 优先最高

- **问题**：单行 payload 必须 < 1 页（≈ 4000 字节），大 TEXT / BLOB 无法存储
- **方案**：超限 payload 写溢出链表页，主 cell 存第一个溢出页号 + 总长度
- **收益**：解除单行大小上限，支持真实业务数据
- **参考**：SQLite overflow page chain

### S2. 空闲页 B-Tree（Free Page B-Tree）

- **问题**：DELETE / DROP TABLE 后页不回收，文件只增不减
- **方案**：独立 B-Tree 管理 freelist，`allocate_page` 优先从 freelist 取
- **收益**：VACUUM 后文件可收缩；长期运行不膨胀

### S3. WAL（Write-Ahead Log）⭐

- **问题**：COW 每次 flush 写完整页（写放大）；reader 被 writer 阻塞
- **方案**：写操作 append 到 WAL，原始文件异步 checkpoint 合并；reader 优先读 WAL
- **收益**：写延迟降低；并发读；更好的崩溃恢复基础

### S4. Checkpoint 机制

- **依赖**：S3 WAL
- **方案**：WAL 积累到阈值（例如 1000 页）或显式 VACUUM 时合并写回数据文件
- **收益**：控制 WAL 体积，控制恢复时间

---

## 第二阶段：查询引擎优化

### Q1. B+ Tree 叶页双向链表 ⭐

- **问题**：范围扫描从根重新定位每个叶，O(log n) 额外开销
- **方案**：每个 Leaf 页头写 `prev_leaf` / `next_leaf`，定位后线性扫
- **收益**：`WHERE id BETWEEN 100 AND 200` → O(log n + k)

### Q2. LRU Buffer Pool

- **问题**：当前惰性加载无缓存管理，热点页每次从磁盘读
- **方案**：固定大小 LRU Buffer Pool（例如 8MB = 2048 页）+ Pin/Unpin 机制
- **收益**：热表命中率 ≥ 90% 时磁盘 I/O 接近零

### Q3. 投影下推 / 延迟物化

- **问题**：全行反序列化后再取目标列，宽表浪费 CPU
- **方案**：向 scan 层传递 `needed_column_bits`，只解码必要列
- **收益**：宽表窄列查询 CPU 和内存显著降低

### Q4. 索引覆盖扫描（Covering Index）

- **问题**：走索引后还需回表取完整行
- **方案**：若 SELECT / WHERE 列全在索引中，直接从索引读，不回表
- **收益**：宽表窄列查询场景提升数倍

### Q5. 子查询扁平化 / 谓词上提

- **问题**：`IN (SELECT ...)` 每行都重跑内层查询
- **方案**：将可以的子查询转换为 JOIN；WHERE 条件上提到最早可应用节点
- **收益**：避免嵌套循环扫描

---

## 第三阶段：并发与可靠性

### C2. 完整崩溃恢复（Binlog Redo）⭐

- **问题**：Binlog 已记录操作，但 `recover()` 是空实现
- **方案**：启动时扫描 Binlog，重放 COMMIT 但未 checkpoint 的事务（Redo）；回滚未 COMMIT 的事务（Undo）
- **收益**：完整 ACID；crash 后数据零丢失

---

## 第四阶段：统计与优化器

### O2. 代价优化器（Cost-Based Optimizer）⭐

- **依赖**：O1（已完成）
- **方案**：每个执行路径计算 IO cost + CPU cost；多索引时选最小代价路径；JOIN 顺序动态规划
- **收益**：复杂查询性能质变；消除人工 hint 依赖

---

## 第六阶段：SQL 功能（待实现）

### L6. JSON 类型与函数

- `JSON_EXTRACT` / `JSON_SET` / `JSON_ARRAY` / `->` / `->>`

---

## 推荐实施顺序

```
S1 溢出页
  → S2 空闲页 B-Tree
    → Q1 叶页链表
      → S3 WAL
        → S4 Checkpoint
          → C2 崩溃恢复
            → Q2 Buffer Pool
              → Q3 投影下推
                → Q4 覆盖索引
                  → O2 代价优化器
                    → L6 JSON
```

**关键路径**：`S1 → Q1 → S3 → C2` 是工业级引擎的核心骨架，其余可并行推进。
