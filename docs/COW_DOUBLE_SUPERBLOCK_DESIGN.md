# CoW + 双元页（A/B Superblock）完整设计方案

## 1. 目标

本方案用于将 KKDB 从“进程内事务快照”升级为“崩溃一致性存储引擎”，在进程崩溃、主机断电、异常重启等场景下避免数据库损坏。

目标如下：

1. 原子提交：崩溃后状态只能是“提交前”或“提交后”
2. 可恢复：启动时自动选择有效元页，无需人工修复
3. 可检测：页级/元页级校验，识别部分写入与损坏
4. 可扩展：后续可平滑演进到并发读与增量 checkpoint

## 2. 核心思想

1. CoW（Copy-on-Write）B+Tree：写入永不覆盖旧页，只生成新页
2. 双元页：固定 `page 1` 和 `page 2` 分别作为 superblock A/B
3. 提交切换：每次提交只写“非活动槽”superblock，成功后切换代际
4. 启动恢复：读取 A/B，校验并选择 `generation` 更高且有效的一份

该模型的关键优势是：不需要回滚日志重放，也不会出现“半页覆盖后回不去”的问题。

## 3. 磁盘布局（Format v2）

## 3.1 页面分配

1. `page 1`：Superblock A
2. `page 2`：Superblock B
3. `page >= 3`：普通数据页（表 B+Tree、索引 B+Tree、空闲管理页等）

## 3.2 Superblock 建议结构

```rust
struct SuperblockV2 {
    magic: [u8; 16],          // "KKDB_COW_V2"
    format_version: u16,      // 2
    page_size: u16,           // 4096
    flags: u32,               // 预留
    generation: u64,          // 提交代号，单调递增
    db_uuid: [u8; 16],        // 库唯一ID

    schema_root: u32,         // schema btree root
    free_root: u32,           // 当前可分配空闲页集合
    pending_free_root: u32,   // 本代回收、下代才可用
    page_count: u32,          // 文件高水位页数

    checksum: u32,            // superblock 校验
}
```

## 3.3 普通页建议

每个普通页增加固定页头字段：

1. `page_type`
2. `page_lsn` 或 `generation`
3. `payload`
4. `page_checksum`（CRC32C）

读取时先校验，再解码。

## 4. 提交协议（单写者）

提交顺序必须固定：

1. 事务开始时读取当前活动 superblock（记为 `S_old`）
2. 所有修改在 CoW 新页上完成，不改旧页
3. 得到新根与新空闲状态，形成 `S_new`（`generation = S_old + 1`）
4. 先写全部新数据页（`page >= 3`）
5. `fsync(db file)`
6. 写“非活动槽”superblock（A 或 B）
7. `fsync(db file)`
8. 提交完成

崩溃语义：

1. 在步骤 1-5 崩溃：仍使用 `S_old`
2. 在步骤 6 崩溃：通过 checksum 判定损坏槽并忽略
3. 步骤 7 成功后，`S_new` 生效

## 5. 启动恢复协议

启动时执行：

1. 读取 A/B 两个 superblock
2. 分别校验：`magic/version/page_size/checksum/page_count/root范围`
3. 两个都无效：报 `CorruptDatabase`
4. 一个有效：使用该槽
5. 两个都有效：选 `generation` 更大的槽

恢复不做页重放，元页选择即恢复。

## 6. CoW B+Tree 改造

写路径改为“返回新根”：

1. `insert(root, key, row) -> new_root`
2. `update(root, key, row) -> new_root`
3. `delete(root, key) -> new_root`

规则：

1. 叶子页修改：复制叶子并改新页
2. 内部节点更新：沿路径逐层复制并重连 child pointer
3. split/merge 在新页上进行，旧页保持不变

读路径保持不变：始终从当前活动 superblock 的 root 进入。

## 7. 空闲页管理（两代池）

为避免同代重用引发一致性问题，引入两代空闲集合：

1. `free_root`：本代可分配
2. `pending_free_root`：本次提交产生的废弃页，下代才可分配

提交时：

1. `new_free = merge(old_free, old_pending) - newly_allocated`
2. `new_pending = pages_obsoleted_in_this_commit`

## 8. Schema 层变更

当前实现里 schema 默认固定在 `page 1`，需要改为：

1. schema 根页由 superblock 字段 `schema_root` 给出
2. `Schema::load_from_pager` 从活动 superblock 读取 root
3. DDL/DML 与数据变更在同一事务提交中完成

## 9. 与当前代码的映射

重点改造文件：

1. `src/storage/pager.rs`：实现 A/B superblock 读写、校验、提交切换
2. `src/storage/btree.rs`：写路径 CoW 化，输出新 root
3. `src/schema.rs`：去除“page1 固定 schema root”假设
4. `src/vm/execute.rs`：事务边界接入新提交器
5. `src/vm/exec_ddl.rs`：DDL 统一走 CoW 提交路径

## 10. 迁移策略（v1 -> v2）

建议离线迁移，避免原地风险：

1. 读取 v1 全量对象（schema + 数据 + 索引）
2. 用 v2 格式重建新库
3. 校验通过后替换旧库

不建议在原文件上直接就地升级。

## 11. 测试与验收

必须实现故障注入测试：

1. 新页写到一半崩溃
2. 新页写完未 fsync 崩溃
3. 元页写入后未 fsync 崩溃
4. 元页 fsync 后崩溃

每个场景断言：

1. 数据库可打开
2. 状态是旧版本或新版本之一（无中间态）
3. B+Tree 结构可遍历且 key 有序

长期稳定性测试：

1. 随机 DDL/DML 压力
2. 随机 kill 进程
3. 重启后做一致性检查

## 12. 分阶段落地计划

1. Phase 1：引入 superblock A/B + 校验 + 启动选择
2. Phase 2：Pager 提交协议切换（先数据页后元页）
3. Phase 3：B+Tree 写路径 CoW 化
4. Phase 4：schema root 解耦 page1
5. Phase 5：free/pending_free 双池
6. Phase 6：故障注入测试与迁移工具

## 13. 风险与边界

1. 单写者模型优先，不先做多写者并发
2. 必须使用 `sync_data/sync_all`，不能仅 `flush`
3. Windows 下 rename/fsync 语义需单独验证
4. 若页校验开销过高，可按配置开关，但默认应开启

## 14. Binlog 扩展

已补充独立 Binlog 设计文档：`docs/BINLOG_DESIGN.md`。

建议实现顺序：

1. 先完成 CoW + 双元页主存储一致性
2. 再接入 Binlog 的 PREPARE/COMMIT 协议
3. 最后实现复制、轮转与清理
