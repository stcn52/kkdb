// Round 9 coverage tests: MVCC isolation levels, BM25 config, stop words,
// Raft compaction stats, and membership helpers.

use crate::vm::execute::{VM, ExecResult};
use crate::types::Value;

// ─── MVCC Isolation Level Tests ──────────────────────────────────────────────

#[test]
fn test_isolation_level_enum_display() {
    use crate::vm::mvcc::IsolationLevel;
    assert_eq!(IsolationLevel::Serializable.to_string(), "SERIALIZABLE");
    assert_eq!(IsolationLevel::RepeatableRead.to_string(), "REPEATABLE READ");
    assert_eq!(IsolationLevel::ReadCommitted.to_string(), "READ COMMITTED");
    assert_eq!(IsolationLevel::ReadUncommitted.to_string(), "READ UNCOMMITTED");
}

#[test]
fn test_isolation_level_from_str_loose() {
    use crate::vm::mvcc::IsolationLevel;
    assert_eq!(IsolationLevel::from_str_loose("serializable"), Some(IsolationLevel::Serializable));
    assert_eq!(IsolationLevel::from_str_loose("SERIALIZABLE"), Some(IsolationLevel::Serializable));
    assert_eq!(IsolationLevel::from_str_loose("snapshot"), Some(IsolationLevel::Serializable));
    assert_eq!(IsolationLevel::from_str_loose("repeatable read"), Some(IsolationLevel::RepeatableRead));
    assert_eq!(IsolationLevel::from_str_loose("REPEATABLE-READ"), Some(IsolationLevel::RepeatableRead));
    assert_eq!(IsolationLevel::from_str_loose("repeatable_read"), Some(IsolationLevel::RepeatableRead));
    assert_eq!(IsolationLevel::from_str_loose("read committed"), Some(IsolationLevel::ReadCommitted));
    assert_eq!(IsolationLevel::from_str_loose("READ-COMMITTED"), Some(IsolationLevel::ReadCommitted));
    assert_eq!(IsolationLevel::from_str_loose("read uncommitted"), Some(IsolationLevel::ReadUncommitted));
    assert_eq!(IsolationLevel::from_str_loose("READ_UNCOMMITTED"), Some(IsolationLevel::ReadUncommitted));
    assert_eq!(IsolationLevel::from_str_loose("nonexistent"), None);
    assert_eq!(IsolationLevel::from_str_loose(""), None);
}

#[test]
fn test_isolation_level_properties() {
    use crate::vm::mvcc::IsolationLevel;
    // Serializable
    assert!(IsolationLevel::Serializable.uses_begin_snapshot());
    assert!(IsolationLevel::Serializable.requires_read_set_validation());
    assert!(!IsolationLevel::Serializable.allows_dirty_reads());

    // RepeatableRead
    assert!(IsolationLevel::RepeatableRead.uses_begin_snapshot());
    assert!(!IsolationLevel::RepeatableRead.requires_read_set_validation());
    assert!(!IsolationLevel::RepeatableRead.allows_dirty_reads());

    // ReadCommitted
    assert!(!IsolationLevel::ReadCommitted.uses_begin_snapshot());
    assert!(!IsolationLevel::ReadCommitted.requires_read_set_validation());
    assert!(!IsolationLevel::ReadCommitted.allows_dirty_reads());

    // ReadUncommitted
    assert!(!IsolationLevel::ReadUncommitted.uses_begin_snapshot());
    assert!(!IsolationLevel::ReadUncommitted.requires_read_set_validation());
    assert!(IsolationLevel::ReadUncommitted.allows_dirty_reads());
}

#[test]
fn test_isolation_level_default() {
    use crate::vm::mvcc::IsolationLevel;
    assert_eq!(IsolationLevel::default(), IsolationLevel::Serializable);
}

#[test]
fn test_set_isolation_level_all_four() {
    let mut vm = VM::new_memory();
    // Serializable (default)
    let r = vm.execute_sql("SET isolation_level = 'serializable'").unwrap();
    assert!(format!("{:?}", r).contains("SERIALIZABLE"));

    // RepeatableRead
    let r = vm.execute_sql("SET isolation_level = 'repeatable read'").unwrap();
    assert!(format!("{:?}", r).contains("REPEATABLE READ"));

    // ReadCommitted
    let r = vm.execute_sql("SET isolation_level = 'read committed'").unwrap();
    assert!(format!("{:?}", r).contains("READ COMMITTED"));

    // ReadUncommitted
    let r = vm.execute_sql("SET isolation_level = 'read uncommitted'").unwrap();
    assert!(format!("{:?}", r).contains("READ UNCOMMITTED"));
}

#[test]
fn test_set_isolation_level_invalid() {
    let mut vm = VM::new_memory();
    let r = vm.execute_sql("SET isolation_level = 'fantasy_level'");
    assert!(r.is_err());
    let msg = format!("{:?}", r.unwrap_err());
    assert!(msg.contains("unknown isolation level"));
}

#[test]
fn test_set_isolation_hyphen_underscore_variants() {
    let mut vm = VM::new_memory();
    // Hyphen
    let r = vm.execute_sql("SET isolation_level = 'repeatable-read'").unwrap();
    assert!(format!("{:?}", r).contains("REPEATABLE READ"));

    // Underscore
    let r = vm.execute_sql("SET isolation_level = 'read_committed'").unwrap();
    assert!(format!("{:?}", r).contains("READ COMMITTED"));
}

#[test]
fn test_mvcc_snapshot_read_uncommitted() {
    use crate::vm::mvcc::TransactionRegistry;
    let mut reg = TransactionRegistry::new();
    let t1 = reg.begin();
    let t2 = reg.begin();

    // Normal snapshot: t2 is invisible to t1's snapshot (both active)
    let normal_snap = reg.snapshot(t1);
    assert!(!normal_snap.is_visible(t2)); // t2 still active

    // ReadUncommitted: everything visible
    let dirty_snap = reg.snapshot_read_uncommitted(t1);
    assert!(dirty_snap.is_visible(t2)); // dirty read: t2 visible even though uncommitted
    assert!(dirty_snap.is_visible(999)); // future txn also visible
    assert!(dirty_snap.is_visible(0)); // past txn visible
}

#[test]
fn test_mvcc_snapshot_for_isolation() {
    use crate::vm::mvcc::{TransactionRegistry, IsolationLevel};
    let mut reg = TransactionRegistry::new();
    let t1 = reg.begin();

    // Serializable → normal snapshot
    let snap = reg.snapshot_for_isolation(t1, IsolationLevel::Serializable);
    assert!(!snap.is_visible(999)); // future not visible

    // ReadUncommitted → dirty snapshot
    let snap = reg.snapshot_for_isolation(t1, IsolationLevel::ReadUncommitted);
    assert!(snap.is_visible(999)); // everything visible
}

#[test]
fn test_transaction_registry_auto_purge() {
    use crate::vm::mvcc::{TransactionRegistry, UndoLog, UndoEntry};
    let mut reg = TransactionRegistry::new();
    let t1 = reg.begin();
    let t2 = reg.begin();

    let mut undo = UndoLog::new();
    undo.push(UndoEntry::Insert { table: "t".into(), rowid: 1, txn_id: t1 });
    undo.push(UndoEntry::Insert { table: "t".into(), rowid: 2, txn_id: t2 });
    assert_eq!(undo.len(), 2);

    // With both active, no purge possible
    let purged = reg.auto_purge(&mut undo);
    assert_eq!(purged, 0);
    assert_eq!(undo.len(), 2);

    // Commit t1, now t2 is min_active
    reg.commit(t1);
    let purged = reg.auto_purge(&mut undo);
    assert_eq!(purged, 1); // t1's entry purged
    assert_eq!(undo.len(), 1);

    // Commit t2
    reg.commit(t2);
    let purged = reg.auto_purge(&mut undo);
    assert_eq!(purged, 1); // t2's entry purged
    assert_eq!(undo.len(), 0);
}

#[test]
fn test_transaction_registry_max_committed() {
    use crate::vm::mvcc::TransactionRegistry;
    let mut reg = TransactionRegistry::new();
    assert_eq!(reg.max_committed(), 0);

    let t1 = reg.begin();
    let t2 = reg.begin();
    reg.commit(t2);
    assert_eq!(reg.max_committed(), t2);
    reg.commit(t1);
    // t1 < t2, but commit is called with t1 which is less than max_committed
    // The max_committed should still be t2 since t2 > t1
    assert_eq!(reg.max_committed(), t2);
}

#[test]
fn test_read_uncommitted_sql_integration() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ru_test (id INTEGER, name TEXT)").unwrap();
    vm.execute_sql("INSERT INTO ru_test VALUES (1, 'alice')").unwrap();

    // Set ReadUncommitted
    vm.execute_sql("SET isolation_level = 'read uncommitted'").unwrap();

    // Begin transaction
    vm.execute_sql("BEGIN").unwrap();

    // Can still read data
    let rows = match vm.execute_sql("SELECT * FROM ru_test").unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    };
    assert_eq!(rows.len(), 1);

    vm.execute_sql("COMMIT").unwrap();
}

#[test]
fn test_repeatable_read_sql_integration() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rr_test (id INTEGER, val TEXT)").unwrap();
    vm.execute_sql("INSERT INTO rr_test VALUES (1, 'initial')").unwrap();

    // Set RepeatableRead
    vm.execute_sql("SET isolation_level = 'repeatable read'").unwrap();

    // Begin transaction
    vm.execute_sql("BEGIN").unwrap();

    // Read within transaction
    let rows = match vm.execute_sql("SELECT * FROM rr_test").unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    };
    assert_eq!(rows.len(), 1);

    vm.execute_sql("COMMIT").unwrap();
}

// ─── BM25 Config Tests ──────────────────────────────────────────────────────

#[test]
fn test_bm25_config_default() {
    use crate::fulltext::index::Bm25Config;
    let cfg = Bm25Config::default();
    assert!((cfg.k1 - 1.2).abs() < 1e-10);
    assert!((cfg.b - 0.75).abs() < 1e-10);
}

#[test]
fn test_bm25_config_new_clamping() {
    use crate::fulltext::index::Bm25Config;
    // Normal range
    let cfg = Bm25Config::new(1.5, 0.5);
    assert!((cfg.k1 - 1.5).abs() < 1e-10);
    assert!((cfg.b - 0.5).abs() < 1e-10);

    // k1 too high → clamped to 10.0
    let cfg = Bm25Config::new(100.0, 0.5);
    assert!((cfg.k1 - 10.0).abs() < 1e-10);

    // k1 negative → clamped to 0.0
    let cfg = Bm25Config::new(-5.0, 0.5);
    assert!((cfg.k1 - 0.0).abs() < 1e-10);

    // b too high → clamped to 1.0
    let cfg = Bm25Config::new(1.2, 5.0);
    assert!((cfg.b - 1.0).abs() < 1e-10);

    // b negative → clamped to 0.0
    let cfg = Bm25Config::new(1.2, -1.0);
    assert!((cfg.b - 0.0).abs() < 1e-10);
}

#[test]
fn test_bm25_config_from_option_str() {
    use crate::fulltext::index::Bm25Config;
    let cfg = Bm25Config::from_option_str("k1=2.0, b=0.5");
    assert!((cfg.k1 - 2.0).abs() < 1e-10);
    assert!((cfg.b - 0.5).abs() < 1e-10);

    // Only k1
    let cfg = Bm25Config::from_option_str("k1=0.8");
    assert!((cfg.k1 - 0.8).abs() < 1e-10);
    assert!((cfg.b - 0.75).abs() < 1e-10); // default

    // Only b
    let cfg = Bm25Config::from_option_str("b=0.3");
    assert!((cfg.k1 - 1.2).abs() < 1e-10); // default
    assert!((cfg.b - 0.3).abs() < 1e-10);

    // Invalid values → defaults
    let cfg = Bm25Config::from_option_str("k1=abc, b=xyz");
    assert!((cfg.k1 - 1.2).abs() < 1e-10);
    assert!((cfg.b - 0.75).abs() < 1e-10);

    // Empty string
    let cfg = Bm25Config::from_option_str("");
    assert!((cfg.k1 - 1.2).abs() < 1e-10);
    assert!((cfg.b - 0.75).abs() < 1e-10);
}

#[test]
fn test_bm25_score_with_config() {
    use crate::fulltext::index::{bm25_score, bm25_score_with_config, Bm25Config};

    // Default config should match bm25_score
    let default_cfg = Bm25Config::default();
    let score_default = bm25_score(2, 10, 5, 100, 10.0);
    let score_config = bm25_score_with_config(2, 10, 5, 100, 10.0, &default_cfg);
    assert!((score_default - score_config).abs() < 1e-10);

    // Higher k1 → TF has more influence → higher score for high TF
    let high_k1 = Bm25Config::new(3.0, 0.75);
    let score_high_k1 = bm25_score_with_config(5, 10, 5, 100, 10.0, &high_k1);
    let score_normal = bm25_score_with_config(5, 10, 5, 100, 10.0, &default_cfg);
    // Both should be positive
    assert!(score_high_k1 > 0.0);
    assert!(score_normal > 0.0);

    // b=0 → no length normalization → short and long docs score same (for same TF)
    let no_norm = Bm25Config::new(1.2, 0.0);
    let short = bm25_score_with_config(3, 5, 10, 100, 10.0, &no_norm);
    let long = bm25_score_with_config(3, 50, 10, 100, 10.0, &no_norm);
    assert!((short - long).abs() < 1e-10, "b=0 should eliminate length effect");

    // Edge cases
    assert_eq!(bm25_score_with_config(1, 10, 0, 100, 10.0, &default_cfg), 0.0);
    assert_eq!(bm25_score_with_config(1, 10, 5, 0, 10.0, &default_cfg), 0.0);
}

// ─── Stop Word Tests ─────────────────────────────────────────────────────────

#[test]
fn test_english_stop_words() {
    use crate::fulltext::tokenizer::is_stopword;
    assert!(is_stopword("the"));
    assert!(is_stopword("is"));
    assert!(is_stopword("and"));
    assert!(is_stopword("of"));
    assert!(is_stopword("to"));
    assert!(!is_stopword("database"));
    assert!(!is_stopword("rust"));
    assert!(!is_stopword("query"));
}

#[test]
fn test_chinese_stop_words() {
    use crate::fulltext::tokenizer::is_stopword;
    assert!(is_stopword("的"));
    assert!(is_stopword("了"));
    assert!(is_stopword("是"));
    assert!(is_stopword("在"));
    assert!(!is_stopword("数据库"));
    assert!(!is_stopword("引擎"));
}

#[test]
fn test_simple_tokenize_filtered() {
    use crate::fulltext::tokenizer::simple_tokenize_filtered;
    let tokens = simple_tokenize_filtered("the cat is on the mat");
    // "the", "is", "on" should be removed
    assert!(!tokens.contains(&"the".to_string()));
    assert!(!tokens.contains(&"is".to_string()));
    assert!(!tokens.contains(&"on".to_string()));
    assert!(tokens.contains(&"cat".to_string()));
    assert!(tokens.contains(&"mat".to_string()));
}

#[test]
fn test_simple_tokenize_filtered_preserves_tf() {
    use crate::fulltext::tokenizer::simple_tokenize_filtered;
    // Non-stop words should still have their TF preserved
    let tokens = simple_tokenize_filtered("cat cat dog");
    assert_eq!(tokens.iter().filter(|t| t.as_str() == "cat").count(), 2);
    assert_eq!(tokens.iter().filter(|t| t.as_str() == "dog").count(), 1);
}

#[test]
fn test_query_tokenize_filtered() {
    use crate::fulltext::tokenizer::query_tokenize_filtered;
    let tokens = query_tokenize_filtered("the cat is the cat");
    // Stop words removed + deduplicated
    assert!(!tokens.contains(&"the".to_string()));
    assert!(!tokens.contains(&"is".to_string()));
    assert_eq!(tokens.iter().filter(|t| t.as_str() == "cat").count(), 1);
}

#[test]
fn test_simple_tokenize_filtered_empty() {
    use crate::fulltext::tokenizer::simple_tokenize_filtered;
    assert!(simple_tokenize_filtered("").is_empty());
    // All stop words → empty
    assert!(simple_tokenize_filtered("the is and of to").is_empty());
}

#[test]
fn test_chinese_tokenize_filtered() {
    use crate::fulltext::tokenizer::simple_tokenize_filtered;
    // Chinese text with stop words
    let tokens = simple_tokenize_filtered("数据库的引擎是很好的");
    // "的", "是", "很" should be removed
    assert!(!tokens.contains(&"的".to_string()));
    assert!(!tokens.contains(&"是".to_string()));
    // "数据库" or "引擎" should remain
    assert!(
        tokens.iter().any(|t| t.contains("数据") || t.contains("引擎")),
        "Expected content tokens in {:?}",
        tokens
    );
}

// ─── Raft Compaction Stats Tests ─────────────────────────────────────────────

#[test]
fn test_raft_log_store_default_has_threshold() {
    use crate::raft::log_store::KkdbLogStore;
    let store = KkdbLogStore::default();
    // In-memory default: compact_threshold = 0 (derive Default).
    // File-backed stores get COMPACT_THRESHOLD (1000).
    assert_eq!(store.compact_threshold(), 0);

    // Set it manually
    store.set_compact_threshold(1000);
    assert_eq!(store.compact_threshold(), 1000);
}

#[test]
fn test_raft_log_store_set_compact_threshold() {
    use crate::raft::log_store::KkdbLogStore;
    let store = KkdbLogStore::default();
    store.set_compact_threshold(500);
    assert_eq!(store.compact_threshold(), 500);
    store.set_compact_threshold(0);
    assert_eq!(store.compact_threshold(), 0);
}

#[test]
fn test_raft_log_store_detailed_compaction_stats() {
    use crate::raft::log_store::KkdbLogStore;
    let store = KkdbLogStore::default();
    let stats = store.detailed_compaction_stats();
    assert_eq!(stats.live_records, 0);
    assert_eq!(stats.total_records, 0);
    assert_eq!(stats.dead_records, 0);
    assert_eq!(stats.compact_threshold, 0); // derive Default → 0
    assert_eq!(stats.compaction_count, 0);
    assert_eq!(stats.total_dead_eliminated, 0);
}

#[test]
fn test_raft_log_store_compaction_stats_after_operations() {
    use crate::raft::log_store::KkdbLogStore;
    use openraft::{Entry, LogId};

    let store = KkdbLogStore::default();
    // Append some entries
    let entries: Vec<Entry<crate::raft::types::KkdbTypeConfig>> = (1..=5).map(|i| {
        Entry {
            log_id: LogId { leader_id: openraft::CommittedLeaderId::new(1, 0), index: i },
            payload: openraft::EntryPayload::Blank,
        }
    }).collect();
    store.append_direct(entries).unwrap();

    let (live, total, dead) = store.compaction_stats();
    assert_eq!(live, 5);
    // In-memory mode: total_records only incremented with WAL writes, not in-memory
    // So total may equal 5 or 0 depending on wal_file presence
    let _ = (total, dead); // just check it doesn't panic

    assert_eq!(store.entry_count(), 5);
    assert_eq!(store.last_index(), Some(5));
}

#[test]
fn test_raft_compaction_with_file() {
    use crate::raft::log_store::KkdbLogStore;
    use openraft::{Entry, LogId};

    let tmpdir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(tmpdir.path()).unwrap();

    // Set low threshold for testing
    store.set_compact_threshold(2);

    // Append 5 entries
    let entries: Vec<Entry<crate::raft::types::KkdbTypeConfig>> = (1..=5).map(|i| {
        Entry {
            log_id: LogId { leader_id: openraft::CommittedLeaderId::new(1, 0), index: i },
            payload: openraft::EntryPayload::Blank,
        }
    }).collect();
    store.append_direct(entries).unwrap();

    // Check stats
    let stats = store.detailed_compaction_stats();
    assert_eq!(stats.live_records, 5);
    assert_eq!(stats.total_records, 5);
    assert_eq!(stats.dead_records, 0);

    // Purge first 3 entries
    store.purge_direct(LogId { leader_id: openraft::CommittedLeaderId::new(1, 0), index: 3 }).unwrap();
    assert_eq!(store.entry_count(), 2); // entries 4,5 remain

    // The dead records should be tracked
    let stats = store.detailed_compaction_stats();
    assert_eq!(stats.live_records, 2);
    // After purge + possible auto-compact, compaction may have run
    // Just verify the stats structure works
    assert!(stats.compact_threshold == 2);
}

// ─── MVCC Compute Visibility Delta with ReadUncommitted ──────────────────────

#[test]
fn test_compute_visibility_delta_read_uncommitted() {
    use crate::vm::mvcc::*;
    use crate::types::Value;

    let mut undo = UndoLog::new();
    let mut reg = TransactionRegistry::new();
    let t1 = reg.begin();
    let t2 = reg.begin();

    // t2 inserts a row (uncommitted)
    undo.push(UndoEntry::Insert { table: "tab".into(), rowid: 10, txn_id: t2 });

    // Normal snapshot for t1: t2's insert is invisible
    let snap_normal = reg.snapshot(t1);
    let (invisible, restored) = compute_visibility_delta(&undo, &snap_normal, "tab");
    assert!(invisible.contains(&10), "t2's insert should be invisible in normal snapshot");
    assert!(restored.is_empty());

    // ReadUncommitted snapshot for t1: t2's insert is visible (no filtering)
    let snap_dirty = reg.snapshot_read_uncommitted(t1);
    let (invisible2, restored2) = compute_visibility_delta(&undo, &snap_dirty, "tab");
    assert!(invisible2.is_empty(), "ReadUncommitted should see everything");
    assert!(restored2.is_empty());
}

#[test]
fn test_compute_visibility_delta_repeatable_read() {
    use crate::vm::mvcc::*;
    use crate::types::Value;

    let mut undo = UndoLog::new();
    let mut reg = TransactionRegistry::new();
    let t1 = reg.begin();

    // Commit a row before t2 starts
    reg.commit(t1);

    let t2 = reg.begin();

    // t1 updated a row (committed)
    undo.push(UndoEntry::Update {
        table: "tab".into(),
        rowid: 5,
        old_row: vec![Value::Integer(99)],
        txn_id: t1,
    });

    // For RepeatableRead: use normal snapshot (same as Serializable for visibility)
    let snap = reg.snapshot_for_isolation(t2, IsolationLevel::RepeatableRead);
    let (invisible, restored) = compute_visibility_delta(&undo, &snap, "tab");

    // t1 is committed and visible in t2's snapshot → no filtering needed
    assert!(invisible.is_empty());
    assert!(restored.is_empty());
}

// ─── RowLockManager GC Tests ─────────────────────────────────────────────────

#[test]
fn test_row_lock_manager_gc_versions() {
    use crate::vm::mvcc::RowLockManager;

    let mut mgr = RowLockManager::new();

    // Simulate: txn 1 locks and commits row (tab, 1)
    mgr.try_lock_row("tab", 1, 1).unwrap();
    mgr.commit_version(1);
    mgr.release_all(1);

    // txn 5 locks and commits row (tab, 2)
    mgr.try_lock_row("tab", 2, 5).unwrap();
    mgr.commit_version(5);
    mgr.release_all(5);

    assert_eq!(mgr.committed_versions.len(), 2);

    // GC versions < 3: removes txn 1's version
    mgr.gc_versions(3);
    assert_eq!(mgr.committed_versions.len(), 1);

    // GC versions < 10: removes all
    mgr.gc_versions(10);
    assert!(mgr.committed_versions.is_empty());
}

// ─── UndoLog Iter Tests ─────────────────────────────────────────────────────

#[test]
fn test_undo_log_iter_rev() {
    use crate::vm::mvcc::{UndoLog, UndoEntry};

    let mut log = UndoLog::new();
    log.push(UndoEntry::Insert { table: "t".into(), rowid: 1, txn_id: 1 });
    log.push(UndoEntry::Insert { table: "t".into(), rowid: 2, txn_id: 2 });
    log.push(UndoEntry::Insert { table: "t".into(), rowid: 3, txn_id: 3 });

    let ids: Vec<u64> = log.iter_rev().map(|e| e.txn_id()).collect();
    assert_eq!(ids, vec![3, 2, 1]);
}

#[test]
fn test_undo_log_entry_table_names() {
    use crate::vm::mvcc::UndoEntry;
    use crate::types::Value;

    let insert = UndoEntry::Insert { table: "users".into(), rowid: 1, txn_id: 1 };
    assert_eq!(insert.table_name(), Some("users"));

    let update = UndoEntry::Update {
        table: "orders".into(), rowid: 2,
        old_row: vec![Value::Integer(1)], txn_id: 2
    };
    assert_eq!(update.table_name(), Some("orders"));

    let delete = UndoEntry::Delete {
        table: "items".into(), rowid: 3,
        old_row: vec![], txn_id: 3
    };
    assert_eq!(delete.table_name(), Some("items"));

    let sp = UndoEntry::Savepoint { name: "sp".into(), txn_id: 4 };
    assert_eq!(sp.table_name(), None);
}

// ─── Raft KkdbNode Members Helper ────────────────────────────────────────────

// Cannot test add_learner/promote_to_voter without a running Raft cluster,
// but we can test the non-async helpers.

#[test]
fn test_raft_log_store_open_and_recover() {
    use crate::raft::log_store::KkdbLogStore;
    use openraft::{Entry, LogId};

    let tmpdir = tempfile::tempdir().unwrap();

    // Write some entries
    {
        let store = KkdbLogStore::open(tmpdir.path()).unwrap();
        let entries: Vec<Entry<crate::raft::types::KkdbTypeConfig>> = (1..=3).map(|i| {
            Entry {
                log_id: LogId { leader_id: openraft::CommittedLeaderId::new(1, 0), index: i },
                payload: openraft::EntryPayload::Blank,
            }
        }).collect();
        store.append_direct(entries).unwrap();
        assert_eq!(store.entry_count(), 3);
    }

    // Reopen and verify recovery
    {
        let store = KkdbLogStore::open(tmpdir.path()).unwrap();
        assert_eq!(store.entry_count(), 3);
        assert_eq!(store.last_index(), Some(3));
    }
}

#[test]
fn test_raft_log_store_truncate_and_compact() {
    use crate::raft::log_store::KkdbLogStore;
    use openraft::{Entry, LogId};

    let tmpdir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(tmpdir.path()).unwrap();

    // Append 10 entries
    let entries: Vec<Entry<crate::raft::types::KkdbTypeConfig>> = (1..=10).map(|i| {
        Entry {
            log_id: LogId { leader_id: openraft::CommittedLeaderId::new(1, 0), index: i },
            payload: openraft::EntryPayload::Blank,
        }
    }).collect();
    store.append_direct(entries).unwrap();

    // Truncate from index 6
    store.truncate_direct(6).unwrap();
    assert_eq!(store.entry_count(), 5); // entries 1..5

    // Manual compact
    let dead = store.compact().unwrap();
    assert!(dead > 0);

    let stats = store.detailed_compaction_stats();
    assert_eq!(stats.compaction_count, 1);
    assert!(stats.total_dead_eliminated > 0);
}

// ─── Fulltext Index Module Public API Tests ──────────────────────────────────

#[test]
fn test_fulltext_mod_tokenize_to_tf() {
    use crate::fulltext::tokenizer::simple_tokenize_to_tf;
    let (tf, total) = simple_tokenize_to_tf("hello world hello");
    assert_eq!(total, 3);
    assert_eq!(tf["hello"], 2);
    assert_eq!(tf["world"], 1);
}
