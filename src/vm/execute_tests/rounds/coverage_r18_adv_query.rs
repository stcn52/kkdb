// R18 coverage tests — advanced query processing

#[cfg(test)]
mod tests {
    use crate::vm::adv_query::*;

    // ── Auto-Refresh ──────────────────────────────────────────────────

    #[test]
    fn test_auto_refresh_manual_never_auto() {
        let mut arm = AutoRefreshManager::new();
        arm.register_view("mv1", vec!["t1".to_string()], RefreshStrategy::Manual, 0);
        arm.notify_table_change("t1");
        let needing = arm.views_needing_refresh(9999);
        assert!(needing.is_empty()); // manual never auto-triggers
    }

    #[test]
    fn test_auto_refresh_multi_source() {
        let mut arm = AutoRefreshManager::new();
        arm.register_view(
            "mv_joined",
            vec!["orders".to_string(), "customers".to_string()],
            RefreshStrategy::OnChange,
            0,
        );
        arm.mark_refreshed("mv_joined", 10);
        arm.notify_table_change("customers");
        assert_eq!(arm.stale_count(), 1);
    }

    #[test]
    fn test_auto_refresh_view_count() {
        let mut arm = AutoRefreshManager::new();
        arm.register_view("a", vec![], RefreshStrategy::Periodic, 30);
        arm.register_view("b", vec![], RefreshStrategy::OnChange, 0);
        arm.register_view("c", vec![], RefreshStrategy::Manual, 0);
        assert_eq!(arm.view_count(), 3);
    }

    // ── Query Router ──────────────────────────────────────────────────

    #[test]
    fn test_query_router_wildcard() {
        let mut qr = QueryRouter::new(RouteTarget::Local);
        qr.add_rule("log_*", RouteTarget::Shard(2), 5);
        assert_eq!(qr.route(&["log_events"]), RouteTarget::Shard(2));
        assert_eq!(qr.route(&["users"]), RouteTarget::Local); // no match
    }

    #[test]
    fn test_query_router_priority() {
        let mut qr = QueryRouter::new(RouteTarget::Local);
        qr.add_rule("orders", RouteTarget::Shard(1), 5);
        qr.add_rule("orders", RouteTarget::Shard(2), 10); // higher priority
        assert_eq!(qr.route(&["orders"]), RouteTarget::Shard(2));
    }

    #[test]
    fn test_query_router_broadcast() {
        let mut qr = QueryRouter::new(RouteTarget::Local);
        qr.add_rule("*", RouteTarget::Broadcast, 1);
        assert_eq!(qr.route(&["anything"]), RouteTarget::Broadcast);
    }

    // ── Streaming Result ──────────────────────────────────────────────

    #[test]
    fn test_streaming_result_complete() {
        let mut sr = StreamingResult::new(10);
        sr.produce(vec![vec!["row1".to_string()]], false);
        sr.produce(vec![vec!["row2".to_string()]], true);
        assert_eq!(sr.total_rows_sent(), 2);
        assert!(!sr.is_complete()); // still buffered
        sr.consume();
        sr.consume();
        assert!(sr.is_complete());
    }

    #[test]
    fn test_streaming_result_empty_buffer() {
        let mut sr = StreamingResult::new(5);
        assert!(sr.consume().is_none());
        assert_eq!(sr.buffered_chunks(), 0);
    }

    // ── Dynamic Partition Pruning ─────────────────────────────────────

    #[test]
    fn test_dynamic_pruning_no_match_all_pruned() {
        let parts = vec![
            RuntimePartition {
                partition_id: 0,
                lower_bound: Some(0),
                upper_bound: Some(49),
                row_count: 50,
            },
            RuntimePartition {
                partition_id: 1,
                lower_bound: Some(50),
                upper_bound: Some(99),
                row_count: 50,
            },
        ];
        let mut dpp = DynamicPartitionPruner::new(parts);
        dpp.prune_with_values(&[200, 300]); // no partition matches
        assert_eq!(dpp.pruned_count(), 2);
        assert!(dpp.surviving_partitions().is_empty());
        assert_eq!(dpp.estimated_rows(), 0);
    }

    #[test]
    fn test_dynamic_pruning_unbounded() {
        let parts = vec![
            RuntimePartition {
                partition_id: 0,
                lower_bound: None,
                upper_bound: Some(99),
                row_count: 100,
            },
            RuntimePartition {
                partition_id: 1,
                lower_bound: Some(100),
                upper_bound: None,
                row_count: 100,
            },
        ];
        let mut dpp = DynamicPartitionPruner::new(parts);
        dpp.prune_with_values(&[50]);
        assert_eq!(dpp.surviving_partitions(), vec![0]); // only p0 matches
    }

    // ── Temp Table ────────────────────────────────────────────────────

    #[test]
    fn test_temp_table_drop() {
        let mut ttm = TempTableManager::new(10, 10_000);
        ttm.create("tmp1", TempScope::Session, 1, 1).unwrap();
        assert!(ttm.drop_table("tmp1"));
        assert!(!ttm.drop_table("nonexistent"));
        assert_eq!(ttm.table_count(), 0);
    }

    #[test]
    fn test_temp_table_size_tracking() {
        let mut ttm = TempTableManager::new(10, 1000);
        ttm.create("t1", TempScope::Session, 1, 1).unwrap();
        ttm.update_stats("t1", 100, 800);
        assert_eq!(ttm.total_bytes(), 800);
        assert!(!ttm.is_over_limit());
        ttm.update_stats("t1", 200, 1500);
        assert!(ttm.is_over_limit());
    }

    #[test]
    fn test_temp_table_multi_session() {
        let mut ttm = TempTableManager::new(5, 100_000);
        ttm.create("s1_t1", TempScope::Session, 1, 1).unwrap();
        ttm.create("s2_t1", TempScope::Session, 2, 1).unwrap();
        ttm.create("s2_t2", TempScope::Transaction, 2, 1).unwrap();
        assert_eq!(ttm.table_count(), 3);
        let cleaned = ttm.cleanup_session(2);
        assert_eq!(cleaned, 2); // both session 2 tables
        assert_eq!(ttm.table_count(), 1);
    }
}
