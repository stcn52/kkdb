# KKDB 技术架构文档

本文档深入介绍 KKDB 底层采用的核心技术、算法与设计决策。

---

## 目录

1. [B-Tree 存储引擎](#1-b-tree-存储引擎)
2. [COW 双超块一致性模型](#2-cow-双超块一致性模型)
3. [WAL 预写日志](#3-wal-预写日志)
4. [MVCC 多版本并发控制](#4-mvcc-多版本并发控制)
5. [BM25 全文检索算法](#5-bm25-全文检索算法)
6. [HNSW 向量搜索算法](#6-hnsw-向量搜索算法)
7. [Raft 共识协议](#7-raft-共识协议)
8. [查询优化器](#8-查询优化器)
9. [Buffer Pool 管理](#9-buffer-pool-管理)
10. [技术对比](#10-技术对比)

---

## 1. B-Tree 存储引擎

### 1.1 页面格式

KKDB 使用类 SQLite 的页面格式：

```
┌──────────────────────────────┐
│  Page Header (8-12 bytes)    │
│  - page_type: u8             │  0x0D = 叶页, 0x05 = 内部页
│  - free_start: u16           │  空闲空间起始偏移
│  - cell_count: u16           │  单元格数量
│  - content_offset: u16       │  内容区起始
│  - right_child: u32 (内部页) │  最右子页指针
├──────────────────────────────┤
│  Cell Pointer Array          │  每个指针 2 bytes
│  [ptr_0, ptr_1, ..., ptr_n]  │
├──────────────────────────────┤
│  Free Space                  │  可用于新单元格
├──────────────────────────────┤
│  Cell Content Area           │  从页尾向页头增长
│  ┌─────────────────┐         │
│  │ Cell N           │         │
│  │ - left_child: u32│ (内部页)│
│  │ - key_len: varint│         │
│  │ - key_data       │         │
│  │ - payload_len    │         │
│  │ - payload        │         │
│  └─────────────────┘         │
└──────────────────────────────┘
```

### 1.2 关键特性

- **页大小**：默认 4096 字节，可配置 512 ~ 65536
- **溢出页**：payload 超过页面阈值时自动分链存储
- **叶页双向链表**：`next_leaf` + `prev_leaf` 支持正向/逆向范围扫描
- **前缀压缩**：同一叶页内的排序键共享公共前缀
- **变长编码**：行 ID 使用 LEB128 变长整数，节省空间

### 1.3 操作复杂度

| 操作 | 平均 | 最坏 |
|------|------|------|
| 点查找 | $O(\log n)$ | $O(\log n)$ |
| 插入 | $O(\log n)$ | $O(\log n)$ (含分裂) |
| 删除 | $O(\log n)$ | $O(\log n)$ |
| 范围扫描 | $O(\log n + k)$ | $O(\log n + k)$ |

其中 $n$ 为总行数，$k$ 为扫描范围内的行数。

---

## 2. COW 双超块一致性模型

### 2.1 工作原理

```
磁盘文件:
┌────────────────┬────────────────┬──────────────────┐
│  Superblock A  │  Superblock B  │  Data Pages ...  │
│  (gen=5, ✓)    │  (gen=4, ✓)    │                  │
└────────────────┴────────────────┴──────────────────┘

写入流程:
1. 写入新数据页 (COW: 不修改原页, 写新页)
2. 更新 Superblock B (gen=6, 指向新根页)
3. fsync
4. 此时 A=gen5(旧), B=gen6(新)

崩溃恢复:
- 读取两个超块
- 选择校验和正确且 generation 最大的超块
- 保证至少一个超块完整可用
```

### 2.2 崩溃安全保证

- **原子提交**：超块写入是 512 字节对齐的单次写操作
- **双超块冗余**：A/B 交替写入，任何崩溃点至少一个超块有效
- **校验和验证**：每个超块含 CRC32 校验和
- **Generation Number**：单调递增，用于选择最新有效超块

详细设计请参见 [COW 双超块设计文档](COW_DOUBLE_SUPERBLOCK_DESIGN.md)。

---

## 3. WAL 预写日志

### 3.1 帧格式

```
WAL Frame:
┌──────────────────────────────┐
│  Frame Header (24 bytes)     │
│  - page_number: u32          │
│  - size_after_commit: u32    │  非零表示事务结束
│  - salt_1: u32               │
│  - salt_2: u32               │
│  - checksum_1: u32           │  FNV-1a
│  - checksum_2: u32           │
├──────────────────────────────┤
│  Page Data (PAGE_SIZE bytes) │
└──────────────────────────────┘
```

### 3.2 检查点（Checkpoint）

WAL 检查点将已提交的 WAL 帧写回主数据库文件：

```
写操作 → WAL (append-only, 顺序写) → 累积帧
                                        ↓ (auto_checkpoint 阈值)
                                    Checkpoint → 写回主文件
                                        ↓
                                    WAL 截断
```

- **自动检查点**：帧数达到 `wal_auto_checkpoint` 阈值时自动执行
- **手动检查点**：通过 `VACUUM` 命令触发
- **Group Commit**：多事务合并一次 fsync，减少 I/O 开销

---

## 4. MVCC 多版本并发控制

### 4.1 版本模型

```
当前版本        Undo Log（版本链）
┌─────────┐    ┌─────────┐    ┌─────────┐
│ Row v3  │ →  │ Row v2  │ →  │ Row v1  │
│ txn=100 │    │ txn=50  │    │ txn=10  │
│ (active)│    │ (commit)│    │ (commit)│
└─────────┘    └─────────┘    └─────────┘
```

### 4.2 快照隔离

- **事务开始时**：记录当前所有活跃事务 ID 集合
- **读取时**：沿版本链查找对当前事务可见的最新版本
- **可见性规则**：
  - 版本的创建事务 ID < 快照开始时的活跃事务最小 ID → 可见
  - 版本的创建事务在快照的活跃集合中 → 不可见
  - 版本的创建事务 ID > 快照事务 ID → 不可见

### 4.3 GC（垃圾回收）

- **水位线计算**：所有活跃事务中最小的快照 ID
- **清理规则**：比水位线更旧的版本且非当前版本的 Undo 记录可安全清理
- **自动触发**：事务提交后检查是否需要 GC

---

## 5. BM25 全文检索算法

### 5.1 算法公式

$$\text{BM25}(q, d) = \sum_{t \in q} \text{IDF}(t) \cdot \frac{tf(t, d) \cdot (k_1 + 1)}{tf(t, d) + k_1 \cdot \left(1 - b + b \cdot \frac{|d|}{\text{avgdl}}\right)}$$

参数说明：
- $tf(t, d)$：词项 $t$ 在文档 $d$ 中的词频
- $|d|$：文档 $d$ 的长度（词数）
- $\text{avgdl}$：所有文档的平均长度
- $k_1 = 1.2$：词频饱和参数
- $b = 0.75$：文档长度归一化参数
- $\text{IDF}(t) = \ln\left(\frac{N - n(t) + 0.5}{n(t) + 0.5}\right)$

### 5.2 倒排索引存储

```
B-Tree 键格式:
\x00FTS\x01{IndexID}\x02{Token}\x03{RowID}

存储内容:
- Postings: (rowid, tf) 对列表
- IDF: 文档频率信息
- Global: 总文档数、平均文档长度
```

### 5.3 分词器

| 文本类型 | 分词方式 | 说明 |
|----------|----------|------|
| Latin/ASCII | 空白分割 + 小写化 | `"Hello World"` → `["hello", "world"]` |
| CJK（中日韩） | jieba-rs 分词 | `"数据库引擎"` → `["数据库", "引擎"]` |
| 混合文本 | 逐段识别 + 分别处理 | 自动检测脚本类型 |

- 内置英文停用词表（"the", "is", "at", ...）
- 内置中文停用词表（"的", "了", "在", ...）
- 小写归一化

---

## 6. HNSW 向量搜索算法

### 6.1 算法原理

HNSW（Hierarchical Navigable Small World）是一种分层图结构的近似最近邻搜索算法：

```
Layer 2:  [A] ————————————— [E]         (稀疏, 长距离连接)
           |                 |
Layer 1:  [A] —— [C] —— [E] —— [G]    (中等密度)
           |      |      |      |
Layer 0:  [A]-[B]-[C]-[D]-[E]-[F]-[G]  (稠密, 所有节点)
```

### 6.2 搜索过程

1. 从最高层的入口点开始
2. 在当前层贪心搜索: 移动到离查询点最近的邻居
3. 当无法在当前层进一步靠近时，下降到下一层
4. 在最底层（Layer 0）执行精确的 beam search，维护 TopK 候选集

### 6.3 参数影响

| 参数 | 增大效果 | 减小效果 |
|------|---------|---------|
| $M$ (邻居数) | 更精确，更多内存 | 更快，更少内存 |
| $ef_{construction}$ | 构建更慢，索引更好 | 构建更快，可能丢精度 |
| $ef_{search}$ | 搜索更精确，更慢 | 搜索更快，可能丢精度 |

### 6.4 复杂度

| 操作 | 时间复杂度 | 空间复杂度 |
|------|-----------|-----------|
| 插入 | $O(M \cdot \log n)$ | $O(M \cdot n)$ |
| 搜索 | $O(M \cdot \log n)$ | $O(ef_{search})$ |
| 删除 | $O(1)$ (惰性标记) | — |

详细设计请参见 [向量搜索设计文档](VECTOR_SEARCH_DESIGN.md)。

---

## 7. Raft 共识协议

### 7.1 核心保证

- **Leader 选举**：超时后自动选举，保证单一 Leader
- **日志复制**：Leader 将 SQL 命令（日志条目）复制到多数节点
- **安全性**：已提交的日志不会丢失，所有节点最终状态一致
- **活性**：只要多数节点存活，系统可以持续响应请求

### 7.2 KKDB Raft 实现

```
客户端请求 (SQL)
    ↓
Leader 节点
    ↓
1. 追加到本地 Raft 日志 (wal.log)
    ↓
2. 复制到 Follower 节点 (AppendEntries RPC)
    ↓
3. 等待多数确认 (Quorum)
    ↓
4. 提交日志，应用到状态机 (VM::execute_sql)
    ↓
5. 返回结果给客户端
```

### 7.3 KKDB Raft 架构

| 组件 | 实现 | 说明 |
|------|------|------|
| Raft 核心 | openraft v0.9 | 选举、日志复制、成员变更 |
| 日志存储 | `log_store.rs` | WAL 持久化 + vote.json |
| 状态机 | `state_machine.rs` | SQL 回放到 VM |
| 网络层 | `http_network.rs` | JSON-RPC over HTTP |
| 快照 | JSON 格式 | 全量 Schema + 数据序列化 |

### 7.4 分布式事务（2PC/3PC）

```
阶段 1 (Prepare):
Coordinator → Participant_1: PREPARE tx1
Coordinator → Participant_2: PREPARE tx1
Participant_1 → Coordinator: VOTE YES
Participant_2 → Coordinator: VOTE YES

阶段 2 (Commit):
Coordinator → Participant_1: COMMIT tx1
Coordinator → Participant_2: COMMIT tx1
```

详细文档请参见 [分布式集群文档](DISTRIBUTED.md)。

---

## 8. 查询优化器

### 8.1 优化流程

```
SQL 文本
    ↓ (解析)
AST
    ↓ (语义分析)
逻辑计划
    ↓ (规则优化)
├── 常量折叠
├── 谓词下推
├── 投影裁剪
├── 子查询去关联
└── DISTINCT 消除
    ↓ (代价优化)
├── 索引选择 (IndexScan vs SeqScan)
├── Join 算法选择 (NL/Hash/SortMerge)
├── Join 顺序枚举 (DPccp)
└── 统计信息 (直方图, 选择率)
    ↓
物理计划
    ↓ (执行)
结果集
```

### 8.2 代价模型

KKDB CBO 使用以下代价因子：

$$\text{Cost} = w_{io} \cdot C_{io} + w_{cpu} \cdot C_{cpu}$$

- $C_{io}$：I/O 代价（页面读取次数）
- $C_{cpu}$：CPU 代价（比较/计算次数）
- $w_{io} = 1.0$，$w_{cpu} = 0.01$（I/O 权重远大于 CPU）

索引扫描代价估算：

$$C_{index} = \text{index\_height} + \text{selectivity} \times \text{table\_pages}$$

全表扫描代价估算：

$$C_{seqscan} = \text{table\_pages}$$

### 8.3 统计信息

通过 `ANALYZE TABLE` 收集：

- **行数估计** (`row_count`)
- **列的不同值数** (`distinct_count`)
- **主键分布**
- **直方图**（等高直方图，默认 10 个桶）
- **NULL 占比**

---

## 9. Buffer Pool 管理

### 9.1 LRU-K(2) 算法

KKDB 使用 LRU-K(2) 页面替换策略：

- 追踪每个页面的**最近 2 次访问时间**
- 淘汰时选择"第 2 次最近访问"时间最久远的页面
- 相比简单 LRU，更能抵抗一次性扫描污染

### 9.2 特性

| 特性 | 说明 |
|------|------|
| 预读取 (Read-ahead) | 顺序扫描时预加载后续页面 |
| 写合并 (Write Coalescing) | 脏页批量刷盘 |
| Pin/Unpin | 固定关键页面不被淘汰 |
| 命中率统计 | `SHOW ENGINE STATUS` 可查看 |

---

## 10. 技术对比

### 10.1 KKDB vs 同类数据库

| 特性 | KKDB | SQLite | DuckDB | TiKV |
|------|------|--------|--------|------|
| 语言 | Rust | C | C++ | Rust |
| 存储格式 | B-Tree (SQLite 兼容) | B-Tree | 列存 | LSM-Tree |
| 并发模型 | MVCC + WAL | WAL | MVCC | Raft + MVCC |
| 全文检索 | BM25 内置 | FTS5 扩展 | 无 | 无 |
| 向量搜索 | HNSW 内置 | 无 | 无 | 无 |
| 分布式 | Raft 内置 | 无 | 无 | Raft 内置 |
| 网络协议 | MySQL + HTTP | 无 | 无 | gRPC |
| 嵌入式 | ✅ | ✅ | ✅ | ❌ |

### 10.2 适用场景

| 场景 | 推荐度 | 说明 |
|------|--------|------|
| 嵌入式应用 | ⭐⭐⭐⭐⭐ | 零外部依赖，单 crate 集成 |
| 全文检索 | ⭐⭐⭐⭐ | 内置 BM25 + 中文分词 |
| AI 向量检索 | ⭐⭐⭐⭐ | 内置 HNSW |
| 小型 Web 应用 | ⭐⭐⭐⭐ | MySQL 兼容 + HTTP API |
| 教学/原型 | ⭐⭐⭐⭐⭐ | 纯 Rust，代码可读性强 |
| 大规模 OLTP | ⭐⭐ | 单机性能有限 |
| 大规模 OLAP | ⭐⭐ | 行存格式非最优 |

---

## 相关文档

- [完全使用手册](USAGE.md) — 全部功能的综合参考
- [项目总览](PROJECT.md) — 架构概述与模块结构
- [COW 双超块设计](COW_DOUBLE_SUPERBLOCK_DESIGN.md) — 存储引擎详细设计
- [向量搜索设计](VECTOR_SEARCH_DESIGN.md) — HNSW 实现设计
- [Binlog 设计](BINLOG_DESIGN.md) — 日志格式与复制
- [分布式集群](DISTRIBUTED.md) — Raft 共识与集群管理
- [Rust API 参考](API.md) — Crate 公开接口
