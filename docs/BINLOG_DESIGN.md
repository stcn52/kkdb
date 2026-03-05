# KKDB Binlog 设计（与 CoW + 双元页配套）

## 1. 设计目标

Binlog 在本项目中定位为“可回放变更日志层”，用于：

1. PITR（Point-In-Time Recovery，时间点恢复）
2. 增量复制（主从/异地）
3. 审计与变更追踪
4. 逻辑回放与调试

注意：主存储一致性仍由 CoW + 双元页保证；Binlog 不替代元页切换提交协议。

## 2. 与主存储的一致性关系

1. 崩溃一致性主锚点：`superblock generation`
2. Binlog 一致性锚点：`txid + generation + commit marker`
3. 可见性规则：只有带 `COMMIT` 的事务在 Binlog 中可见

## 3. 文件与段管理

建议使用分段日志：

1. 命名：`kkdb-binlog.000001`, `kkdb-binlog.000002` ...
2. 每段固定最大大小（例如 128MB 或 512MB）
3. 段头包含：
   - `magic` / `format_version`
   - `db_uuid`
   - `segment_id`
   - `start_lsn`
   - `header_checksum`

## 4. 记录格式

## 4.1 通用头

```rust
struct BinlogRecordHeader {
    record_type: u16,     // BEGIN / CHANGE / PREPARE / COMMIT / ABORT / CHECKPOINT
    flags: u16,
    lsn: u64,             // 全局递增日志序号
    txid: u64,            // 事务ID
    payload_len: u32,
    header_crc32c: u32,
    payload_crc32c: u32,
}
```

## 4.2 记录类型

1. `TX_BEGIN`
2. `ROW_CHANGE` 或 `PAGE_CHANGE`（建议先做逻辑变更：table/index + before/after）
3. `DDL_CHANGE`
4. `TX_PREPARE`（携带 `target_generation`、superblock 摘要）
5. `TX_COMMIT`
6. `TX_ABORT`
7. `CHECKPOINT_MARK`

## 5. 提交流程（关键）

为避免“数据库已提交但 Binlog 丢失”与“Binlog 已提交但数据库未提交”的分裂，推荐以下顺序：

1. 生成事务变更集（内存）
2. 追加 `TX_BEGIN + CHANGE... + TX_PREPARE(target_generation=g+1)` 到 Binlog
3. `fsync(binlog)`
4. 按 CoW 协议提交数据库（写新页 -> fsync(db) -> 写非活动 superblock -> fsync(db)）
5. 追加 `TX_COMMIT(generation=g+1, slot, superblock_checksum)` 到 Binlog
6. `fsync(binlog)`
7. 返回成功

这样可保证：

1. Binlog 至少有 PREPARE（崩溃可判定）
2. 数据库提交完成后一定尝试写 COMMIT

## 6. 崩溃恢复策略（Binlog 维度）

启动恢复时：

1. 先按 CoW 规则选定活动 superblock，得到 `active_generation = G`
2. 扫描 Binlog 尾部事务状态
3. 对“有 PREPARE 无 COMMIT”的事务：
   - 若 `prepare.target_generation <= G` 且 superblock 摘要匹配：判定该事务已提交，补写或逻辑确认 COMMIT
   - 否则判定未提交，忽略该事务
4. 对“无 PREPARE”的事务：直接忽略

可选：实现 `reconcile` 过程，在启动后补齐 COMMIT 标记，便于复制消费端连续读取。

## 7. 复制语义

建议复制侧采用“只消费 COMMIT 事务”的规则：

1. 拉取记录流
2. 按 `txid` 聚合
3. 仅在见到 `TX_COMMIT` 后应用变更
4. 若 `TX_ABORT` 或连接中断无 COMMIT，则丢弃未完成事务

## 8. Binlog 与检查点/快照

为了长期运行，需要日志截断策略：

1. 周期性做全量快照（或依赖主库文件备份点）
2. 记录 `safe_lsn`（复制端最小已消费位点）
3. 仅删除所有消费者都越过的段

建议在 superblock 中增加可选字段：

1. `binlog_min_lsn`
2. `binlog_epoch`

## 9. 加固建议

1. 每条记录双 CRC（头+载荷）
2. 段级尾部 `TRAILER`（最后 LSN、记录数、段校验）
3. 写放大控制：支持 group commit（短时间批量 fsync）
4. 提供 `binlog verify` 工具

## 10. 分阶段落地建议

1. Phase B1：定义格式与读写器（无复制）
2. Phase B2：写入 `BEGIN/CHANGE/PREPARE/COMMIT`，与 CoW 提交集成
3. Phase B3：启动恢复 reconcile
4. Phase B4：复制拉流接口与消费端
5. Phase B5：rotate + purge + verify 工具链
