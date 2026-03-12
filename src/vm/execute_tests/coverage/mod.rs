//! Legacy coverage push tests (R1-R6) — boost series, surgical, sprint, finals

pub(crate) use super::{exec, exec_multi, query_rows};
pub(crate) use crate::types::Value;
pub(crate) use crate::vm::execute::{like_match, ExecResult, VM};

mod coverage_boost;
mod coverage_boost10;
mod coverage_boost11;
mod coverage_boost2;
mod coverage_boost3;
mod coverage_boost4;
mod coverage_boost5;
mod coverage_boost6;
mod coverage_boost7;
mod coverage_boost8;
mod coverage_boost9;
mod coverage_deep_r6;
mod coverage_direct_api;
mod coverage_direct_api2;
mod coverage_final75;
mod coverage_final_push;
mod coverage_r6;
mod coverage_sprint75;
mod coverage_surgical;
mod coverage_wave6;
