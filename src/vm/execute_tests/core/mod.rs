//! Core SQL tests — basic operations, SELECT, operators, functions, JOIN, DDL, params

pub(crate) use super::{exec, exec_multi, query_rows};
pub(crate) use crate::types::Value;
pub(crate) use crate::vm::execute::{like_match, ExecResult, VM};

mod basic;
mod ddl;
mod functions;
mod join;
mod operators;
mod params;
mod select;
