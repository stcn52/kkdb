//! Feature-specific tests — R5, emoji, query cache, binlog, raft, boost_r4

pub(crate) use super::{exec, exec_multi, query_rows};
pub(crate) use crate::types::Value;
pub(crate) use crate::vm::execute::{like_match, ExecResult, VM};

mod binlog_coverage;
mod coverage_boost_r4;
mod emoji_compat;
mod query_cache_integration;
mod r5_features;
mod raft_coverage;
