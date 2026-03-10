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
use crate::error::{Result, KkdbError};
use crate::schema::Schema;
use crate::sql::ast::*;
use crate::storage::pager::Pager;
use crate::types::{Row, Value};
use std::collections::{HashMap, HashSet, VecDeque};

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
