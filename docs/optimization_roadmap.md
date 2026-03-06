# KKDB 完整最优化方案

> 原则：早期做对，不留技术债。按照工业级嵌入式数据库引擎的完整路径规划。

---

## ✅ 已完成

| 项目 | 说明 |
|------|------|
| COW 双超块原子刷盘 | crash-safe 页写入 |
| VarInt 编码 | 减少序列化体积 |
| B-Tree 右边界追加 | 单调 rowid 写入跳过分裂 |
| 按表分文件 | 类 MySQL InnoDB，catalog.kkdb + 各表.kkdb |
| 索引下推 | `=` / `IN` / 范围 / `BETWEEN` 走索引 |
| Top-N 排序 | ORDER BY + LIMIT 不全排序 |
| 大候选集批量回表 | 全扫 + HashMap，避免大量点查 |
| 语句缓存 | FIFO 256 条，AST 复用 |
| [insert_with_buf](file:///e:/ai/kkdb/src/storage/btree.rs#89-111) | 批量写复用 buffer |

---

## 第一阶段：存储引擎基础补完

### S1. 溢出页（Overflow Pages）⭐ 优先最高
- **问题**：单行 payload 必须 < 1 页（≈4000 字节），大 TEXT / BLOB 无法存储
- **方案**：超限 payload 写入溢出链表页，主 cell 存第一个溢出页号 + 总长度
- **收益**：解除单行大小上限，支持真实业务数据
- **参考**：SQLite overflow page chain

### S2. 空闲页 B-Tree（Free Page B-Tree）
- **问题**：DELETE / DROP TABLE 后页不回收，文件只增不减
- **方案**：用独立 B-Tree 管理 freelist，[allocate_page](file:///e:/ai/kkdb/src/storage/pager.rs#791-832) 优先从 freelist 取
- **收益**：VACUUM 后文件可收缩；长期运行不膨胀

### S3. WAL（Write-Ahead Log）⭐
- **问题**：COW 每次 flush 写完整页（写放大）；reader 被 writer 阻塞；无法并发读
- **方案**：  
  - 写操作 append 到 WAL 文件，原始数据文件异步 checkpoint 合并  
  - Reader 优先读 WAL，找不到再读数据文件（版本链）  
  - Writer 独占，Reader 可多个并发  
- **收益**：写延迟降低；并发读；更好的崩溃恢复基础
- **参考**：SQLite WAL 模式 / PostgreSQL WAL

### S4. Checkpoint 机制
- **依赖**：S3 WAL
- **方案**：WAL 积累到阈值（如 1000 页）或显式 VACUUM 时，将 WAL 合并写回数据文件
- **收益**：控制 WAL 体积，控制恢复时间

---

## 第二阶段：查询引擎优化

### Q1. B+ Tree 叶页双向链表 ⭐
- **问题**：范围扫描从根重新定位每个叶，`O(log n)` 额外开销
- **方案**：每个 Leaf 页头写 `prev_leaf` / `next_leaf` 页号，定位后线性扫
- **收益**：`WHERE id BETWEEN 100 AND 200` 变为 `O(log n + k)`，k 为结果数
- **影响**：split / merge 时需维护链表指针

### Q2. LRU Buffer Pool
- **问题**：当前惰性加载无缓存管理，热点页每次都从磁盘读
- **方案**：固定大小 LRU Buffer Pool（如 8MB = 2048 页）；Pin/Unpin 机制防止 evict 正在使用的页
- **收益**：热表命中率 ≥ 90% 时磁盘 I/O 接近零

### Q3. 投影下推 / 延迟物化
- **问题**：全行反序列化后再取目标列，SELECT 少量列时浪费 CPU
- **方案**：向 scan 层传递 `needed_column_bits`，只解码必要列（VarInt 跳过不需要的字段）
- **收益**：宽表查询 CPU 和内存显著降低

### Q4. 索引覆盖扫描（Covering Index）
- **问题**：走索引后还需回表取完整行
- **方案**：若 SELECT / WHERE 列全在索引中，直接从索引读，不回表
- **收益**：宽表窄列查询场景提升数倍

### Q5. LIMIT 提前终止（已部分完成，继续完善）
- **方案**：Full-scan / 索引扫描 / 聚合中传递剩余 limit，一旦满足立即中断
- **收益**：`SELECT * FROM t LIMIT 1` 扫一行即停

### Q6. 子查询扁平化 / 谓词上提
- **问题**：`IN (SELECT ...)` 当前执行为相关子查询，每行都重跑
- **方案**：将可以的子查询转换为 JOIN；将 WHERE 条件上提到最早可应用的节点
- **收益**：避免嵌套循环扫描

---

## 第三阶段：并发与可靠性

### C1. MVCC（多版本并发控制）⭐
- **问题**：无并发控制；多线程访问不安全
- **方案**：
  - 每行附加 [(xmin, xmax)](file:///e:/ai/kkdb/src/vm/execute.rs#29-46) 事务版本号
  - Reader 按 snapshot isolation 读对其可见的最新版本
  - Writer 独占写锁；Reader 无锁读旧版本
- **收益**：真正的读写并发；读不阻塞写，写不阻塞读
- **参考**：PostgreSQL MVCC

### C2. 完整崩溃恢复（Binlog Redo）⭐
- **问题**：Binlog 已记录操作，但 [recover()](file:///e:/ai/kkdb/src/binlog/mod.rs#173-182) 是空实现
- **方案**：启动时扫描 Binlog，重放 COMMIT 但未 checkpoint 的事务（Redo）；回滚未 COMMIT 的事务（Undo）
- **收益**：完整 ACID；crash 后数据零丢失

### C3. 死锁检测
- **依赖**：C1 MVCC
- **方案**：等待图（Wait-for Graph）周期检测环，选代价最小事务为 victim 回滚
- **收益**：多事务并发时不死锁

---

## 第四阶段：统计与优化器

### O1. 列统计信息（Column Statistics）
- **方案**：`ANALYZE TABLE t` 收集：`ndv`（distinct 数）、`min/max`、等高直方图；存于 `catalog.kkdb` 统计表
- **收益**：选择率估算从"盲目"变为"有据"

### O2. 代价优化器（Cost-Based Optimizer）⭐
- **依赖**：O1
- **方案**：
  - 每个执行路径计算 `IO cost + CPU cost`
  - 多索引时选最小代价路径
  - JOIN 顺序枚举（动态规划 / 贪心）
- **收益**：复杂查询性能质变；消除人工 hint 依赖

### O3. 自适应索引决策
- **方案**：运行时统计每个索引的实际命中率，低命中率索引降权；支持热点自动建议索引
- **收益**：查询性能随数据分布自适应优化

---

## 第五阶段：压缩与格式升级

### F1. 索引键前缀压缩
- **方案**：B-Tree 叶页内相邻 cell 的键共同前缀只存一次（delta encoding）
- **收益**：字符串索引体积减少 40-70%

### F2. 页内数据压缩（LZ4）
- **方案**：flush 前压缩，load 后解压；Buffer Pool 存原始数据
- **收益**：热数据多时磁盘 I/O 减少；冷存储文件体积减半

### F3. 可变页大小（4KB / 8KB / 16KB）
- **方案**：创建数据库时指定，写入文件头；Pager 按配置读写
- **收益**：宽行 / 大字段减少溢出链深度；可针对场景调优

---

## 第六阶段：SQL 功能完善

### L1. 外键约束（Foreign Key）
- schema 存储引用关系；INSERT/UPDATE/DELETE 时检查参照完整性；CASCADE / SET NULL 动作

### L2. CHECK 约束
- CREATE TABLE 时写入 schema；INSERT/UPDATE 时表达式验证

### L3. 触发器（BEFORE / AFTER）
- DDL 注册触发体；DML 执行前后回调

### L4. 全文索引（Full-Text Search）
- 倒排索引存于独立 `<table>_fts.kkdb`；支持 `MATCH ... AGAINST ...`

### L5. 窗口函数完善
- 补全 `PARTITION BY` + `ROWS / RANGE BETWEEN`；支持 `LEAD / LAG / NTILE` 等

### L6. JSON 类型与函数
- `JSON_EXTRACT` / `JSON_SET` / `JSON_ARRAY` / `->` / `->>`

### L7. 递归 CTE（WITH RECURSIVE）
- 树形结构查询支持

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
              → C1 MVCC
                → Q3 投影下推
                  → Q4 覆盖索引
                    → O1 列统计
                      → O2 代价优化器
                        → F1-F3 压缩格式
                          → L1-L7 SQL 功能
```

**关键路径**：`S1 → Q1 → S3 → C2 → C1` 是工业级引擎的核心骨架，其余可并行推进。
