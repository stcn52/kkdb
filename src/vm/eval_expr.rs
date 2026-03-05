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
                if column.bytes().any(|b| b.is_ascii_uppercase()) {
                    let lower = column.to_ascii_lowercase();
                    if let Some(&idx) = col_map.get(lower.as_str()) {
                        return if idx < row.len() {
                            Ok(row[idx].clone())
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
                negated,
            } => {
                let val = self.eval_expr(expr, row, col_map)?;
                let pat = self.eval_expr(pattern, row, col_map)?;
                if matches!(val, Value::Null) || matches!(pat, Value::Null) {
                    return Ok(Value::Null);
                }
                match (&val, &pat) {
                    (Value::Text(s), Value::Text(p)) => {
                        let matches = like_match(s, p);
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
                    .map_or(false, |o| o != std::cmp::Ordering::Less)
                    && val
                        .partial_cmp(&hi)
                        .map_or(false, |o| o != std::cmp::Ordering::Greater);
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
                        Value::Text(s) => Ok(Value::Integer(s.len() as i64)),
                        Value::Blob(b) => Ok(Value::Integer(b.len() as i64)),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Integer(format!("{}", val).len() as i64)),
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
                    Ok(Value::Text(s.replace(&*from, &*to).into()))
                } else if n.eq_ignore_ascii_case("TRIM") {
                    if args.is_empty() {
                        return Ok(Value::Null);
                    }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Text(s) => Ok(Value::Text(s.trim().into())),
                        v => Ok(v),
                    }
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
                let result = self.exec_select(query)?;
                match result {
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
                let result = self.exec_select(subquery)?;
                match result {
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

            Expr::Exists(subquery) => {
                let result = self.exec_select(subquery)?;
                match result {
                    ExecResult::QueryResult { rows, .. } => {
                        Ok(Value::Integer(if rows.is_empty() { 0 } else { 1 }))
                    }
                    _ => Err(KkdbError::Internal("subquery did not return rows".into())),
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
            BinaryOperator::Add => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a.wrapping_add(*b))),
                (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a + b)),
                (Value::Integer(a), Value::Real(b)) => Ok(Value::Real(*a as f64 + b)),
                (Value::Real(a), Value::Integer(b)) => Ok(Value::Real(a + *b as f64)),
                _ => Ok(Value::Integer(0)),
            },
            BinaryOperator::Subtract => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a.wrapping_sub(*b))),
                (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a - b)),
                (Value::Integer(a), Value::Real(b)) => Ok(Value::Real(*a as f64 - b)),
                (Value::Real(a), Value::Integer(b)) => Ok(Value::Real(a - *b as f64)),
                _ => Ok(Value::Integer(0)),
            },
            BinaryOperator::Multiply => match (left, right) {
                (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a.wrapping_mul(*b))),
                (Value::Real(a), Value::Real(b)) => Ok(Value::Real(a * b)),
                (Value::Integer(a), Value::Real(b)) => Ok(Value::Real(*a as f64 * b)),
                (Value::Real(a), Value::Integer(b)) => Ok(Value::Real(a * *b as f64)),
                _ => Ok(Value::Integer(0)),
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
                    .map_or(false, |o| o != std::cmp::Ordering::Greater)
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
                    .map_or(false, |o| o != std::cmp::Ordering::Less)
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
        }
    }
}

/// SQL LIKE pattern matching (iterative, O(n*m) worst case)
/// % matches any sequence of characters
/// _ matches any single character
pub(crate) fn like_match(text: &str, pattern: &str) -> bool {
    let tb = text.as_bytes();
    let pb = pattern.as_bytes();
    let (tlen, plen) = (tb.len(), pb.len());

    let mut ti = 0usize;
    let mut pi = 0usize;
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0usize;

    while ti < tlen {
        if pi < plen && pb[pi] == b'_' {
            // _ matches any single character
            ti += 1;
            pi += 1;
        } else if pi < plen && pb[pi] == b'%' {
            // % - record position and try matching zero chars
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if pi < plen && tb[ti].eq_ignore_ascii_case(&pb[pi]) {
            // Exact character match (case-insensitive)
            ti += 1;
            pi += 1;
        } else if let Some(sp) = star_pi {
            // Mismatch - backtrack to last % and consume one more char
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    // Skip trailing % in pattern
    while pi < plen && pb[pi] == b'%' {
        pi += 1;
    }

    pi == plen
}
