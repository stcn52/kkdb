//! Expression tests — expression evaluation, sqlparser coverage, r5 enhancements

#[allow(unused_imports)]
pub(crate) use super::{exec, exec_multi, query_rows};
#[allow(unused_imports)]
pub(crate) use crate::types::Value;
#[allow(unused_imports)]
pub(crate) use crate::vm::execute::{like_match, ExecResult, VM};

mod eval_expr_coverage;
mod eval_expr_r5;
mod expressions;
mod sqlparser_expr_coverage;
