// R13 integration tests: vectorized execution, HA failover, MVCC GC, diagnostics.

use std::collections::HashSet;
use std::time::Duration;

use crate::raft::ha::{FailoverManager, LeaderElection, NodeState, ReadReplicaRouter};
use crate::vm::diagnostics::{
    DiagnosticBuilder, DiagnosticContext, DiagnosticSeverity, ExplainAnalyze, ExplainNode,
    SysCatalogColumn, SysCatalogIndex, SysCatalogTable, SystemCatalog,
};
use crate::vm::gc::{
    CascadeAction, ForeignKeyCascade, ForeignKeyDef, IsolationLevel, IsolationVerifier,
    MvccGarbageCollector, VersionedRow,
};
use crate::vm::vectorized::{
    AggType, BinOpKind, ColumnBatch, ExprPattern, FilterOp, Pipeline, PipelineResult,
    PipelineStage, VectorOp,
};

// ── Vectorized Execution ──────────────────────────────────────────────

#[test]
fn r13_column_batch_from_rows_and_back() {
    let batch = ColumnBatch::from_rows(vec!["a".into(), "b".into()], &[vec![1, 10], vec![2, 20]]);
    assert_eq!(batch.row_count, 2);
    assert_eq!(batch.num_columns(), 2);
    let back = batch.to_rows();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0][0], 1);
    assert_eq!(back[1][1], 20);
}

#[test]
fn r13_vector_filter_and_project() {
    let batch = ColumnBatch::from_rows(
        vec!["id".into(), "val".into()],
        &[vec![1, 10], vec![2, 20], vec![3, 30]],
    );
    // Filter: id > 1
    let filtered = VectorOp::filter(&batch, 0, |v| v > 1);
    assert_eq!(filtered.row_count, 2);
    // Project column 1
    let projected = VectorOp::project(&filtered, &[1]);
    assert_eq!(projected.num_columns(), 1);
    assert_eq!(projected.row_count, 2);
}

#[test]
fn r13_vector_aggregates() {
    let batch = ColumnBatch::from_rows(vec!["x".into()], &[vec![10], vec![20], vec![30]]);
    assert_eq!(VectorOp::sum(&batch, 0), Some(60));
    assert_eq!(VectorOp::count(&batch), 3);
    assert_eq!(VectorOp::min(&batch, 0), Some(10));
    assert_eq!(VectorOp::max(&batch, 0), Some(30));
}

#[test]
fn r13_pipeline_filter_project() {
    let source = ColumnBatch::from_rows(
        vec!["a".into(), "b".into()],
        &[vec![1, 100], vec![2, 200], vec![3, 300]],
    );
    let mut pipe = Pipeline::new();
    pipe.add_stage(PipelineStage::Filter {
        col_idx: 0,
        op: FilterOp::Ge,
        value: 2,
    });
    pipe.add_stage(PipelineStage::Project {
        col_indices: vec![1],
    });
    match pipe.execute(source) {
        PipelineResult::Batch(b) => {
            assert_eq!(b.row_count, 2);
            assert_eq!(b.num_columns(), 1);
        }
        _ => panic!("expected Batch"),
    }
}

#[test]
fn r13_pipeline_aggregate() {
    let source = ColumnBatch::from_rows(vec!["v".into()], &[vec![5], vec![15]]);
    let mut pipe = Pipeline::new();
    pipe.add_stage(PipelineStage::Aggregate {
        agg_type: AggType::Sum,
        col_idx: 0,
    });
    match pipe.execute(source) {
        PipelineResult::Scalar(v) => assert_eq!(v, 20),
        _ => panic!("expected Scalar"),
    }
}

#[test]
fn r13_expr_pattern_eval_and_fold() {
    let add = ExprPattern::BinOp {
        op: BinOpKind::Add,
        left: Box::new(ExprPattern::Const(3)),
        right: Box::new(ExprPattern::Const(7)),
    };
    assert_eq!(add.eval(&[]), 10);

    let folded = add.constant_fold();
    assert!(folded.is_constant());
    assert!(matches!(folded, ExprPattern::Const(10)));
}

#[test]
fn r13_expr_col_ref() {
    let expr = ExprPattern::ColRef(1);
    let row = vec![42, 99];
    assert_eq!(expr.eval(&row), 99);
}

// ── HA Failover ───────────────────────────────────────────────────────

#[test]
fn r13_leader_election_full_cycle() {
    let mut le = LeaderElection::new(1, 5, Duration::from_millis(100));
    assert_eq!(le.state(), NodeState::Follower);

    le.start_pre_vote();
    assert_eq!(le.state(), NodeState::PreVote);

    le.start_election();
    assert_eq!(le.state(), NodeState::Candidate);
    assert_eq!(le.current_term(), 1);

    // Receive 2 more votes (self already voted, need 3 total for 5-node cluster)
    assert!(!le.receive_vote(2)); // 2 votes: not yet majority
    assert!(le.receive_vote(3)); // 3 votes: majority!
    assert!(le.is_leader());
    assert_eq!(le.leader_id(), Some(1));
}

#[test]
fn r13_leader_election_heartbeat_resets() {
    let mut le = LeaderElection::new(2, 3, Duration::from_millis(100));
    le.start_election();
    assert_eq!(le.current_term(), 1);

    // Receive heartbeat from leader with higher term
    le.receive_heartbeat(1, 5);
    assert_eq!(le.state(), NodeState::Follower);
    assert_eq!(le.current_term(), 5);
}

#[test]
fn r13_failover_manager_leader_failure() {
    let mut fm = FailoverManager::new();
    fm.add_node(1, 2);
    fm.add_node(2, 2);
    fm.add_node(3, 2);
    fm.set_leader(1);

    // Leader health is fine
    fm.health_check(1, true);
    assert_eq!(fm.check_failover(), None);

    // Leader fails 2 times -> offline
    fm.health_check(1, false);
    fm.health_check(1, false);
    let new = fm.check_failover().unwrap();
    assert_eq!(new, 2); // lowest alive node
    assert_eq!(fm.failover_count(), 1);
}

#[test]
fn r13_failover_remove_node() {
    let mut fm = FailoverManager::new();
    fm.add_node(1, 3);
    fm.add_node(2, 3);
    assert_eq!(fm.node_count(), 2);
    fm.set_leader(1);
    fm.remove_node(1);
    assert_eq!(fm.node_count(), 1);
    assert_eq!(fm.current_leader(), None); // leader removed
}

#[test]
fn r13_read_replica_routing() {
    let mut rr = ReadReplicaRouter::new();
    rr.set_leader(1);
    rr.add_replica(1, 5);
    rr.add_replica(2, 10);
    rr.add_replica(3, 20);

    // Reads should not go to leader
    let target = rr.route_read().unwrap();
    assert_ne!(target, 1);

    // Writes always go to leader
    assert_eq!(rr.route_write(), Some(1));

    // With load awareness
    rr.update_load(2, 50);
    rr.update_load(3, 0);
    let target = rr.route_read().unwrap();
    assert_eq!(target, 3); // less loaded
}

// ── MVCC GC ───────────────────────────────────────────────────────────

#[test]
fn r13_gc_purge_old_versions() {
    let mut gc = MvccGarbageCollector::new(0);
    // row 1 has 3 versions
    gc.add_version(VersionedRow::new(1, 1, vec![0; 50]));
    gc.add_version(VersionedRow::new(1, 5, vec![0; 50]));
    gc.add_version(VersionedRow::new(1, 10, vec![0; 50]));
    assert_eq!(gc.version_count(), 3);

    gc.advance_watermark(7);
    let purged = gc.purge();
    assert_eq!(purged, 1); // txn 1 purged, txn 5 kept as latest ≤ watermark
    assert_eq!(gc.version_count(), 2);

    gc.advance_watermark(100);
    let purged = gc.purge();
    assert_eq!(purged, 1); // txn 5 purged
    assert_eq!(gc.version_count(), 1);
}

#[test]
fn r13_gc_tombstone_cleanup() {
    let mut gc = MvccGarbageCollector::new(10);
    gc.add_version(VersionedRow::new_deleted(1, 5));
    gc.add_version(VersionedRow::new(2, 8, vec![1, 2]));
    let purged = gc.purge_tombstones();
    assert_eq!(purged, 1); // tombstone for row 1 removed
    assert_eq!(gc.row_count(), 1);
}

#[test]
fn r13_isolation_verifier_write_conflict() {
    let mut v = IsolationVerifier::new(IsolationLevel::Serializable);
    v.record_write("users", 1);
    v.record_write("users", 2);

    let mut other = HashSet::new();
    other.insert(("users".to_string(), 2u64));
    assert!(v.has_write_conflict(&other));
}

#[test]
fn r13_isolation_verifier_phantom_read() {
    let mut rc = IsolationVerifier::new(IsolationLevel::ReadCommitted);
    rc.record_range_predicate("t1", "age > 20");
    assert!(rc.can_have_phantom_read());

    let mut ser = IsolationVerifier::new(IsolationLevel::Serializable);
    ser.record_range_predicate("t1", "age > 20");
    assert!(!ser.can_have_phantom_read()); // serializable prevents phantoms
}

#[test]
fn r13_fk_cascade_delete() {
    let mut fkc = ForeignKeyCascade::new();
    fkc.add_fk(ForeignKeyDef {
        name: "fk_order_user".into(),
        child_table: "orders".into(),
        child_columns: vec!["user_id".into()],
        parent_table: "users".into(),
        parent_columns: vec!["id".into()],
        on_delete: CascadeAction::Cascade,
        on_update: CascadeAction::SetNull,
    });
    let ops = fkc.on_delete("users", &["42".into()]);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].action, CascadeAction::Cascade);

    let ops = fkc.on_update("users", &["42".into()]);
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].action, CascadeAction::SetNull);
}

#[test]
fn r13_fk_restrict_prevents_delete() {
    let mut fkc = ForeignKeyCascade::new();
    fkc.add_fk(ForeignKeyDef {
        name: "fk_restrict".into(),
        child_table: "items".into(),
        child_columns: vec!["order_id".into()],
        parent_table: "orders".into(),
        parent_columns: vec!["id".into()],
        on_delete: CascadeAction::Restrict,
        on_update: CascadeAction::NoAction,
    });
    assert!(fkc.would_restrict_delete("orders"));
    assert!(!fkc.would_restrict_delete("users"));
}

// ── Diagnostics ───────────────────────────────────────────────────────

#[test]
fn r13_explain_node_tree() {
    let mut root = ExplainNode::new("HashJoin")
        .with_estimates(500, 100.0)
        .with_actuals(490, 3000);
    root.set_extra("join_type", "INNER");

    let child1 = ExplainNode::new("SeqScan")
        .with_table("orders")
        .with_estimates(1000, 50.0)
        .with_actuals(1000, 2000);
    let child2 = ExplainNode::new("IndexScan")
        .with_table("users")
        .with_estimates(100, 10.0)
        .with_actuals(100, 500);
    root.add_child(child1);
    root.add_child(child2);

    assert_eq!(root.total_time_us(), 5500);
    let bn = root.bottleneck();
    assert_eq!(bn.op, "HashJoin"); // root has 3000µs, children have less
}

#[test]
fn r13_explain_analyze_format() {
    let root = ExplainNode::new("SeqScan")
        .with_table("t1")
        .with_estimates(100, 10.0)
        .with_actuals(95, 1500);
    let ea = ExplainAnalyze::new(
        root,
        Duration::from_micros(200),
        Duration::from_micros(1500),
    );
    let text = ea.format();
    assert!(text.contains("SeqScan on t1"));
    assert!(text.contains("Planning time"));
    assert!(text.contains("Execution time"));
    assert_eq!(ea.total_time(), Duration::from_micros(1700));
}

#[test]
fn r13_system_catalog_operations() {
    let mut cat = SystemCatalog::new();
    cat.register_table(SysCatalogTable {
        name: "users".into(),
        row_count_estimate: 5000,
        columns: vec![
            SysCatalogColumn {
                name: "id".into(),
                data_type: "INTEGER".into(),
                nullable: false,
                default_value: None,
                ordinal_position: 0,
            },
            SysCatalogColumn {
                name: "email".into(),
                data_type: "TEXT".into(),
                nullable: true,
                default_value: None,
                ordinal_position: 1,
            },
        ],
        indexes: vec![SysCatalogIndex {
            name: "pk_users".into(),
            table_name: "users".into(),
            columns: vec!["id".into()],
            is_unique: true,
            is_primary: true,
        }],
        created_at: Some("2024-01-01".into()),
    });

    assert_eq!(cat.table_count(), 1);
    assert_eq!(cat.table_names(), vec!["users".to_string()]);

    let t = cat.get_table("users").unwrap();
    assert_eq!(t.columns.len(), 2);
    assert_eq!(t.indexes.len(), 1);

    let idx = cat.indexes_for_table("users");
    assert_eq!(idx.len(), 1);
    assert!(idx[0].is_primary);

    let cols = cat.find_columns("email");
    assert_eq!(cols.len(), 1);

    assert!(cat.unregister_table("users"));
    assert_eq!(cat.table_count(), 0);
}

#[test]
fn r13_diagnostic_table_not_found() {
    let ctx = DiagnosticBuilder::table_not_found("missing_tbl");
    assert_eq!(ctx.severity, DiagnosticSeverity::Error);
    assert_eq!(ctx.error_code, "42P01");
    let formatted = ctx.format();
    assert!(formatted.contains("missing_tbl"));
    assert!(formatted.contains("Suggestion"));
}

#[test]
fn r13_diagnostic_syntax_error() {
    let ctx = DiagnosticBuilder::syntax_error("unexpected token", "SELECT * FORM t1", 9);
    let formatted = ctx.format();
    assert!(formatted.contains("42601"));
    assert!(formatted.contains("^"));
    assert!(formatted.contains("FORM"));
}

#[test]
fn r13_diagnostic_column_not_found() {
    let ctx = DiagnosticBuilder::column_not_found("naem", "users");
    assert!(ctx.message.contains("naem"));
    assert!(ctx.hint.is_some());
}

#[test]
fn r13_diagnostic_type_mismatch() {
    let ctx = DiagnosticBuilder::type_mismatch("INTEGER", "TEXT");
    assert!(ctx.message.contains("INTEGER"));
    assert!(ctx.hint.as_ref().unwrap().contains("CAST"));
}

#[test]
fn r13_diagnostic_context_builder_chain() {
    let mut ctx = DiagnosticContext::error("E001", "test error")
        .with_detail("detail info")
        .with_hint("try this")
        .with_sql("SELECT 1")
        .with_position(7);
    ctx.add_suggestion("fix it", Some("SELECT 2"));

    let formatted = ctx.format();
    assert!(formatted.contains("Detail: detail info"));
    assert!(formatted.contains("Hint: try this"));
    assert!(formatted.contains("Fix: SELECT 2"));
}
