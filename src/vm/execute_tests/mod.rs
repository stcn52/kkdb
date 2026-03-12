//! Unit tests for the KKDB VM executor.
//!
//! Tests are organized into logical categories:
//!
//! - `core`     — basic SQL: CREATE/DROP, INSERT, SELECT, UPDATE, DELETE,
//!                operators, functions, JOIN, DDL, params
//! - `expr`     — expression evaluation, sqlparser coverage, R5 expressions
//! - `txn`      — transactions, MVCC visibility, row locks, read committed
//! - `feature`  — feature-specific: R5, emoji, query cache, binlog, raft
//! - `coverage` — legacy coverage pushes (R1-R6): boost series, surgical,
//!                sprint/final75, direct_api, wave6
//! - `push80`   — R7 coverage push80 series (a-m)
//! - `rounds`   — round-based coverage (R8-R18)

use crate::vm::execute::{VM, ExecResult, like_match};
use crate::types::Value;

// ── Shared helpers ────────────────────────────────────────────────────────────

pub(crate) fn exec(sql: &str) -> ExecResult {
    let mut vm = VM::new_memory();
    vm.execute_sql(sql).unwrap()
}

pub(crate) fn exec_multi(sqls: &[&str]) -> Vec<ExecResult> {
    let mut vm = VM::new_memory();
    sqls.iter().map(|s| vm.execute_sql(s).unwrap()).collect()
}

pub(crate) fn query_rows(vm: &mut VM, sql: &str) -> Vec<Vec<Value>> {
    match vm.execute_sql(sql).unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    }
}

// ── Test sub-module categories ────────────────────────────────────────────────

mod core;
mod expr;
mod txn;
mod feature;
mod coverage;
mod push80;
mod rounds;