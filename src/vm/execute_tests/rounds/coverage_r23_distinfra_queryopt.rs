//! R23 integration tests — 分布式基础设施 + 查询优化器马力升级

use crate::raft::features::dist_infra::*;
use crate::vm::optimizer::query_opt_v2::*;

// ─── NodeDiscovery ───────────────────────────────────────────────────

#[test]
fn test_r23_node_discovery_lifecycle() {
    let mut nd = NodeDiscovery::new(3000);
    nd.tick(0);
    nd.register("n1", "10.0.0.1", 8080, NodeRole::Leader);
    nd.register("n2", "10.0.0.2", 8080, NodeRole::Follower);
    nd.register("n3", "10.0.0.3", 8080, NodeRole::Observer);
    assert_eq!(nd.node_count(), 3);

    nd.tick(2000);
    nd.heartbeat("n1");
    assert_eq!(nd.node_status("n1"), Some(NodeStatus::Healthy));
    assert_eq!(nd.node_status("n2"), Some(NodeStatus::Suspect));

    nd.tick(5000);
    assert_eq!(nd.node_status("n2"), Some(NodeStatus::Dead));
    assert_eq!(nd.healthy_nodes().len(), 1); // only n1 had heartbeat at 2000

    nd.set_metadata("n1", "region", "us-east");
    assert!(nd.deregister("n3"));
    assert_eq!(nd.node_count(), 2);
}

#[test]
fn test_r23_node_discovery_roles() {
    let mut nd = NodeDiscovery::new(5000);
    nd.tick(0);
    nd.register("a", "10.0.0.1", 8080, NodeRole::Follower);
    nd.register("b", "10.0.0.2", 8080, NodeRole::Follower);
    nd.register("c", "10.0.0.3", 8080, NodeRole::Leader);
    assert_eq!(nd.nodes_by_role(&NodeRole::Follower).len(), 2);
    assert_eq!(nd.nodes_by_role(&NodeRole::Leader).len(), 1);
    assert_eq!(nd.nodes_by_role(&NodeRole::Candidate).len(), 0);
}

// ─── ConfigCenter ────────────────────────────────────────────────────

#[test]
fn test_r23_config_center_crud() {
    let mut cc = ConfigCenter::new();
    cc.set_time(1000);
    let v1 = cc.put("db", "pool_size", "10", ConfigSource::Default);
    let v2 = cc.put("db", "timeout", "30s", ConfigSource::Local);
    assert_eq!(v2, v1 + 1);

    let entry = cc.get("db", "pool_size").unwrap();
    assert_eq!(entry.value, "10");

    assert!(cc.delete("db", "timeout"));
    assert_eq!(cc.entry_count(), 1);
}

#[test]
fn test_r23_config_center_watch_and_version() {
    let mut cc = ConfigCenter::new();
    cc.put("app", "feature_flag", "true", ConfigSource::Remote);
    let snap = cc.version();

    cc.watch("app", "feature_flag", "consumer-1");
    assert_eq!(cc.get_watchers("app", "feature_flag").len(), 1);

    cc.put("app", "new_key", "val", ConfigSource::Override);
    let changes = cc.changes_since(snap);
    assert_eq!(changes.len(), 1);
}

// ─── ServiceMesh ─────────────────────────────────────────────────────

#[test]
fn test_r23_service_mesh_round_robin() {
    let mut mesh = ServiceMesh::new();
    mesh.register_service("api", "n1", "10.0.0.1", 9000, 10);
    mesh.register_service("api", "n2", "10.0.0.2", 9000, 10);
    mesh.register_service("api", "n3", "10.0.0.3", 9000, 10);
    mesh.set_routing("api", RoutingStrategy::RoundRobin);

    let ids: Vec<String> = (0..6)
        .map(|_| mesh.resolve("api").unwrap().node_id.clone())
        .collect();
    assert_eq!(ids, vec!["n1", "n2", "n3", "n1", "n2", "n3"]);
}

#[test]
fn test_r23_service_mesh_health_tracking() {
    let mut mesh = ServiceMesh::new();
    mesh.register_service("db", "n1", "10.0.0.1", 5432, 10);
    mesh.register_service("db", "n2", "10.0.0.2", 5432, 10);
    assert_eq!(mesh.healthy_count("db"), 2);

    mesh.mark_unhealthy("db", "n1");
    assert_eq!(mesh.healthy_count("db"), 1);
    assert_eq!(mesh.resolve("db").unwrap().node_id, "n2");

    mesh.mark_healthy("db", "n1");
    assert_eq!(mesh.healthy_count("db"), 2);
}

// ─── LinkEncryption ──────────────────────────────────────────────────

#[test]
fn test_r23_link_encryption_lifecycle() {
    let mut le = LinkEncryption::new(EncryptionAlgo::Aes256Gcm, 10000);
    le.set_time(0);
    le.create_session_key("peer-a");
    le.create_session_key("peer-b");
    assert_eq!(le.active_key_count(), 2);

    le.set_time(11000);
    assert!(!le.is_key_valid("peer-a"));
    assert_eq!(le.expired_keys().len(), 2);

    le.rotate_key("peer-a");
    assert!(le.is_key_valid("peer-a"));
}

#[test]
fn test_r23_link_encryption_roundtrip() {
    let mut le = LinkEncryption::new(EncryptionAlgo::ChaCha20Poly1305, 60000);
    le.set_time(100);
    le.create_session_key("node-1");

    let msg = b"KKDB cluster sync data";
    let ct = le.encrypt("node-1", msg).unwrap();
    let pt = le.decrypt("node-1", &ct).unwrap();
    assert_eq!(pt, msg);
    assert_ne!(ct.as_slice(), msg.as_slice());
}

#[test]
fn test_r23_link_encryption_certs() {
    let mut le = LinkEncryption::new(EncryptionAlgo::Aes128Gcm, 5000);
    le.set_time(5000);
    le.register_cert(
        "n1",
        CertInfo {
            subject: "n1.kkdb.local".into(),
            issuer: "ca.kkdb.local".into(),
            serial: "001".into(),
            not_before_ms: 1000,
            not_after_ms: 50000,
            fingerprint: "sha256:abc".into(),
        },
    );
    assert!(le.is_cert_valid("n1"));
    let cert = le.get_cert("n1").unwrap();
    assert_eq!(cert.subject, "n1.kkdb.local");

    le.set_time(60000);
    assert!(!le.is_cert_valid("n1"));
}

// ─── GlobalIndexOptimizer ────────────────────────────────────────────

#[test]
fn test_r23_global_index_optimizer() {
    let mut opt = GlobalIndexOptimizer::new();
    opt.register_index(IndexDescriptor {
        index_name: "idx_u_email".into(),
        table_name: "users".into(),
        columns: vec!["email".into()],
        is_unique: true,
        is_covering: false,
        selectivity: 0.01,
    });
    opt.register_index(IndexDescriptor {
        index_name: "idx_u_name_age".into(),
        table_name: "users".into(),
        columns: vec!["name".into(), "age".into()],
        is_unique: false,
        is_covering: true,
        selectivity: 0.3,
    });
    assert_eq!(opt.total_indexes(), 2);

    let best = opt.best_index("users", &["email"]).unwrap();
    assert_eq!(best.index_name, "idx_u_email");

    let covering = opt.find_covering_indexes("users", &["name", "age"]);
    assert_eq!(covering.len(), 1);
    assert_eq!(covering[0].index_name, "idx_u_name_age");
}

// ─── QueryRewriter ───────────────────────────────────────────────────

#[test]
fn test_r23_query_rewriter() {
    let mut rw = QueryRewriter::new();
    rw.add_rule(RewriteRule {
        name: "const_fold".into(),
        pattern: RewritePattern::ConstantFolding,
        priority: 100,
        enabled: true,
    });
    rw.add_rule(RewriteRule {
        name: "pred_push".into(),
        pattern: RewritePattern::PredicatePushdown,
        priority: 80,
        enabled: true,
    });

    let results = rw.apply_rules(&[
        RewritePattern::ConstantFolding,
        RewritePattern::SubqueryToJoin,
    ]);
    assert_eq!(results.len(), 1);
    assert!(results[0].applied);
    assert_eq!(rw.rule_applications("const_fold"), 1);
    assert_eq!(rw.rule_applications("pred_push"), 0);
}

// ─── AutoIndexAdvisor ────────────────────────────────────────────────

#[test]
fn test_r23_auto_index_advisor() {
    let mut advisor = AutoIndexAdvisor::new(5);
    advisor.record_access(ColumnAccess {
        table: "orders".into(),
        column: "customer_id".into(),
        access_type: AccessType::EqualityFilter,
        frequency: 500,
    });
    advisor.record_access(ColumnAccess {
        table: "orders".into(),
        column: "created_at".into(),
        access_type: AccessType::OrderBy,
        frequency: 200,
    });
    assert_eq!(advisor.total_accesses(), 2);

    let recs = advisor.recommend();
    assert!(!recs.is_empty());
    // customer_id should rank higher: 500 * 1.0 = 500 vs created_at: 200 * 0.5 = 100
    assert_eq!(recs[0].table, "orders");
    assert_eq!(recs[0].columns[0], "customer_id");
}

#[test]
fn test_r23_auto_index_advisor_skip_existing() {
    let mut advisor = AutoIndexAdvisor::new(3);
    advisor.register_existing_index("users", vec!["id".into()]);
    advisor.record_access(ColumnAccess {
        table: "users".into(),
        column: "id".into(),
        access_type: AccessType::EqualityFilter,
        frequency: 9999,
    });
    let recs = advisor.recommend();
    assert!(recs.is_empty());
}

// ─── StatsEnhancer ───────────────────────────────────────────────────

#[test]
fn test_r23_stats_enhancer_selectivity() {
    let mut se = StatsEnhancer::new(0.1, 60000);
    let mut cols = std::collections::HashMap::new();
    cols.insert(
        "color".to_string(),
        ColumnStats {
            column_name: "color".into(),
            null_count: 2,
            distinct_count: 10,
            min_value: Some("blue".into()),
            max_value: Some("yellow".into()),
            avg_length: 5.0,
            histogram: vec![],
        },
    );
    se.update_table_stats(TableStats {
        table_name: "items".into(),
        row_count: 1000,
        page_count: 50,
        avg_row_size: 80.0,
        last_analyzed_ms: 10000,
        columns: cols,
    });

    let sel = se.estimate_selectivity("items", "color");
    assert!((sel - 0.1).abs() < 0.001); // 1/10

    let rows = se.estimate_filtered_rows("items", "color");
    assert_eq!(rows, 100); // 1000 * 0.1
}

#[test]
fn test_r23_stats_enhancer_staleness() {
    let mut se = StatsEnhancer::new(0.5, 10000);
    se.update_table_stats(TableStats {
        table_name: "logs".into(),
        row_count: 50000,
        page_count: 2000,
        avg_row_size: 256.0,
        last_analyzed_ms: 5000,
        columns: std::collections::HashMap::new(),
    });

    assert!(!se.is_stale("logs", 10000)); // 5000ms < 10000 threshold
    assert!(se.is_stale("logs", 20000)); // 15000ms > 10000 threshold
    assert!(se.is_stale("no_table", 0)); // nonexistent = stale

    let stale = se.stale_tables(20000);
    assert_eq!(stale.len(), 1);
}
