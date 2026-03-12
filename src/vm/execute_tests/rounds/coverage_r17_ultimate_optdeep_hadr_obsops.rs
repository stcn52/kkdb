// R17 coverage tests — storage ultimate, query optimizer deep, HA/DR, observability ops

#[cfg(test)]
mod tests {
    // ── Storage Ultimate ──────────────────────────────────────────────

    use crate::storage::ultimate::*;

    #[test]
    fn test_adaptive_page_size_default() {
        let aps = AdaptivePageSize::new(PageSize::Small);
        assert_eq!(aps.get_page_size("unknown"), PageSize::Small);
    }

    #[test]
    fn test_adaptive_page_size_observe_medium() {
        let mut aps = AdaptivePageSize::new(PageSize::Small);
        aps.observe_row_size("table1", 700);
        assert_eq!(aps.get_page_size("table1"), PageSize::Medium);
    }

    #[test]
    fn test_adaptive_page_size_observe_huge() {
        let mut aps = AdaptivePageSize::new(PageSize::Small);
        aps.observe_row_size("blob_table", 10000);
        assert_eq!(aps.get_page_size("blob_table"), PageSize::Huge);
    }

    #[test]
    fn test_adaptive_page_size_manual_set() {
        let mut aps = AdaptivePageSize::new(PageSize::Small);
        aps.set_page_size("t1", PageSize::Large);
        assert_eq!(aps.get_page_size("t1"), PageSize::Large);
    }

    #[test]
    fn test_wal_group_commit_no_early_flush() {
        let mut gc = WalGroupCommit::new(5, 2000);
        assert!(!gc.add(1, 100));
        assert!(!gc.add(2, 100));
        assert_eq!(gc.pending_count(), 2);
    }

    #[test]
    fn test_wal_group_commit_full_batch() {
        let mut gc = WalGroupCommit::new(2, 1000);
        gc.add(1, 50);
        assert!(gc.add(2, 50)); // triggers at 2
        let flushed = gc.flush();
        assert_eq!(flushed.len(), 2);
        assert!((gc.avg_batch_size() - 2.0).abs() < 0.01);
    }

    #[test]
    fn test_space_reclaimer_no_compaction_needed() {
        let mut sr = SpaceReclaimer::new(0.3);
        sr.update_page(1, 4096, 3500, 1); // 85% util
        assert!(sr.pages_needing_compaction().is_empty());
    }

    #[test]
    fn test_space_reclaimer_wasted_space() {
        let mut sr = SpaceReclaimer::new(0.5);
        sr.update_page(1, 4096, 1000, 3);
        sr.update_page(2, 4096, 500, 5);
        assert_eq!(sr.page_count(), 2);
        let wasted = sr.total_wasted();
        assert_eq!(wasted, (4096 - 1000) + (4096 - 500));
    }

    #[test]
    fn test_storage_histogram_null_fraction() {
        let mut hist = StorageHistogram::new("col1");
        let vals: Vec<i64> = (0..50).collect();
        hist.build_from_sorted(&vals, 5);
        hist.set_null_count(10);
        assert!((hist.null_fraction() - 0.2).abs() < 0.01);
    }

    #[test]
    fn test_storage_histogram_range_selectivity_full() {
        let mut hist = StorageHistogram::new("id");
        let vals: Vec<i64> = (0..100).collect();
        hist.build_from_sorted(&vals, 10);
        let sel = hist.estimate_range_selectivity(0, 99);
        assert!((sel - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_parallel_checkpoint_worker_assignment() {
        let mut cp = ParallelCheckpoint::new(2);
        cp.start(vec![10, 20, 30, 40], 50);
        assert_eq!(cp.worker_count(), 2);
        assert_eq!(cp.checkpoint_lsn(), 50);
    }

    #[test]
    fn test_parallel_checkpoint_incomplete() {
        let mut cp = ParallelCheckpoint::new(2);
        cp.start(vec![1, 2, 3, 4], 10);
        cp.worker_complete(0);
        assert!(!cp.is_complete()); // worker 1 not done
        cp.worker_complete(1);
        assert!(cp.is_complete());
    }

    // ── Query Optimizer Deep ──────────────────────────────────────────

    use crate::vm::query_opt_deep::*;

    #[test]
    fn test_cost_calibrator_defaults() {
        let cc = CostCalibrator::new();
        assert_eq!(cc.get_factor("random_page_cost"), Some(4.0));
        assert_eq!(cc.get_factor("cpu_tuple_cost"), Some(0.01));
    }

    #[test]
    fn test_cost_calibrator_recalibrate() {
        let mut cc = CostCalibrator::new();
        cc.observe("random_page_cost", 3.0);
        cc.observe("random_page_cost", 5.0);
        cc.calibrate_all();
        assert!((cc.get_factor("random_page_cost").unwrap() - 4.0).abs() < 0.01);
    }

    #[test]
    fn test_join_enumerator_two_tables() {
        let mut je = JoinEnumerator::new();
        je.add_relation("big", 100000.0);
        je.add_relation("small", 100.0);
        je.add_join_edge("small", "big", 0.01);
        let order = je.find_best_order();
        assert_eq!(order[0], "small"); // smaller first
    }

    #[test]
    fn test_predicate_pushdown_multi_table() {
        use std::collections::HashSet;
        let pred = Predicate {
            expr: "t1.x = t2.y".to_string(),
            referenced_tables: {
                let mut s = HashSet::new();
                s.insert("t1".to_string());
                s.insert("t2".to_string());
                s
            },
        };
        let mut root = PushdownNode {
            node_type: "join".to_string(),
            table: None,
            predicates: vec![],
            children: vec![
                PushdownNode {
                    node_type: "scan".to_string(),
                    table: Some("t1".to_string()),
                    predicates: vec![],
                    children: vec![],
                },
                PushdownNode {
                    node_type: "scan".to_string(),
                    table: Some("t2".to_string()),
                    predicates: vec![],
                    children: vec![],
                },
            ],
        };
        // Multi-table pred cannot be pushed to individual scans
        let pushed = PredicatePushdown::push_down(&mut root, pred);
        assert!(!pushed);
        assert_eq!(root.predicates.len(), 1); // stays at join
    }

    #[test]
    fn test_subquery_decorrelation_inner() {
        let sub = CorrelatedSubquery {
            subquery_id: 2,
            outer_refs: vec!["o.id".to_string()],
            inner_table: "items".to_string(),
            predicate: "o.id = i.order_id".to_string(),
            is_exists: false,
        };
        let join = SubqueryDecorrelator::decorrelate(&sub);
        assert_eq!(join.join_type, "inner");
    }

    #[test]
    fn test_subquery_decorrelation_batch() {
        let subs = vec![
            CorrelatedSubquery {
                subquery_id: 1,
                outer_refs: vec!["a.id".to_string()],
                inner_table: "b".to_string(),
                predicate: "a.id=b.a_id".to_string(),
                is_exists: true,
            },
            CorrelatedSubquery {
                subquery_id: 2,
                outer_refs: vec!["c.id".to_string()],
                inner_table: "d".to_string(),
                predicate: "c.id=d.c_id".to_string(),
                is_exists: false,
            },
        ];
        let joins = SubqueryDecorrelator::decorrelate_all(&subs);
        assert_eq!(joins.len(), 2);
        assert_eq!(joins[0].join_type, "semi");
        assert_eq!(joins[1].join_type, "inner");
    }

    #[test]
    fn test_stats_sampler_system_page() {
        let mut s = StatsSampler::new(SamplingMethod::SystemPage, 5);
        for i in 0..3 {
            s.add(i * 10);
        }
        assert_eq!(s.sample_count(), 3);
        assert_eq!(s.total_seen(), 3);
    }

    #[test]
    fn test_stats_sampler_ndv_and_range() {
        let mut s = StatsSampler::new(SamplingMethod::Reservoir, 20);
        for i in 0..20 {
            s.add(i % 5); // 5 distinct values
        }
        assert_eq!(s.estimate_ndv(), 5);
        let (min, max) = s.sample_range().unwrap();
        assert_eq!(min, 0);
        assert_eq!(max, 4);
    }

    // ── HA/DR ─────────────────────────────────────────────────────────

    use crate::raft::ha_dr::*;

    #[test]
    fn test_failover_chain_skip_unhealthy() {
        let mut fc = FailoverChain::new();
        fc.set_leader("n1");
        fc.add_candidate(FailoverCandidate {
            node_id: "n2".to_string(),
            priority: 1,
            is_healthy: false,
            last_sync_lsn: 999,
            region: "us".to_string(),
        });
        fc.add_candidate(FailoverCandidate {
            node_id: "n3".to_string(),
            priority: 2,
            is_healthy: true,
            last_sync_lsn: 500,
            region: "eu".to_string(),
        });
        let best = fc.select_failover().unwrap();
        assert_eq!(best.node_id, "n3"); // n2 skipped
    }

    #[test]
    fn test_replica_syncer_quorum() {
        let mut rs = ReplicaSyncer::new();
        rs.set_primary_lsn(100);
        rs.add_replica("r1");
        rs.add_replica("r2");
        rs.add_replica("r3");
        rs.update_replica("r1", 95, 10);
        rs.update_replica("r2", 90, 20);
        rs.update_replica("r3", 50, 100);
        assert!(rs.quorum_in_sync(15)); // r1(5 lag) and r2(10 lag) in sync = 2/3
    }

    #[test]
    fn test_cross_region_dr_multi_region() {
        let mut dr = CrossRegionDR::new("us-east");
        dr.add_region(RegionConfig {
            region_name: "eu-west".to_string(),
            endpoint: "eu.db".to_string(),
            rpo_target_s: 30,
            rto_target_s: 120,
            last_replicated_lsn: 0,
            last_replicated_ts: 0,
        });
        dr.add_region(RegionConfig {
            region_name: "ap-east".to_string(),
            endpoint: "ap.db".to_string(),
            rpo_target_s: 60,
            rto_target_s: 300,
            last_replicated_lsn: 0,
            last_replicated_ts: 0,
        });
        dr.set_current_time(100);
        dr.update_replication("eu-west", 50, 80); // lag=20
        dr.update_replication("ap-east", 40, 30); // lag=70
        assert!(dr.is_within_rpo("eu-west"));
        assert!(!dr.is_within_rpo("ap-east"));
        assert_eq!(dr.region_count(), 2);
    }

    #[test]
    fn test_rolling_upgrade_full_lifecycle() {
        let mut rc = RollingUpgradeCoordinator::new("3.0.0", 1);
        rc.add_node("a", "2.0.0");
        rc.add_node("b", "2.0.0");
        // Upgrade node a fully
        for _ in 0..4 {
            rc.advance("a");
        }
        let (done, total) = rc.progress();
        assert_eq!(done, 1);
        assert_eq!(total, 2);
        assert!(!rc.all_complete());
    }

    #[test]
    fn test_rolling_upgrade_failure() {
        let mut rc = RollingUpgradeCoordinator::new("3.0.0", 2);
        rc.add_node("a", "2.0.0");
        rc.mark_failed("a");
        assert!(rc.any_failed());
        assert!(!rc.all_complete());
    }

    #[test]
    fn test_health_probe_stale() {
        let mut hp = HealthProbe::new(HealthThresholds::default());
        hp.record("storage", 50, true, "ok", 10);
        assert_eq!(hp.get_status("storage"), HealthStatus::Healthy);
        let stale = hp.stale_probes(100); // age=90 > max_age_s=30
        assert!(stale.contains(&"storage"));
    }

    #[test]
    fn test_health_probe_unknown_component() {
        let hp = HealthProbe::new(HealthThresholds::default());
        assert_eq!(hp.get_status("nonexistent"), HealthStatus::Unknown);
    }

    // ── Observability & Ops ───────────────────────────────────────────

    use crate::vm::observability_ops::*;

    #[test]
    fn test_slow_query_threshold_filter() {
        let mut sq = SlowQueryCollector::new(1000, 50);
        sq.record("SELECT 1", 500, 1, 1, 1); // below threshold
        sq.record("SELECT 1", 999, 2, 1, 1); // below
        assert_eq!(sq.count(), 0);
        sq.record("SELECT * FROM t", 1000, 3, 100, 10);
        assert_eq!(sq.count(), 1);
    }

    #[test]
    fn test_slow_query_eviction() {
        let mut sq = SlowQueryCollector::new(100, 2);
        sq.record("q1", 200, 1, 10, 1);
        sq.record("q2", 300, 2, 10, 1);
        // Full -> evicts fastest (200) if new is slower
        assert!(sq.record("q3", 500, 3, 10, 1));
        assert_eq!(sq.count(), 2);
        let top = sq.top_n(2);
        assert_eq!(top[0].duration_us, 500);
        assert_eq!(top[1].duration_us, 300);
    }

    #[test]
    fn test_resource_watermark_critical() {
        let mut rw = ResourceWatermark::new();
        rw.register("disk", 70.0, 90.0);
        rw.update("disk", 95.0, 1);
        assert_eq!(rw.get_level("disk"), AlertLevel::Critical);
        assert_eq!(rw.alert_history_count(), 1);
    }

    #[test]
    fn test_conn_pool_monitor_remove() {
        let mut cpm = ConnPoolMonitor::new(50);
        cpm.add_connection(1, "admin");
        cpm.add_connection(2, "user");
        assert_eq!(cpm.total_count(), 2);
        cpm.remove_connection(1);
        assert_eq!(cpm.total_count(), 1);
    }

    #[test]
    fn test_lock_wait_graph_no_cycle() {
        let mut lwg = LockWaitGraph::new();
        lwg.add_wait(1, 2, "t1", 100);
        lwg.add_wait(2, 3, "t2", 101);
        // No cycle: 1->2->3
        let cycles = lwg.detect_cycles();
        assert!(cycles.is_empty());
    }

    #[test]
    fn test_lock_wait_graph_remove_edge() {
        let mut lwg = LockWaitGraph::new();
        lwg.add_wait(1, 2, "t1", 100);
        lwg.add_wait(2, 1, "t2", 101);
        assert_eq!(lwg.edge_count(), 2);
        lwg.remove_wait(1, 2);
        assert_eq!(lwg.edge_count(), 1);
    }

    #[test]
    fn test_hot_config_unknown_key() {
        let mut hc = HotConfigReload::new();
        assert!(hc.update("nonexistent", "val").is_err());
    }

    #[test]
    fn test_hot_config_version_tracking() {
        let mut hc = HotConfigReload::new();
        hc.register("a", "1", true);
        hc.register("b", "2", true);
        hc.update("a", "10").unwrap();
        hc.update("b", "20").unwrap();
        assert_eq!(hc.current_version(), 2);
        assert_eq!(hc.change_count(), 2);
        assert_eq!(hc.get("a"), Some("10"));
        assert_eq!(hc.get("b"), Some("20"));
    }
}
