// ── coverage_r20_storage_disttxn_sqlext_secadv.rs ──
// R20 集成测试: 高级存储 + 分布式事务 + SQL扩展 + 安全审计

use crate::storage::adv_storage::*;
use crate::raft::dist_txn_adv::*;
use crate::vm::sql_ext::*;
use crate::vm::security_adv::*;

// ═══════════════════════════════════════════════════════════════════════
// A. 高级存储引擎优化
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_r20_adaptive_compressor_auto_select() {
    let mut ac = AdaptiveCompressor::new(200, 7200);
    for i in 0..10 {
        ac.add_sample("logs", CompressionSample {
            algo: CompressionAlgo::Lz4,
            original_bytes: 4096,
            compressed_bytes: 2200 + i * 10,
            compress_us: 8,
        });
        ac.add_sample("logs", CompressionSample {
            algo: CompressionAlgo::Zstd,
            original_bytes: 4096,
            compressed_bytes: 1200 + i * 5,
            compress_us: 40,
        });
    }
    let algo = ac.select_algo("logs");
    assert_eq!(algo, CompressionAlgo::Zstd);
    assert_eq!(ac.cold_threshold(), 7200);
}

#[test]
fn test_r20_compressor_default_for_unknown_table() {
    let mut ac = AdaptiveCompressor::new(10, 3600);
    assert_eq!(ac.select_algo("nonexistent"), CompressionAlgo::Lz4);
}

#[test]
fn test_r20_compressor_sample_eviction() {
    let mut ac = AdaptiveCompressor::new(3, 1000);
    for i in 0..5 {
        ac.add_sample("t", CompressionSample {
            algo: CompressionAlgo::Snappy,
            original_bytes: 1000,
            compressed_bytes: 600 + i * 10,
            compress_us: 5,
        });
    }
    // Only 3 samples kept
    assert_eq!(ac.table_count(), 1);
}

#[test]
fn test_r20_page_prefetcher_adaptive() {
    let mut pf = PagePrefetcher::new(4);
    // Non-uniform access → adaptive mode
    pf.record_access(1);
    pf.record_access(5);
    pf.record_access(2);
    pf.record_access(9);
    assert_eq!(pf.current_mode(), PrefetchMode::Adaptive);
    let reqs = pf.generate_prefetch();
    assert!(!reqs.is_empty());
}

#[test]
fn test_r20_prefetcher_empty_history() {
    let pf = PagePrefetcher::new(4);
    assert!(pf.generate_prefetch().is_empty());
    assert_eq!(pf.hit_rate(), 0.0);
}

#[test]
fn test_r20_incremental_merger_leveled() {
    let mut m = IncrementalMerger::new(MergeStrategy::Leveled);
    for i in 0..5 {
        m.add_segment(0, 50, 500, vec![i * 10], vec![i * 10 + 9]);
    }
    m.add_segment(1, 200, 2000, vec![0], vec![50]);
    let candidates = m.pick_merge_candidates();
    assert!(!candidates.is_empty());
    assert!(candidates[0].len() >= 5); // L0 + overlapping L1
}

#[test]
fn test_r20_merger_hybrid_strategy() {
    let mut m = IncrementalMerger::new(MergeStrategy::Hybrid);
    for i in 0..5 {
        m.add_segment(0, 10, 100, vec![i], vec![i + 5]);
    }
    let candidates = m.pick_merge_candidates();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].len(), 5);
}

#[test]
fn test_r20_storage_monitor_comprehensive() {
    let mut mon = StorageLayerMonitor::new(50);
    mon.record_io(IoOp::Read, 4096, 50);
    mon.record_io(IoOp::Write, 8192, 200);
    mon.record_io(IoOp::Fsync, 0, 5000);
    mon.record_page_cache_hit();
    mon.record_page_cache_miss();
    mon.record_wal_write(1024);
    mon.record_checkpoint();

    assert_eq!(mon.wal_writes(), 1);
    assert_eq!(mon.checkpoint_count(), 1);
    assert!((mon.page_cache_hit_rate() - 0.5).abs() < 0.001);

    let summary = mon.summary();
    assert!(summary.contains_key("wal_writes"));
    assert!(summary.contains_key("checkpoints"));
}

#[test]
fn test_r20_io_stats_throughput() {
    let mut stats = IoStats::default();
    stats.record(1024 * 1024, 1_000_000); // 1MB in 1s
    let tp = stats.throughput_mbps(1.0);
    assert!((tp - 1.0).abs() < 0.01);
}

// ═══════════════════════════════════════════════════════════════════════
// B. 分布式事务增强
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_r20_saga_full_lifecycle() {
    let mut saga = SagaOrchestrator::new(100);
    saga.add_step("create_order", "order-svc", true);
    saga.add_step("debit_account", "payment-svc", true);
    saga.add_step("reserve_inventory", "inventory-svc", true);
    saga.add_step("send_email", "notification-svc", false);

    for _ in 0..4 {
        saga.advance();
        saga.complete_current();
    }
    assert_eq!(saga.advance(), SagaState::Completed);
    assert_eq!(saga.completed_steps(), 4);
    assert_eq!(saga.step_count(), 4);
}

#[test]
fn test_r20_saga_partial_failure_compensation() {
    let mut saga = SagaOrchestrator::new(200);
    saga.add_step("step_a", "svc_a", true);
    saga.add_step("step_b", "svc_b", true);
    saga.add_step("step_c", "svc_c", true);

    // Complete first two
    saga.advance();
    saga.complete_current();
    saga.advance();
    saga.complete_current();
    // Third fails
    saga.advance();
    for _ in 0..4 {
        saga.fail_current();
    }
    assert_eq!(saga.state(), SagaState::Aborted);
    // Two completed steps should be compensated
    assert_eq!(saga.compensated_steps(), 2);
}

#[test]
fn test_r20_compensating_txn_multi_table() {
    let mut log = CompensatingTxnLog::new();
    log.record(10, "users", CompensationOp::UndoInsert { rowid: 1 });
    log.record(10, "orders", CompensationOp::UndoInsert { rowid: 100 });
    log.record(10, "payments", CompensationOp::UndoUpdate { rowid: 50, old_values: vec!["0".into()] });

    assert_eq!(log.action_count(10), 3);
    assert_eq!(log.pending_count(), 1);

    let actions = log.compensate(10);
    assert_eq!(actions.len(), 3);
    // Reversed order: payments first
    assert_eq!(actions[0].table, "payments");
    assert_eq!(log.pending_count(), 0);
}

#[test]
fn test_r20_global_deadlock_cross_node() {
    let mut dd = GlobalDeadlockDetector::new();
    // Node 0: txn 1 waits for txn 2
    dd.add_edge(1, 2, "table_a.row_1", 0);
    // Node 1: txn 2 waits for txn 3
    dd.add_edge(2, 3, "table_b.row_5", 1);
    // Node 2: txn 3 waits for txn 1 → cycle!
    dd.add_edge(3, 1, "table_c.row_10", 2);

    let cycles = dd.detect();
    assert_eq!(cycles.len(), 1);
    let victim = dd.pick_victim(&cycles[0]);
    assert_eq!(victim, Some(3)); // Highest txn_id
    assert_eq!(dd.detection_runs(), 1);
}

#[test]
fn test_r20_deadlock_after_resolution() {
    let mut dd = GlobalDeadlockDetector::new();
    dd.add_edge(1, 2, "r", 0);
    dd.add_edge(2, 1, "r", 0);
    let cycles = dd.detect();
    assert_eq!(cycles.len(), 1);

    // Resolve by removing victim
    dd.remove_edges_for_txn(2);
    let cycles2 = dd.detect();
    assert!(cycles2.is_empty());
}

#[test]
fn test_r20_distributed_snapshot_multi_node() {
    let mut coord = DistributedSnapshotCoord::new(vec![1, 2, 3, 4, 5]);
    let sid = coord.initiate();
    assert!(sid > 0);

    for node in 1..=5 {
        coord.mark_recording(node, 1000 + node as u64);
        coord.record_channel_message(node, vec![node as u8; 4]);
    }
    assert_eq!(coord.channel_message_count(), 5);

    for node in 1..=5 {
        coord.complete_node(node);
    }
    assert!(coord.is_complete());
    assert!(coord.finalize());
    assert_eq!(coord.snapshots_taken(), 1);
    assert_eq!(coord.progress(), (5, 5));
}

// ═══════════════════════════════════════════════════════════════════════
// C. SQL 功能扩展
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_r20_window_ntile_uneven() {
    let tiles = WindowFuncEvaluator::eval_ntile(4, 10);
    // 10 / 4 = 2 base, 2 remainder → tiles 1-2 get 3, tiles 3-4 get 2
    assert_eq!(tiles.len(), 10);
    assert_eq!(tiles.iter().filter(|&&t| t == 1).count(), 3);
    assert_eq!(tiles.iter().filter(|&&t| t == 2).count(), 3);
}

#[test]
fn test_r20_window_lead_lag_boundary() {
    let vals = vec![100, 200, 300];
    let lead = WindowFuncEvaluator::eval_lead(&vals, 5, -1);
    assert_eq!(lead, vec![-1, -1, -1]); // All out of bounds

    let lag = WindowFuncEvaluator::eval_lag(&vals, 0, 0);
    assert_eq!(lag, vec![100, 200, 300]); // offset=0 → identity
}

#[test]
fn test_r20_merge_when_matched_and() {
    let merge = MergeStatement::new("inventory", "new_stock", "inventory.sku = new_stock.sku")
        .when_matched_and("new_stock.qty > 0", MergeAction::UpdateSet(vec![
            ("qty".into(), "inventory.qty + new_stock.qty".into()),
        ]))
        .when_not_matched(MergeAction::InsertValues(vec!["new_stock.sku".into(), "new_stock.qty".into()]));
    assert_eq!(merge.clause_count(), 2);
    assert_eq!(merge.clauses[0].condition, Some("new_stock.qty > 0".to_string()));
}

#[test]
fn test_r20_merge_do_nothing() {
    let merge = MergeStatement::new("t", "s", "t.id = s.id")
        .when_matched(MergeAction::DoNothing)
        .when_not_matched(MergeAction::DoNothing);
    let stats = merge.simulate_execute(100, 50);
    assert_eq!(stats.total_affected(), 0);
}

#[test]
fn test_r20_materialized_view_on_commit() {
    let mut mgr = MaterializedViewManager::new();
    mgr.register(MaterializedViewTracker::new(
        "mv_totals",
        "SELECT SUM(amt) FROM orders",
        vec!["orders".into()],
        RefreshPolicy::OnCommit,
    ));
    mgr.notify_table_change("orders", 1);
    let needing = mgr.views_needing_refresh(0);
    assert_eq!(needing.len(), 1);
    mgr.mark_refreshed("mv_totals", 100);
    assert_eq!(mgr.stale_count(), 0);
}

#[test]
fn test_r20_materialized_view_unrelated_table() {
    let mut mgr = MaterializedViewManager::new();
    mgr.register(MaterializedViewTracker::new(
        "mv_users",
        "SELECT * FROM users",
        vec!["users".into()],
        RefreshPolicy::Threshold(10),
    ));
    mgr.notify_table_change("orders", 100); // unrelated
    assert_eq!(mgr.stale_count(), 1); // Still stale from init but no changes accumulated
    assert!(mgr.views_needing_refresh(0).is_empty()); // 0 < 10
}

#[test]
fn test_r20_batch_upsert_replace_strategy() {
    let mut bu = BatchUpsert::new("products", "id", ConflictStrategy::Replace);
    bu.add_row(vec!["1".into(), "Widget A".into()]);
    bu.add_row(vec!["2".into(), "Widget B".into()]);
    bu.add_row(vec!["3".into(), "Widget C".into()]);
    let result = bu.simulate(&["1".to_string(), "3".to_string()]);
    assert_eq!(result.inserted, 1);
    assert_eq!(result.updated, 2);
    assert!(result.success());
    assert_eq!(bu.batch_size(), 3);
    assert_eq!(bu.table(), "products");
    assert_eq!(bu.strategy(), ConflictStrategy::Replace);
}

// ═══════════════════════════════════════════════════════════════════════
// D. 安全与审计强化
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_r20_fine_grained_multi_role() {
    let mut mgr = FineGrainedPermManager::new();
    mgr.grant_column("admin", "users", "salary", vec![PermOp::Select, PermOp::Update]);
    mgr.grant_column("viewer", "users", "name", vec![PermOp::Select]);

    assert!(mgr.check_column("admin", "users", "salary", PermOp::Update));
    assert!(!mgr.check_column("viewer", "users", "salary", PermOp::Select));
    assert!(mgr.check_column("viewer", "users", "name", PermOp::Select));
    assert_eq!(mgr.check_count(), 3);
}

#[test]
fn test_r20_row_policy_multi_op() {
    let mut mgr = FineGrainedPermManager::new();
    mgr.add_row_policy("doctor", RowPolicy {
        policy_name: "patient_data".into(),
        table: "patients".into(),
        predicate: "doctor_id = CURRENT_USER_ID".into(),
        for_ops: vec![PermOp::Select, PermOp::Update],
    });
    assert!(mgr.row_filter("doctor", "patients", PermOp::Select).is_some());
    assert!(mgr.row_filter("doctor", "patients", PermOp::Delete).is_none());
    assert_eq!(mgr.policy_count(), 1);
}

#[test]
fn test_r20_data_masker_hash() {
    let mut masker = DataMasker::new();
    masker.add_policy(MaskingPolicy {
        table: "users".into(),
        column: "ssn".into(),
        rule: MaskingRule::Hash,
        exempt_roles: std::collections::HashSet::new(),
    });
    let masked = masker.mask_value("users", "ssn", "viewer", "123-45-6789");
    assert!(masked.starts_with("HASH_"));
    assert_eq!(masked.len(), "HASH_".len() + 16);
}

#[test]
fn test_r20_data_masker_no_policy() {
    let mut masker = DataMasker::new();
    let result = masker.mask_value("other", "col", "user", "plain_text");
    assert_eq!(result, "plain_text"); // No policy → no masking
    assert_eq!(masker.masked_count(), 0);
}

#[test]
fn test_r20_encrypted_storage_multi_column() {
    let mut mgr = EncryptedStorageManager::new();
    let kid = mgr.create_key(EncryptionAlgo::Aes256Gcm);
    mgr.configure_table("secrets", vec!["token".into(), "api_key".into()], kid, EncryptionAlgo::Aes256Gcm);

    let enc1 = mgr.encrypt("secrets", "token", b"secret1").unwrap();
    let enc2 = mgr.encrypt("secrets", "api_key", b"secret2").unwrap();
    assert_ne!(enc1, enc2);

    // Non-encrypted column
    assert!(mgr.encrypt("secrets", "name", b"public").is_none());
    assert_eq!(mgr.encrypt_ops(), 2);
}

#[test]
fn test_r20_encrypted_storage_invalid_table() {
    let mut mgr = EncryptedStorageManager::new();
    assert!(mgr.encrypt("nonexist", "col", b"data").is_none());
}

#[test]
fn test_r20_compliance_audit_query_filter() {
    let mut logger = ComplianceAuditLogger::new(500, vec![ComplianceStandard::Gdpr]);
    logger.log(AuditEventType::Login, "admin", "system", "LOGIN", AuditResult::Success);
    logger.log(AuditEventType::DataAccess, "admin", "users", "SELECT *", AuditResult::Success);
    logger.log(AuditEventType::DataAccess, "user1", "orders", "SELECT *", AuditResult::Success);
    logger.log(AuditEventType::PermissionChange, "admin", "roles", "GRANT", AuditResult::Success);

    let admin_events = logger.query(Some("admin"), None);
    assert_eq!(admin_events.len(), 3);

    let data_events = logger.query(None, Some(AuditEventType::DataAccess));
    assert_eq!(data_events.len(), 2);
}

#[test]
fn test_r20_audit_with_details() {
    let mut logger = ComplianceAuditLogger::new(100, vec![ComplianceStandard::Pci]);
    let mut details = std::collections::HashMap::new();
    details.insert("ip".to_string(), "192.168.1.1".to_string());
    details.insert("query".to_string(), "SELECT * FROM cards".to_string());

    let id = logger.log_with_details(
        AuditEventType::DataAccess,
        "teller",
        "cards",
        "SELECT",
        AuditResult::Success,
        details,
    );
    assert!(id > 0);
    let entries = logger.query(Some("teller"), None);
    assert_eq!(entries[0].details.get("ip").unwrap(), "192.168.1.1");
}

#[test]
fn test_r20_audit_compliance_standards() {
    let logger = ComplianceAuditLogger::new(100, vec![ComplianceStandard::Hipaa, ComplianceStandard::Sox]);
    assert!(logger.has_standard(ComplianceStandard::Hipaa));
    assert!(logger.has_standard(ComplianceStandard::Sox));
    assert!(!logger.has_standard(ComplianceStandard::Pci));
}
