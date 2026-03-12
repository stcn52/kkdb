//! Unit tests for the KKDB VM executor.
//!
//! This module is split into logical sub-modules:
//! - `basic`        — like_match, VM construction, CREATE/DROP, INSERT
//! - `select`       — SELECT expressions, UPDATE/DELETE
//! - `operators`    — binary/unary operators, IS NULL, LIKE, BETWEEN
//! - `functions`    — scalar functions, aggregate functions
//! - `join`         — JOIN (INNER, LEFT, RIGHT)
//! - `ddl`          — EXPLAIN, CREATE INDEX, aliases, subqueries
//! - `expressions`  — fine-grained expression & coverage tests
//! - `transactions` — transaction tests
//! - `r5_features`  — R5 new features, UNNEST, GROUP BY alias
//! - `params`       — parameterized queries (`execute_params`)

use crate::vm::execute::{VM, ExecResult, like_match};
use crate::types::Value;

// ── Shared helpers ────────────────────────────────────────────────────────────

pub(super) fn exec(sql: &str) -> ExecResult {
    let mut vm = VM::new_memory();
    vm.execute_sql(sql).unwrap()
}

pub(super) fn exec_multi(sqls: &[&str]) -> Vec<ExecResult> {
    let mut vm = VM::new_memory();
    sqls.iter().map(|s| vm.execute_sql(s).unwrap()).collect()
}

pub(super) fn query_rows(vm: &mut VM, sql: &str) -> Vec<Vec<Value>> {
    match vm.execute_sql(sql).unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    }
}

// ── Sub-modules ───────────────────────────────────────────────────────────────

mod basic;
mod select;
mod operators;
mod functions;
mod join;
mod ddl;
mod expressions;
mod transactions;
mod r5_features;
mod params;
mod eval_expr_coverage;
mod sqlparser_expr_coverage;
mod coverage_boost;
mod coverage_boost2;
mod coverage_boost3;
mod coverage_boost4;
mod coverage_boost5;
mod coverage_boost6;
mod coverage_boost7;
mod coverage_boost8;
mod coverage_boost9;
mod emoji_compat;
mod coverage_boost10;
mod mvcc_visibility;
mod query_cache_integration;
mod coverage_boost_r4;
mod binlog_coverage;
mod eval_expr_r5;
mod raft_coverage;
mod mvcc_row_lock;
mod select_for_update;
mod coverage_r6;
mod read_committed;
mod coverage_boost11;mod coverage_deep_r6;
mod coverage_surgical;
mod coverage_sprint75;
mod coverage_final75;
mod coverage_direct_api;
mod coverage_wave6;
mod coverage_direct_api2;
mod coverage_final_push;
mod coverage_r7_push80;
mod coverage_r7_push80b;
mod coverage_r7_push80c;
mod coverage_r7_push80d;
mod coverage_r7_push80e;
mod coverage_r7_push80f;
mod coverage_r7_push80g;
mod coverage_r7_push80h;
mod coverage_r7_push80i;
mod coverage_r7_push80j;
mod coverage_r7_push80k;
mod coverage_r7_push80l;
mod coverage_r7_push80m;
mod coverage_r8_optimizer_wal;
mod coverage_r9_mvcc_fts_raft;
mod coverage_r10_prepared_bloom_wf;
mod coverage_r11_audit_hash_histogram;
mod coverage_r12_join_rbac_lsm_perf;