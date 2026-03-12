// R14 integration tests: buffer pool, query compiler, distributed txn, observability.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::storage::buffer_pool::{
    LruKEvictor, ReadAheadManager, PrefetchStrategy, WriteCoalescer, AdaptiveBufferPool,
};
use crate::vm::query_compiler::{
    QueryTemplate, TemplateCache, CompiledExpr, CodeOp, RuntimeSpecializer,
    RecompilationTracker,
};
use crate::raft::snapshot_isolation::{
    DistributedSnapshot, SnapshotManager, GlobalDeadlockDetector,
    PartialAggregate, CrossShardPushdown, ShardRebalancer, ShardLoad,
};
use crate::vm::observability::{
    QueryTracer, ResourceQuota, QuotaManager, DdlProgressTracker, DdlState,
    AutoStatsUpdater, StatsRefreshConfig,
};

// ── Buffer Pool ───────────────────────────────────────────────────────

#[test]
fn r14_lru_k_evictor_access_and_evict() {
    let mut ev = LruKEvictor::new(3, 2);
    ev.access(10);
    ev.access(20);
    ev.access(30);
    assert_eq!(ev.len(), 3);
    assert!(ev.is_full());

    let victim = ev.select_victim().unwrap();
    ev.evict(victim);
    assert_eq!(ev.len(), 2);
}

#[test]
fn r14_lru_k_dirty_and_pin() {
    let mut ev = LruKEvictor::new(10, 2);
    ev.access(1);
    ev.access(2);
    ev.mark_dirty(1);
    ev.pin(2);

    assert_eq!(ev.dirty_pages().len(), 1);
    // Pinned page cannot be evicted
    ev.pin(1);
    assert_eq!(ev.select_victim(), None);
}

#[test]
fn r14_read_ahead_sequential() {
    let mut ra = ReadAheadManager::new(PrefetchStrategy::Sequential, 4);
    let pages = ra.on_access(100);
    assert_eq!(pages, vec![101, 102, 103, 104]);
}

#[test]
fn r14_read_ahead_stride() {
    let mut ra = ReadAheadManager::new(PrefetchStrategy::StrideBased, 2);
    ra.on_access(10);
    ra.on_access(20);
    let pages = ra.on_access(30); // stride = 10
    assert_eq!(pages, vec![40, 50]);
}

#[test]
fn r14_write_coalescer() {
    let mut wc = WriteCoalescer::new(3);
    wc.add_write(1, vec![0; 100]);
    wc.add_write(2, vec![0; 200]);
    assert!(!wc.has_pending(5));
    assert!(wc.has_pending(1));
    assert!(wc.add_write(3, vec![0; 300])); // triggers threshold
    let (pages, bytes) = wc.flush();
    assert_eq!(pages.len(), 3);
    assert_eq!(bytes, 600);
}

#[test]
fn r14_adaptive_pool_hit_miss() {
    let mut pool = AdaptiveBufferPool::new(10, 2, 5);
    pool.access_page(1); // miss
    pool.access_page(2); // miss
    pool.access_page(1); // hit
    assert_eq!(pool.hit_count(), 1);
    assert_eq!(pool.miss_count(), 2);
    assert!(pool.hit_ratio() > 0.0 && pool.hit_ratio() < 1.0);
}

#[test]
fn r14_adaptive_pool_adapt() {
    let mut pool = AdaptiveBufferPool::new(5, 2, 5);
    // All misses → low hit ratio
    for i in 0..10 {
        pool.access_page(i);
    }
    pool.adapt();
    assert_eq!(pool.read_ahead.strategy(), PrefetchStrategy::StrideBased);
}

// ── Query Compiler ────────────────────────────────────────────────────

#[test]
fn r14_query_template_bind() {
    let mut t = QueryTemplate::new(
        "q1",
        "SELECT * FROM t WHERE id = $1",
        vec!["$1".into()],
        Duration::from_micros(50),
    );
    let sql = t.bind(&["42"]);
    assert_eq!(sql, "SELECT * FROM t WHERE id = 42");
    assert_eq!(t.use_count, 1);
}

#[test]
fn r14_template_cache() {
    let mut cache = TemplateCache::new(2);
    cache.insert(QueryTemplate::new("a", "SELECT a", vec![], Duration::ZERO));
    cache.insert(QueryTemplate::new("b", "SELECT b", vec![], Duration::ZERO));
    assert_eq!(cache.len(), 2);
    assert!(cache.get("a").is_some());
    assert!(cache.remove("b"));
    assert_eq!(cache.len(), 1);
}

#[test]
fn r14_compiled_expr_arithmetic() {
    // col[0] * 2 + col[1]
    let expr = CompiledExpr::new(vec![
        CodeOp::LoadCol(0),
        CodeOp::LoadConst(2),
        CodeOp::Mul,
        CodeOp::LoadCol(1),
        CodeOp::Add,
    ], 0);
    assert_eq!(expr.eval(&[5, 3]), 13); // 5*2 + 3 = 13
}

#[test]
fn r14_compiled_expr_comparison_and_logic() {
    // col[0] > 5 AND NOT (col[1] < 10)
    let expr = CompiledExpr::new(vec![
        CodeOp::LoadCol(0),
        CodeOp::LoadConst(5),
        CodeOp::Gt,
        CodeOp::LoadCol(1),
        CodeOp::LoadConst(10),
        CodeOp::Lt,
        CodeOp::Not,
        CodeOp::And,
    ], 0);
    assert_eq!(expr.eval(&[10, 20]), 1); // 10>5=true, 20<10=false, NOT false=true, true AND true=1
    assert_eq!(expr.eval(&[3, 20]), 0);  // 3>5=false
}

#[test]
fn r14_compiled_expr_batch() {
    let expr = CompiledExpr::new(vec![
        CodeOp::LoadCol(0),
        CodeOp::LoadConst(10),
        CodeOp::Add,
    ], 0);
    let results = expr.eval_batch(&[vec![1], vec![2], vec![3]]);
    assert_eq!(results, vec![11, 12, 13]);
}

#[test]
fn r14_runtime_specializer() {
    let expr = CompiledExpr::new(vec![
        CodeOp::LoadCol(0),
        CodeOp::LoadCol(1),
        CodeOp::Add,
    ], 0);
    let mut consts = HashMap::new();
    consts.insert(1usize, 99i64);
    let specialized = RuntimeSpecializer::specialize(&expr, &consts);
    assert_eq!(specialized.eval(&[1, 0]), 100); // col[0]=1 + const 99
}

#[test]
fn r14_peephole_fold() {
    let expr = CompiledExpr::new(vec![
        CodeOp::LoadConst(6),
        CodeOp::LoadConst(7),
        CodeOp::Mul,
    ], 0);
    let folded = RuntimeSpecializer::peephole_fold(&expr);
    assert_eq!(folded.instruction_count(), 1); // just LoadConst(42)
    assert_eq!(folded.eval(&[]), 42);
}

#[test]
fn r14_recompilation_tracker() {
    let mut tracker = RecompilationTracker::new(Duration::from_millis(10), 2);
    tracker.record("q1", Duration::from_millis(50));
    assert!(!tracker.should_recompile("q1")); // only 1 exec
    tracker.record("q1", Duration::from_millis(50));
    assert!(tracker.should_recompile("q1")); // avg 50ms > 10ms threshold
    assert_eq!(tracker.execution_count("q1"), 2);
}

// ── Distributed Txn ──────────────────────────────────────────────────

#[test]
fn r14_distributed_snapshot() {
    let mut snap = DistributedSnapshot::new(1);
    snap.set_node_timestamp(1, 100);
    snap.set_node_timestamp(2, 90);
    snap.add_active_txn(50);

    assert!(snap.is_visible(30));   // committed
    assert!(!snap.is_visible(50));  // active
    assert_eq!(snap.global_watermark(), 90);
    assert_eq!(snap.node_count(), 2);
}

#[test]
fn r14_snapshot_manager() {
    let mut sm = SnapshotManager::new();
    let mut ts = HashMap::new();
    ts.insert(1u64, 100u64);
    let id = sm.create_snapshot(ts, HashSet::new());
    assert!(sm.get_snapshot(id).is_some());
    sm.release_snapshot(id);
    assert_eq!(sm.active_count(), 0);
}

#[test]
fn r14_global_deadlock_detection() {
    let mut dd = GlobalDeadlockDetector::new();
    dd.add_edge(1, 2, 0);
    dd.add_edge(2, 3, 0);
    dd.add_edge(3, 1, 0); // cycle: 1→2→3→1
    let cycles = dd.detect_cycles();
    assert!(!cycles.is_empty());

    let victim = GlobalDeadlockDetector::select_victim(&cycles[0]);
    assert!(victim.is_some());
}

#[test]
fn r14_global_deadlock_no_cycle() {
    let mut dd = GlobalDeadlockDetector::new();
    dd.add_edge(1, 2, 0);
    dd.add_edge(2, 3, 0);
    assert!(dd.detect_cycles().is_empty());
    dd.remove_txn(2);
    assert_eq!(dd.edge_count(), 0);
}

#[test]
fn r14_partial_aggregate_merge() {
    let s1 = PartialAggregate::Sum(100);
    let s2 = PartialAggregate::Sum(200);
    let merged = s1.merge(s2).unwrap();
    assert_eq!(merged.finalize(), 300.0);

    let m1 = PartialAggregate::Min(10);
    let m2 = PartialAggregate::Min(5);
    assert_eq!(m1.merge(m2).unwrap().finalize(), 5.0);
}

#[test]
fn r14_cross_shard_pushdown() {
    let mut csp = CrossShardPushdown::new();
    csp.add_partial(1, PartialAggregate::Count(100));
    csp.add_partial(2, PartialAggregate::Count(200));
    let results = csp.merge_all();
    let count = results.iter().find(|r| matches!(r, PartialAggregate::Count(_))).unwrap();
    assert_eq!(count.finalize(), 300.0);
}

#[test]
fn r14_shard_rebalancer() {
    let mut rb = ShardRebalancer::new(1.3);
    rb.update_shard(ShardLoad { shard_id: 1, row_count: 900, disk_bytes: 0, qps: 0.0 });
    rb.update_shard(ShardLoad { shard_id: 2, row_count: 100, disk_bytes: 0, qps: 0.0 });
    assert!(rb.needs_rebalance());
    let plan = rb.plan_rebalance();
    assert!(!plan.is_empty());
    assert_eq!(plan[0].0, 1); // from shard 1
    assert_eq!(plan[0].1, 2); // to shard 2
}

// ── Observability ─────────────────────────────────────────────────────

#[test]
fn r14_query_tracer() {
    let mut tracer = QueryTracer::new(1, "SELECT * FROM t");
    let s1 = tracer.start_span("parse", None);
    tracer.finish_span(s1);
    let s2 = tracer.start_span("execute", None);
    tracer.set_span_metadata(s2, "rows", "100");
    tracer.finish_span(s2);
    assert_eq!(tracer.span_count(), 2);
    assert!(tracer.spans()[0].is_finished());
}

#[test]
fn r14_resource_quota() {
    let mut qm = QuotaManager::new();
    qm.set_quota(
        ResourceQuota::new("user1")
            .with_concurrent_queries(1)
            .with_memory(1024)
    );
    assert!(qm.can_start_query("user1"));
    qm.query_started("user1");
    assert!(!qm.can_start_query("user1"));
    qm.query_finished("user1");
    assert!(qm.can_start_query("user1"));

    qm.update_memory("user1", 800);
    assert!(qm.check_memory("user1", 200));
    assert!(!qm.check_memory("user1", 300));
}

#[test]
fn r14_ddl_progress() {
    let mut tracker = DdlProgressTracker::new();
    let id = tracker.start("ALTER TABLE t ADD COLUMN c INT", 500);
    {
        let p = tracker.get_mut(id).unwrap();
        p.set_state(DdlState::CopyingData);
        p.advance(250);
        assert!((p.percent_complete() - 50.0).abs() < 0.1);
    }
    {
        let p = tracker.get_mut(id).unwrap();
        p.advance(250);
        p.set_state(DdlState::Completed);
    }
    assert_eq!(tracker.cleanup(), 1);
    assert_eq!(tracker.active_count(), 0);
}

#[test]
fn r14_auto_stats_updater() {
    let mut updater = AutoStatsUpdater::new();
    updater.register(StatsRefreshConfig {
        table_name: "orders".into(),
        change_threshold: 0.1,
        min_interval: Duration::from_millis(0),
    });
    updater.set_row_count("orders", 1000);
    updater.record_modification("orders", 50); // 5%
    assert!(!updater.needs_refresh("orders"));
    updater.record_modification("orders", 60); // 11%
    assert!(updater.needs_refresh("orders"));
    let tables = updater.tables_needing_refresh();
    assert!(tables.contains(&"orders".to_string()));
    updater.mark_refreshed("orders");
    assert!(!updater.needs_refresh("orders"));
}
