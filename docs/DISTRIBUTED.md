# KKDB 分布式模式文档

KKDB 通过内置的 [openraft](https://github.com/datafuselabs/openraft) 共识库实现了强一致的分布式高可用集群。本文档涵盖分布式架构设计、集群搭建、运维管理以及 HTTP API 参考。

---

## 目录

1. [架构概览](#1-架构概览)
2. [关键组件](#2-关键组件)
3. [集群部署](#3-集群部署)
4. [Raft HTTP API](#4-raft-http-api)
5. [Binlog 流与拉取复制](#5-binlog-流与拉取复制)
6. [WAL 日志存储](#6-wal-日志存储)
7. [快照（Snapshot）](#7-快照snapshot)
8. [监控与可观测性](#8-监控与可观测性)
9. [运维操作](#9-运维操作)
10. [故障排查](#10-故障排查)

---

## 1. 架构概览

```
客户端连接（MySQL 3306 / HTTP 6543）
        │
        ▼
   KKDB Server
   （tokio 异步服务层）
        │
        ├─ 写操作（DDL/DML）
        │       │
        │       ▼
        │   KkdbNode::write()
        │       │ client_write
        │       ▼
        │   openraft Raft Core
        │       │ 多数派确认后
        │       ▼
        │   KkdbStateMachine::apply()
        │       │ 执行 SQL，写入 B-Tree
        │       │ 发射 Binlog 记录
        │       ▼
        │   磁盘（.kkdb + raft/wal.log）
        │
        └─ 读操作（SELECT）
                │ ensure_linearizable()
                │（ReadIndex：等待本节点 apply 至最新 commit）
                ▼
           本地 VM::execute_sql()

节点间通信（Raft HTTP RPC，端口 7001/7002/...）
   AppendEntries / Vote / InstallSnapshot
```

### 端口约定

| 参数 | 默认值 | 用途 |
|------|--------|------|
| `--port` | 3306 | 原生 KKDB TCP 协议 |
| `--mysql-port` | 3307 | MySQL 有线协议（DBeaver、mysql2）|
| `--http-port` | 6543 | HTTP REST API（Supabase 风格）|
| `--raft-addr` | — | Raft 节点间 RPC HTTP 端口（必须显式指定）|

---

## 2. 关键组件

### 2.1 KkdbNode（`src/raft/node.rs`）

高层 Raft 节点封装，对外提供：

| 方法 | 说明 |
|------|------|
| `KkdbNode::new()` | 创建进程内节点（测试用，共享内存通信）|
| `new_with_http_network()` | 创建跨进程节点（生产部署，HTTP 通信）|
| `init_single()` | 单节点自举为 Leader |
| `init_with_members()` | 多节点联合初始化集群 |
| `write(req)` | 通过 Raft 提交 SQL 写入 |
| `ensure_linearizable()` | 线性一致读屏障（Follower 等待 apply） |
| `is_leader()` | 判断当前节点是否为 Leader |
| `wait_for_leader(timeout)` | 等待集群选出 Leader |
| `metrics()` | 获取 openraft 指标快照 |
| `shutdown()` | 优雅关闭 |

**Raft 定时器配置**（编译时确定）：

```
heartbeat_interval    = 250 ms
election_timeout_min  = 299 ms
election_timeout_max  = 500 ms
```

### 2.2 KkdbStateMachine（`src/raft/state_machine.rs`）

Raft 状态机，实现 openraft `RaftStateMachine` 接口。

**apply 流程**（每次多数派确认后调用）：

1. 更新 `last_applied_log`
2. 按 `user_id` 路由到对应的 `VM`（隔离多租户数据）
3. 执行 SQL（DDL / DML）写入 B-Tree 存储
4. 如配置了 `BinlogBroadcaster`，追加 `LogRecord::Sql` 并广播

**多租户隔离**：

- `user_id` 为空 → 写入全局 `auth_vm`（DDL、用户管理）
- `user_id` 非空 → 写入对应用户的独立 `VM`，数据目录为 `{data_dir}/{user_id}/`

### 2.3 KkdbLogStore（`src/raft/log_store.rs`）

持久化 WAL 日志存储，基于追加写文件实现：

- 路径：`{data_dir}/raft/wal.log`
- 支持 Log 清除（compaction）：只保留未被快照覆盖的活跃条目
- 提供 `compaction_stats()` 供监控接口使用

### 2.4 KkdbTypeConfig（`src/raft/types.rs`）

Raft 类型参数：

```rust
KkdbRequest  { sql: String, user_id: String }  // 提案负载
KkdbResponse { message: String, ok: bool }      // 执行结果
KkdbNodeId   = u64                              // 节点 ID
```

---

## 3. 集群部署

### 3.1 构建二进制

```bash
cargo build --release
# 可选：部署到 PATH
cp target/release/kkdb /usr/local/bin/
```

### 3.2 CLI 参数速查

| 参数 | 必填 | 默认值 | 说明 |
|------|------|--------|------|
| `--server` | ✅ | — | 开启服务器模式（REPL 以外）|
| `--node-id <u64>` | ✅（集群模式）| — | 唯一节点编号 |
| `--raft-addr <host:port>` | ✅（集群模式）| — | 本节点 Raft RPC 监听地址 |
| `--peers <id=host:port,...>` | 多节点必填 | — | 对等节点列表（**裸地址，不加 `http://`**）|
| `--data-dir <path>` | 推荐 | 内存模式 | 持久化数据根目录 |
| `--port <port>` | 否 | 3306 | 原生 KKDB TCP 协议端口 |
| `--mysql-port <port>` | 否 | 3307 | MySQL 有线协议端口 |
| `--http-port <port>` | 否 | 6543 | HTTP REST API 端口 |

> **`--peers` 格式说明**：填写 `id=host:port`（**裸地址**），程序启动时自动在内部加上 `http://` 前缀。
> 例如：`--peers "2=192.168.1.2:7002,3=192.168.1.3:7003"`

### 3.3 单节点开发模式

无 `--peers` 参数时，节点启动后自动调用 `init_single()` 成为 Leader，无需额外初始化步骤。

```bash
kkdb --server \
     --node-id 1 \
     --raft-addr 127.0.0.1:7001 \
     --data-dir ./data \
     --mysql-port 3307 \
     --http-port 6543
```

连接验证：

```bash
mysql -h 127.0.0.1 -P 3307 -u root -p
# 或
curl -X POST http://127.0.0.1:6543/query \
     -H 'Content-Type: application/json' \
     -d '{"sql": "SELECT 1"}'
```

### 3.4 三节点高可用集群

三节点集群可容忍 1 个节点故障（多数派 = 2/3）。以下示例假设三台机器 IP 分别为 `192.168.1.1`、`192.168.1.2`、`192.168.1.3`。

#### Step 1：启动全部节点

> ⚠️ 配置了 `--peers` 时，所有节点以 **Learner**（无投票权）身份启动，直到调用 `/raft/init`。

**节点 1（192.168.1.1）：**

```bash
kkdb --server \
     --node-id 1 \
     --raft-addr 192.168.1.1:7001 \
     --peers "2=192.168.1.2:7002,3=192.168.1.3:7003" \
     --data-dir /data/kkdb_n1 \
     --mysql-port 3307 \
     --http-port 6543
```

**节点 2（192.168.1.2）：**

```bash
kkdb --server \
     --node-id 2 \
     --raft-addr 192.168.1.2:7002 \
     --peers "1=192.168.1.1:7001,3=192.168.1.3:7003" \
     --data-dir /data/kkdb_n2 \
     --mysql-port 3307 \
     --http-port 6543
```

**节点 3（192.168.1.3）：**

```bash
kkdb --server \
     --node-id 3 \
     --raft-addr 192.168.1.3:7003 \
     --peers "1=192.168.1.1:7001,2=192.168.1.2:7002" \
     --data-dir /data/kkdb_n3 \
     --mysql-port 3307 \
     --http-port 6543
```

#### Step 2：初始化集群（仅首次部署执行一次）

等待三个节点全部启动后，向**任意一个**节点的 Raft 端口发起初始化请求：

```bash
curl -X POST http://192.168.1.1:7001/raft/init \
     -H "Content-Type: application/json" \
     -d '{
       "nodes": {
         "1": "http://192.168.1.1:7001",
         "2": "http://192.168.1.2:7002",
         "3": "http://192.168.1.3:7003"
       }
     }'
```

> `/raft/init` 的请求体使用完整 `http://` URL（与 `--peers` 裸地址格式不同），因为这是发给 HTTP 服务的 JSON 负载。

响应 `{"ok":true}` 说明 Leader 选举完成，集群就绪。

> ✅ **后续重启无需再次调用 `/raft/init`**，节点自动从 WAL + Snapshot 恢复并重新加入集群。

#### Step 3：验证集群状态

```bash
curl http://192.168.1.1:7001/raft/metrics
```

```jsonc
{
  "node_id": 1,
  "role": "Leader",          // Leader / Follower / Candidate
  "current_leader": 1,
  "current_term": 1,
  "last_log_index": 42,
  "last_applied_index": 42,
  "membership_voter_ids": [1, 2, 3],
  "wal": { "live_records": 40, "total_records": 42, "dead_records": 2, "compaction_ratio_pct": 4 }
}
```

### 3.5 本地三节点快速测试（单机模拟）

```bash
# 终端 1 ── 节点 1
kkdb --server --node-id 1 \
     --raft-addr 127.0.0.1:7001 \
     --peers "2=127.0.0.1:7002,3=127.0.0.1:7003" \
     --data-dir /tmp/kkdb_n1 \
     --mysql-port 3310 --http-port 6541

# 终端 2 ── 节点 2
kkdb --server --node-id 2 \
     --raft-addr 127.0.0.1:7002 \
     --peers "1=127.0.0.1:7001,3=127.0.0.1:7003" \
     --data-dir /tmp/kkdb_n2 \
     --mysql-port 3311 --http-port 6542

# 终端 3 ── 节点 3
kkdb --server --node-id 3 \
     --raft-addr 127.0.0.1:7003 \
     --peers "1=127.0.0.1:7001,2=127.0.0.1:7002" \
     --data-dir /tmp/kkdb_n3 \
     --mysql-port 3312 --http-port 6543

# 终端 4 ── 初始化（三个节点均启动后执行）
curl -X POST http://127.0.0.1:7001/raft/init \
     -H "Content-Type: application/json" \
     -d '{"nodes":{"1":"http://127.0.0.1:7001","2":"http://127.0.0.1:7002","3":"http://127.0.0.1:7003"}}'

# 验证：连接任意节点执行 SQL
mysql -h 127.0.0.1 -P 3310 -u root -p -e 'CREATE TABLE t (id INT PRIMARY KEY);'
mysql -h 127.0.0.1 -P 3311 -u root -p -e 'SHOW TABLES;'  # Follower 也能读到
```

### 3.6 数据目录结构

启动后 `--data-dir` 下的文件布局：

```
{data_dir}/
  {user_id}/            ← 每个用户独立的数据目录
    catalog.kkdb        ← Schema 元数据 B-Tree
    {table}.kkdb        ← 各表数据 B-Tree
  binlog.kkdb           ← Binlog 文件（Raft 模式下使用）
  raft/
    wal.log             ← Raft WAL 日志（追加写）
    vote.json           ← 持久化投票状态
    purge.json          ← 日志清除水位
    snapshot.json       ← 最新状态快照
    snapshot.tmp        ← 快照原子写临时文件
```

---

## 4. Raft HTTP API

所有 Raft 管理接口挂载在 Raft RPC 端口（默认 `700x`）。

### 集群管理接口

| 方法 | 路径 | 说明 |
|------|------|------|
| `POST` | `/raft/init` | 初始化集群，传入完整节点列表 |
| `POST` | `/raft/add-learner` | 加入新 Learner 节点（不参与投票） |
| `POST` | `/raft/change-membership` | 将 Learner 提升为 Voter / 移除节点 |
| `POST` | `/raft/status` | 简要集群状态（兼容旧接口） |
| `GET`  | `/raft/metrics` | 详细 JSON 指标 |
| `GET`  | `/raft/metrics/prometheus` | Prometheus 文本格式指标 |

### Raft 内部 RPC（节点间调用，勿手动使用）

| 方法 | 路径 | 说明 |
|------|------|------|
| `POST` | `/raft/append-entries` | Leader → Follower 日志复制 |
| `POST` | `/raft/vote` | Candidate 拉票 |
| `POST` | `/raft/install-snapshot` | Leader → Follower 快照安装 |

### 接口详情

#### `POST /raft/init`

**请求体：**

```json
{
  "nodes": {
    "1": "http://host1:7001",
    "2": "http://host2:7002",
    "3": "http://host3:7003"
  }
}
```

**响应：** `{"ok": true}` 或 `{"error": "..."}`

---

#### `POST /raft/add-learner`

将一个新节点加入为 Learner（数据副本，不参与选举计票）。

**请求体：**

```json
{
  "node_id": 4,
  "addr": "http://host4:7004"
}
```

---

#### `POST /raft/change-membership`

将 Learner 提升为 Voter，或从集群移除节点。

**请求体：**

```json
{
  "node_id": 4,
  "retain": true
}
```

- `retain: true` — 将 `node_id` 加入现有 Voter 集合（扩容）
- `retain: false` — 将 Voter 集合替换为仅 `node_id`（缩容，谨慎使用）

---

## 5. Binlog 流与拉取复制

KKDB 提供基于 HTTP 的 Binlog 拉取端点，适用于跨广域网异步复制、CDC 消费或只读副本场景。

### 端点

```
GET /binlog/stream?from_pos=<offset>
```

- `from_pos`：字节偏移量（默认 0 = 从头读取）
- 响应格式：NDJSON，每行一条记录

**响应格式（每行）：**

```json
{"pos": 1024, "data": "<base64>"}
```

- `pos`：此条记录之后的字节偏移，用作下次请求的 `from_pos`
- `data`：Base64 编码的帧数据，格式为 `[len: u32 LE][crc32: u32 LE][payload]`

### 使用示例

```bash
# 全量拉取
curl "http://host1:7001/binlog/stream?from_pos=0"

# 增量拉取（记录上次返回的 pos）
curl "http://host1:7001/binlog/stream?from_pos=1024"
```

### 内置 BinlogFollower

`src/binlog/mod.rs` 中提供了 `BinlogFollower` 参考实现，可作为自定义 CDC 下游消费程序的蓝本：

```rust
// 示例：持续轮询并解析 Binlog
let follower = BinlogFollower::new("http://leader:7001");
follower.run_loop(|record| {
    // 处理 LogRecord::Sql { sql, user_id, raft_index }
}).await;
```

---

## 6. WAL 日志存储

WAL（Write-Ahead Log）文件存储于：

```
{data_dir}/raft/wal.log    ← Raft 日志条目（追加写）
{data_dir}/raft/vote.json  ← 持久化投票状态（防止重启后脑裂）
{data_dir}/raft/purge.json ← 日志清除水位标记
```

**特性：**

- 追加写（Append-only），高吞吐、低延迟
- 日志清除（Log Compaction）：快照之前的条目标记为 dead，触发 GC 回收空间
- `compaction_stats()` 返回 `(live, total, dead)` 三元组

通过 `/raft/metrics` 可查看 WAL 健康状况：

```json
"wal": {
  "live_records": 150,
  "total_records": 200,
  "dead_records": 50,
  "compaction_ratio_pct": 25
}
```

---

## 7. 快照（Snapshot）

KKDB 使用 **Statement-Based Snapshot**：将全部已 apply 的 SQL 语句列表序列化为 JSON。

### 快照文件

```
{data_dir}/raft/snapshot.json   ← 当前最新快照
{data_dir}/raft/snapshot.tmp    ← 原子写中间文件（写完后 rename）
```

### 快照内容（`KkdbSnapshotData`）

```json
{
  "entries": [
    {"sql": "CREATE TABLE users (...)", "user_id": ""},
    {"sql": "INSERT INTO users VALUES (...)", "user_id": "alice"}
  ],
  "last_applied": {"leader_id": 1, "index": 42},
  "last_membership": { ... }
}
```

### 快照生命周期

1. **build_snapshot**：openraft 周期性或手动触发，将 `applied_entries` 序列化写入磁盘（原子 rename）
2. **install_snapshot**：Follower 落后太多时，Leader 发送整个快照；Follower 收到后写盘并重放所有 SQL 恢复 VM 状态
3. **启动恢复**：进程重启时自动加载 `snapshot.json`，重放 SQL，然后继续应用 WAL 中尚未快照的增量条目

---

## 8. 监控与可观测性

### JSON 指标

```bash
curl http://<node>:<raft-port>/raft/metrics
```

| 字段 | 说明 |
|------|------|
| `node_id` | 本节点 ID |
| `role` | `Leader` / `Follower` / `Candidate` |
| `current_leader` | 当前 Leader 的节点 ID |
| `current_term` | 当前 Raft 任期号 |
| `last_log_index` | WAL 中最后一条日志的 index |
| `last_applied_index` | 已 apply 到状态机的最新 index |
| `snapshot_last_log_index` | 最新快照覆盖到的 index |
| `membership_voter_ids` | 当前投票成员列表 |
| `wal.live_records` | WAL 活跃条目数 |
| `wal.dead_records` | 待 GC 的已清除条目数 |
| `wal.compaction_ratio_pct` | dead / total 百分比 |

### Prometheus 指标

```bash
curl http://<node>:<raft-port>/raft/metrics/prometheus
```

暴露的 Prometheus 指标（以 `kkdb_` 为前缀）：

| 指标名 | 类型 | 说明 |
|--------|------|------|
| `kkdb_raft_is_leader` | gauge | 是否为 Leader（1/0）|
| `kkdb_raft_current_term` | gauge | 当前任期 |
| `kkdb_raft_last_log_index` | gauge | 最新日志 index |
| `kkdb_raft_last_applied_index` | gauge | 最新已 apply index |
| `kkdb_raft_snapshot_last_log_index` | gauge | 快照覆盖 index |
| `kkdb_wal_live_records` | gauge | WAL 活跃条目 |
| `kkdb_wal_total_records` | counter | WAL 总写入条目 |
| `kkdb_wal_dead_records` | gauge | WAL 待 GC 条目 |
| `kkdb_wal_compaction_ratio_pct` | gauge | WAL 碎片率（0-100）|
| `kkdb_membership_voter_count` | gauge | 投票成员数 |

---

## 9. 运维操作

### 9.1 扩容：加入只读副本（Learner）

Learner 节点接收完整数据复制，但不参与选举，适合做报表库或异地容灾副本。

```bash
# 1. 启动新节点（--peers 只需填写任意一个在线节点即可）
kkdb --server --node-id 4 \
     --raft-addr 192.168.1.4:7004 \
     --peers "1=192.168.1.1:7001" \
     --data-dir /data/kkdb_n4 \
     --mysql-port 3307 --http-port 6544

# 2. 通知 Leader 将其纳入复制视野
curl -X POST http://192.168.1.1:7001/raft/add-learner \
     -H "Content-Type: application/json" \
     -d '{"node_id": 4, "addr": "http://192.168.1.4:7004"}'
```

### 9.2 扩容：将 Learner 提升为 Voter

```bash
curl -X POST http://host1:7001/raft/change-membership \
     -H "Content-Type: application/json" \
     -d '{"node_id": 4, "retain": true}'
```

> ⚠️ 将 Voter 数量从奇数变为偶数会降低容错能力（4 节点仍只容 1 故障）。推荐维持奇数 Voter 集合（3 / 5 / 7）。

### 9.3 缩容：移除故障节点

```bash
# 将 Voter 集合替换为健康节点（retain: false）
curl -X POST http://host1:7001/raft/change-membership \
     -H "Content-Type: application/json" \
     -d '{"node_id": 1, "retain": false}'
# 此命令将 Voter 集合缩减为仅节点 1
# 若需保留多个节点，需多次调用或扩展 API
```

> ⚠️ 当存活节点数少于 `⌊N/2⌋ + 1` 时集群停止写入（脑裂保护）。出现这种情况需先恢复节点，或通过 `change-membership` 重建多数派。

### 9.4 数据恢复（灾难恢复）

若某节点数据目录损坏：

1. 清空该节点的 `{data_dir}/raft/` 目录和 `.kkdb` 文件
2. 重新启动该节点（带相同 `--node-id` 和 `--peers`）
3. 集群 Leader 检测到该节点落后后，自动触发 `InstallSnapshot` 将完整数据推送过来
4. 节点恢复正常服务无需人工干预

---

## 10. 故障排查

### Q：写入报错 `Cannot propose to follower`

**原因**：请求发到了 Follower 节点。

**解法**：KKDB Server 层会自动将写请求转发给 Leader。如果手动调用 API，请先查询 `/raft/metrics` 确认 `current_leader`，然后连接对应节点。

---

### Q：集群停止响应写入，提示 `Quorum Not Achieved`

**原因**：存活节点数不足多数派 `⌊N/2⌋ + 1`。

**解法**：
1. 检查所有节点进程状态；
2. 确认网络连通性（curl 节点的 Raft 端口）；
3. 若有节点永久故障，使用 `change-membership` 将其从 Voter 集合移除；
4. 最坏情况：清空剩余节点重建集群（从最新备份恢复）。

---

### Q：节点重启后数据不一致

**原因**：WAL 或快照文件损坏。

**解法**：
1. 尝试直接重启，进程会自动从 WAL + Snapshot 恢复；
2. 若失败，清空 `{data_dir}/raft/` 目录，让节点以 Learner 身份加入并接受 InstallSnapshot；
3. KKDB 的 COW B-Tree 内置 CRC32 校验，单页损坏不会扩散。

---

### Q：Binlog 流端点返回 `binlog not enabled on this node`

**原因**：该节点启动时未配置 `BinlogBroadcaster`（非 Leader 节点默认不开启）。

**解法**：将 Binlog 拉取请求发给 Leader 节点；或重新配置该节点开启 Binlog。

---

## 参考

- [openraft 文档](https://datafuselabs.github.io/openraft/)
- Raft 核心论文：[In Search of an Understandable Consensus Algorithm](https://raft.github.io/raft.pdf)
- 相关文档：
  - [`BINLOG_DESIGN.md`](BINLOG_DESIGN.md) — Binlog 详细设计
  - [`HTTP_API.md`](HTTP_API.md) — HTTP REST API 参考
  - [`MYSQL_SERVER.md`](MYSQL_SERVER.md) — MySQL 协议服务器文档
  - [`PROJECT.md`](PROJECT.md) — 项目总览与架构
