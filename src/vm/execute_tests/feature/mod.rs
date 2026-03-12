//! Feature-specific tests — R5, emoji, query cache, binlog, raft, boost_r4

pub(crate) use super::{exec, exec_multi, query_rows};
pub(crate) use crate::vm::execute::{VM, ExecResult, like_match};
pub(crate) use crate::types::Value;

mod r5_features;
mod emoji_compat;
mod query_cache_integration;
mod binlog_coverage;
mod raft_coverage;
mod coverage_boost_r4;
