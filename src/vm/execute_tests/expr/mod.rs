//! Expression tests — expression evaluation, sqlparser coverage, r5 enhancements

pub(crate) use super::{exec, exec_multi, query_rows};
pub(crate) use crate::vm::execute::{VM, ExecResult, like_match};
pub(crate) use crate::types::Value;

mod expressions;
mod eval_expr_coverage;
mod sqlparser_expr_coverage;
mod eval_expr_r5;
