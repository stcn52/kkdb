// ── R21 集成测试: 查询执行引擎v2 / 全文检索高级 / 向量搜索进阶 / 可观测性v2 ──

// ═══════════════════════════════════════════════════════════════════
// 1. VectorizedEngine2 / CompiledExpr / ParallelQueryCoord / AdaptiveMemoryManager
// ═══════════════════════════════════════════════════════════════════

use crate::vm::optimizer::exec_engine_v2::{
    AdaptiveMemoryManager, CompiledExpr, DataBatch, JitExpr, MemRegion, ParallelQueryCoord,
    PartitionStrategy, VectorizedEngine2,
};

#[test]
fn test_r21_vectorized_engine2_filter_sum() {
    let mut eng = VectorizedEngine2::new(1024);
    let mut batch = DataBatch::new();
    batch.add_int_column("id", vec![1, 2, 3, 4, 5]);
    batch.add_int_column("val", vec![10, 20, 30, 40, 50]);

    let filtered = eng.filter_gt(&batch, "id", 2);
    assert_eq!(filtered.row_count, 3);
    let sum = eng.sum_int(&batch, "val");
    assert_eq!(sum, 150);
    assert_eq!(eng.ops_executed(), 2);
}

#[test]
fn test_r21_expr_jit_compile_eval() {
    let expr = JitExpr::Add(Box::new(JitExpr::Column(0)), Box::new(JitExpr::Const(100)));
    let mut compiled = CompiledExpr::compile(expr);
    let row = vec![42i64, 0];
    assert_eq!(compiled.eval(&row), 142);
}

#[test]
fn test_r21_expr_jit_batch() {
    let expr = JitExpr::Mul(Box::new(JitExpr::Column(0)), Box::new(JitExpr::Column(1)));
    let mut compiled = CompiledExpr::compile(expr);
    let rows = vec![vec![3, 4], vec![5, 6]];
    let results = compiled.eval_batch(&rows);
    assert_eq!(results, vec![12, 30]);
}

#[test]
fn test_r21_parallel_query_coord() {
    let mut coord = ParallelQueryCoord::new(4, PartitionStrategy::RoundRobin);
    let shards = coord.plan_shards(100);
    assert_eq!(shards.len(), 4);
    for shard in &shards {
        coord.complete_shard(shard.shard_id);
    }
    assert!(coord.all_complete());
    assert_eq!(coord.progress(), (4, 4));
}

#[test]
fn test_r21_adaptive_memory_mgr() {
    let mut mgr = AdaptiveMemoryManager::new(1024 * 1024, 0.9); // 1MB, 90%
    assert!(mgr.allocate(MemRegion::BufferPool, 256 * 1024));
    assert!(mgr.allocate(MemRegion::SortBuffer, 512 * 1024));
    assert!(!mgr.should_spill());
    // Try to allocate beyond capacity
    assert!(!mgr.allocate(MemRegion::HashTable, 512 * 1024));
    mgr.release(MemRegion::BufferPool, 256 * 1024);
    assert!(mgr.allocate(MemRegion::HashTable, 256 * 1024));
    assert_eq!(mgr.region_usage(MemRegion::SortBuffer), 512 * 1024);
}

// ═══════════════════════════════════════════════════════════════════
// 2. FuzzySearcher / SynonymExpander / FacetedSearchManager / RealTimeIndexer
// ═══════════════════════════════════════════════════════════════════

use crate::fulltext::fts_advanced::{
    FacetedSearchManager, FuzzySearcher, IndexOp, RealTimeIndexer, SynonymExpander,
};

#[test]
fn test_r21_fuzzy_search_exact() {
    let mut searcher = FuzzySearcher::new(2);
    searcher.add_term("database");
    searcher.add_term("dataflow");
    searcher.add_term("framework");
    let results = searcher.search("databse"); // typo
    assert!(!results.is_empty());
    assert_eq!(results[0].term, "database");
}

#[test]
fn test_r21_fuzzy_edit_distance() {
    assert_eq!(FuzzySearcher::edit_distance("kitten", "sitting"), 3);
    assert_eq!(FuzzySearcher::edit_distance("", "abc"), 3);
    assert_eq!(FuzzySearcher::edit_distance("same", "same"), 0);
}

#[test]
fn test_r21_synonym_expander() {
    let mut exp = SynonymExpander::new();
    exp.add_group("fast", vec!["quick", "rapid"]);
    exp.add_group("big", vec!["large", "huge"]);
    let expanded = exp.expand("fast");
    assert!(expanded.contains(&"quick".to_string()));
    assert!(expanded.contains(&"rapid".to_string()));
    let query = exp.expand_query(&["fast", "big"]);
    assert!(query.contains(&"quick".to_string()) || query.contains(&"rapid".to_string()));
}

#[test]
fn test_r21_faceted_search() {
    let mut facets = FacetedSearchManager::new();
    facets.define_facet("category");
    facets.define_facet("color");
    facets.index_document(&[("category", "electronics")]);
    facets.index_document(&[("category", "electronics")]);
    facets.index_document(&[("category", "clothing")]);
    facets.index_document(&[("color", "red")]);
    facets.index_document(&[("color", "blue")]);

    let cat = facets.get_facet("category").unwrap();
    assert_eq!(cat.unique_values(), 2);
    let top = cat.top_values(1);
    assert_eq!(top[0].value, "electronics");
    assert_eq!(top[0].count, 2);
}

#[test]
fn test_r21_realtime_indexer() {
    let mut indexer = RealTimeIndexer::new(3);
    indexer.enqueue(IndexOp::Insert {
        doc_id: 1,
        terms: vec!["hello".into(), "world".into()],
    });
    indexer.enqueue(IndexOp::Insert {
        doc_id: 2,
        terms: vec!["foo".into()],
    });
    assert!(!indexer.should_flush());
    indexer.enqueue(IndexOp::Insert {
        doc_id: 3,
        terms: vec!["baz".into()],
    });
    assert!(indexer.should_flush());
    let ops = indexer.flush();
    assert_eq!(ops.len(), 3);
    assert!(!indexer.should_flush());
    assert_eq!(indexer.indexed_doc_count(), 3);
}

// ═══════════════════════════════════════════════════════════════════
// 3. MultiVectorIndex / HybridSearcher / QuantizedCompressor / BatchImporter
// ═══════════════════════════════════════════════════════════════════

use crate::vector::vector_advanced::{
    BatchImporter, DistanceMetric, HybridSearcher, ImportStatus, MultiVectorIndex, QuantizeMethod,
    QuantizedCompressor, VectorIndexConfig,
};

#[test]
fn test_r21_multi_vector_index() {
    let mut idx = MultiVectorIndex::new();
    idx.create_index(VectorIndexConfig {
        name: "emb128".into(),
        dim: 128,
        metric: DistanceMetric::Cosine,
        ef_construction: 200,
        max_neighbors: 16,
    });
    idx.insert("emb128", 1, vec![1.0; 128]);
    idx.insert("emb128", 2, vec![0.5; 128]);
    let results = idx.search("emb128", &vec![1.0; 128], 2);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, 1); // closest to self
}

#[test]
fn test_r21_multi_vector_euclidean() {
    let mut idx = MultiVectorIndex::new();
    idx.create_index(VectorIndexConfig {
        name: "pos".into(),
        dim: 3,
        metric: DistanceMetric::Euclidean,
        ef_construction: 100,
        max_neighbors: 8,
    });
    idx.insert("pos", 1, vec![0.0, 0.0, 0.0]);
    idx.insert("pos", 2, vec![1.0, 1.0, 1.0]);
    idx.insert("pos", 3, vec![10.0, 10.0, 10.0]);
    let results = idx.search("pos", &[0.0, 0.0, 0.0], 2);
    assert_eq!(results[0].0, 1);
    assert_eq!(results[1].0, 2);
}

#[test]
fn test_r21_hybrid_searcher() {
    let mut hs = HybridSearcher::new(0.6);
    let vec_results = vec![(1u64, 0.1f32), (2, 0.5), (3, 0.9)];
    let kw_results = vec![(2u64, 0.8f32), (4, 0.6)];
    let merged = hs.merge(&vec_results, &kw_results, 5);
    assert!(!merged.is_empty());
    // doc 2 should appear since it's in both
    let doc2 = merged.iter().find(|r| r.doc_id == 2).unwrap();
    assert!(doc2.combined_score > 0.0);
}

#[test]
fn test_r21_quantized_compressor() {
    let mut comp = QuantizedCompressor::new(QuantizeMethod::Scalar8);
    comp.train(&[vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]]);
    let q = comp.compress(1, &[1.5, 2.5, 3.5]);
    assert_eq!(q.data.len(), 3);
    assert_eq!(q.original_dim, 3);
}

#[test]
fn test_r21_batch_importer() {
    let mut importer = BatchImporter::new(100);
    let job_id = importer.create_job("emb_index", 500);
    assert!(importer.import_batch(job_id, 200));
    assert_eq!(importer.job_progress(job_id), Some((200, 500)));
    assert!(importer.import_batch(job_id, 300));
    assert_eq!(importer.job_status(job_id), Some(ImportStatus::Complete));
    assert_eq!(importer.total_imported(), 500);
}

// ═══════════════════════════════════════════════════════════════════
// 4. DistributedTracer / MetricsAggregator / HealthDashboard / AlertRuleEngine
// ═══════════════════════════════════════════════════════════════════

use crate::vm::monitor::observability_v2::{
    AlertCondition, AlertLevel, AlertRuleEngine, DistributedTracer, HealthDashboard, HealthState,
    MetricsAggregator, SpanStatus,
};

#[test]
fn test_r21_distributed_tracer_multi_span() {
    let mut tracer = DistributedTracer::new(500);
    let (tid, root) = tracer.start_trace("SELECT * FROM t", "frontend");
    let parse = tracer.start_span(tid, Some(root), "parse", "sql-parser");
    tracer.finish_span(parse, 50, SpanStatus::Ok);
    let exec = tracer.start_span(tid, Some(root), "execute", "vm");
    let scan = tracer.start_span(tid, Some(exec), "table_scan", "storage");
    tracer.finish_span(scan, 200, SpanStatus::Ok);
    tracer.finish_span(exec, 300, SpanStatus::Ok);
    tracer.finish_span(root, 400, SpanStatus::Ok);

    let trace = tracer.get_trace(tid);
    assert_eq!(trace.len(), 4);
    assert_eq!(tracer.error_spans().len(), 0);
}

#[test]
fn test_r21_metrics_aggregator_combined() {
    let mut agg = MetricsAggregator::new(50);
    agg.inc_counter("insert_ops", 100.0);
    agg.set_gauge("active_txns", 5.0);
    for i in 0..20 {
        agg.observe("scan_time_us", (i * 10) as f64);
    }
    assert_eq!(agg.get_counter("insert_ops"), 100.0);
    assert_eq!(agg.get_gauge("active_txns"), 5.0);
    let w = agg.get_window("scan_time_us").unwrap();
    assert_eq!(w.min(), 0.0);
    assert_eq!(w.max(), 190.0);
    assert!((w.avg() - 95.0).abs() < 0.1);
    let names = agg.metric_names();
    assert!(names.len() >= 3);
}

#[test]
fn test_r21_health_dashboard_lifecycle() {
    let mut dash = HealthDashboard::new(1000, 2);
    dash.register("pager");
    dash.register("wal");
    assert_eq!(dash.overall_state(), HealthState::Degraded); // Unknown initially

    dash.report_healthy("pager", "ok", 100);
    dash.report_healthy("wal", "ok", 100);
    assert_eq!(dash.overall_state(), HealthState::Healthy);

    dash.report_failure("wal", "fsync slow", 200);
    assert_eq!(dash.overall_state(), HealthState::Degraded);
    dash.report_failure("wal", "fsync failed", 300);
    assert_eq!(dash.overall_state(), HealthState::Unhealthy);
    assert_eq!(dash.component_count(), 2);
}

#[test]
fn test_r21_alert_engine_full_flow() {
    let mut engine = AlertRuleEngine::new(200);
    engine.add_rule(
        "high_latency",
        "query_latency_ms",
        AlertCondition::ThresholdAbove(500.0),
        AlertLevel::Warning,
        1000,
    );
    engine.add_rule(
        "disk_full",
        "disk_free_pct",
        AlertCondition::ThresholdBelow(10.0),
        AlertLevel::Critical,
        0,
    );

    // Normal values — no alerts
    let a1 = engine.evaluate("query_latency_ms", 100.0, 1000);
    assert!(a1.is_empty());

    // High latency triggers
    let a2 = engine.evaluate("query_latency_ms", 600.0, 2000);
    assert_eq!(a2.len(), 1);
    assert_eq!(a2[0].level, AlertLevel::Warning);

    // Cooldown — no re-fire
    let a3 = engine.evaluate("query_latency_ms", 700.0, 2500);
    assert!(a3.is_empty());

    // Disk alert
    let a4 = engine.evaluate("disk_free_pct", 5.0, 3000);
    assert_eq!(a4.len(), 1);
    assert_eq!(a4[0].level, AlertLevel::Critical);

    assert_eq!(engine.total_fired(), 2);
    assert_eq!(engine.critical_alerts().len(), 1);
}

#[test]
fn test_r21_alert_disable_and_count() {
    let mut engine = AlertRuleEngine::new(50);
    let r1 = engine.add_rule(
        "a",
        "m",
        AlertCondition::ThresholdAbove(0.0),
        AlertLevel::Info,
        0,
    );
    let _r2 = engine.add_rule(
        "b",
        "m",
        AlertCondition::ThresholdAbove(0.0),
        AlertLevel::Info,
        0,
    );
    assert_eq!(engine.rule_count(), 2);
    assert_eq!(engine.active_rule_count(), 2);
    engine.disable_rule(r1);
    assert_eq!(engine.active_rule_count(), 1);
}
