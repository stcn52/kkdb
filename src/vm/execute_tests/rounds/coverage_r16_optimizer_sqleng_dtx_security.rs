// R16 integration tests for:
//   - storage::optimizer (AdaptiveCompression, DataTierManager, IoScheduler, PageWarmer, IncrementalBackup)
//   - vm::sql_engine_adv (MaterializedView, CursorPager, AsyncPipeline, JitCompiledExpr, PlanCacheEvictor)
//   - raft::dist_txn (LockUpgradeManager, GlobalSerializer, DistributedDdlCoordinator, SchemaVersionManager)
//   - vm::security (ColumnEncryption, AuditArchiver, DataMasker, TlsConfig, PasswordPolicy)

// ── storage::optimizer ────────────────────────────────────────────────

use crate::storage::optimizer::*;

#[test]
fn test_adaptive_compression_hot_cold() {
    let mut ac = AdaptiveCompression::new(3);
    assert_eq!(ac.on_access(1), CompressionAlgo::Zstd); // cold
    ac.on_access(1);
    assert_eq!(ac.on_access(1), CompressionAlgo::Lz4); // 3rd access → hot
    assert_eq!(ac.page_count(), 1);
}

#[test]
fn test_adaptive_compression_default() {
    let mut ac = AdaptiveCompression::new(10);
    ac.set_default(CompressionAlgo::Snappy);
    assert_eq!(ac.recommend(999), CompressionAlgo::Snappy);
}

#[test]
fn test_data_tier_promotion_demotion() {
    let mut dtm = DataTierManager::new(5, 2, 10);
    dtm.add_segment(1, 4096);
    dtm.add_segment(2, 4096);
    // Access segment 1 enough to promote to hot
    for _ in 0..6 {
        dtm.access(1);
    }
    assert_eq!(dtm.segments_in_tier(DataTier::Hot), vec![1]);

    // Access segment 2 enough to advance tick, then check cold demotion
    // Segment 2 was added but never accessed after add, so it'll be demoted
    // But we need tick to advance enough
    for _ in 0..12 {
        dtm.access(1);
    } // advance tick via seg 1
    let demoted = dtm.demote_cold();
    assert!(demoted.contains(&2));
    assert_eq!(dtm.segment_count(), 2);
}

#[test]
fn test_io_scheduler_ordering() {
    let mut sched = IoScheduler::new();
    sched.submit(IoType::Write, 1); // prio 80
    sched.submit(IoType::Prefetch, 2); // prio 50
    sched.submit(IoType::Read, 3); // prio 100
    sched.submit(IoType::Sync, 4); // prio 120

    let first = sched.next().unwrap();
    assert_eq!(first.io_type, IoType::Sync);
    let second = sched.next().unwrap();
    assert_eq!(second.io_type, IoType::Read);
    assert_eq!(sched.pending(), 2);
    assert_eq!(sched.completed(), 2);
}

#[test]
fn test_io_scheduler_custom_priority() {
    let mut sched = IoScheduler::new();
    sched.submit_priority(IoType::Prefetch, 1, 999);
    sched.submit(IoType::Sync, 2);
    let first = sched.next().unwrap();
    assert_eq!(first.io_type, IoType::Prefetch); // custom prio 999 > 120
}

#[test]
fn test_page_warmer_top_n() {
    let mut pw = PageWarmer::new(2);
    for _ in 0..100 {
        pw.record(5);
    }
    for _ in 0..50 {
        pw.record(3);
    }
    for _ in 0..10 {
        pw.record(7);
    }
    let warm = pw.warm_list();
    assert_eq!(warm.len(), 2);
    assert_eq!(warm[0], 5);
    assert_eq!(warm[1], 3);
    pw.reset();
    assert_eq!(pw.tracked_pages(), 0);
}

#[test]
fn test_incremental_backup_restore_chain() {
    let mut bk = IncrementalBackup::new();
    let full = bk.full_backup(100, 5000, 1);
    let inc1 = bk.incremental_backup(full, 100, 200, 200, 2).unwrap();
    let inc2 = bk.incremental_backup(inc1, 200, 300, 100, 3).unwrap();

    assert_eq!(bk.restore_chain(inc2), vec![full, inc1, inc2]);
    assert_eq!(bk.chain_size(inc2), 5300);
    assert_eq!(bk.latest_lsn(), 300);
    assert_eq!(bk.backup_count(), 3);
    assert!(bk.incremental_backup(999, 0, 50, 100, 4).is_none());
}

#[test]
fn test_compression_algo_cost() {
    assert!(CompressionAlgo::Zstd.cpu_cost() > CompressionAlgo::Lz4.cpu_cost());
    assert_eq!(CompressionAlgo::None.estimated_ratio(), 1.0);
}

// ── vm::sql_engine_adv ───────────────────────────────────────────────

use crate::vm::sql_engine_adv::*;

#[test]
fn test_materialized_view_lifecycle() {
    let mut mv = MaterializedView::new(
        "sales_summary",
        "SELECT region, SUM(amount) FROM sales GROUP BY region",
        vec!["sales".to_string()],
    );
    mv.full_refresh(50, 1);
    assert!(!mv.is_stale());
    assert_eq!(mv.row_count(), 50);

    mv.on_source_change(ViewChange {
        change_type: ChangeType::Insert,
        table: "sales".to_string(),
        row_id: 100,
        timestamp: 2,
    });
    mv.on_source_change(ViewChange {
        change_type: ChangeType::Delete,
        table: "sales".to_string(),
        row_id: 5,
        timestamp: 3,
    });
    assert!(mv.is_stale());
    assert_eq!(mv.pending_count(), 2);

    let applied = mv.incremental_refresh(3);
    assert_eq!(applied, 2);
    assert_eq!(mv.row_count(), 50); // +1 -1 = net zero
}

#[test]
fn test_materialized_view_ignore_unrelated() {
    let mut mv = MaterializedView::new("v", "q", vec!["t1".to_string()]);
    mv.full_refresh(10, 1);
    mv.on_source_change(ViewChange {
        change_type: ChangeType::Insert,
        table: "t2".to_string(), // not in source_tables
        row_id: 1,
        timestamp: 2,
    });
    assert!(!mv.is_stale());
}

#[test]
fn test_cursor_pager_lifecycle() {
    let mut cp = CursorPager::new();
    let c1 = cp.open("SELECT * FROM big_table", 20);
    cp.set_total(c1, 50);

    assert_eq!(cp.next_page(c1), Some((0, 20)));
    assert_eq!(cp.next_page(c1), Some((20, 20)));
    assert_eq!(cp.next_page(c1), Some((40, 20)));
    assert!(cp.is_exhausted(c1));
    assert_eq!(cp.active_cursors(), 1);
    assert!(cp.close(c1));
    assert_eq!(cp.active_cursors(), 0);
}

#[test]
fn test_cursor_pager_keyed() {
    let mut cp = CursorPager::new();
    let c = cp.open("SELECT * FROM t WHERE id > ?", 10);
    cp.set_last_key(c, 42);
    // Just verifying keyset state is stored
    assert!(!cp.is_exhausted(c));
}

#[test]
fn test_async_pipeline_stages() {
    let mut pipe = AsyncPipeline::new();
    let scan = pipe.add_stage("scan");
    let filter = pipe.add_stage("filter");
    let output = pipe.add_stage("output");

    pipe.stage_mut(scan).unwrap().produce(1000);
    pipe.stage_mut(filter).unwrap().consume(500);
    pipe.stage_mut(filter).unwrap().produce(250);
    pipe.stage_mut(output).unwrap().consume(250);

    assert_eq!(pipe.stage(scan).unwrap().buffer_size(), 1000); // all still buffered
    assert!(!pipe.is_complete());

    for s in 0..3 {
        pipe.stage_mut(s).unwrap().complete();
    }
    assert!(pipe.is_complete());
    assert_eq!(pipe.stage_count(), 3);
}

#[test]
fn test_jit_expr_arithmetic() {
    let expr = JitCompiledExpr::new(
        vec![
            JitOp::LoadReg(0),
            JitOp::LoadReg(1),
            JitOp::Add,
            JitOp::LoadImm(10),
            JitOp::Mul,
            JitOp::Ret,
        ],
        2,
    );
    assert_eq!(expr.eval(&[3, 7]), 100); // (3+7)*10
}

#[test]
fn test_jit_expr_div_by_zero() {
    let expr = JitCompiledExpr::new(
        vec![
            JitOp::LoadImm(10),
            JitOp::LoadImm(0),
            JitOp::Div,
            JitOp::Ret,
        ],
        0,
    );
    assert_eq!(expr.eval(&[]), 0); // safe div by zero
}

#[test]
fn test_plan_cache_evictor_lfu() {
    let mut cache = PlanCacheEvictor::new(3, 100000);
    for i in 1..=3 {
        cache.insert(CachedPlan {
            plan_id: i,
            sql_hash: i * 10,
            use_count: 0,
            last_used: 0,
            cost: 1.0,
            byte_size: 100,
        });
    }
    // Touch plan 2 many times
    for _ in 0..20 {
        cache.touch(2, 1);
    }
    // Insert 4th → evicts least used
    cache.insert(CachedPlan {
        plan_id: 4,
        sql_hash: 40,
        use_count: 0,
        last_used: 0,
        cost: 1.0,
        byte_size: 100,
    });
    assert_eq!(cache.len(), 3);
    assert!(cache.get(2).is_some());
    assert!(cache.eviction_count() > 0);
}

// ── raft::dist_txn ────────────────────────────────────────────────────

use crate::raft::dist_txn::*;

#[test]
fn test_lock_upgrade_s_to_x() {
    let mut lm = LockUpgradeManager::new();
    assert!(lm.acquire(1, "users", LockMode::Shared));
    // Only txn 1 holds the lock → upgrade should succeed
    assert!(lm.upgrade(1, "users"));
    assert!(lm.holds_lock(1, "users", LockMode::Exclusive));
}

#[test]
fn test_lock_upgrade_blocked_by_other() {
    let mut lm = LockUpgradeManager::new();
    assert!(lm.acquire(1, "r", LockMode::Shared));
    assert!(lm.acquire(2, "r", LockMode::Shared));
    assert!(!lm.upgrade(1, "r")); // txn2 holds lock
    assert_eq!(lm.upgrade_queue_len(), 1);
    lm.release(2);
    assert!(lm.upgrade(1, "r"));
    assert!(lm.holds_lock(1, "r", LockMode::Exclusive));
}

#[test]
fn test_global_serializer_conflict() {
    let mut gs = GlobalSerializer::new();
    let begin_ts = gs.allocate_ts();
    let ws: std::collections::HashSet<String> = vec!["x".to_string()].into_iter().collect();
    gs.commit(1, std::collections::HashSet::new(), ws);
    // Txn 2 reads "x" but txn 1 wrote to "x" after begin_ts
    let rs: std::collections::HashSet<String> = vec!["x".to_string()].into_iter().collect();
    assert!(!gs.validate(2, &rs, &std::collections::HashSet::new(), begin_ts));
    assert_eq!(gs.committed_count(), 1);
}

#[test]
fn test_global_serializer_no_conflict() {
    let mut gs = GlobalSerializer::new();
    let begin_ts = gs.allocate_ts();
    let ws: std::collections::HashSet<String> = vec!["x".to_string()].into_iter().collect();
    gs.commit(1, std::collections::HashSet::new(), ws);
    let rs: std::collections::HashSet<String> = vec!["y".to_string()].into_iter().collect();
    assert!(gs.validate(2, &rs, &std::collections::HashSet::new(), begin_ts));
}

#[test]
fn test_distributed_ddl_full_cycle() {
    let mut coord = DistributedDdlCoordinator::new();
    let nodes: std::collections::HashSet<u64> = vec![1, 2].into_iter().collect();
    let op = coord.propose("CREATE INDEX idx ON t(c)", nodes);

    assert_eq!(coord.phase(op), Some(&DdlPhase::Propose));
    coord.receive_ack(op, 1);
    let p = coord.receive_ack(op, 2).unwrap();
    assert_eq!(p, DdlPhase::Prepare);

    // Prepare phase acks
    coord.receive_ack(op, 1);
    coord.receive_ack(op, 2);
    assert_eq!(coord.phase(op), Some(&DdlPhase::Execute));
}

#[test]
fn test_schema_version_manager() {
    let mut svm = SchemaVersionManager::new();
    let v1 = svm.add_version(
        "orders",
        vec![
            ("id".to_string(), "INT".to_string()),
            ("amount".to_string(), "REAL".to_string()),
        ],
        1,
    );
    let v2 = svm.add_version(
        "orders",
        vec![
            ("id".to_string(), "INT".to_string()),
            ("amount".to_string(), "REAL".to_string()),
            ("status".to_string(), "TEXT".to_string()),
        ],
        2,
    );

    assert_eq!(svm.active_version("orders").unwrap().version, v2);
    assert_eq!(svm.version_count("orders"), 2);
    assert_eq!(svm.table_count(), 1);

    match svm.check_compat("orders", v1, v2) {
        SchemaCompat::AddedColumns(cols) => assert!(cols.contains(&"status".to_string())),
        _ => panic!("expected AddedColumns"),
    }
}

// ── vm::security ──────────────────────────────────────────────────────

use crate::vm::security::*;

#[test]
fn test_column_encryption_roundtrip() {
    let mut ce = ColumnEncryption::new();
    ce.add_key("k1", vec![0x42, 0x43, 0x44, 0x45]);
    assert!(ce.encrypt_column("t", "secret", EncryptionAlgo::Aes256, "k1"));
    let plain = b"hello world";
    let enc = ce.encrypt("t", "secret", plain).unwrap();
    assert_ne!(&enc, plain);
    let dec = ce.decrypt("t", "secret", &enc).unwrap();
    assert_eq!(&dec, plain);
    assert_eq!(ce.encrypted_column_count(), 1);
}

#[test]
fn test_audit_archiver_rotation_and_purge() {
    let mut arch = AuditArchiver::new(3, 100000, 50);
    for i in 0..3 {
        arch.add_entry(100, i);
    } // triggers rotation at 3rd entry
    assert_eq!(arch.archive_count(), 1);
    for i in 0..3 {
        arch.add_entry(100, 100 + i);
    } // another rotation
    assert_eq!(arch.archive_count(), 2);
    // First archive end_time=2, second end_time=102
    // purge_old(200): 200-2=198>50, 200-102=98>50 → both purged
    let purged = arch.purge_old(200);
    assert_eq!(purged, 2);
}

#[test]
fn test_data_masker_email_phone() {
    let mut dm = DataMasker::new();
    dm.add_rule(
        "users",
        "email",
        MaskStrategy::Email,
        vec!["dba".to_string()],
    );
    dm.add_rule("users", "phone", MaskStrategy::Phone, Vec::new());

    assert_eq!(
        dm.mask("users", "email", "alice@example.com", "reader"),
        "a***@example.com"
    );
    assert_eq!(
        dm.mask("users", "email", "alice@example.com", "dba"),
        "alice@example.com"
    );
    assert_eq!(
        dm.mask("users", "phone", "12345678901", "reader"),
        "***8901"
    );
    assert_eq!(dm.rule_count(), 2);
}

#[test]
fn test_data_masker_full_and_partial() {
    let mut dm = DataMasker::new();
    dm.add_rule(
        "t",
        "ssn",
        MaskStrategy::Full("***-**-****".to_string()),
        Vec::new(),
    );
    assert_eq!(dm.mask("t", "ssn", "123-45-6789", "user"), "***-**-****");

    dm.add_rule(
        "t",
        "card",
        MaskStrategy::Partial {
            show_first: 4,
            show_last: 4,
            mask_char: 'X',
        },
        Vec::new(),
    );
    assert_eq!(
        dm.mask("t", "card", "4111111111111111", "user"),
        "4111XXXXXXXX1111"
    );
}

#[test]
fn test_tls_config() {
    let mut tls = TlsConfig::new(TlsLevel::VerifyFull)
        .with_cert("/etc/ssl/cert.pem", "/etc/ssl/key.pem")
        .with_ca("/etc/ssl/ca.pem");
    assert!(!tls.allows_plain());
    tls.record_connection(true);
    tls.record_connection(true);
    assert_eq!(tls.tls_connections(), 2);
    assert!((tls.tls_ratio() - 1.0).abs() < 0.01);
}

#[test]
fn test_password_policy_default() {
    let policy = PasswordPolicy::new();
    assert!(policy.is_valid("Secure1xy"));
    assert!(!policy.is_valid("weak"));
    let issues = policy.validate("123");
    assert!(issues.len() >= 3);
}

#[test]
fn test_password_policy_strict() {
    let policy = PasswordPolicy::strict();
    assert!(!policy.is_valid("Abcdefg1h"));
    assert!(policy.is_valid("MyStr0ng!Pass"));
}

#[test]
fn test_password_strength_score() {
    let weak = PasswordPolicy::strength_score("abc");
    let medium = PasswordPolicy::strength_score("Abcdef1g");
    let strong = PasswordPolicy::strength_score("My$tr0ng!P@ss#2024");
    assert!(weak < medium);
    assert!(medium < strong);
}
