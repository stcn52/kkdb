# KKDB 升级计划（CoW + 双元页 + Binlog）

## 1. 升级目标

本次升级目标是将 KKDB 从当前实现升级为可抗崩溃损坏的存储体系，并为后续增量复制能力打基础。

目标优先级：

1. 崩溃一致性（最高优先）
2. 可恢复与可验证
3. Binlog（PITR/复制/审计）

## 2. 升级范围

包含：

1. 存储格式升级为 `format v2`（CoW + A/B superblock）
2. Pager 提交协议重构
3. B+Tree 写路径 CoW 化
4. Schema root 解耦 `page1`
5. 空闲页两代池（`free_root` + `pending_free_root`）
6. Binlog 完整实现（因果一致性 + 运维能力）
7. 故障注入测试

不包含（本阶段）：

1. 多写者并发控制
2. 分布式共识复制
3. WAL 模式并行实现

## 3. 总体阶段

各 Phase 按依赖顺序推进，Phase 4 与 Phase 5 可并行，其余严格串行。

## Phase 0：基线冻结与保障

目标：

1. 为后续写路径大改建立可注入故障的测试基础设施

任务：

1. 增加 crash-injection 测试框架骨架（可在关键提交点模拟崩溃）
2. 增加统一 `fsync` 封装（文件/目录），隔离平台差异

验收：

1. 现有 598 个测试保持全绿（回归门禁）
2. 可在关键提交点注入故障并观察到正确的恢复行为

---

## Phase 1：Superblock v2 读写与启动选择

目标：

1. 引入 A/B superblock 结构和校验
2. 启动时可从双槽选择活动元页

任务：

1. 新增 `SuperblockV2` 序列化/反序列化/CRC
2. Pager 打开时读取 A/B 并执行有效性判定
3. 维护 `generation` 与 `active_slot`
4. 检测到非 v2 格式时直接报 `UnsupportedFormat`，不做兼容处理

验收：

1. A 槽损坏可自动切 B
2. B 槽损坏可自动切 A
3. 双槽均坏时报 `CorruptDatabase`
4. 非 v2 格式文件拒绝打开，报 `UnsupportedFormat`

---

## Phase 2：Pager CoW 提交协议

目标：

1. 建立“先数据页后元页”的原子提交序列

任务：

1. 事务内只写新页，不覆盖旧页
2. 提交步骤固定：`write pages -> fsync(db) -> write inactive superblock -> fsync(db)`
3. 引入 `txid/generation` 对应关系
4. 移除旧快照事务语义对持久一致性的误导
5. 内存模式保持原有语义，不执行 fsync 路径

验收：

1. 在提交各步骤崩溃后，重启状态只可能是旧或新
2. 不出现“半提交”

---

## Phase 3：B+Tree 写路径 CoW 化

目标：

1. 表数据与索引的插入/更新/删除全量改造为路径复制并返回 `new_root`

任务：

1. 表数据和索引的 `insert/update/delete` 写路径不改旧页
2. split/merge 在新页上完成（表 B-Tree 与索引 B-Tree 均适用）
3. 写 API 统一返回新 root
4. 读路径保持从活动 superblock root 进入

验收：

1. 所有 DML 测试通过（含索引更新路径）
2. B+Tree 不变量（有序性、可遍历）通过（表与索引均验证）

---

## Phase 4：Schema 与系统根页解耦（可与 Phase 5 并行）

目标：

1. schema root 从 superblock 提供，不再硬编码 page1

任务：

1. `Schema::load_from_pager` 改为读取 `schema_root`
2. DDL 路径统一使用新 root 提交
3. 移除 page1 固定 schema 假设

验收：

1. DDL/DML 混合场景稳定
2. 重启后 schema 完整且一致

---

## Phase 5：空闲页两代池

目标：

1. 避免同代回收后立即重用导致的一致性问题

任务：

1. 引入 `free_root` 与 `pending_free_root`
2. 提交时执行两代池轮转
3. 分配策略：优先 `free_root`，不足再扩文件

验收：

1. 长时间随机写入无页重用冲突
2. 文件大小与空闲页可解释

---

## Phase 6：Binlog 完整实现

目标：

1. 打通 Binlog 与 CoW 提交的因果一致性，并具备长期可运行的运维能力

任务：

1. 实现记录类型：`BEGIN/CHANGE/PREPARE/COMMIT/ABORT`，其中 `CHANGE` 为行级粒度（含 before/after values），每条 DML 产生一条或多条 `CHANGE` 记录
2. 提交流程：先写 PREPARE 并 fsync，再提交 DB，再写 COMMIT 并 fsync
3. 启动 reconcile：处理有 PREPARE 无 COMMIT 的尾部事务
4. 日志分段 rotate
5. 离线校验与修复工具（verify/repair CLI）
6. 保留策略（按 LSN/时间/复制位点）

验收：

1. 崩溃后 Binlog 与 DB generation 对账正确
2. 仅 COMMIT 事务对外可见
3. 长跑压测无无限增长与损坏
4. 复制消费者位点前移后可安全清理旧段

---

## 4. 风险与缓解

主要风险：

1. 写路径大改导致隐性数据结构 bug
2. fsync/目录同步在不同平台语义差异
3. Binlog 与 DB 提交时序不一致

缓解手段：

1. 强制故障注入测试门禁
2. Windows/Linux 分平台验证清单
3. 统一提交状态机与审计日志

## 5. 测试策略

测试层级：

1. 单元测试：序列化、校验、状态机
2. 组件测试：Pager/B+Tree/Schema
3. 故障注入测试：提交每一步崩溃
4. 随机压力测试：随机 SQL + 随机 kill
5. 回归测试：现有 598 个用例全量

强制验收条件：

1. 崩溃后可打开率 100%
2. 只能恢复到旧态或新态
3. 不允许出现“部分事务可见”

## 6. 里程碑与交付

里程碑建议：

1. M1：完成 Phase 0-2（可抗崩溃提交骨架）
2. M2：完成 Phase 3-5（完整 CoW 存储）
3. M3：完成 Phase 6（Binlog 完整实现）

每个里程碑交付物：

1. 设计文档更新
2. 代码实现与测试报告
3. 风险清单与回滚方案

## 7. 回滚策略

1. 功能层面：通过 feature flag 控制新旧路径
2. 发布层面：先灰度/小样本，再全量切换

## 8. 实施顺序建议（执行版）

1. 先落地 Phase 0-2，拿到“崩溃不坏库”的最小闭环
2. 再推进 Phase 3-5，完成 CoW 存储闭环
3. 最后接 Binlog Phase 6（完整实现）

该顺序可最大化降低风险，并尽早获得高价值稳定性收益。
