# KKDB 向量搜索引擎设计文档

KKDB 向量搜索（Vector Search）引擎是继全文检索（BM25）之后的第二个专用检索引擎，直接集成于现有存储与 SQL 体系内部，无需部署外部向量数据库（如 Pinecone、Milvus、Qdrant）。

本文档涵盖：架构设计、数据存储模型、HNSW 索引算法、SQL 接口、分布式集成，以及分阶段实现计划。

---

## 目录

1. [设计目标与约束](#1-设计目标与约束)
2. [整体架构](#2-整体架构)
3. [存储模型（B-Tree 编码）](#3-存储模型b-tree-编码)
4. [HNSW 索引算法](#4-hnsw-索引算法)
5. [SQL 接口设计](#5-sql-接口设计)
6. [与现有模块的集成点](#6-与现有模块的集成点)
7. [分布式集成（Raft）](#7-分布式集成raft)
8. [性能特性与局限](#8-性能特性与局限)
9. [分阶段实现计划](#9-分阶段实现计划)
10. [源码目录规划](#10-源码目录规划)

---

## 1. 设计目标与约束

### 目标

| 目标 | 说明 |
|------|------|
| **SQL 原生** | 向量索引通过 DDL 创建，查询通过内置函数完成，无需额外协议 |
| **与 B-Tree 共存** | 向量原始数据存入现有 B-Tree（特殊 key 前缀），无需新文件格式 |
| **HNSW 近似最近邻** | 内存中维护 HNSW 图，实现 O(log N) 查询复杂度 |
| **DML 自动维护** | INSERT / UPDATE / DELETE 自动同步向量索引，无需手工调用 |
| **Raft 可复制** | 向量写入作为普通 SQL 语句进入 Raft 日志，Follower 重建后恢复 HNSW 图 |
| **RAG 友好** | 直接服务 LLM 嵌入向量匹配，可与 SQL 条件组合过滤 |

### 约束

- **不实现 GPU 加速**：纯 CPU HNSW，适合中小规模（百万级向量以内）
- **不实现磁盘 HNSW**：图结构常驻内存，启动时从 B-Tree 重建（向量数据已持久化）
- **维度上限**：无硬性限制，但 dim ≥ 4096 时内存压力显著（每条向量 16 KB）
- **距离度量**：初期支持余弦相似度（Cosine）和欧几里得距离（L2），后续可扩展

---

## 2. 整体架构

```
客户端 SQL
  │
  │  SELECT ..., VEC_SEARCH('table', 'idx', '[0.1, ...]') AS score
  ▼
sql/sqlparser_adapter.rs
  │  解析 VEC_SEARCH 函数调用
  ▼
vm/exec_select.rs
  │  识别 VectorSearch 表达式
  │  调用 VectorIndex::search(query_vec, top_k)
  ▼
src/vector/
  ├── mod.rs       — VectorIndex trait + 管理入口
  ├── hnsw.rs      — HNSW 图：插入 / 删除 / KNN 搜索
  ├── index.rs     — B-Tree key / value 编码（存储层）
  └── distance.rs  — 余弦相似度 / L2 距离计算
  │
  ├─ 查询路径：HNSW 图（内存）→ 返回 (rowid, score) 列表
  └─ 写入路径：B-Tree（持久化原始向量） + HNSW 图（内存追加）
  │
  ▼
storage/pager.rs + storage/btree.rs
  │  向量数据以特殊前缀 key 存储在表对应的 .kkdb 文件中
  ▼
磁盘文件（{table}.kkdb）
```

### 与全文检索引擎的对比

| 维度 | 全文检索（BM25）| 向量搜索（HNSW）|
|------|--------------|--------------|
| 索引结构 | 倒排索引（B-Tree 存储）| HNSW 图（内存）+ 向量数据（B-Tree）|
| 查询复杂度 | O(posting list 长度) | O(log N)（近似）|
| SQL 函数 | `FTS_MATCH(table, idx, query)` | `VEC_SEARCH(table, idx, vec)` |
| DDL | `CREATE FULLTEXT INDEX` | `CREATE VECTOR INDEX` |
| 持久化 | 全量在 B-Tree | 向量数据在 B-Tree，图在内存（重建）|
| 适用场景 | 关键字语义模糊匹配 | 嵌入向量语义相似度匹配（RAG）|

---

## 3. 存储模型（B-Tree 编码）

向量数据使用特殊 key 前缀存入表对应的 `.kkdb` B-Tree 文件（与用户数据、FTS 数据共享同一 Pager，通过前缀隔离命名空间）。

### Key 格式

```
\x00VEC\x01{index_id: u32 BE}\x02{row_id: u64 BE}
```

| 字段 | 字节数 | 说明 |
|------|--------|------|
| `\x00VEC\x01` | 5 | 命名空间前缀（不与用户 key 冲突）|
| `index_id` | 4 | 向量索引 ID（u32，大端序）|
| `\x02` | 1 | 分隔符 |
| `row_id` | 8 | 目标行的 rowid（u64，大端序）|

### Value 格式

```
[dim: u32 LE][f32 × dim]
```

| 字段 | 字节数 | 说明 |
|------|--------|------|
| `dim` | 4 | 向量维度（u32，小端序）|
| `f32 × dim` | `dim × 4` | 向量数据（IEEE 754 单精度，小端序）|

**示例**：dim=4 的向量 `[0.1, 0.2, 0.3, 0.4]` 存储为 20 字节。

### 索引元信息 Key（全局统计）

```
\x00VEC\x01{index_id: u32 BE}\x03META
```

Value 格式（32 字节）：

```
[dim: u32 LE][distance_type: u8][total_vectors: u64 LE][reserved: 19 bytes]
```

`distance_type`：`0x01` = Cosine，`0x02` = L2

### 扫描前缀

扫描某个索引的所有向量：

```rust
let prefix = vec![0x00, b'V', b'E', b'C', 0x01, ...index_id_bytes..., 0x02];
btree.scan_prefix(&prefix)
```

---

## 4. HNSW 索引算法

### 简介

HNSW（Hierarchical Navigable Small World）是目前最主流的近似最近邻（ANN）算法，时间复杂度 O(log N)，召回率高（通常 > 95%）。

```
Layer 2: 1 ──────── 5
Layer 1: 1 ── 3 ── 5 ── 8
Layer 0: 1─2─3─4─5─6─7─8─9  (全量节点，图密度最高)
```

查询时从最高层入口点贪心下降，在 Layer 0 精细搜索 K 近邻。

### 核心参数

| 参数 | 推荐值 | 含义 |
|------|--------|------|
| `M` | 16 | 每个节点在 Layer 0 的最大邻居数 |
| `M_max0` | 32 | Layer 0 最大邻居数（= 2M）|
| `ef_construction` | 200 | 构建时候选集大小（越大质量越高、越慢）|
| `ef_search` | 50 | 查询时候选集大小（影响召回率 vs 速度）|
| `level_mult` | `1 / ln(M)` ≈ 0.36 | 随机层高概率系数 |

### 数据结构（Rust）

```rust
// src/vector/hnsw.rs

pub struct HnswGraph {
    /// 每个节点的各层邻居列表: node_id → [layer → [neighbor_ids]]
    nodes: HashMap<u64, Vec<Vec<u64>>>,
    /// 向量数据: node_id → Vec<f32>（从 B-Tree 加载到内存）
    vectors: HashMap<u64, Vec<f32>>,
    /// 当前最高层的入口节点 ID
    entry_point: Option<u64>,
    /// 当前最高层数
    max_level: usize,
    /// 超参数
    pub m: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub distance: DistanceMetric,
}

pub enum DistanceMetric {
    Cosine,
    L2,
}
```

### 插入流程

```
1. 为新节点随机生成层高 l（指数分布：P(l) = exp(-l / level_mult)）
2. 从入口点在 Layer max_level .. l+1 中贪心下降，找到第 l 层最近邻
3. 在 Layer l .. 0 中：
   a. 用 ef_construction 大小的候选集找 M 个最近邻
   b. 双向连接：新节点 ↔ 邻居
   c. 若邻居度数超过 M_max，裁剪（保留最近的 M 个）
4. 若 l > max_level：更新入口点和 max_level
```

### 搜索流程

```
1. 从入口点在 Layer max_level .. 1 中贪心下降（每层 ef=1）
2. 在 Layer 0 用 ef_search 大小候选集进行精细搜索
3. 返回候选集中 top-K（按距离排序）
```

### 删除策略

HNSW 标准算法不支持直接删除。KKDB 采用 **懒惰删除（Lazy Delete）**：

- 维护 `deleted: HashSet<u64>` 标记已删除的 rowid
- 查询时过滤掉已删除节点
- 触发重建阈值：`deleted.len() > 0.2 * nodes.len()` 时异步全量重建（从 B-Tree 重扫）

---

## 5. SQL 接口设计

### DDL

#### 创建向量索引

```sql
-- 余弦相似度索引（默认，适合嵌入向量）
CREATE VECTOR INDEX idx_embedding ON articles (embedding) DIM 1536;

-- 欧几里得距离索引
CREATE VECTOR INDEX idx_vec ON products (feature_vec) DIM 128 DISTANCE L2;
```

**约束**：
- `DIM` 必须显式指定，用于验证写入维度一致性
- 一个列只能有一个向量索引
- 向量列数据类型为 `BLOB`（存储序列化的 `f32` 数组）

#### 删除向量索引

```sql
DROP INDEX idx_embedding ON articles;
```

### DML（自动维护）

```sql
-- 插入带向量的行（向量列用十六进制 BLOB 或函数编码）
INSERT INTO articles (id, title, embedding)
VALUES (1, 'Rust 数据库', VEC('[0.12, 0.34, ..., 0.56]'));

-- 更新向量（自动从旧位置删除、插入新向量）
UPDATE articles SET embedding = VEC('[0.99, ...]') WHERE id = 1;

-- 删除行（向量索引自动懒惰删除）
DELETE FROM articles WHERE id = 1;
```

`VEC(json_array_string)` 是内置标量函数，将 JSON 数字数组解析为 `BLOB` 格式向量。

### 查询

#### 基本 KNN 查询

```sql
-- 返回最相似的 10 条文章（余弦相似度，值越接近 1 越相似）
SELECT id, title,
       VEC_SEARCH('articles', 'idx_embedding', VEC('[0.1, 0.2, ..., 0.9]')) AS score
FROM articles
WHERE VEC_SEARCH('articles', 'idx_embedding', VEC('[0.1, 0.2, ..., 0.9]')) > 0.85
ORDER BY score DESC
LIMIT 10;
```

#### 混合查询（向量 + SQL 过滤）

```sql
-- 在向量相似度基础上叠加 SQL 条件（Pre-filtering）
SELECT id, title, score
FROM (
  SELECT id, title,
         VEC_SEARCH('articles', 'idx_embedding', VEC('[...]')) AS score
  FROM articles
  WHERE category = 'tech'   -- 先 SQL 过滤，再向量搜索
) t
WHERE score > 0.8
ORDER BY score DESC
LIMIT 5;
```

#### 混合检索（向量 + BM25 融合）

```sql
-- RRF (Reciprocal Rank Fusion) 风格的混合检索
SELECT id, title,
       (0.5 * VEC_SEARCH('articles', 'idx_emb', VEC('[...]')) +
        0.5 * FTS_MATCH('articles', 'idx_ft', 'rust database')) AS hybrid_score
FROM articles
ORDER BY hybrid_score DESC
LIMIT 10;
```

### 工具函数

| 函数 | 说明 |
|------|------|
| `VEC(json)` | JSON 数字数组 → BLOB 向量（`[0.1, 0.2, ...]`）|
| `VEC_DIM(blob)` | 返回向量维度 |
| `VEC_DISTANCE(blob1, blob2, metric)` | 计算两向量距离（`'cosine'` / `'l2'`）|
| `VEC_SEARCH(table, index, query_vec)` | KNN 搜索，返回相似度分数 |
| `VEC_NORMALIZE(blob)` | L2 归一化（余弦相似度预处理）|

---

## 6. 与现有模块的集成点

### 6.1 Schema 元数据（`src/schema.rs`）

新增 `VectorIndex` 结构体，存入 `Schema.vector_indexes`：

```rust
pub struct VectorIndex {
    pub name: String,
    pub table: String,
    pub column: String,
    pub dim: u32,
    pub distance: DistanceMetric,
    pub index_id: u32,       // 分配的数值 ID（B-Tree key 中使用）
    pub hnsw: Arc<RwLock<HnswGraph>>, // 内存中的图
}
```

### 6.2 DDL 执行（`src/vm/exec_ddl.rs`）

- `CREATE VECTOR INDEX` → 分配 `index_id`，写元信息到 B-Tree，初始化空 `HnswGraph`，扫描现有行批量插入
- `DROP INDEX`（向量类型）→ 从 B-Tree 删除 `\x00VEC\x01{id}\x02*` 前缀的所有 key，释放内存图

### 6.3 DML 执行（`src/vm/exec_dml.rs`）

与 FTS 维护代码完全平行（在 FTS 更新之后追加）：

```rust
// INSERT 后
for vidx in schema.vector_indexes_on(table) {
    if let Some(vec_blob) = row.get(vidx.column_idx) {
        let vec = parse_blob_to_f32(vec_blob)?;
        // 1. 写 B-Tree
        btree.insert(vec_key(vidx.index_id, rowid), encode_vec(&vec));
        // 2. 更新内存 HNSW
        vidx.hnsw.write().insert(rowid, vec);
    }
}
```

### 6.4 SELECT 执行（`src/vm/exec_select.rs` + `eval_expr.rs`）

`VEC_SEARCH` 函数处理流程：

```
1. 解析函数参数：table_name, index_name, query_vec_blob
2. 在 schema 中查找对应 VectorIndex
3. 调用 hnsw.read().search(query_vec, ef_search) → Vec<(rowid, score)>
4. 构建 rowid → score 的 HashMap，在行评估时按 rowid 返回 score
5. WHERE 子句和 ORDER BY 的过滤/排序由上层正常处理（利用已有框架）
```

### 6.5 启动加载（`src/vm/execute.rs`）

`VM::open()` 时扫描 catalog 中的向量索引定义，然后扫描对应 B-Tree 前缀重建 HNSW 图：

```rust
for vidx in schema.vector_indexes.values() {
    let mut graph = HnswGraph::new(vidx.m, vidx.ef_construction, vidx.distance.clone());
    let prefix = vec_prefix(vidx.index_id);
    for (key, val) in btree.scan_prefix(&prefix) {
        let rowid = decode_rowid_from_key(&key);
        let vec = decode_blob_to_f32(&val);
        graph.insert(rowid, vec);
    }
    vidx.hnsw = Arc::new(RwLock::new(graph));
}
```

---

## 7. 分布式集成（Raft）

### 写入路径

向量写入完全透明地走现有 Raft 路径：

```
客户端 INSERT (含 VEC 列)
  ↓
KkdbNode::write(KkdbRequest { sql: "INSERT INTO ...", user_id })
  ↓
openraft → 多数派确认
  ↓
KkdbStateMachine::apply()
  ↓
VM::execute_sql()  ← 同普通 SQL，exec_dml 内部自动维护向量索引
  ↓
B-Tree 持久化向量数据 + 内存 HNSW 图更新
```

**无需对 Raft 层做任何修改。**

### 快照与恢复

HNSW 图不序列化进快照（语句列表快照中已包含所有 INSERT/UPDATE），Follower 安装快照后：

1. 重放快照中的所有 SQL（含向量写入）→ B-Tree 恢复
2. VM 内部自动重建 HNSW 图（等同于启动加载逻辑）

**重建耗时估算**：100 万条 dim=1536 向量，HNSW 插入约 10-30 秒（一次性），正常业务无感知。

### Follower 读

Follower 的 `ensure_linearizable()` 保证 apply 到最新 commit 后，本地 HNSW 图即是最新状态，直接可以服务向量查询。

---

## 8. 性能特性与局限

### 性能参考（CPU，dim=1536，M=16）

| 规模 | 插入吞吐 | KNN 延迟（top-10）| 内存占用 |
|------|----------|-----------------|--------|
| 10 万条 | ~5,000 vec/s | ~5 ms | ~1 GB |
| 100 万条 | ~3,000 vec/s | ~15 ms | ~10 GB |
| 1,000 万条 | ~1,500 vec/s | ~40 ms | ~100 GB |

> 以上为估算值，实际受 CPU 核数和内存带宽影响。dim=384（MiniLM）的场景内存约为 1/4。

### 局限性

| 局限 | 说明 |
|------|------|
| **纯内存图** | HNSW 图常驻内存，规模受限于单机 RAM |
| **启动重建** | 图不持久化，重启时需重建（可用后台线程并发进行）|
| **近似搜索** | 非精确最近邻，高召回需调大 `ef_search` |
| **单列索引** | 当前不支持多列组合向量索引 |
| **无 GPU 加速** | 纯 CPU 实现，高并发查询瓶颈在 CPU |

### 调优建议

- **小模型（dim ≤ 384）**：`M=16, ef_construction=100, ef_search=40`，适合嵌入式部署
- **中等规模（dim=768）**：`M=16, ef_construction=200, ef_search=64`，平衡召回与速度
- **大模型（dim=1536）**：`M=32, ef_construction=400, ef_search=128`，高召回优先
- **开启 LZ4 压缩**：B-Tree 的向量页开启 LZ4 可节省 20-30% 磁盘空间

---

## 9. 分阶段实现计划

### Phase 1：内存 HNSW + 基础 SQL（约 2 周）

**目标**：可用的端到端向量查询，暂不持久化

- [ ] `src/vector/hnsw.rs`：完整 HNSW 实现（插入 / 搜索 / 懒惰删除）
- [ ] `src/vector/distance.rs`：Cosine + L2 距离
- [ ] `src/vector/mod.rs`：`VectorIndex` 管理接口
- [ ] `sql/` 层：解析 `VEC_SEARCH(table, idx, vec)` 和 `VEC(json)` 函数
- [ ] `vm/eval_expr.rs`：执行 `VEC_SEARCH`（纯内存，索引手动注册）
- [ ] 集成测试：`tests/vector_*.rs`

**验收标准**：

```sql
-- 手动创建内存索引后可执行以下查询
SELECT id, VEC_SEARCH('t', 'idx', VEC('[1.0, 0.0, 0.0]')) AS score
FROM t ORDER BY score DESC LIMIT 5;
```

---

### Phase 2：B-Tree 持久化 + DDL（约 1 周）

**目标**：向量数据持久化，重启后自动恢复

- [ ] `src/vector/index.rs`：B-Tree key / value 编码（前缀 `\x00VEC\x01`）
- [ ] `src/schema.rs`：`VectorIndex` schema 结构
- [ ] `vm/exec_ddl.rs`：`CREATE VECTOR INDEX` / `DROP INDEX`
- [ ] `vm/exec_dml.rs`：INSERT / UPDATE / DELETE 自动维护
- [ ] `vm/execute.rs`：`VM::open()` 启动重建 HNSW
- [ ] `src/vector/mod.rs`：持久化元信息读写

**验收标准**：

```sql
CREATE VECTOR INDEX idx_emb ON articles (embedding) DIM 3;
INSERT INTO articles VALUES (1, 'test', VEC('[1.0, 0.0, 0.0]'));
-- 重启进程后
SELECT VEC_SEARCH('articles', 'idx_emb', VEC('[1.0, 0.0, 0.0]')) AS s FROM articles;
-- s ≈ 1.0
```

---

### Phase 3：性能优化与工具函数（约 1 周）

**目标**：生产可用

- [ ] `VEC_NORMALIZE`、`VEC_DIM`、`VEC_DISTANCE` 内置函数
- [ ] HNSW 后台异步重建（懒惰删除达阈值后）
- [ ] `ef_search` 可配置（`SET kkdb.vec_ef_search = 64`）
- [ ] B-Tree 向量页 LZ4 压缩（复用现有压缩框架）
- [ ] Benchmark 脚本（`scripts/bench_vector.sh`）

---

### Phase 4：分布式验证（约 3 天）

**目标**：Raft 集群下向量写入和查询正确性

- [ ] 3 节点集群集成测试：Leader 写入，Follower 查询（`tests/raft_vector.rs`）
- [ ] 快照安装后 HNSW 重建验证
- [ ] Follower ReadIndex 后向量查询一致性测试

---

## 10. 源码目录规划

```
src/
  vector/
    mod.rs          — VectorIndex trait、管理入口、VectorIndexRegistry
    hnsw.rs         — HnswGraph：insert / search（KNN）/ lazy_delete / rebuild
    distance.rs     — DistanceMetric enum、cosine_similarity、l2_distance、dot_product
    index.rs        — B-Tree key 编码：vec_key()、meta_key()、vec_prefix()
                      B-Tree value 编码：encode_vector()、decode_vector()
```

**与现有 `fulltext/` 的对比**：

```
src/fulltext/
  mod.rs         ↔  src/vector/mod.rs
  index.rs       ↔  src/vector/index.rs
  tokenizer.rs   ↔  src/vector/distance.rs
```

设计完全对称，复用同样的集成模式（DDL → schema，DML → 自动维护，SELECT → 内置函数）。

---

## 参考资料

- HNSW 原论文：[Malkov & Yashunin (2018)](https://arxiv.org/abs/1603.09320)
- [hnswlib](https://github.com/nmslib/hnswlib)（C++ 参考实现）
- KKDB 全文检索设计：[`docs/PROJECT.md`](PROJECT.md) §5.1
- 分布式架构：[`docs/DISTRIBUTED.md`](DISTRIBUTED.md)
- 存储引擎：[`docs/COW_DOUBLE_SUPERBLOCK_DESIGN.md`](COW_DOUBLE_SUPERBLOCK_DESIGN.md)
