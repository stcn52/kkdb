// ── Core execution pipeline (stay at root) ────────────────────────────
pub mod data_transfer;
mod eval_expr;
mod exec_ddl;
mod exec_dml;
mod exec_select;
pub mod execute;
pub mod lock_manager;
pub mod mvcc;
pub mod connection_pool;
pub mod prepared;
pub mod gc;

// ── Grouped sub-modules ───────────────────────────────────────────────
pub mod optimizer;    // query_compiler, query_cache, query_opt_deep, adaptive_join, vectorized
pub mod auth;         // rbac, audit, security (data protection)
pub mod monitor;      // perf_counter, diagnostics, observability, observability_ops
pub mod engine;       // exec_engine, sql_engine_adv, dev_tools

// ── Backward-compatible re-exports ────────────────────────────────────
pub use optimizer::query_compiler;
pub use optimizer::query_cache;
pub use optimizer::query_opt_deep;
pub use optimizer::adaptive_join;
pub use optimizer::vectorized;

pub use auth::rbac;
pub use auth::audit;
pub use auth::security;

pub use monitor::perf_counter;
pub use monitor::diagnostics;
pub use monitor::observability;
pub use monitor::observability_ops;
pub use monitor::test_catalog;
pub use monitor::bench_framework;

pub use engine::exec_engine;
pub use engine::sql_engine_adv;
pub use engine::dev_tools;
pub use engine::adv_query;
pub use engine::sql_ext;

pub use auth::security_adv;

pub use optimizer::exec_engine_v2;
pub use monitor::observability_v2;

pub use engine::sql_pipeline;
pub use engine::dev_experience;

#[cfg(test)]
mod execute_tests;
