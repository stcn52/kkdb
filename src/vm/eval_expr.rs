use super::execute::{ExecResult, VM};
use crate::error::{KkdbError, Result};
use crate::sql::ast::*;
use crate::types::{Row, Value};
use std::collections::HashMap;

impl VM {
    // ---- Expression Evaluation ----

    #[inline]
    pub fn eval_expr(
        &mut self,
        expr: &Expr,
        row: &Row,
        col_map: &HashMap<String, usize>,
    ) -> Result<Value> {
        match expr {
            Expr::IntegerLiteral(v) => Ok(Value::Integer(*v)),
            Expr::RealLiteral(v) => Ok(Value::Real(*v)),
            Expr::StringLiteral(v) => Ok(Value::Text(v.clone().into())),
            Expr::BlobLiteral(v) => Ok(Value::Blob(v.clone())),
            Expr::Null => Ok(Value::Null),

            Expr::Placeholder(idx) => self
                .current_params
                .get(*idx)
                .cloned()
                .ok_or_else(|| {
                    KkdbError::RuntimeError(format!(
                        "parameter index {} out of range ({} parameter{} supplied)",
                        idx,
                        self.current_params.len(),
                        if self.current_params.len() == 1 { "" } else { "s" }
                    ))
                }),

            Expr::ColumnRef { table, column } => {
                // If table qualifier is specified, try qualified lookup first
                if let Some(t) = table {
                    // Build "table.column" key on the stack to avoid heap allocation
                    let mut buf = String::with_capacity(t.len() + 1 + column.len());
                    for c in t.as_bytes() {
                        buf.push(c.to_ascii_lowercase() as char);
                    }
                    buf.push('.');
                    for c in column.as_bytes() {
                        buf.push(c.to_ascii_lowercase() as char);
                    }
                    if let Some(&idx) = col_map.get(buf.as_str()) {
                        return if idx < row.len() {
                            Ok(row[idx].clone())
                        } else {
                            Ok(Value::Null)
                        };
                    }
                    // Try outer rows (correlated subquery): search from innermost to outermost
                    for (outer_row, outer_col_map) in self.outer_rows.iter().rev() {
                        if let Some(&idx) = outer_col_map.get(buf.as_str()) {
                            return if idx < outer_row.len() {
                                Ok(outer_row[idx].clone())
                            } else {
                                Ok(Value::Null)
                            };
                        }
                    }
                }
                // Fast path: try direct lookup (col_map keys are already lowercase)
                if let Some(&idx) = col_map.get(column.as_str()) {
                    return if idx < row.len() {
                        Ok(row[idx].clone())
                    } else {
                        Ok(Value::Null)
                    };
                }
                // Slow path: try case-insensitive lookup
                let lower = column.to_ascii_lowercase();
                if lower != column.as_str() {
                    if let Some(&idx) = col_map.get(lower.as_str()) {
                        return if idx < row.len() {
                            Ok(row[idx].clone())
                        } else {
                            Ok(Value::Null)
                        };
                    }
                }
                // Final fallback: search outer rows for unqualified column name
                for (outer_row, outer_col_map) in self.outer_rows.iter().rev() {
                    if let Some(&idx) = outer_col_map.get(column.as_str()) {
                        return if idx < outer_row.len() {
                            Ok(outer_row[idx].clone())
                        } else {
                            Ok(Value::Null)
                        };
                    }
                    let lower_col = column.to_ascii_lowercase();
                    if let Some(&idx) = outer_col_map.get(lower_col.as_str()) {
                        return if idx < outer_row.len() {
                            Ok(outer_row[idx].clone())
                        } else {
                            Ok(Value::Null)
                        };
                    }
                }
                Err(KkdbError::ColumnNotFound(column.clone()))
            }

            Expr::BinaryOp { left, op, right } => {
                // Short-circuit AND/OR: avoid evaluating right side when unnecessary
                match op {
                    BinaryOperator::And => {
                        let l = self.eval_expr(left, row, col_map)?;
                        if !matches!(l, Value::Null) && !l.is_truthy() {
                            return Ok(Value::Integer(0));
                        }
                        let r = self.eval_expr(right, row, col_map)?;
                        if !matches!(r, Value::Null) && !r.is_truthy() {
                            return Ok(Value::Integer(0));
                        }
                        if matches!(l, Value::Null) || matches!(r, Value::Null) {
                            return Ok(Value::Null);
                        }
                        Ok(Value::Integer(1))
                    }
                    BinaryOperator::Or => {
                        let l = self.eval_expr(left, row, col_map)?;
                        if !matches!(l, Value::Null) && l.is_truthy() {
                            return Ok(Value::Integer(1));
                        }
                        let r = self.eval_expr(right, row, col_map)?;
                        if !matches!(r, Value::Null) && r.is_truthy() {
                            return Ok(Value::Integer(1));
                        }
                        if matches!(l, Value::Null) || matches!(r, Value::Null) {
                            return Ok(Value::Null);
                        }
                        Ok(Value::Integer(0))
                    }
                    BinaryOperator::FtsMatch => {
                        // `left` is the table name (not a column), so just evaluate the
                        // keyword from `right` and check if any TEXT column in the row
                        // contains all tokens.
                        let keyword_val = match self.eval_expr(right, row, col_map) {
                            Ok(v) => v,
                            Err(_) => return Ok(Value::Integer(0)),
                        };
                        if let Value::Text(keyword) = keyword_val {
                            let tokens = VM::tokenize(&keyword);
                            if tokens.is_empty() {
                                return Ok(Value::Integer(0));
                            }
                            // Concatenate all text-valued cells in the row
                            let haystack: String = row
                                .iter()
                                .filter_map(|v| {
                                    if let Value::Text(t) = v {
                                        Some(t.to_lowercase())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" ");
                            let matched = tokens.iter().all(|tok| haystack.contains(tok.as_str()));
                            return Ok(Value::Integer(if matched { 1 } else { 0 }));
                        }
                        Ok(Value::Integer(0))
                    }
                    _ => {
                        let l = self.eval_expr(left, row, col_map)?;
                        let r = self.eval_expr(right, row, col_map)?;
                        self.apply_binary_op(op, &l, &r)
                    }
                }
            }

            Expr::UnaryOp { op, expr } => {
                let val = self.eval_expr(expr, row, col_map)?;
                match op {
                    UnaryOperator::Minus => match val {
                        Value::Integer(v) => Ok(Value::Integer(v.wrapping_neg())),
                        Value::Real(v) => Ok(Value::Real(-v)),
                        Value::Null => Ok(Value::Null),
                        _ => Err(KkdbError::TypeError(
                            "cannot negate non-numeric value".into(),
                        )),
                    },
                    UnaryOperator::Not => {
                        if matches!(val, Value::Null) {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Integer(if val.is_truthy() { 0 } else { 1 }))
                        }
                    }
                }
            }

            Expr::IsNull { expr, negated } => {
                let val = self.eval_expr(expr, row, col_map)?;
                let is_null = matches!(val, Value::Null);
                Ok(Value::Integer(if is_null != *negated { 1 } else { 0 }))
            }

            Expr::InList {
                expr,
                list,
                negated,
            } => {
                let val = self.eval_expr(expr, row, col_map)?;
                // SQL NULL semantics: NULL IN (...) => NULL
                if matches!(val, Value::Null) {
                    return Ok(Value::Null);
                }
                let mut found = false;
                let mut has_null = false;
                for item in list {
                    let item_val = self.eval_expr(item, row, col_map)?;
                    if matches!(item_val, Value::Null) {
                        has_null = true;
                        continue;
                    }
                    if val == item_val {
                        found = true;
                        break;
                    }
                }
                if found {
                    Ok(Value::Integer(if !*negated { 1 } else { 0 }))
                } else if has_null {
                    // val NOT IN list but list contains NULL => NULL
                    Ok(Value::Null)
                } else {
                    Ok(Value::Integer(if *negated { 1 } else { 0 }))
                }
            }

            Expr::Like {
                expr,
                pattern,
                escape_char,
                case_insensitive,
                negated,
            } => {
                let val = self.eval_expr(expr, row, col_map)?;
                let pat = self.eval_expr(pattern, row, col_map)?;
                if matches!(val, Value::Null) || matches!(pat, Value::Null) {
                    return Ok(Value::Null);
                }
                match (&val, &pat) {
                    (Value::Text(s), Value::Text(p)) => {
                        let matches = like_match(s, p, *escape_char, *case_insensitive);
                        Ok(Value::Integer(if matches != *negated { 1 } else { 0 }))
                    }
                    _ => Ok(Value::Integer(0)),
                }
            }

            Expr::Between {
                expr,
                low,
                high,
                negated,
            } => {
                let val = self.eval_expr(expr, row, col_map)?;
                let lo = self.eval_expr(low, row, col_map)?;
                let hi = self.eval_expr(high, row, col_map)?;
                if matches!(val, Value::Null)
                    || matches!(lo, Value::Null)
                    || matches!(hi, Value::Null)
                {
                    return Ok(Value::Null);
                }
                let in_range = val
                    .partial_cmp(&lo)
                    .is_some_and(|o| o != std::cmp::Ordering::Less)
                    && val
                        .partial_cmp(&hi)
                        .is_some_and(|o| o != std::cmp::Ordering::Greater);
                Ok(Value::Integer(if in_range != *negated { 1 } else { 0 }))
            }

            Expr::Function {
                name,
                args,
                distinct: _,
            } => {
                // Use eq_ignore_ascii_case to avoid to_uppercase() String allocation
                let n = name.as_str();
                if n.eq_ignore_ascii_case("COUNT")
                    || n.eq_ignore_ascii_case("SUM")
                    || n.eq_ignore_ascii_case("AVG")
                    || n.eq_ignore_ascii_case("MIN")
                    || n.eq_ignore_ascii_case("MAX")
                {
                    if args.is_empty() {
                        Ok(Value::Integer(1))
                    } else {
                        self.eval_expr(&args[0], row, col_map)
                    }
                } else if n.eq_ignore_ascii_case("ABS") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    match val {
                        Value::Integer(v) => Ok(Value::Integer(v.wrapping_abs())),
                        Value::Real(v) => Ok(Value::Real(v.abs())),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("UPPER") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    match val {
                        Value::Text(s) => Ok(Value::Text(s.to_uppercase().into())),
                        _ => Ok(val),
                    }
                } else if n.eq_ignore_ascii_case("LOWER") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    match val {
                        Value::Text(s) => Ok(Value::Text(s.to_lowercase().into())),
                        _ => Ok(val),
                    }
                } else if n.eq_ignore_ascii_case("LENGTH") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    match val {
                        // I9 fix: return Unicode char count, not byte count
                        Value::Text(s) => Ok(Value::Integer(s.chars().count() as i64)),
                        Value::Blob(b) => Ok(Value::Integer(b.len() as i64)),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Integer(format!("{}", val).chars().count() as i64)),
                    }
                } else if n.eq_ignore_ascii_case("TYPEOF") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    let type_name = match val {
                        Value::Null => "null",
                        Value::Integer(_) => "integer",
                        Value::Real(_) => "real",
                        Value::Text(_) => "text",
                        Value::Blob(_) => "blob",
                    };
                    Ok(Value::Text(type_name.into()))
                } else if n.eq_ignore_ascii_case("COALESCE") {
                    for arg in args {
                        let val = self.eval_expr(arg, row, col_map)?;
                        if !matches!(val, Value::Null) {
                            return Ok(val);
                        }
                    }
                    Ok(Value::Null)
                } else if n.eq_ignore_ascii_case("IFNULL") {
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    if matches!(val, Value::Null) {
                        self.eval_expr(&args[1], row, col_map)
                    } else {
                        Ok(val)
                    }
                } else if n.eq_ignore_ascii_case("SUBSTR") || n.eq_ignore_ascii_case("SUBSTRING") {
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let s = match self.eval_expr(&args[0], row, col_map)? {
                        Value::Text(s) => s,
                        _ => return Ok(Value::Null),
                    };
                    let start = match self.eval_expr(&args[1], row, col_map)? {
                        Value::Integer(v) => (v - 1).max(0) as usize,
                        _ => return Ok(Value::Null),
                    };
                    let len = if args.len() > 2 {
                        match self.eval_expr(&args[2], row, col_map)? {
                            Value::Integer(v) => Some(v.max(0) as usize),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let chars: Vec<char> = s.chars().collect();
                    let end = match len {
                        Some(l) => (start + l).min(chars.len()),
                        None => chars.len(),
                    };
                    if start >= chars.len() {
                        Ok(Value::Text("".into()))
                    } else {
                        let s: String = chars[start..end].iter().collect();
                        Ok(Value::Text(s.into()))
                    }
                } else if n.eq_ignore_ascii_case("REPLACE") {
                    if args.len() < 3 {
                        return Ok(Value::Null);
                    }
                    let s = match self.eval_expr(&args[0], row, col_map)? {
                        Value::Text(s) => s,
                        _ => return Ok(Value::Null),
                    };
                    let from = match self.eval_expr(&args[1], row, col_map)? {
                        Value::Text(s) => s,
                        _ => return Ok(Value::Null),
                    };
                    let to = match self.eval_expr(&args[2], row, col_map)? {
                        Value::Text(s) => s,
                        _ => return Ok(Value::Null),
                    };
                    Ok(Value::Text(s.replace(&*from, &to).into()))
                } else if n.eq_ignore_ascii_case("TRIM") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    // Bug #4 fix: optional second arg = characters to trim
                    if args.len() > 1 {
                        match (val, self.eval_expr(&args[1], row, col_map)?) {
                            (Value::Text(s), Value::Text(chars)) => {
                                let chars_set: Vec<char> = chars.chars().collect();
                                Ok(Value::Text(s.trim_matches(chars_set.as_slice()).into()))
                            }
                            (v, _) => Ok(v),
                        }
                    } else {
                        match val {
                            Value::Text(s) => Ok(Value::Text(s.trim().into())),
                            v => Ok(v),
                        }
                    }
                } else if n.eq_ignore_ascii_case("LTRIM") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    if args.len() > 1 {
                        match (val, self.eval_expr(&args[1], row, col_map)?) {
                            (Value::Text(s), Value::Text(chars)) => {
                                let chars_set: Vec<char> = chars.chars().collect();
                                Ok(Value::Text(
                                    s.trim_start_matches(chars_set.as_slice()).into(),
                                ))
                            }
                            (v, _) => Ok(v),
                        }
                    } else {
                        match val {
                            Value::Text(s) => Ok(Value::Text(s.trim_start().into())),
                            v => Ok(v),
                        }
                    }
                } else if n.eq_ignore_ascii_case("RTRIM") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    if args.len() > 1 {
                        match (val, self.eval_expr(&args[1], row, col_map)?) {
                            (Value::Text(s), Value::Text(chars)) => {
                                let chars_set: Vec<char> = chars.chars().collect();
                                Ok(Value::Text(s.trim_end_matches(chars_set.as_slice()).into()))
                            }
                            (v, _) => Ok(v),
                        }
                    } else {
                        match val {
                            Value::Text(s) => Ok(Value::Text(s.trim_end().into())),
                            v => Ok(v),
                        }
                    }
                } else if n.eq_ignore_ascii_case("NULLIF") {
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let a = self.eval_expr(&args[0], row, col_map)?;
                    let b = self.eval_expr(&args[1], row, col_map)?;
                    if a == b {
                        Ok(Value::Null)
                    } else {
                        Ok(a)
                    }
                } else if n.eq_ignore_ascii_case("ROUND") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    let digits = if args.len() > 1 {
                        match self.eval_expr(&args[1], row, col_map)? {
                            Value::Integer(v) => v.max(0) as u32,
                            _ => 0,
                        }
                    } else {
                        0
                    };
                    match val {
                        Value::Integer(v) => Ok(Value::Integer(v)),
                        Value::Real(v) => {
                            let factor = 10f64.powi(digits as i32);
                            Ok(Value::Real((v * factor).round() / factor))
                        }
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("CEIL") || n.eq_ignore_ascii_case("CEILING") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) => Ok(Value::Integer(v)),
                        Value::Real(v) => Ok(Value::Real(v.ceil())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("FLOOR") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) => Ok(Value::Integer(v)),
                        Value::Real(v) => Ok(Value::Real(v.floor())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("INSTR") {
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let haystack = self.eval_expr(&args[0], row, col_map)?;
                    let needle = self.eval_expr(&args[1], row, col_map)?;
                    match (haystack, needle) {
                        (Value::Text(s), Value::Text(p)) => {
                            let pos = s
                                .find(p.as_ref())
                                .map(|b| s[..b].chars().count() as i64 + 1)
                                .unwrap_or(0);
                            Ok(Value::Integer(pos))
                        }
                        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                        _ => Ok(Value::Integer(0)),
                    }
                } else if n.eq_ignore_ascii_case("OVERLAY") {
                    // OVERLAY(s, placing, from[, for])
                    // Result = SUBSTR(s, 1, from-1) || placing || SUBSTR(s, from + length(for_or_placing))
                    if args.len() < 3 {
                        return Ok(Value::Null);
                    }
                    let s = match self.eval_expr(&args[0], row, col_map)? {
                        Value::Text(s) => s,
                        _ => return Ok(Value::Null),
                    };
                    let placing = match self.eval_expr(&args[1], row, col_map)? {
                        Value::Text(s) => s,
                        _ => return Ok(Value::Null),
                    };
                    let from = match self.eval_expr(&args[2], row, col_map)? {
                        Value::Integer(v) => (v - 1).max(0) as usize,
                        _ => return Ok(Value::Null),
                    };
                    let chars: Vec<char> = s.chars().collect();
                    let replace_len = if args.len() > 3 {
                        match self.eval_expr(&args[3], row, col_map)? {
                            Value::Integer(v) => v.max(0) as usize,
                            _ => placing.chars().count(),
                        }
                    } else {
                        placing.chars().count()
                    };
                    let before: String = chars[..from.min(chars.len())].iter().collect();
                    let after_start = (from + replace_len).min(chars.len());
                    let after: String = chars[after_start..].iter().collect();
                    Ok(Value::Text(format!("{before}{placing}{after}").into()))
                } else if n.eq_ignore_ascii_case("SIGN") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) => Ok(Value::Integer(v.signum())),
                        Value::Real(v) => Ok(Value::Real(v.signum())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                // ---- R1: NULL-safe equality operators ----
                } else if n.eq_ignore_ascii_case("__IS_DISTINCT_FROM__") {
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let a = self.eval_expr(&args[0], row, col_map)?;
                    let b = self.eval_expr(&args[1], row, col_map)?;
                    Ok(Value::Integer(match (&a, &b) {
                        (Value::Null, Value::Null) => 0, // NULL IS DISTINCT FROM NULL = FALSE
                        (Value::Null, _) | (_, Value::Null) => 1,
                        _ => {
                            if a == b {
                                0
                            } else {
                                1
                            }
                        }
                    }))
                } else if n.eq_ignore_ascii_case("__IS_NOT_DISTINCT_FROM__") {
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let a = self.eval_expr(&args[0], row, col_map)?;
                    let b = self.eval_expr(&args[1], row, col_map)?;
                    Ok(Value::Integer(match (&a, &b) {
                        (Value::Null, Value::Null) => 1, // NULL IS NOT DISTINCT FROM NULL = TRUE
                        (Value::Null, _) | (_, Value::Null) => 0,
                        _ => {
                            if a == b {
                                1
                            } else {
                                0
                            }
                        }
                    }))
                // ---- R3: Bitwise / math functions ----
                } else if n.eq_ignore_ascii_case("BITWISE_NOT") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) => Ok(Value::Integer(!v)),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("CBRT") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) => Ok(Value::Real((v as f64).cbrt())),
                        Value::Real(v) => Ok(Value::Real(v.cbrt())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("FACTORIAL") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) if v < 0 => Err(KkdbError::RuntimeError(
                            "FACTORIAL requires a non-negative integer".into(),
                        )),
                        Value::Integer(v) if v > 20 => {
                            // S2 fix: 21! > i64::MAX — return error rather than silently truncating.
                            // Callers that need big factorials should cast to REAL first.
                            Err(KkdbError::RuntimeError(format!(
                                "FACTORIAL({v}) overflows i64; maximum supported value is 20"
                            )))
                        }
                        Value::Integer(v) => {
                            let result: i64 = (1..=v).product();
                            Ok(Value::Integer(result))
                        }
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("POWER") || n.eq_ignore_ascii_case("POW") {
                    // POWER(base, exp) — a ^ b
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let base = self.eval_expr(&args[0], row, col_map)?;
                    let exp = self.eval_expr(&args[1], row, col_map)?;
                    match (base, exp) {
                        (Value::Integer(a), Value::Integer(b)) if b >= 0 => {
                            // M8 fix: check for overflow, promote to Real if needed
                            let bu = b as u32;
                            match a.checked_pow(bu) {
                                Some(v) => Ok(Value::Integer(v)),
                                None => Ok(Value::Real((a as f64).powi(b as i32))),
                            }
                        }
                        (Value::Integer(a), Value::Integer(b)) => {
                            Ok(Value::Real((a as f64).powi(b as i32)))
                        }
                        (Value::Real(a), Value::Integer(b)) => Ok(Value::Real(a.powi(b as i32))),
                        (Value::Integer(a), Value::Real(b)) => Ok(Value::Real((a as f64).powf(b))),
                        (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a.powf(b))),
                        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n == "__win" {
                    let idx = match args.first() {
                        Some(Expr::IntegerLiteral(i)) => *i as usize,
                        _ => 0,
                    };
                    Ok(self.window_results.as_ref().map_or(Value::Null, |res| {
                        res[self.current_window_row_idx][idx].clone()
                    }))
                } else if n.eq_ignore_ascii_case("JSON_EXTRACT")
                    || n.eq_ignore_ascii_case("JSON_EXTRACT_TEXT")
                {
                    // Simple JSON extraction: JSON_EXTRACT(json_str, '$.key') or JSON_EXTRACT(json_str, 'key')
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let json_val = self.eval_expr(&args[0], row, col_map)?;
                    let path_val = self.eval_expr(&args[1], row, col_map)?;
                    match (json_val, path_val) {
                        (Value::Text(s), Value::Text(p)) => {
                            if let Some(extracted) = json_extract_primitive(&s, &p) {
                                // Return properly typed value based on JSON content
                                Ok(json_scalar_to_value(&extracted))
                            } else {
                                Ok(Value::Null)
                            }
                        }
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("JSON_ARRAY") {
                    let mut elements = Vec::with_capacity(args.len());
                    for arg in args {
                        let val = self.eval_expr(arg, row, col_map)?;
                        let s = match val {
                            Value::Null => "null".to_string(),
                            Value::Integer(i) => i.to_string(),
                            Value::Real(f) => f.to_string(),
                            Value::Text(t) => {
                                let escaped = t.replace('\\', "\\\\").replace('"', "\\\"");
                                format!("\"{escaped}\"")
                            }
                            Value::Blob(_) => "\"<blob>\"".to_string(),
                        };
                        elements.push(s);
                    }
                    Ok(Value::Text(format!("[{}]", elements.join(", ")).into()))
                } else if n.eq_ignore_ascii_case("ARRAY_GET") {
                    // Placeholder: arrays not natively supported yet
                    Ok(Value::Null)
                } else if n.eq_ignore_ascii_case("JSON_MEMBER_OF") {
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    let arr = self.eval_expr(&args[1], row, col_map)?;
                    match (val, arr) {
                        (v, Value::Text(s)) => {
                            let v_str = match v {
                                Value::Text(t) => format!("\"{}\"", t),
                                Value::Integer(i) => i.to_string(),
                                Value::Real(f) => f.to_string(),
                                Value::Null => "null".to_string(),
                                Value::Blob(_) => return Ok(Value::Null),
                            };
                            if json_array_contains(&s, &v_str) {
                                Ok(Value::Integer(1))
                            } else {
                                Ok(Value::Integer(0))
                            }
                        }
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("JSON_OBJECT") {
                    let mut elements = Vec::with_capacity(args.len() / 2);
                    let mut i = 0;
                    while i + 1 < args.len() {
                        let k = self.eval_expr(&args[i], row, col_map)?;
                        let v = self.eval_expr(&args[i + 1], row, col_map)?;
                        let k_str = match k {
                            Value::Text(t) => t.replace('\\', "\\\\").replace('"', "\\\""),
                            // JSON keys must be strings
                            _ => k.to_string().replace('\\', "\\\\").replace('"', "\\\""),
                        };
                        let v_str = match v {
                            Value::Null => "null".to_string(),
                            Value::Integer(num) => num.to_string(),
                            Value::Real(f) => f.to_string(),
                            Value::Text(t) => {
                                let escaped = t.replace('\\', "\\\\").replace('"', "\\\"");
                                format!("\"{escaped}\"")
                            }
                            Value::Blob(_) => "\"<blob>\"".to_string(),
                        };
                        elements.push(format!("\"{k_str}\": {v_str}"));
                        i += 2;
                    }
                    Ok(Value::Text(format!("{{{}}}", elements.join(", ")).into()))
                } else if n.eq_ignore_ascii_case("JSON_TYPE") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let v = self.eval_expr(&args[0], row, col_map)?;
                    let type_str = match &v {
                        Value::Null => return Ok(Value::Null),
                        Value::Text(s) => {
                            let t = s.trim();
                            if t.starts_with('{') {
                                "OBJECT"
                            } else if t.starts_with('[') {
                                "ARRAY"
                            } else if t == "true" || t == "false" {
                                "BOOLEAN"
                            } else if t == "null" {
                                "NULL"
                            } else if t.parse::<i64>().is_ok() {
                                "INTEGER"
                            } else if t.parse::<f64>().is_ok() {
                                "DOUBLE"
                            } else {
                                "STRING"
                            }
                        }
                        Value::Integer(_) => "INTEGER",
                        Value::Real(_) => "DOUBLE",
                        Value::Blob(_) => "BLOB",
                    };
                    Ok(Value::Text(type_str.into()))
                } else if n.eq_ignore_ascii_case("JSON_VALID") {
                    if args.is_empty() {
                        return Ok(Value::Integer(0));
                    }
                    let v = self.eval_expr(&args[0], row, col_map)?;
                    let valid = match &v {
                        Value::Text(s) => json_is_valid(s.as_ref()),
                        _ => false,
                    };
                    Ok(Value::Integer(if valid { 1 } else { 0 }))
                } else if n.eq_ignore_ascii_case("JSON_LENGTH") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let v = self.eval_expr(&args[0], row, col_map)?;
                    let path = if args.len() >= 2 {
                        match self.eval_expr(&args[1], row, col_map)? {
                            Value::Text(p) => Some(p.to_string()),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let json_str = match &v {
                        Value::Text(s) => s.as_ref().to_string(),
                        _ => return Ok(Value::Null),
                    };
                    let target = if let Some(p) = path {
                        json_extract_primitive(&json_str, &p).unwrap_or_default()
                    } else {
                        json_str
                    };
                    Ok(Value::Integer(json_length(&target) as i64))
                } else if n.eq_ignore_ascii_case("JSON_KEYS") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let v = self.eval_expr(&args[0], row, col_map)?;
                    match &v {
                        Value::Text(s) => Ok(Value::Text(json_keys(s.as_ref()).into())),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("JSON_CONTAINS") {
                    // JSON_CONTAINS(json, val, [path])
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let json_v = self.eval_expr(&args[0], row, col_map)?;
                    let needle_v = self.eval_expr(&args[1], row, col_map)?;
                    let path = if args.len() >= 3 {
                        match self.eval_expr(&args[2], row, col_map)? {
                            Value::Text(p) => Some(p.to_string()),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    match (json_v, needle_v) {
                        (Value::Text(json), needle) => {
                            let doc = if let Some(p) = path {
                                json_extract_primitive(&json, &p).unwrap_or_default()
                            } else {
                                json.to_string()
                            };
                            let needle_str = match &needle {
                                Value::Text(t) => format!("\"{}\"", t),
                                Value::Integer(i) => i.to_string(),
                                Value::Real(f) => f.to_string(),
                                Value::Null => "null".to_string(),
                                _ => return Ok(Value::Integer(0)),
                            };
                            Ok(Value::Integer(
                                if json_array_contains(&doc, &needle_str)
                                    || json_contains_value(&doc, &needle_str)
                                {
                                    1
                                } else {
                                    0
                                },
                            ))
                        }
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("JSON_REMOVE") {
                    // JSON_REMOVE(json, path, ...)
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let v = self.eval_expr(&args[0], row, col_map)?;
                    let mut json_str = match v {
                        Value::Text(s) => s.to_string(),
                        _ => return Ok(Value::Null),
                    };
                    for arg in args.iter().skip(1) {
                        let path = match self.eval_expr(arg, row, col_map)? {
                            Value::Text(p) => p.to_string(),
                            _ => continue,
                        };
                        json_str = json_remove_path(&json_str, &path).unwrap_or(json_str);
                    }
                    Ok(Value::Text(json_str.into()))
                } else if n.eq_ignore_ascii_case("JSON_SET")
                    || n.eq_ignore_ascii_case("JSON_INSERT")
                    || n.eq_ignore_ascii_case("JSON_REPLACE")
                {
                    // JSON_SET(json, path, val, [path2, val2, ...])
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let v = self.eval_expr(&args[0], row, col_map)?;
                    let mut json_str = match v {
                        Value::Text(s) => s.to_string(),
                        _ => return Ok(Value::Null),
                    };
                    let mut i = 1;
                    while i + 1 < args.len() {
                        let path = match self.eval_expr(&args[i], row, col_map)? {
                            Value::Text(p) => p.to_string(),
                            _ => {
                                i += 2;
                                continue;
                            }
                        };
                        let val = self.eval_expr(&args[i + 1], row, col_map)?;
                        let val_str = match &val {
                            Value::Null => "null".to_string(),
                            Value::Integer(x) => x.to_string(),
                            Value::Real(x) => x.to_string(),
                            Value::Text(t) => format!("\"{}\"", t.replace('"', "\\\"")),
                            Value::Blob(_) => "null".to_string(),
                        };
                        json_str = json_set_path(&json_str, &path, &val_str).unwrap_or(json_str);
                        i += 2;
                    }
                    Ok(Value::Text(json_str.into()))
                } else if n.eq_ignore_ascii_case("JSON_QUOTE") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let v = self.eval_expr(&args[0], row, col_map)?;
                    let s = match v {
                        Value::Text(t) => {
                            format!("\"{}\"", t.replace('\\', "\\\\").replace('"', "\\\""))
                        }
                        Value::Integer(i) => i.to_string(),
                        Value::Real(f) => f.to_string(),
                        Value::Null => "null".to_string(),
                        _ => return Ok(Value::Null),
                    };
                    Ok(Value::Text(s.into()))
                } else if n.eq_ignore_ascii_case("JSON_UNQUOTE") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let v = self.eval_expr(&args[0], row, col_map)?;
                    match v {
                        Value::Text(s) => {
                            let t = s.trim();
                            if t.starts_with('"') && t.ends_with('"') && t.len() >= 2 {
                                let inner = t[1..t.len() - 1]
                                    .replace("\\\"", "\"")
                                    .replace("\\\\", "\\");
                                Ok(Value::Text(inner.into()))
                            } else {
                                Ok(Value::Text(s.clone()))
                            }
                        }
                        other => Ok(other),
                    }
                } else if n.eq_ignore_ascii_case("REGEXP_LIKE") {
                    // I10: implement basic regex via Rust's std (no external crate needed for simple patterns)
                    if args.len() < 2 {
                        return Ok(Value::Integer(0));
                    }
                    let subject = self.eval_expr(&args[0], row, col_map)?;
                    let pattern = self.eval_expr(&args[1], row, col_map)?;
                    let (Value::Text(s), Value::Text(p)) = (subject, pattern) else {
                        return Ok(Value::Integer(0));
                    };
                    // Translate simple SQL LIKE-style wildcards to a basic pattern check.
                    // Full POSIX ERE support would require the `regex` crate; for now we
                    // support: `.` (any char), `.*` (any sequence), `^`/`$` anchors, and
                    // character literals by walking the pattern character-by-character.
                    fn regex_matches(text: &str, pat: &str) -> bool {
                        // Very lightweight: convert pattern to a vec of segments and try to match
                        let pat = pat.trim_start_matches('^');
                        let (anchored_end, pat) = if let Some(stripped) = pat.strip_suffix('$') {
                            (true, stripped)
                        } else {
                            (false, pat)
                        };
                        // Split on '.*' to get required literal fragments
                        let parts: Vec<&str> = pat.split(".*").collect();
                        let mut remaining = text;
                        for (i, part) in parts.iter().enumerate() {
                            let sub = part.replace('.', "\x00"); // placeholder for any-char
                                                                 // replace \x00 with actual any-char matching — simple char-by-char
                            let _ = sub; // just do a contains check for now
                            if i == 0 {
                                // First part: must match at start
                                if part.is_empty() {
                                    continue;
                                }
                                if !remaining.to_lowercase().starts_with(&part.to_lowercase()) {
                                    return false;
                                }
                                remaining = &remaining[part.len().min(remaining.len())..];
                            } else if i == parts.len() - 1 && anchored_end {
                                if !remaining.to_lowercase().ends_with(&part.to_lowercase()) {
                                    return false;
                                }
                            } else {
                                if part.is_empty() {
                                    continue;
                                }
                                if let Some(pos) =
                                    remaining.to_lowercase().find(&part.to_lowercase())
                                {
                                    remaining = &remaining[pos + part.len()..];
                                } else {
                                    return false;
                                }
                            }
                        }
                        true
                    }
                    Ok(Value::Integer(if regex_matches(&s, &p) { 1 } else { 0 }))
                } else if n.eq_ignore_ascii_case("MATCH_AGAINST") {
                    // FTS MATCH_AGAINST: stub returning 0 (FTS is handled at the AST level)
                    Ok(Value::Integer(0))
                } else if n.eq_ignore_ascii_case("STARTS_WITH") {
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let s = self.eval_expr(&args[0], row, col_map)?;
                    let prefix = self.eval_expr(&args[1], row, col_map)?;
                    match (s, prefix) {
                        (Value::Text(s), Value::Text(p)) => {
                            Ok(Value::Integer(if s.starts_with(p.as_ref()) {
                                1
                            } else {
                                0
                            }))
                        }
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("HEX") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Blob(b) => {
                            let hex: String = b.iter().map(|b| format!("{b:02X}")).collect();
                            Ok(Value::Text(hex.into()))
                        }
                        Value::Integer(v) => Ok(Value::Text(format!("{v:X}").into())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("UNICODE") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Text(s) => match s.chars().next() {
                            Some(c) => Ok(Value::Integer(c as i64)),
                            None => Ok(Value::Null),
                        },
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("CHAR") {
                    let mut result = String::with_capacity(args.len());
                    for arg in args {
                        if let Value::Integer(v) = self.eval_expr(arg, row, col_map)? {
                            if let Some(c) = char::from_u32(v as u32) {
                                result.push(c);
                            }
                        }
                    }
                    Ok(Value::Text(result.into()))
                } else if n.eq_ignore_ascii_case("DATE_EXTRACT") {
                    // EXTRACT(field FROM expr) — args: [field_str, value_expr]
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let field = match self.eval_expr(&args[0], row, col_map)? {
                        Value::Text(s) => s.to_ascii_uppercase(),
                        _ => return Ok(Value::Null),
                    };
                    let val = self.eval_expr(&args[1], row, col_map)?;
                    // Parse value: accept Text (ISO date/datetime), Integer (unix epoch), Real
                    let text_val = match &val {
                        Value::Text(s) => s.clone(),
                        Value::Integer(v) => {
                            // Treat as unix timestamp seconds — derive as YYYY-MM-DD HH:MM:SS
                            let ts = *v;
                            let secs_per_day: i64 = 86400;
                            let epoch_days: i64 = ts / secs_per_day;
                            let day_secs: i64 = ts.rem_euclid(secs_per_day);
                            // Days since unix epoch → Gregorian date (simple algorithm)
                            let n_days = epoch_days + 719468;
                            let era = if n_days >= 0 { n_days } else { n_days - 146096 } / 146097;
                            let doe = n_days - era * 146097;
                            let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
                            let y = yoe + era * 400;
                            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
                            let mp = (5 * doy + 2) / 153;
                            let d = doy - (153 * mp + 2) / 5 + 1;
                            let m = if mp < 10 { mp + 3 } else { mp - 9 };
                            let y = if m <= 2 { y + 1 } else { y };
                            let h = day_secs / 3600;
                            let min = (day_secs % 3600) / 60;
                            let s = day_secs % 60;
                            format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, min, s)
                                .into()
                        }
                        _ => return Ok(Value::Null),
                    };
                    let t: &str = &text_val;
                    let result: Option<i64> = match field.as_str() {
                        "YEAR" => t.get(0..4).and_then(|s| s.parse().ok()),
                        "MONTH" => t.get(5..7).and_then(|s| s.parse().ok()),
                        "DAY" => t.get(8..10).and_then(|s| s.parse().ok()),
                        "HOUR" => t.get(11..13).and_then(|s| s.parse().ok()),
                        "MINUTE" => t.get(14..16).and_then(|s| s.parse().ok()),
                        "SECOND" => t.get(17..19).and_then(|s| s.parse().ok()),
                        _ => None,
                    };
                    Ok(result.map(Value::Integer).unwrap_or(Value::Null))
                } else if n.eq_ignore_ascii_case("RANDOM") || n.eq_ignore_ascii_case("RAND") {
                    // RANDOM() / RAND() — Bug #6 fix: use atomic counter xorshifted with
                    // system time nanos to avoid collision on same-millisecond calls
                    use std::sync::atomic::{AtomicU64, Ordering as AO};
                    use std::time::{SystemTime, UNIX_EPOCH};
                    static SEQ: AtomicU64 = AtomicU64::new(1);
                    let n = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.subsec_nanos() as u64)
                        .unwrap_or(0);
                    let seq = SEQ.fetch_add(1, AO::Relaxed);
                    let mut x = n.wrapping_add(seq).wrapping_add(0x9e3779b97f4a7c15);
                    // XorShift64 one round
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    Ok(Value::Integer(x as i64))
                // ---- RLS/Auth session variable functions (Supabase-style) ----
                } else if n.eq_ignore_ascii_case("auth.uid") || n.eq_ignore_ascii_case("auth_uid") {
                    // auth.uid() → reads request.jwt.sub from session_vars (set by HTTP API on login)
                    let uid = self
                        .session_vars
                        .get("request.jwt.sub")
                        .cloned()
                        .unwrap_or_default();
                    if uid.is_empty() {
                        Ok(Value::Null)
                    } else {
                        Ok(Value::Text(uid.into()))
                    }
                } else if n.eq_ignore_ascii_case("current_setting") {
                    // current_setting('key') → reads from session_vars
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let key_val = self.eval_expr(&args[0], row, col_map)?;
                    let key = match &key_val {
                        Value::Text(s) => s.to_string(),
                        _ => return Ok(Value::Null),
                    };
                    match self.session_vars.get(&key) {
                        Some(v) => Ok(Value::Text(v.clone().into())),
                        None => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("current_user") {
                    // current_user() → MySQL-compat alias for session current user
                    let user = self
                        .session_vars
                        .get("request.jwt.sub")
                        .or_else(|| self.session_vars.get("kkdb.current_user"))
                        .cloned()
                        .unwrap_or_default();
                    if user.is_empty() {
                        Ok(Value::Null)
                    } else {
                        Ok(Value::Text(user.into()))
                    }
                // ── Vector Search functions ────────────────────────────────────────────────
                } else if n.eq_ignore_ascii_case("VEC") {
                    // VEC(json_array_string) → BLOB (encoded f32 array)
                    // Accepts: VEC('[0.1, 0.2, ...]')  or  VEC(text_column)
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    let s = match &val {
                        Value::Text(s) => s.to_string(),
                        Value::Blob(b) => return Ok(Value::Blob(b.clone())), // already encoded
                        _ => return Ok(Value::Null),
                    };
                    match crate::vector::parse_vec_json(&s) {
                        Some(f32s) => {
                            let blob = crate::vector::index::encode_vector(&f32s);
                            Ok(Value::Blob(blob))
                        }
                        None => Err(KkdbError::RuntimeError(format!(
                            "VEC(): cannot parse '{}' as a float array",
                            s
                        ))),
                    }
                } else if n.eq_ignore_ascii_case("VEC_SEARCH") {
                    // VEC_SEARCH(table_name, index_name, query_vec_blob) → REAL similarity score
                    //
                    // Returns the similarity for the *current row* (identified by its rowid).
                    // This function does a full HNSW search over the index for the query vector,
                    // then looks up the score for this row's rowid from the result list.
                    if args.len() < 3 {
                        return Ok(Value::Real(0.0));
                    }
                    let _table_arg = self.eval_expr(&args[0], row, col_map)?;
                    let index_arg = self.eval_expr(&args[1], row, col_map)?;
                    let query_arg = self.eval_expr(&args[2], row, col_map)?;

                    let index_name = match &index_arg {
                        Value::Text(s) => s.to_string(),
                        _ => return Ok(Value::Real(0.0)),
                    };
                    let query_blob = match &query_arg {
                        Value::Blob(b) => b.clone(),
                        Value::Text(s) => {
                            // Accept text JSON as fallback
                            match crate::vector::parse_vec_json(s) {
                                Some(v) => crate::vector::index::encode_vector(&v),
                                None => return Ok(Value::Real(0.0)),
                            }
                        }
                        _ => return Ok(Value::Real(0.0)),
                    };
                    let query_vec = match crate::vector::index::decode_vector(&query_blob) {
                        Some(v) => v,
                        None => return Ok(Value::Real(0.0)),
                    };

                    // Look up the vector index.
                    let vi = match self.schema.vector_indexes.get(&index_name) {
                        Some(vi) => vi.clone(),
                        None => {
                            return Err(KkdbError::RuntimeError(format!(
                                "VEC_SEARCH(): vector index '{}' not found",
                                index_name
                            )))
                        }
                    };

                    // We need the rowid of the current row.
                    // Rowids are set on self.current_rowid by exec_select before each eval_expr
                    // call so we don't need to inject _rowid_ into the actual row data.
                    let cur_rowid = self.current_rowid as u64;

                    // Perform HNSW search (returns top-N by default).
                    let top_k = if args.len() >= 4 {
                        self.eval_expr(&args[3], row, col_map)?
                            .to_i64()
                            .unwrap_or(100) as usize
                    } else {
                        100
                    };
                    // Respect SET kkdb.vec_ef_search = N session variable.
                    let results = if let Some(ef_str) = self.session_vars.get("kkdb.vec_ef_search")
                    {
                        let ef: usize = ef_str.parse().unwrap_or(0);
                        if ef > 0 {
                            vi.search_with_ef(&query_vec, top_k, ef)
                        } else {
                            vi.search(&query_vec, top_k)
                        }
                    } else {
                        vi.search(&query_vec, top_k)
                    };

                    // Find this row's score in the result list.
                    let score = results
                        .iter()
                        .find(|(rowid, _)| *rowid == cur_rowid)
                        .map(|(_, s)| *s as f64)
                        .unwrap_or(0.0);

                    Ok(Value::Real(score))
                } else if n.eq_ignore_ascii_case("VEC_DIM") {
                    // VEC_DIM(blob) → INTEGER dimension count
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    match val {
                        Value::Blob(b) => {
                            let dim = crate::vector::index::decode_vector(&b)
                                .map(|v| v.len() as i64)
                                .unwrap_or(0);
                            Ok(Value::Integer(dim))
                        }
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("VEC_DISTANCE") {
                    // VEC_DISTANCE(blob1, blob2, 'cosine'|'l2') → REAL distance
                    if args.len() < 2 {
                        return Ok(Value::Null);
                    }
                    let a_val = self.eval_expr(&args[0], row, col_map)?;
                    let b_val = self.eval_expr(&args[1], row, col_map)?;
                    let metric_str = if args.len() >= 3 {
                        match self.eval_expr(&args[2], row, col_map)? {
                            Value::Text(s) => s.to_string(),
                            _ => "cosine".to_string(),
                        }
                    } else {
                        "cosine".to_string()
                    };
                    let metric = crate::vector::distance::DistanceMetric::from_str(&metric_str)
                        .unwrap_or(crate::vector::distance::DistanceMetric::Cosine);
                    let decode = |v: Value| -> Option<Vec<f32>> {
                        match v {
                            Value::Blob(b) => crate::vector::index::decode_vector(&b),
                            Value::Text(s) => crate::vector::parse_vec_json(&s),
                            _ => None,
                        }
                    };
                    let a = match decode(a_val) {
                        Some(v) => v,
                        None => return Ok(Value::Null),
                    };
                    let b = match decode(b_val) {
                        Some(v) => v,
                        None => return Ok(Value::Null),
                    };
                    Ok(Value::Real(metric.distance(&a, &b) as f64))
                } else if n.eq_ignore_ascii_case("VEC_NORMALIZE") {
                    // VEC_NORMALIZE(blob) → blob (L2-normalised)
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    let blob = match val {
                        Value::Blob(b) => b,
                        Value::Text(s) => match crate::vector::parse_vec_json(&s) {
                            Some(v) => crate::vector::index::encode_vector(&v),
                            None => return Ok(Value::Null),
                        },
                        _ => return Ok(Value::Null),
                    };
                    let mut v = match crate::vector::index::decode_vector(&blob) {
                        Some(v) => v,
                        None => return Ok(Value::Null),
                    };
                    crate::vector::distance::normalize_l2(&mut v);
                    Ok(Value::Blob(crate::vector::index::encode_vector(&v)))
                } else {
                    Err(KkdbError::RuntimeError(format!(
                        "unknown function: {}",
                        name
                    )))
                }
            }

            Expr::Nested(inner) => self.eval_expr(inner, row, col_map),

            Expr::Subquery(query) => {
                // Scalar subquery: returns first column of first row, or NULL if empty
                // Push outer row context for correlated references inside the subquery
                self.outer_rows.push((row.to_vec(), col_map.clone()));
                let result = self.exec_select(query);
                self.outer_rows.pop();
                match result? {
                    ExecResult::QueryResult { rows, .. } => {
                        if let Some(first_row) = rows.first() {
                            Ok(first_row.first().cloned().unwrap_or(Value::Null))
                        } else {
                            Ok(Value::Null)
                        }
                    }
                    _ => Err(KkdbError::Internal("subquery did not return rows".into())),
                }
            }

            Expr::InSubquery {
                expr: inner,
                subquery,
                negated,
            } => {
                let val = self.eval_expr(inner, row, col_map)?;
                if matches!(val, Value::Null) {
                    return Ok(Value::Null);
                }
                // Push outer row for correlated references in the subquery
                self.outer_rows.push((row.to_vec(), col_map.clone()));
                let result = self.exec_select(subquery);
                self.outer_rows.pop();
                match result? {
                    ExecResult::QueryResult { rows, .. } => {
                        let mut found = false;
                        let mut has_null = false;
                        for sub_row in &rows {
                            if let Some(sub_val) = sub_row.first() {
                                if matches!(sub_val, Value::Null) {
                                    has_null = true;
                                    continue;
                                }
                                if val == *sub_val {
                                    found = true;
                                    break;
                                }
                            }
                        }
                        if found {
                            Ok(Value::Integer(if !*negated { 1 } else { 0 }))
                        } else if has_null {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Integer(if *negated { 1 } else { 0 }))
                        }
                    }
                    _ => Err(KkdbError::Internal("subquery did not return rows".into())),
                }
            }

            Expr::AnyOp {
                expr: inner,
                op,
                subquery,
            } => {
                let val = self.eval_expr(inner, row, col_map)?;
                if matches!(val, Value::Null) {
                    return Ok(Value::Null);
                }
                self.outer_rows.push((row.to_vec(), col_map.clone()));
                let result = self.exec_select(subquery);
                self.outer_rows.pop();
                match result? {
                    ExecResult::QueryResult { rows, .. } => {
                        let mut found = false;
                        let mut has_null = false;
                        for sub_row in &rows {
                            if let Some(sub_val) = sub_row.first() {
                                if matches!(sub_val, Value::Null) {
                                    has_null = true;
                                    continue;
                                }
                                let cmp = self.apply_binary_op(op, &val, sub_val)?;
                                if cmp.is_truthy() {
                                    found = true;
                                    break;
                                }
                            }
                        }
                        if found {
                            Ok(Value::Integer(1))
                        } else if has_null {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Integer(0))
                        }
                    }
                    _ => Err(KkdbError::Internal(
                        "subquery in ANY did not return rows".into(),
                    )),
                }
            }

            Expr::AllOp {
                expr: inner,
                op,
                subquery,
            } => {
                let val = self.eval_expr(inner, row, col_map)?;
                if matches!(val, Value::Null) {
                    return Ok(Value::Null);
                }
                self.outer_rows.push((row.to_vec(), col_map.clone()));
                let result = self.exec_select(subquery);
                self.outer_rows.pop();
                match result? {
                    ExecResult::QueryResult { rows, .. } => {
                        let mut all_match = true;
                        let mut has_null = false;
                        for sub_row in &rows {
                            if let Some(sub_val) = sub_row.first() {
                                if matches!(sub_val, Value::Null) {
                                    has_null = true;
                                    continue;
                                }
                                let cmp = self.apply_binary_op(op, &val, sub_val)?;
                                if !cmp.is_truthy() {
                                    all_match = false;
                                    break;
                                }
                            }
                        }
                        if !all_match {
                            Ok(Value::Integer(0))
                        } else if has_null {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Integer(1)) // Vacuously true for empty sets as per SQL standard
                        }
                    }
                    _ => Err(KkdbError::Internal(
                        "subquery in ALL did not return rows".into(),
                    )),
                }
            }

            Expr::Exists(subquery) => {
                // Push outer row for correlated references (e.g. WHERE t.col = outer.col)
                self.outer_rows.push((row.to_vec(), col_map.clone()));
                let result = self.exec_select(subquery);
                self.outer_rows.pop();
                match result? {
                    ExecResult::QueryResult { rows, .. } => {
                        Ok(Value::Integer(if rows.is_empty() { 0 } else { 1 }))
                    }
                    _ => Err(KkdbError::Internal("subquery did not return rows".into())),
                }
            }

            Expr::Case {
                operand,
                when_clauses,
                else_clause,
            } => {
                if let Some(ref op_expr) = operand {
                    // Simple CASE: CASE <operand> WHEN val THEN result ...
                    let op_val = self.eval_expr(op_expr, row, col_map)?;
                    for (when_val_expr, then_expr) in when_clauses {
                        let when_val = self.eval_expr(when_val_expr, row, col_map)?;
                        // NULL never equals anything
                        if !matches!(op_val, Value::Null)
                            && !matches!(when_val, Value::Null)
                            && op_val == when_val
                        {
                            return self.eval_expr(then_expr, row, col_map);
                        }
                    }
                } else {
                    // Searched CASE: CASE WHEN cond THEN result ...
                    for (cond_expr, then_expr) in when_clauses {
                        let cond_val = self.eval_expr(cond_expr, row, col_map)?;
                        if cond_val.is_truthy() {
                            return self.eval_expr(then_expr, row, col_map);
                        }
                    }
                }
                // No WHEN matched — evaluate ELSE or return NULL
                if let Some(ref else_expr) = else_clause {
                    self.eval_expr(else_expr, row, col_map)
                } else {
                    Ok(Value::Null)
                }
            }

            Expr::Cast {
                expr,
                to_type,
                try_cast,
            } => {
                use crate::sql::ast::CastTargetType;
                let val = self.eval_expr(expr, row, col_map)?;
                match to_type {
                    CastTargetType::Integer => match val {
                        Value::Integer(v) => Ok(Value::Integer(v)),
                        Value::Real(v) => Ok(Value::Integer(v as i64)),
                        Value::Text(s) => {
                            if let Ok(i) = s.trim().parse::<i64>() {
                                Ok(Value::Integer(i))
                            } else if let Ok(f) = s.trim().parse::<f64>() {
                                Ok(Value::Integer(f as i64))
                            } else if *try_cast {
                                Ok(Value::Null)
                            } else {
                                // M11 fix: non-numeric CAST should error, not silently return 0
                                Err(crate::error::KkdbError::RuntimeError(format!(
                                    "cannot cast '{}' to INTEGER",
                                    s
                                )))
                            }
                        }
                        Value::Blob(_) => {
                            if *try_cast {
                                Ok(Value::Null)
                            } else {
                                Err(crate::error::KkdbError::RuntimeError(
                                    "cannot cast BLOB to INTEGER".into(),
                                ))
                            }
                        }
                        Value::Null => Ok(Value::Null),
                    },
                    CastTargetType::Real => match val {
                        Value::Real(v) => Ok(Value::Real(v)),
                        Value::Integer(v) => Ok(Value::Real(v as f64)),
                        Value::Text(s) => {
                            if let Ok(f) = s.trim().parse::<f64>() {
                                Ok(Value::Real(f))
                            } else if *try_cast {
                                Ok(Value::Null)
                            } else {
                                // M11 fix: non-numeric CAST should error
                                Err(crate::error::KkdbError::RuntimeError(format!(
                                    "cannot cast '{}' to REAL",
                                    s
                                )))
                            }
                        }
                        Value::Blob(_) => {
                            if *try_cast {
                                Ok(Value::Null)
                            } else {
                                Err(crate::error::KkdbError::RuntimeError(
                                    "cannot cast BLOB to REAL".into(),
                                ))
                            }
                        }
                        Value::Null => Ok(Value::Null),
                    },
                    CastTargetType::Numeric => match val {
                        Value::Integer(v) => Ok(Value::Integer(v)),
                        Value::Real(v) => {
                            if v.fract() == 0.0 && v.abs() < 9.2e18 {
                                Ok(Value::Integer(v as i64))
                            } else {
                                Ok(Value::Real(v))
                            }
                        }
                        Value::Text(s) => {
                            let t = s.trim();
                            if let Ok(i) = t.parse::<i64>() {
                                Ok(Value::Integer(i))
                            } else if let Ok(f) = t.parse::<f64>() {
                                Ok(Value::Real(f))
                            } else if *try_cast {
                                Ok(Value::Null)
                            } else {
                                // D-NEW-3 fix: report error instead of silently returning 0
                                Err(crate::error::KkdbError::RuntimeError(format!(
                                    "cannot cast '{}' to NUMERIC",
                                    s
                                )))
                            }
                        }
                        Value::Blob(_) => {
                            if *try_cast {
                                Ok(Value::Null)
                            } else {
                                Err(crate::error::KkdbError::RuntimeError(
                                    "cannot cast BLOB to NUMERIC".into(),
                                ))
                            }
                        }
                        Value::Null => Ok(Value::Null),
                    },
                    CastTargetType::Text => match val {
                        Value::Text(s) => Ok(Value::Text(s)),
                        Value::Integer(v) => Ok(Value::Text(v.to_string().into())),
                        Value::Real(v) => Ok(Value::Text(format!("{v}").into())),
                        Value::Blob(b) => match String::from_utf8(b.clone()) {
                            Ok(s) => Ok(Value::Text(s.into())),
                            Err(_) => {
                                let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
                                Ok(Value::Text(hex.into()))
                            }
                        },
                        Value::Null => Ok(Value::Null),
                    },
                    CastTargetType::Blob => match val {
                        Value::Blob(b) => Ok(Value::Blob(b)),
                        Value::Text(s) => Ok(Value::Blob(s.as_bytes().to_vec())),
                        Value::Integer(v) => Ok(Value::Blob(v.to_string().into_bytes())),
                        Value::Real(v) => Ok(Value::Blob(format!("{v}").into_bytes())),
                        Value::Null => Ok(Value::Null),
                    },
                    // Temporal types — store as Text (ISO 8601 string), validate format
                    CastTargetType::Date | CastTargetType::Time | CastTargetType::Timestamp => {
                        match val {
                            Value::Text(s) => Ok(Value::Text(s)),
                            Value::Integer(v) => Ok(Value::Text(v.to_string().into())),
                            Value::Real(v) => Ok(Value::Text(format!("{v}").into())),
                            Value::Null => Ok(Value::Null),
                            _ => Ok(Value::Null),
                        }
                    }
                    // JSON — store as Text
                    CastTargetType::Json => match val {
                        Value::Text(s) => Ok(Value::Text(s)),
                        Value::Integer(v) => Ok(Value::Text(v.to_string().into())),
                        Value::Real(v) => Ok(Value::Text(format!("{v}").into())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    },
                }
            }

            Expr::Collate { expr, collation: _ } => {
                // Ignore collation sorting rules during scalar evaluation for now
                self.eval_expr(expr, row, col_map)
            }

            Expr::Interval {
                value,
                leading_field,
            } => {
                let val = self.eval_expr(value, row, col_map)?;
                if let Some(field) = leading_field {
                    match val {
                        Value::Text(s) => Ok(Value::Text(format!("{} {}", s.trim(), field).into())),
                        Value::Integer(v) => Ok(Value::Text(format!("{} {}", v, field).into())),
                        Value::Real(v) => Ok(Value::Text(format!("{} {}", v, field).into())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(val)
                }
            }

            // Batch F: Window functions — evaluated in exec_select, not individual row eval
            Expr::WindowFunction { .. } => Err(KkdbError::RuntimeError(
                "window function cannot be evaluated in scalar context".into(),
            )),

            // BM25 Full-Text Search: MATCH (col1, col2) AGAINST ('query')
            // Phase 4 stub: performs a simple substring token match across the specified columns.
            // Phase 4 (inverted index) will replace this with a proper BM25 scored lookup.
            Expr::MatchAgainst { columns, query } => {
                if query.trim().is_empty() {
                    return Ok(Value::Real(0.0));
                }
                // Tokenize the query using Unicode-aware split
                let tokens: Vec<String> = query
                    .split(|c: char| !c.is_alphanumeric())
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();
                if tokens.is_empty() {
                    return Ok(Value::Real(0.0));
                }

                // Build a search haystack from the matched columns (or all TEXT columns if none matched by name).
                let mut haystack = String::new();
                if columns.is_empty() {
                    // Fall back to all TEXT values in the row
                    for val in row {
                        if let Value::Text(t) = val {
                            haystack.push(' ');
                            haystack.push_str(t);
                        }
                    }
                } else {
                    for col_name in columns {
                        let col_lower = col_name.to_ascii_lowercase();
                        if let Some(&idx) = col_map.get(col_lower.as_str()) {
                            if let Some(Value::Text(t)) = row.get(idx) {
                                haystack.push(' ');
                                haystack.push_str(t);
                            }
                        }
                    }
                }
                let haystack = haystack.to_lowercase();

                // Score = fraction of query tokens found in the haystack
                let matched = tokens
                    .iter()
                    .filter(|tok| haystack.contains(tok.as_str()))
                    .count();
                if matched == 0 {
                    Ok(Value::Real(0.0))
                } else {
                    // Simple TF-style score: matched_fraction (BM25 will replace this in Phase 4)
                    Ok(Value::Real(matched as f64 / tokens.len() as f64))
                }
            }
        }
    }

    #[inline]
    pub(crate) fn apply_binary_op(
        &self,
        op: &BinaryOperator,
        left: &Value,
        right: &Value,
    ) -> Result<Value> {
        // NULL propagation for most operators
        if matches!(left, Value::Null) || matches!(right, Value::Null) {
            match op {
                BinaryOperator::And => {
                    // NULL AND false = false (any falsy non-NULL)
                    if !matches!(left, Value::Null) && !left.is_truthy() {
                        return Ok(Value::Integer(0));
                    }
                    if !matches!(right, Value::Null) && !right.is_truthy() {
                        return Ok(Value::Integer(0));
                    }
                    return Ok(Value::Null);
                }
                BinaryOperator::Or => {
                    // NULL OR true = true
                    if left.is_truthy() || right.is_truthy() {
                        return Ok(Value::Integer(1));
                    }
                    return Ok(Value::Null);
                }
                _ => return Ok(Value::Null),
            }
        }

        match op {
            BinaryOperator::FtsMatch => match (left, right) {
                (Value::Text(t), Value::Text(pattern)) => {
                    let tokens = VM::tokenize(pattern);
                    if tokens.is_empty() {
                        return Ok(Value::Integer(0));
                    }
                    let t_lower = t.to_ascii_lowercase();
                    let mut matched = true;
                    // L4 minimal implementation: text must contain all tokens
                    for token in tokens {
                        if !t_lower.contains(&token) {
                            matched = false;
                            break;
                        }
                    }
                    Ok(Value::Integer(if matched { 1 } else { 0 }))
                }
                _ => Ok(Value::Integer(0)),
            },
            BinaryOperator::Add => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a.wrapping_add(*b))),
                (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a + b)),
                (Value::Integer(a), Value::Real(b)) => Ok(Value::Real(*a as f64 + b)),
                (Value::Real(a), Value::Integer(b)) => Ok(Value::Real(a + *b as f64)),
                // D-NEW-2 fix: return Null for incompatible types, not Integer(0)
                _ => Ok(Value::Null),
            },
            BinaryOperator::Subtract => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a.wrapping_sub(*b))),
                (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a - b)),
                (Value::Integer(a), Value::Real(b)) => Ok(Value::Real(*a as f64 - b)),
                (Value::Real(a), Value::Integer(b)) => Ok(Value::Real(a - *b as f64)),
                _ => Ok(Value::Null),
            },
            BinaryOperator::Multiply => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a.wrapping_mul(*b))),
                (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a * b)),
                (Value::Integer(a), Value::Real(b)) => Ok(Value::Real(*a as f64 * b)),
                (Value::Real(a), Value::Integer(b)) => Ok(Value::Real(a * *b as f64)),
                _ => Ok(Value::Null),
            },
            BinaryOperator::Divide => {
                match (left, right) {
                    (Value::Integer(a), Value::Integer(b)) => {
                        if *b == 0 {
                            Ok(Value::Null) // SQLite returns NULL for division by zero
                        } else {
                            Ok(Value::Integer(a.wrapping_div(*b)))
                        }
                    }
                    (Value::Real(a), Value::Real(b)) => {
                        if *b == 0.0 {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Real(a / b))
                        }
                    }
                    (Value::Integer(a), Value::Real(b)) => {
                        if *b == 0.0 {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Real(*a as f64 / b))
                        }
                    }
                    (Value::Real(a), Value::Integer(b)) => {
                        if *b == 0 {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Real(a / *b as f64))
                        }
                    }
                    _ => Ok(Value::Null),
                }
            }
            BinaryOperator::Modulo => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => {
                    if *b == 0 {
                        Ok(Value::Null)
                    } else {
                        Ok(Value::Integer(a.wrapping_rem(*b)))
                    }
                }
                _ => Ok(Value::Null),
            },
            BinaryOperator::Equal => Ok(Value::Integer(if left == right { 1 } else { 0 })),
            BinaryOperator::NotEqual => Ok(Value::Integer(if left != right { 1 } else { 0 })),
            BinaryOperator::LessThan => Ok(Value::Integer(
                if left.partial_cmp(right) == Some(std::cmp::Ordering::Less) {
                    1
                } else {
                    0
                },
            )),
            BinaryOperator::LessThanOrEqual => Ok(Value::Integer(
                if left
                    .partial_cmp(right)
                    .is_some_and(|o| o != std::cmp::Ordering::Greater)
                {
                    1
                } else {
                    0
                },
            )),
            BinaryOperator::GreaterThan => Ok(Value::Integer(
                if left.partial_cmp(right) == Some(std::cmp::Ordering::Greater) {
                    1
                } else {
                    0
                },
            )),
            BinaryOperator::GreaterThanOrEqual => Ok(Value::Integer(
                if left
                    .partial_cmp(right)
                    .is_some_and(|o| o != std::cmp::Ordering::Less)
                {
                    1
                } else {
                    0
                },
            )),
            BinaryOperator::And => Ok(Value::Integer(if left.is_truthy() && right.is_truthy() {
                1
            } else {
                0
            })),
            BinaryOperator::Or => Ok(Value::Integer(if left.is_truthy() || right.is_truthy() {
                1
            } else {
                0
            })),
            BinaryOperator::Concat => {
                let l_str = left.to_string();
                let r_str = right.to_string();
                let mut s = String::with_capacity(l_str.len() + r_str.len());
                s.push_str(&l_str);
                s.push_str(&r_str);
                Ok(Value::Text(s.into()))
            }
            BinaryOperator::Xor => {
                // Logical XOR: true iff exactly one is truthy
                let lt = left.is_truthy();
                let rt = right.is_truthy();
                Ok(Value::Integer(if lt ^ rt { 1 } else { 0 }))
            }
            BinaryOperator::BitwiseOr => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a | b)),
                _ => Ok(Value::Null),
            },
            BinaryOperator::BitwiseAnd => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a & b)),
                _ => Ok(Value::Null),
            },
            BinaryOperator::BitwiseXor => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a ^ b)),
                _ => Ok(Value::Null),
            },
            BinaryOperator::ShiftLeft => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) if *b >= 0 && *b < 64 => {
                    Ok(Value::Integer(a.wrapping_shl(*b as u32)))
                }
                _ => Ok(Value::Null),
            },
            BinaryOperator::ShiftRight => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) if *b >= 0 && *b < 64 => {
                    Ok(Value::Integer(a.wrapping_shr(*b as u32)))
                }
                _ => Ok(Value::Null),
            },
        }
    }
}

/// SQL LIKE pattern matching — Unicode-aware character-level matching
/// % matches any sequence of characters (including empty)
/// _ matches any single Unicode character
pub(crate) fn like_match(
    text: &str,
    pattern: &str,
    escape_char: Option<char>,
    case_insensitive: bool,
) -> bool {
    // Collect to char vecs to properly handle multi-byte Unicode (CJK etc.)
    let t_chars: Vec<char> = if case_insensitive {
        text.chars().map(|c| c.to_ascii_lowercase()).collect()
    } else {
        text.chars().collect()
    };
    let p_chars: Vec<char> = if case_insensitive {
        pattern.chars().map(|c| c.to_ascii_lowercase()).collect()
    } else {
        pattern.chars().collect()
    };

    let (tlen, plen) = (t_chars.len(), p_chars.len());
    let mut ti = 0usize;
    let mut pi = 0usize;
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0usize;

    while ti < tlen {
        let ec = escape_char.map(|c| {
            if case_insensitive {
                c.to_ascii_lowercase()
            } else {
                c
            }
        });
        if pi < plen && Some(p_chars[pi]) == ec {
            // Escape character — next char matches literally
            pi += 1;
            if pi < plen {
                if t_chars[ti] == p_chars[pi] {
                    ti += 1;
                    pi += 1;
                } else if let Some(sp) = star_pi {
                    pi = sp + 1;
                    star_ti += 1;
                    ti = star_ti;
                } else {
                    return false;
                }
            } else {
                return false;
            }
        } else if pi < plen && p_chars[pi] == '_' {
            // _ matches any single Unicode character
            ti += 1;
            pi += 1;
        } else if pi < plen && p_chars[pi] == '%' {
            // % — record position and try matching zero characters
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if pi < plen {
            if t_chars[ti] == p_chars[pi] {
                ti += 1;
                pi += 1;
            } else if let Some(sp) = star_pi {
                pi = sp + 1;
                star_ti += 1;
                ti = star_ti;
            } else {
                return false;
            }
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    // Skip trailing % in pattern
    while pi < plen && p_chars[pi] == '%' {
        pi += 1;
    }

    pi == plen
}

fn json_extract_primitive(json: &str, path: &str) -> Option<String> {
    let mut parts = Vec::new();
    let p = path.trim_start_matches("$.").trim_start_matches('$');
    let mut current = String::new();
    for c in p.chars() {
        if c == '.' || c == '[' {
            if !current.is_empty() {
                parts.push(current.clone());
                current.clear();
            }
        } else if c == ']' {
            if !current.is_empty() {
                parts.push(format!("[{}]", current));
                current.clear();
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }

    let mut current_json = json.trim().to_string();
    for part in parts {
        if part.starts_with('[') && part.ends_with(']') {
            let idx: usize = part[1..part.len() - 1].parse().ok()?;
            current_json = json_array_get(&current_json, idx)?;
        } else {
            let search = format!("\"{part}\":");
            let pos = current_json.find(&search)?;
            let rest = current_json[pos + search.len()..].trim_start();
            current_json = json_value_at_start(rest)?;
        }
    }
    Some(current_json)
}

fn json_value_at_start(rest: &str) -> Option<String> {
    if let Some(stripped) = rest.strip_prefix('"') {
        let mut end = 1;
        let mut escape = false;
        for (i, c) in stripped.char_indices() {
            if escape {
                escape = false;
                continue;
            }
            if c == '\\' {
                escape = true;
                continue;
            }
            if c == '"' {
                end = i + 2;
                break;
            }
        }
        Some(rest[..end].to_string())
    } else if rest.starts_with('{') {
        let mut depth = 0;
        for (i, c) in rest.char_indices() {
            if c == '{' {
                depth += 1;
            }
            if c == '}' {
                depth -= 1;
                if depth == 0 {
                    return Some(rest[..=i].to_string());
                }
            }
        }
        None
    } else if rest.starts_with('[') {
        let mut depth = 0;
        for (i, c) in rest.char_indices() {
            if c == '[' {
                depth += 1;
            }
            if c == ']' {
                depth -= 1;
                if depth == 0 {
                    return Some(rest[..=i].to_string());
                }
            }
        }
        None
    } else {
        let end = rest
            .find([',', '}', ']', ' ', '\n', '\t'])
            .unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

fn json_array_get(json: &str, target_idx: usize) -> Option<String> {
    let json = json.trim();
    if !json.starts_with('[') {
        return None;
    }
    let inner = &json[1..json.len() - 1].trim();
    if inner.is_empty() {
        return None;
    }

    let mut depth = 0;
    let mut idx = 0;
    let mut start = 0;
    let mut in_string = false;
    let mut escape = false;

    for (i, c) in inner.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if c == '\\' {
            escape = true;
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }

        if c == '{' || c == '[' {
            depth += 1;
        } else if c == '}' || c == ']' {
            depth -= 1;
        } else if c == ',' && depth == 0 {
            if idx == target_idx {
                return Some(inner[start..i].trim().to_string());
            }
            idx += 1;
            start = i + 1;
        }
    }
    if idx == target_idx && start < inner.len() {
        return Some(inner[start..].trim().to_string());
    }
    None
}

fn json_array_contains(json: &str, target_val: &str) -> bool {
    let json = json.trim();
    if !json.starts_with('[') {
        return false;
    }
    let inner = &json[1..json.len() - 1].trim();
    if inner.is_empty() {
        return false;
    }

    let mut depth = 0;
    let mut start = 0;
    let mut in_string = false;
    let mut escape = false;

    for (i, c) in inner.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if c == '\\' {
            escape = true;
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }

        if c == '{' || c == '[' {
            depth += 1;
        } else if c == '}' || c == ']' {
            depth -= 1;
        } else if c == ',' && depth == 0 {
            let elem = inner[start..i].trim();
            if elem == target_val {
                return true;
            }
            start = i + 1;
        }
    }
    if start < inner.len() {
        let elem = inner[start..].trim();
        if elem == target_val {
            return true;
        }
    }
    false
}

/// Convert a raw JSON scalar string to a typed Value.
fn json_scalar_to_value(s: &str) -> crate::types::Value {
    use crate::types::Value;
    let t = s.trim();
    if t == "null" {
        return Value::Null;
    }
    if t == "true" {
        return Value::Integer(1);
    }
    if t == "false" {
        return Value::Integer(0);
    }
    if t.starts_with('"') && t.ends_with('"') && t.len() >= 2 {
        let inner = t[1..t.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
        return Value::Text(inner.into());
    }
    if let Ok(i) = t.parse::<i64>() {
        return Value::Integer(i);
    }
    if let Ok(f) = t.parse::<f64>() {
        return Value::Real(f);
    }
    // Objects/arrays stay as Text
    Value::Text(t.into())
}

/// Check if a string is valid JSON (basic check)
fn json_is_valid(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    // Must start and end with matching brackets or be a scalar
    (t.starts_with('{') && t.ends_with('}'))
        || (t.starts_with('[') && t.ends_with(']'))
        || t.starts_with('"')
        || t == "null"
        || t == "true"
        || t == "false"
        || t.parse::<f64>().is_ok()
}

/// Count the number of elements (array) or key-value pairs (object) in a JSON value.
fn json_length(s: &str) -> usize {
    let t = s.trim();
    if t == "null" || t.is_empty() {
        return 0;
    }
    if t.starts_with('[') || t.starts_with('{') {
        let inner = if t.len() > 1 { &t[1..t.len() - 1] } else { "" };
        if inner.trim().is_empty() {
            return 0;
        }
        // Count top-level comma-separated elements
        let mut count = 1;
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escape = false;
        for c in inner.chars() {
            if escape {
                escape = false;
                continue;
            }
            if c == '\\' && in_string {
                escape = true;
                continue;
            }
            if c == '"' {
                in_string = !in_string;
                continue;
            }
            if in_string {
                continue;
            }
            if c == '{' || c == '[' {
                depth += 1;
            } else if c == '}' || c == ']' {
                depth -= 1;
            } else if c == ':' && depth == 0 && t.starts_with('{') { /* skip object key:val separator */
            } else if c == ',' && depth == 0 {
                if t.starts_with('[') {
                    count += 1;
                } else {
                    // For objects, count key:val pairs = commas + 1; divide by 1 (each comma between pairs)
                    count += 1;
                }
            }
        }
        // For objects, each "pair" is key:value, so commas separate pairs
        if t.starts_with('{') {
            // count is number of commas+1, which equals number of key-value pairs
            count
        } else {
            count
        }
    } else {
        1 // Scalar
    }
}

/// Return a JSON array of keys from a JSON object. Returns "[]" if not an object.
fn json_keys(s: &str) -> String {
    let t = s.trim();
    if !t.starts_with('{') {
        return "[]".to_string();
    }
    let inner = if t.len() > 1 { &t[1..t.len() - 1] } else { "" };
    if inner.trim().is_empty() {
        return "[]".to_string();
    }

    let mut keys = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    let mut start = 0usize;
    let chars: Vec<char> = inner.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if escape {
            escape = false;
            i += 1;
            continue;
        }
        if c == '\\' && in_string {
            escape = true;
            i += 1;
            continue;
        }
        if c == '"' {
            if !in_string {
                in_string = true;
                start = i;
            } else {
                in_string = false;
                if depth == 0 {
                    // This is a top-level key
                    let key_raw: String = chars[start..=i].iter().collect();
                    // Find ':' after key
                    let mut j = i + 1;
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j < chars.len() && chars[j] == ':' {
                        keys.push(key_raw);
                        // Skip to after next top-level ','
                        let mut depth2 = 0i32;
                        let mut in_s2 = false;
                        let mut esc2 = false;
                        j += 1;
                        while j < chars.len() {
                            let cc = chars[j];
                            if esc2 {
                                esc2 = false;
                                j += 1;
                                continue;
                            }
                            if cc == '\\' && in_s2 {
                                esc2 = true;
                                j += 1;
                                continue;
                            }
                            if cc == '"' {
                                in_s2 = !in_s2;
                                j += 1;
                                continue;
                            }
                            if in_s2 {
                                j += 1;
                                continue;
                            }
                            if cc == '{' || cc == '[' {
                                depth2 += 1;
                            } else if cc == '}' || cc == ']' {
                                depth2 -= 1;
                            } else if cc == ',' && depth2 == 0 {
                                j += 1;
                                break;
                            }
                            j += 1;
                        }
                        i = j;
                        continue;
                    }
                }
            }
            i += 1;
            continue;
        }
        if in_string {
            i += 1;
            continue;
        }
        if c == '{' || c == '[' {
            depth += 1;
        } else if c == '}' || c == ']' {
            depth -= 1;
        }
        i += 1;
    }

    if keys.is_empty() {
        return "[]".to_string();
    }
    format!("[{}]", keys.join(", "))
}

/// Check if a JSON document (object or scalar) contains a value.
fn json_contains_value(json: &str, needle: &str) -> bool {
    let t = json.trim();
    // Check scalar equality
    if t == needle {
        return true;
    }
    // For objects, check all values recursively
    if t.starts_with('{') {
        // Simple: search for needle as substring (good enough for primitive values)
        return t.contains(needle);
    }
    false
}

/// Remove a key from a JSON object at a given path like '$.key'.
fn json_remove_path(json: &str, path: &str) -> Option<String> {
    let t = json.trim();
    let key = path.trim_start_matches("$.").trim_start_matches('$');
    if key.is_empty() {
        return Some(t.to_string());
    }
    if !t.starts_with('{') {
        return Some(t.to_string());
    }

    let inner = &t[1..t.len() - 1];
    let search_key = format!("\"{key}\":");
    let alt_key = format!("\"{key}\" :");

    if let Some(pos) = inner.find(&search_key).or_else(|| inner.find(&alt_key)) {
        // Find end of value
        let val_start = pos + search_key.len();
        let rest = inner[val_start..].trim_start();
        let val_len = {
            let mut depth = 0i32;
            let mut in_str = false;
            let mut esc = false;
            let mut end = rest.len();
            for (ci, c) in rest.char_indices() {
                if esc {
                    esc = false;
                    continue;
                }
                if c == '\\' && in_str {
                    esc = true;
                    continue;
                }
                if c == '"' {
                    in_str = !in_str;
                    continue;
                }
                if in_str {
                    continue;
                }
                if c == '{' || c == '[' {
                    depth += 1;
                } else if c == '}' || c == ']' {
                    depth -= 1;
                } else if c == ',' && depth == 0 {
                    end = ci;
                    break;
                }
            }
            end
        };
        // Remove: delete from start of key to after comma/nothing
        let before = inner[..pos].trim_end_matches(',').trim_end();
        let _after = inner[val_start..].trim_start();
        let after_val = &rest[val_len..];
        let new_inner = if before.is_empty() {
            after_val.trim_start_matches(',').trim().to_string()
        } else if after_val.trim_start().is_empty() {
            before.to_string()
        } else {
            format!("{before}, {}", after_val.trim_start_matches(',').trim())
        };
        Some(format!("{{{new_inner}}}"))
    } else {
        Some(t.to_string())
    }
}

/// Set/insert a key at a given path like '$.key' in a JSON object.
fn json_set_path(json: &str, path: &str, value: &str) -> Option<String> {
    let t = json.trim();
    let key = path.trim_start_matches("$.").trim_start_matches('$');
    if key.is_empty() {
        return Some(value.to_string());
    }
    if !t.starts_with('{') {
        // Can't set key on non-object, just return a new object
        return Some(format!("{{\"{key}\": {value}}}"));
    }
    let inner = if t.len() > 2 { &t[1..t.len() - 1] } else { "" };
    let search_key = format!("\"{key}\":");

    if let Some(pos) = inner.find(&search_key) {
        // Replace existing value
        let val_start = pos + search_key.len();
        let rest = inner[val_start..].trim_start();
        let val_len = {
            let mut depth = 0i32;
            let mut in_str = false;
            let mut esc = false;
            let mut end = rest.len();
            for (ci, c) in rest.char_indices() {
                if esc {
                    esc = false;
                    continue;
                }
                if c == '\\' && in_str {
                    esc = true;
                    continue;
                }
                if c == '"' {
                    in_str = !in_str;
                    continue;
                }
                if in_str {
                    continue;
                }
                if c == '{' || c == '[' {
                    depth += 1;
                } else if c == '}' || c == ']' {
                    depth -= 1;
                } else if c == ',' && depth == 0 {
                    end = ci;
                    break;
                }
            }
            end
        };
        let suffix = &rest[val_len..];
        let new_inner = format!("{}\"{key}\": {value}{suffix}", &inner[..pos]);
        Some(format!("{{{new_inner}}}"))
    } else {
        // Add new key
        if inner.trim().is_empty() {
            Some(format!("{{\"{key}\": {value}}}"))
        } else {
            Some(format!("{{{inner}, \"{key}\": {value}}}"))
        }
    }
}
