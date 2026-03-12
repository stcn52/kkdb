// R15 integration tests for:
//   - storage::advanced (WriteAmpTracker, LayeredBloomFilter, PartitionPruner, PageVerificationChain)
//   - vm::exec_engine (StreamingWindow, SortSpillManager, SemiAntiJoinOptimizer, AdaptiveParallelism)
//   - raft::cluster_mgmt (LogCompactor, MembershipChange, ReplicationLagMonitor, TopologyDiscovery)
//   - vm::dev_tools (SqlLintChecker, PlanVisualizer, IndexAdvisor, SchemaMigrationManager)

// ── storage::advanced ─────────────────────────────────────────────────

use crate::storage::advanced::*;

#[test]
fn test_write_amp_tracker_waf() {
    let mut wat = WriteAmpTracker::new(4);
    wat.record_logical(500);
    wat.record_physical(500, Some(0));
    wat.record_physical(500, Some(1));
    wat.record_physical(500, Some(2));
    assert!((wat.waf() - 3.0).abs() < 0.01);
    assert_eq!(wat.logical_bytes(), 500);
    assert_eq!(wat.physical_bytes(), 1500);
}

#[test]
fn test_write_amp_tracker_reset_and_no_logical() {
    let mut wat = WriteAmpTracker::new(2);
    assert!((wat.waf() - 1.0).abs() < 0.01); // 0/0 => 1.0
    wat.record_logical(100);
    wat.record_physical(300, Some(0));
    wat.reset();
    assert_eq!(wat.logical_bytes(), 0);
    assert_eq!(wat.level_breakdown()[0], 0);
}

#[test]
fn test_layered_bloom_filter_lookup() {
    let mut lbf = LayeredBloomFilter::new();
    lbf.add_layer(100, 3);
    lbf.add_layer(200, 5);
    lbf.insert(0, b"apple");
    lbf.insert(1, b"banana");
    assert_eq!(lbf.lookup(b"apple"), Some(0));
    assert_eq!(lbf.lookup(b"banana"), Some(1));
    assert!(!lbf.may_contain(b"cherry"));
    assert_eq!(lbf.layer_count(), 2);
    assert_eq!(lbf.total_items(), 2);
}

#[test]
fn test_bloom_layer_false_positive_rate() {
    let mut bl = BloomLayer::new(500, 5);
    for i in 0..100 {
        bl.insert(format!("item{}", i).as_bytes());
    }
    let fpr = bl.false_positive_rate();
    assert!(fpr < 0.05);
    assert_eq!(bl.item_count(), 100);
}

#[test]
fn test_partition_pruner_eq_range_in() {
    let pruner = PartitionPruner::new(vec![
        PartitionDef::new(0, "q1", Some(0), Some(100)),
        PartitionDef::new(1, "q2", Some(100), Some(200)),
        PartitionDef::new(2, "q3", Some(200), Some(300)),
        PartitionDef::new(3, "q4", Some(300), None),
    ]);
    assert_eq!(pruner.partition_count(), 4);
    assert_eq!(pruner.prune_eq(150), vec![1]);
    assert_eq!(pruner.prune_eq(500), vec![3]);
    let range = pruner.prune_range(50, 250);
    assert!(range.contains(&0));
    assert!(range.contains(&1));
    assert!(range.contains(&2));
    let in_list = pruner.prune_in(&[99, 300]);
    assert!(in_list.contains(&0));
    assert!(in_list.contains(&3));
}

#[test]
fn test_partition_unbounded() {
    let p = PartitionDef::new(0, "all", None, None);
    assert!(p.contains(0));
    assert!(p.contains(i64::MAX));
    assert!(p.overlaps_range(-100, 100));
}

#[test]
fn test_page_verification_chain_append_verify() {
    let mut chain = PageVerificationChain::new();
    chain.append(1, b"data1");
    chain.append(2, b"data2");
    chain.append(3, b"data3");
    assert_eq!(chain.page_count(), 3);
    assert!(chain.verify(1, b"data1"));
    assert!(chain.verify(2, b"data2"));
    assert!(chain.verify(3, b"data3"));
    assert!(!chain.verify(1, b"corrupt"));
    assert!(!chain.verify(99, b"missing"));
}

#[test]
fn test_page_verification_chain_full_verify() {
    let mut chain = PageVerificationChain::new();
    chain.append(1, b"aaa");
    chain.append(2, b"bbb");
    assert!(chain.verify_chain(&[(1, b"aaa" as &[u8]), (2, b"bbb" as &[u8])]));
    assert!(!chain.verify_chain(&[(1, b"xxx" as &[u8]), (2, b"bbb" as &[u8])]));
    assert_ne!(chain.head_checksum(), 0);
}

// ── vm::exec_engine ───────────────────────────────────────────────────

use crate::vm::exec_engine::*;

#[test]
fn test_streaming_window_row_number() {
    let def = WindowDef::new("row_number");
    let mut sw = StreamingWindow::new(def);
    assert_eq!(sw.process_row(&[1, 2, 3]), 1);
    assert_eq!(sw.process_row(&[4, 5, 6]), 2);
    assert_eq!(sw.total_rows(), 2);
    assert_eq!(sw.func_name(), "row_number");
}

#[test]
fn test_streaming_window_sum_partitioned() {
    let def = WindowDef::new("sum")
        .with_partition(vec![0])
        .with_order(1);
    let mut sw = StreamingWindow::new(def);
    assert_eq!(sw.process_row(&[1, 10]), 10);
    assert_eq!(sw.process_row(&[1, 20]), 30);
    assert_eq!(sw.process_row(&[2, 5]), 5);
    sw.reset();
    assert_eq!(sw.total_rows(), 0);
}

#[test]
fn test_streaming_window_count_avg() {
    let def_count = WindowDef::new("count").with_order(1);
    let mut sw = StreamingWindow::new(def_count);
    assert_eq!(sw.process_row(&[10]), 1);
    assert_eq!(sw.process_row(&[20]), 2);

    let def_avg = WindowDef::new("avg").with_order(0);
    let mut sw_avg = StreamingWindow::new(def_avg);
    assert_eq!(sw_avg.process_row(&[10]), 10);
    assert_eq!(sw_avg.process_row(&[30]), 20); // avg(10,30) = 20
}

#[test]
fn test_sort_spill_manager_policies() {
    // ThresholdBased
    let mut mgr = SortSpillManager::new(SpillPolicy::ThresholdBased, 1000);
    assert!(!mgr.add_run(50, 400));
    assert!(!mgr.add_run(50, 500));
    assert!(mgr.add_run(50, 200)); // 1100 > 1000
    assert_eq!(mgr.spill_count(), 1);
    assert_eq!(mgr.run_count(), 3);
    assert_eq!(mgr.policy(), SpillPolicy::ThresholdBased);

    // Never
    let mut mgr2 = SortSpillManager::new(SpillPolicy::Never, 10);
    assert!(!mgr2.add_run(999, 999));
    assert_eq!(mgr2.spill_count(), 0);
}

#[test]
fn test_sort_spill_merge_and_memory() {
    let mut mgr = SortSpillManager::new(SpillPolicy::ThresholdBased, 5000);
    mgr.add_run(100, 500);
    mgr.add_run(200, 500);
    assert_eq!(mgr.merge_runs(), 300); // 100 + 200
    assert_eq!(mgr.memory_used(), 1000);
}

#[test]
fn test_semi_anti_join_optimizer() {
    let mut opt = SemiAntiJoinOptimizer::new();
    let c = opt.rewrite_exists_to_semi("orders", "customers", "cust_id", 1000);
    assert_eq!(c.rewritten_kind, JoinKind::Semi);
    let c2 = opt.rewrite_not_exists_to_anti("orders", "returns", "order_id");
    assert_eq!(c2.rewritten_kind, JoinKind::Anti);
    assert_eq!(opt.rewrite_count(), 2);
}

#[test]
fn test_semi_anti_join_execution() {
    let left = vec![1, 2, 3, 4, 5, 6];
    let right = vec![2, 4, 6, 8];
    let semi = SemiAntiJoinOptimizer::execute_semi(&left, &right);
    assert_eq!(semi, vec![1, 3, 5]); // indices of 2, 4, 6
    let anti = SemiAntiJoinOptimizer::execute_anti(&left, &right);
    assert_eq!(anti, vec![0, 2, 4]); // indices of 1, 3, 5
}

#[test]
fn test_adaptive_parallelism_adjust() {
    let mut ap = AdaptiveParallelism::new(1, 16);
    assert_eq!(ap.current_degree(), 1);

    // Low CPU → scale up
    ap.report_stats(ParallelismStats {
        cpu_utilization: 0.2,
        io_wait_ratio: 0.1,
        queue_depth: 1,
        active_queries: 1,
    });
    let d = ap.adjust();
    assert!(d >= 2);

    // High CPU → scale down
    let mut ap2 = AdaptiveParallelism::new(1, 16);
    ap2.set_degree(10);
    for _ in 0..5 {
        ap2.report_stats(ParallelismStats {
            cpu_utilization: 0.95,
            io_wait_ratio: 0.2,
            queue_depth: 20,
            active_queries: 10,
        });
    }
    let d2 = ap2.adjust();
    assert!(d2 < 10);
    assert!(ap2.history_len() > 0);
}

// ── raft::cluster_mgmt ───────────────────────────────────────────────

use crate::raft::cluster_mgmt::*;

#[test]
fn test_log_compactor_trigger() {
    let mut lc = LogCompactor::new(100);
    lc.append_entries(80);
    assert!(!lc.needs_compaction());
    lc.append_entries(30);
    assert!(lc.needs_compaction()); // 110 > 100
    assert_eq!(lc.log_size(), 110);
}

#[test]
fn test_log_compactor_snapshot() {
    let mut lc = LogCompactor::new(50);
    lc.append_entries(70);
    let snap = lc.compact(60, 3, 2048);
    assert_eq!(snap.last_included_index, 60);
    assert_eq!(snap.last_included_term, 3);
    assert_eq!(lc.last_compacted_index(), 60);
    assert!(!lc.needs_compaction()); // 70 - 60 = 10 < 50
    assert_eq!(lc.snapshot_count(), 1);
}

#[test]
fn test_membership_change_add_commit() {
    let mut mc = MembershipChange::new(vec![1, 2, 3].into_iter().collect());
    assert_eq!(mc.quorum_size(), 2);
    assert!(mc.propose_add(4));
    assert!(!mc.propose_add(1)); // already member
    mc.enter_joint();
    assert_eq!(mc.state(), &MembershipState::Joint);
    mc.commit_change();
    assert_eq!(mc.state(), &MembershipState::Stable);
    assert_eq!(mc.member_count(), 4);
    assert!(mc.members().contains(&4));
}

#[test]
fn test_membership_change_remove() {
    let mut mc = MembershipChange::new(vec![1, 2, 3].into_iter().collect());
    assert!(mc.propose_remove(3));
    assert!(!mc.propose_remove(99)); // not a member
    mc.enter_joint();
    mc.commit_change();
    assert_eq!(mc.member_count(), 2);
    assert!(!mc.members().contains(&3));
}

#[test]
fn test_replication_lag_monitor_alerts() {
    let mut mon = ReplicationLagMonitor::new(100, 500);
    mon.update_lag(1, "us-east", 50, 1000, 1);
    mon.update_lag(2, "eu-west", 200, 990, 1);
    mon.update_lag(3, "ap-south", 700, 900, 1);

    let alerts = mon.check_alerts();
    assert!(alerts.iter().any(|a| matches!(a, LagAlert::Warning(2, _))));
    assert!(alerts.iter().any(|a| matches!(a, LagAlert::Critical(3, _))));
    assert_eq!(mon.replica_count(), 3);
    assert_eq!(mon.max_lag(), 700);

    mon.record_history(1);
    let avg = mon.avg_lag();
    assert!(avg > 0.0);
}

#[test]
fn test_topology_discovery() {
    let mut topo = TopologyDiscovery::new();
    topo.add_node(TopoNode {
        node_id: 1,
        address: "10.0.0.1:8000".to_string(),
        datacenter: "us-east".to_string(),
        role: NodeRole::Leader,
        partitions: vec![0, 1, 2],
    });
    topo.add_node(TopoNode {
        node_id: 2,
        address: "10.0.0.2:8000".to_string(),
        datacenter: "us-east".to_string(),
        role: NodeRole::Follower,
        partitions: vec![0, 1],
    });
    topo.add_node(TopoNode {
        node_id: 3,
        address: "10.0.1.1:8000".to_string(),
        datacenter: "eu-west".to_string(),
        role: NodeRole::Follower,
        partitions: vec![2],
    });
    assert_eq!(topo.node_count(), 3);
    assert_eq!(topo.partition_count(), 3);
    assert_eq!(topo.leader_for_partition(0), Some(1));
    assert_eq!(topo.nodes_in_dc("us-east").len(), 2);
    assert!(topo.version() > 1);
}

#[test]
fn test_topology_remove_node() {
    let mut topo = TopologyDiscovery::new();
    topo.add_node(TopoNode {
        node_id: 1,
        address: "a".to_string(),
        datacenter: "dc1".to_string(),
        role: NodeRole::Leader,
        partitions: vec![0],
    });
    assert!(topo.remove_node(1));
    assert!(!topo.remove_node(1)); // already removed
    assert_eq!(topo.node_count(), 0);
}

// ── vm::dev_tools ─────────────────────────────────────────────────────

use crate::vm::dev_tools::*;

#[test]
fn test_sql_lint_checker_select_star() {
    let checker = SqlLintChecker::new();
    let issues = checker.check("SELECT * FROM orders WHERE id > 10");
    assert!(issues.iter().any(|i| i.rule == "no-select-star"));
}

#[test]
fn test_sql_lint_missing_where() {
    let checker = SqlLintChecker::new();
    let issues = checker.check("UPDATE users SET active = 0");
    assert!(issues.iter().any(|i| i.rule == "missing-where"));
    assert!(issues.iter().any(|i| i.severity == LintSeverity::Error));
}

#[test]
fn test_sql_lint_clean_query() {
    let checker = SqlLintChecker::new();
    let issues = checker.check("SELECT name, email FROM users WHERE id = 42");
    assert!(issues.is_empty());
}

#[test]
fn test_sql_lint_disable_enable() {
    let mut checker = SqlLintChecker::new();
    let count_before = checker.enabled_rule_count();
    checker.disable_rule(&LintRule::NoSelectStar);
    assert_eq!(checker.enabled_rule_count(), count_before - 1);
    let issues = checker.check("SELECT * FROM t");
    assert!(!issues.iter().any(|i| i.rule == "no-select-star"));
    checker.enable_rule(LintRule::NoSelectStar);
    assert_eq!(checker.enabled_rule_count(), count_before);
}

#[test]
fn test_plan_visualizer_render() {
    let mut root = PlanNode::new("Hash Join", 50.0, 500);
    let left = PlanNode::new("Seq Scan", 20.0, 200).with_table("orders");
    let right = PlanNode::new("Index Scan", 10.0, 50)
        .with_table("customers")
        .with_extra("Using idx_cust_id");
    root.add_child(left);
    root.add_child(right);

    let text = PlanVisualizer::render_string(&root);
    assert!(text.contains("Hash Join"));
    assert!(text.contains("Seq Scan"));
    assert!(text.contains("customers"));
    assert!((root.total_cost() - 80.0).abs() < 0.01);
    assert_eq!(root.total_rows(), 750);
}

#[test]
fn test_index_advisor_recommend() {
    let mut advisor = IndexAdvisor::new();
    for _ in 0..5 {
        advisor.observe("orders", &["customer_id"], 0.03);
    }
    advisor.observe("orders", &["status"], 0.8);

    let recs = advisor.recommend();
    assert!(!recs.is_empty());
    assert_eq!(recs[0].table, "orders");
    assert!(recs[0].estimated_speedup > 5.0);
    assert_eq!(advisor.pattern_count(), 2);
}

#[test]
fn test_index_advisor_covered_index() {
    let mut advisor = IndexAdvisor::new();
    advisor.add_existing_index("users", &["email"]);
    for _ in 0..10 {
        advisor.observe("users", &["email"], 0.01);
    }
    let recs = advisor.recommend();
    assert!(recs.is_empty()); // covered by existing index
}

#[test]
fn test_schema_migration_lifecycle() {
    let mut mgr = SchemaMigrationManager::new();
    mgr.add_migration(1, "create_users", "CREATE TABLE users (id INT)", "DROP TABLE users");
    mgr.add_migration(2, "add_email", "ALTER TABLE users ADD email TEXT", "ALTER TABLE users DROP email");
    mgr.add_migration(3, "add_index", "CREATE INDEX idx ON users(email)", "DROP INDEX idx");

    assert_eq!(mgr.migration_count(), 3);
    assert_eq!(mgr.pending().len(), 3);
    assert_eq!(mgr.current_version(), 0);

    let sql1 = mgr.apply(1, 1000).unwrap().to_string();
    assert!(sql1.contains("CREATE TABLE"));
    assert_eq!(mgr.current_version(), 1);

    let sql2 = mgr.apply(2, 2000).unwrap().to_string();
    assert!(sql2.contains("ADD email"));
    assert_eq!(mgr.current_version(), 2);
    assert_eq!(mgr.applied_history().len(), 2);

    // Rollback v2
    let down = mgr.rollback(2).unwrap().to_string();
    assert!(down.contains("DROP email"));
    assert_eq!(mgr.current_version(), 1);
}

#[test]
fn test_schema_migration_rollback_to_zero() {
    let mut mgr = SchemaMigrationManager::new();
    mgr.add_migration(1, "v1", "UP", "DOWN");
    mgr.apply(1, 100);
    mgr.rollback(1);
    assert_eq!(mgr.current_version(), 0);
    assert!(mgr.applied_history().is_empty());
}
