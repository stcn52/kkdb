// ── R22 集成测试: 存储引擎深层 / SQL管线 / 分布式进阶 / 开发者工具 ──

// ═══════════════════════════════════════════════════════════════════
// 1. ColumnarEngine / PartitionManager / TierManager / SpaceReclaimer
// ═══════════════════════════════════════════════════════════════════

use crate::storage::ext::deep_storage::{
    ColumnarEngine, ColumnSegment, ColumnEncoding,
    PartitionManager, PartitionScheme,
    TierManager, DataTier,
    SpaceReclaimer,
};

#[test]
fn test_r22_columnar_project_scan() {
    let mut eng = ColumnarEngine::new(1024);
    eng.add_segment(ColumnSegment {
        column_name: "id".into(), encoding: ColumnEncoding::DeltaBinary,
        row_count: 500, null_count: 0, min_value: 1, max_value: 500,
        compressed_size: 200, uncompressed_size: 4000,
    });
    eng.add_segment(ColumnSegment {
        column_name: "name".into(), encoding: ColumnEncoding::Dictionary,
        row_count: 500, null_count: 2, min_value: 0, max_value: 0,
        compressed_size: 1000, uncompressed_size: 5000,
    });
    let scan = eng.project_scan(&["id"]);
    assert_eq!(scan.len(), 1);
    assert_eq!(eng.column_count(), 2);
    assert!(eng.avg_compression() < 0.5);
}

#[test]
fn test_r22_columnar_segment_skip() {
    let mut eng = ColumnarEngine::new(256);
    for i in 0..10 {
        eng.add_segment(ColumnSegment {
            column_name: "val".into(), encoding: ColumnEncoding::Plain,
            row_count: 100, null_count: 0, min_value: i * 100, max_value: (i + 1) * 100 - 1,
            compressed_size: 400, uncompressed_size: 800,
        });
    }
    let matched = eng.segment_skip("val", 250, 350);
    assert_eq!(matched.len(), 2); // [200-299] and [300-399]
}

#[test]
fn test_r22_partition_manager() {
    let mut pm = PartitionManager::new();
    pm.add_scheme(PartitionScheme::Hash { column: "user_id".into(), num_buckets: 4 });
    let p1 = pm.create_partition("p0", 0);
    let p2 = pm.create_partition("p1", 0);
    pm.add_rows(p1, 1000, 8192);
    pm.add_rows(p2, 500, 4096);
    assert_eq!(pm.total_rows(), 1500);
    pm.deactivate(p2);
    assert_eq!(pm.active_partitions(), 1);
}

#[test]
fn test_r22_tier_manager() {
    let mut tm = TierManager::new(5, 2, 10000);
    tm.register_block(1, 4096, 1000);
    tm.register_block(2, 4096, 1000);
    for _ in 0..6 { tm.access(1, 5000); }
    tm.rebalance(20000);
    let summary = tm.tier_summary();
    assert!(summary.get(&DataTier::Hot).is_some());
    assert_eq!(tm.block_count(), 2);
}

#[test]
fn test_r22_space_reclaimer() {
    let mut sr = SpaceReclaimer::new(100);
    sr.free_page(5, 1, 4096);
    sr.free_page(10, 2, 4096);
    assert_eq!(sr.free_page_count(), 2);
    let reused = sr.allocate_page();
    assert!(reused.is_some());
    assert_eq!(sr.free_page_count(), 1);
    assert!(sr.utilization() > 0.9);
}

// ═══════════════════════════════════════════════════════════════════
// 2. StreamProcessor / MultiStageAggregator / SubqueryOptimizer / PlanCachePool
// ═══════════════════════════════════════════════════════════════════

use crate::vm::engine::sql_pipeline::{
    StreamProcessor, StreamOp, StreamChunk,
    MultiStageAggregator, AggFunc,
    SubqueryOptimizer, SubqueryType, RewriteStrategy,
    PlanCachePool,
};

#[test]
fn test_r22_stream_processor() {
    let mut sp = StreamProcessor::new();
    sp.add_op(StreamOp::Filter { column_idx: 0, threshold: 2 });
    sp.add_op(StreamOp::Project { column_indices: vec![1] });
    let chunk = StreamChunk {
        chunk_id: 1,
        rows: vec![vec![1, 10], vec![2, 20], vec![3, 30], vec![4, 40]],
        is_last: true,
    };
    let result = sp.process(chunk);
    assert_eq!(result.rows.len(), 2); // 3, 4 pass
    assert_eq!(result.rows[0], vec![30]);
}

#[test]
fn test_r22_multi_stage_aggregator() {
    let mut agg = MultiStageAggregator::new(0);
    agg.add_stage(AggFunc::Sum, 1);
    agg.add_stage(AggFunc::Count, 1);
    agg.partial_aggregate(&[
        vec![1, 10], vec![1, 20], vec![2, 30], vec![2, 40],
    ]);
    let results = agg.finalize();
    assert_eq!(results.len(), 2);
    let g1 = results.iter().find(|(k, _)| k == "1").unwrap();
    assert_eq!(g1.1[0], 30.0); // sum
    assert_eq!(g1.1[1], 2.0);  // count
}

#[test]
fn test_r22_subquery_optimizer() {
    let opt = SubqueryOptimizer::new();
    assert_eq!(opt.recommend(SubqueryType::Exists, false), RewriteStrategy::SemiJoin);
    assert_eq!(opt.recommend(SubqueryType::Exists, true), RewriteStrategy::AntiJoin);
    assert_eq!(opt.recommend(SubqueryType::Correlated, false), RewriteStrategy::Decorrelate);
}

#[test]
fn test_r22_plan_cache() {
    let mut cache = PlanCachePool::new(3);
    cache.insert("SELECT * FROM t", 10.0, 1000);
    let hit = cache.lookup("SELECT * FROM t");
    assert!(hit.is_some());
    assert_eq!(cache.total_hits(), 1);
    cache.invalidate("SELECT * FROM t");
    assert_eq!(cache.size(), 0);
}

// ═══════════════════════════════════════════════════════════════════
// 3. MultiRaftGroupManager / CrossRegionReplicator / DynamicLoadBalancer / SelfHealer
// ═══════════════════════════════════════════════════════════════════

use crate::raft::features::dist_advanced::{
    MultiRaftGroupManager, CrossRegionReplicator,
    DynamicLoadBalancer, LbStrategy,
    SelfHealer, FaultType, HealAction,
};

#[test]
fn test_r22_multi_raft_group() {
    let mut mgr = MultiRaftGroupManager::new();
    let g1 = mgr.create_group("shard_1", vec![1, 2, 3]);
    mgr.create_group("shard_2", vec![4, 5, 6]);
    mgr.elect_leader(g1, 2);
    let info = mgr.get_group(g1).unwrap();
    assert_eq!(info.leader_id, Some(2));
    assert_eq!(info.term, 1);
    assert_eq!(mgr.active_groups().len(), 2);
}

#[test]
fn test_r22_cross_region_replication() {
    let mut rep = CrossRegionReplicator::new();
    rep.add_region(1, "us-east", 10, true);
    rep.add_region(2, "eu-west", 80, false);
    rep.setup_replication(1, 2);
    rep.add_lag(1, 2, 500);
    rep.sync_progress(1, 2, 500, 8192);
    assert_eq!(rep.synced_tasks(), 1);
    assert_eq!(rep.total_bytes_transferred(), 8192);
}

#[test]
fn test_r22_load_balancer() {
    let mut lb = DynamicLoadBalancer::new(LbStrategy::LeastConnections);
    lb.register_node(1, 1.0);
    lb.register_node(2, 1.0);
    lb.update_load(1, 50.0, 60.0, 100.0, 10);
    lb.update_load(2, 30.0, 40.0, 50.0, 2);
    let chosen = lb.select_node().unwrap();
    assert_eq!(chosen, 2);
    lb.release_connection(2);
    assert_eq!(lb.dispatches(), 1);
}

#[test]
fn test_r22_self_healer() {
    let mut healer = SelfHealer::new();
    let f1 = healer.report_fault(FaultType::NodeCrash, 3, 1000);
    let f2 = healer.report_fault(FaultType::DiskFull, 5, 2000);
    assert_eq!(healer.unresolved_faults().len(), 2);
    assert_eq!(healer.auto_heal(f1).unwrap(), HealAction::Failover);
    assert_eq!(healer.auto_heal(f2).unwrap(), HealAction::CompactStorage);
    assert!(healer.unresolved_faults().is_empty());
}

// ═══════════════════════════════════════════════════════════════════
// 4. ExplainVisualizer / QueryProfiler / SchemaMigrator / DataTransporter
// ═══════════════════════════════════════════════════════════════════

use crate::vm::engine::dev_experience::{
    ExplainVisualizer, QueryProfiler, QueryProfile,
    SchemaMigrator, MigrationOp,
    DataTransporter, ExportFormat,
};

#[test]
fn test_r22_explain_visualizer() {
    let mut vis = ExplainVisualizer::new();
    let scan = vis.add_node("SeqScan", Some("users"), 1000, 50.0, vec![]);
    let _proj = vis.add_node("Project", None, 100, 5.0, vec![scan]);
    let tree = vis.render_tree();
    assert!(tree.contains("SeqScan"));
    assert!(tree.contains("users"));
    assert_eq!(vis.bottleneck().unwrap().operator, "SeqScan");
}

#[test]
fn test_r22_query_profiler() {
    let mut profiler = QueryProfiler::new(100);
    profiler.record(QueryProfile {
        sql: "SELECT * FROM t".into(),
        parse_us: 10, optimize_us: 50, execute_us: 1000,
        total_us: 1060, rows_scanned: 10000, rows_returned: 100,
        buffer_hits: 900, buffer_misses: 100, io_reads: 10,
    });
    let slowest = profiler.slowest(1);
    assert_eq!(slowest[0].total_us, 1060);
    assert!(profiler.avg_latency_us() > 0.0);
}

#[test]
fn test_r22_schema_migrator() {
    let mut mig = SchemaMigrator::new();
    mig.add_migration(1, "v1", MigrationOp::AddColumn, "UP 1", "DOWN 1");
    mig.add_migration(2, "v2", MigrationOp::AddIndex, "UP 2", "DOWN 2");
    let sqls = mig.migrate_to(2);
    assert_eq!(sqls.len(), 2);
    assert_eq!(mig.current_version(), 2);
    let downs = mig.rollback_to(0);
    assert_eq!(downs.len(), 2);
    assert_eq!(mig.pending_count(), 2);
}

#[test]
fn test_r22_data_transporter() {
    let mut dt = DataTransporter::new();
    let jid = dt.create_export("users", ExportFormat::Csv);
    dt.process_batch(jid, 1000, 50000);
    dt.complete_job(jid);
    assert_eq!(dt.total_exported_rows(), 1000);
    assert_eq!(dt.active_jobs(), 0);
    let csv = DataTransporter::format_csv_row(&["a", "b,c", "d\"e"]);
    assert!(csv.contains("\"b,c\""));
}
