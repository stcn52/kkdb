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
                    if args.is_empty() { return Ok(Value::Null); }
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
                    if args.is_empty() { return Ok(Value::Null); }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    if args.len() > 1 {
                        match (val, self.eval_expr(&args[1], row, col_map)?) {
                            (Value::Text(s), Value::Text(chars)) => {
                                let chars_set: Vec<char> = chars.chars().collect();
                                Ok(Value::Text(s.trim_start_matches(chars_set.as_slice()).into()))
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
                    if args.is_empty() { return Ok(Value::Null); }
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
                    if args.len() < 2 { return Ok(Value::Null); }
                    let a = self.eval_expr(&args[0], row, col_map)?;
                    let b = self.eval_expr(&args[1], row, col_map)?;
                    if a == b { Ok(Value::Null) } else { Ok(a) }
                } else if n.eq_ignore_ascii_case("ROUND") {
                    if args.is_empty() { return Ok(Value::Null); }
                    let val = self.eval_expr(&args[0], row, col_map)?;
                    let digits = if args.len() > 1 {
                        match self.eval_expr(&args[1], row, col_map)? {
                            Value::Integer(v) => v.max(0) as u32,
                            _ => 0,
                        }
                    } else { 0 };
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
                    if args.is_empty() { return Ok(Value::Null); }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) => Ok(Value::Integer(v)),
                        Value::Real(v) => Ok(Value::Real(v.ceil())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("FLOOR") {
                    if args.is_empty() { return Ok(Value::Null); }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) => Ok(Value::Integer(v)),
                        Value::Real(v) => Ok(Value::Real(v.floor())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("INSTR") {
                    if args.len() < 2 { return Ok(Value::Null); }
                    let haystack = self.eval_expr(&args[0], row, col_map)?;
                    let needle = self.eval_expr(&args[1], row, col_map)?;
                    match (haystack, needle) {
                        (Value::Text(s), Value::Text(p)) => {
                            let pos = s.find(p.as_ref())
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
                    if args.len() < 3 { return Ok(Value::Null); }
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
                    if args.is_empty() { return Ok(Value::Null); }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) => Ok(Value::Integer(v.signum())),
                        Value::Real(v) => Ok(Value::Real(v.signum())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                // ---- R1: NULL-safe equality operators ----
                } else if n.eq_ignore_ascii_case("__IS_DISTINCT_FROM__") {
                    if args.len() < 2 { return Ok(Value::Null); }
                    let a = self.eval_expr(&args[0], row, col_map)?;
                    let b = self.eval_expr(&args[1], row, col_map)?;
                    Ok(Value::Integer(match (&a, &b) {
                        (Value::Null, Value::Null) => 0, // NULL IS DISTINCT FROM NULL = FALSE
                        (Value::Null, _) | (_, Value::Null) => 1,
                        _ => if a == b { 0 } else { 1 },
                    }))
                } else if n.eq_ignore_ascii_case("__IS_NOT_DISTINCT_FROM__") {
                    if args.len() < 2 { return Ok(Value::Null); }
                    let a = self.eval_expr(&args[0], row, col_map)?;
                    let b = self.eval_expr(&args[1], row, col_map)?;
                    Ok(Value::Integer(match (&a, &b) {
                        (Value::Null, Value::Null) => 1, // NULL IS NOT DISTINCT FROM NULL = TRUE
                        (Value::Null, _) | (_, Value::Null) => 0,
                        _ => if a == b { 1 } else { 0 },
                    }))
                // ---- R3: Bitwise / math functions ----
                } else if n.eq_ignore_ascii_case("BITWISE_NOT") {
                    if args.is_empty() { return Ok(Value::Null); }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) => Ok(Value::Integer(!v)),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("CBRT") {
                    if args.is_empty() { return Ok(Value::Null); }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) => Ok(Value::Real((v as f64).cbrt())),
                        Value::Real(v) => Ok(Value::Real(v.cbrt())),
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("FACTORIAL") {
                    if args.is_empty() { return Ok(Value::Null); }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Integer(v) if v >= 0 => {
                            let result: i64 = (1..=v.min(20)).product();
                            Ok(Value::Integer(result))
                        }
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("POWER") || n.eq_ignore_ascii_case("POW") {
                    // POWER(base, exp) — a ^ b
                    if args.len() < 2 { return Ok(Value::Null); }
                    let base = self.eval_expr(&args[0], row, col_map)?;
                    let exp = self.eval_expr(&args[1], row, col_map)?;
                    match (base, exp) {
                        (Value::Integer(a), Value::Integer(b)) if b >= 0 => {
                            Ok(Value::Integer(a.wrapping_pow(b as u32)))
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
                    return Ok(self.window_results.as_ref().map_or(Value::Null, |res| res[self.current_window_row_idx][idx].clone()));
                } else if n.eq_ignore_ascii_case("JSON_EXTRACT") || n.eq_ignore_ascii_case("JSON_EXTRACT_TEXT") {
                    // Simple JSON extraction: JSON_EXTRACT(json_str, '$.key') or JSON_EXTRACT(json_str, 'key')
                    if args.len() < 2 { return Ok(Value::Null); }
                    let json_val = self.eval_expr(&args[0], row, col_map)?;
                    let path_val = self.eval_expr(&args[1], row, col_map)?;
                    match (json_val, path_val) {
                        (Value::Text(s), Value::Text(p)) => {
                            if let Some(mut extracted) = json_extract_primitive(&s, &p) {
                                // If it's a quoted string, unquote it
                                if extracted.starts_with('"') && extracted.ends_with('"') && extracted.len() >= 2 {
                                    extracted = extracted[1..extracted.len()-1].to_string();
                                }
                                Ok(Value::Text(extracted.into()))
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
                    if args.len() < 2 { return Ok(Value::Null); }
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
                        let v = self.eval_expr(&args[i+1], row, col_map)?;
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
                } else if n.eq_ignore_ascii_case("REGEXP_LIKE") {
                    // Safe placeholder: always assume false unless full regex crate is imported
                    Ok(Value::Integer(0))
                } else if n.eq_ignore_ascii_case("MATCH_AGAINST") {
                    // Safe placeholder: always assume false
                    Ok(Value::Integer(0))
                } else if n.eq_ignore_ascii_case("STARTS_WITH") {
                    if args.len() < 2 { return Ok(Value::Null); }
                    let s = self.eval_expr(&args[0], row, col_map)?;
                    let prefix = self.eval_expr(&args[1], row, col_map)?;
                    match (s, prefix) {
                        (Value::Text(s), Value::Text(p)) => {
                            Ok(Value::Integer(if s.starts_with(p.as_ref()) { 1 } else { 0 }))
                        }
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("HEX") {
                    if args.is_empty() { return Ok(Value::Null); }
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
                    if args.is_empty() { return Ok(Value::Null); }
                    match self.eval_expr(&args[0], row, col_map)? {
                        Value::Text(s) => {
                            match s.chars().next() {
                                Some(c) => Ok(Value::Integer(c as i64)),
                                None => Ok(Value::Null),
                            }
                        }
                        Value::Null => Ok(Value::Null),
                        _ => Ok(Value::Null),
                    }
                } else if n.eq_ignore_ascii_case("CHAR") {
                    let mut result = String::with_capacity(args.len());
                    for arg in args {
                        match self.eval_expr(arg, row, col_map)? {
                            Value::Integer(v) => {
                                if let Some(c) = char::from_u32(v as u32) {
                                    result.push(c);
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(Value::Text(result.into()))
                } else if n.eq_ignore_ascii_case("DATE_EXTRACT") {
                    // EXTRACT(field FROM expr) — args: [field_str, value_expr]
                    if args.len() < 2 { return Ok(Value::Null); }
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
                            let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;
                            let y = yoe + era * 400;
                            let doy = doe - (365*yoe + yoe/4 - yoe/100);
                            let mp = (5*doy + 2)/153;
                            let d = doy - (153*mp+2)/5 + 1;
                            let m = if mp < 10 { mp + 3 } else { mp - 9 };
                            let y = if m <= 2 { y + 1 } else { y };
                            let h = day_secs / 3600;
                            let min = (day_secs % 3600) / 60;
                            let s = day_secs % 60;
                            format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, min, s).into()
                        }
                        _ => return Ok(Value::Null),
                    };
                    let t: &str = &text_val;
                    let result: Option<i64> = match field.as_str() {
                        "YEAR"   => t.get(0..4).and_then(|s| s.parse().ok()),
                        "MONTH"  => t.get(5..7).and_then(|s| s.parse().ok()),
                        "DAY"    => t.get(8..10).and_then(|s| s.parse().ok()),
                        "HOUR"   => t.get(11..13).and_then(|s| s.parse().ok()),
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
                    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                    Ok(Value::Integer(x as i64))
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

            Expr::AnyOp { expr: inner, op, subquery } => {
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
                    _ => Err(KkdbError::Internal("subquery in ANY did not return rows".into())),
                }
            }

            Expr::AllOp { expr: inner, op, subquery } => {
                let val = self.eval_expr(inner, row, col_map)?;
                if matches!(val, Value::Null) {
                    return Ok(Value::Null);
                }
                let result = self.exec_select(subquery)?;
                match result {
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
                    _ => Err(KkdbError::Internal("subquery in ALL did not return rows".into())),
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

            Expr::Cast { expr, to_type, try_cast } => {
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
                                Ok(Value::Integer(0))
                            }
                        }
                        Value::Blob(_) => if *try_cast { Ok(Value::Null) } else { Ok(Value::Integer(0)) },
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
                                Ok(Value::Real(0.0))
                            }
                        }
                        Value::Blob(_) => if *try_cast { Ok(Value::Null) } else { Ok(Value::Real(0.0)) },
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
                                Ok(Value::Integer(0))
                            }
                        }
                        Value::Blob(_) => if *try_cast { Ok(Value::Null) } else { Ok(Value::Integer(0)) },
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

            Expr::Interval { value, leading_field } => {
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
            Expr::WindowFunction { .. } => {
                Err(KkdbError::RuntimeError(
                    "window function cannot be evaluated in scalar context".into(),
                ))
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

/// SQL LIKE pattern matching (iterative, O(n*m) worst case)
/// % matches any sequence of characters
/// _ matches any single character
pub(crate) fn like_match(text: &str, pattern: &str, escape_char: Option<char>, case_insensitive: bool) -> bool {
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
        } else if pi < plen && escape_char.is_some() && pb[pi] == escape_char.unwrap() as u8 {
            // Escape character logic
            pi += 1;
            if pi < plen {
                let match_char = if case_insensitive {
                    tb[ti].eq_ignore_ascii_case(&pb[pi])
                } else {
                    tb[ti] == pb[pi]
                };
                if match_char {
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
        } else if pi < plen {
            let match_char = if case_insensitive {
                tb[ti].eq_ignore_ascii_case(&pb[pi])
            } else {
                tb[ti] == pb[pi]
            };
            if match_char {
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
        } else if let Some(sp) = star_pi {
            // Mismatch with pattern exhausted - backtrack to last %
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

fn json_extract_primitive(json: &str, path: &str) -> Option<String> {
    let mut parts = Vec::new();
    let p = path.trim_start_matches("$.").trim_start_matches('$');
    let mut current = String::new();
    for c in p.chars() {
        if c == '.' {
            if !current.is_empty() { parts.push(current.clone()); current.clear(); }
        } else if c == '[' {
            if !current.is_empty() { parts.push(current.clone()); current.clear(); }
        } else if c == ']' {
            if !current.is_empty() { parts.push(format!("[{}]", current)); current.clear(); }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() { parts.push(current); }

    let mut current_json = json.trim().to_string();
    for part in parts {
        if part.starts_with('[') && part.ends_with(']') {
            let idx: usize = part[1..part.len()-1].parse().ok()?;
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
    if rest.starts_with('"') {
        let mut end = 1;
        let mut escape = false;
        for (i, c) in rest[1..].char_indices() {
            if escape { escape = false; continue; }
            if c == '\\' { escape = true; continue; }
            if c == '"' { end = i + 2; break; }
        }
        Some(rest[..end].to_string())
    } else if rest.starts_with('{') {
        let mut depth = 0;
        for (i, c) in rest.char_indices() {
            if c == '{' { depth += 1; }
            if c == '}' { depth -= 1; if depth == 0 { return Some(rest[..=i].to_string()); } }
        }
        None
    } else if rest.starts_with('[') {
        let mut depth = 0;
        for (i, c) in rest.char_indices() {
            if c == '[' { depth += 1; }
            if c == ']' { depth -= 1; if depth == 0 { return Some(rest[..=i].to_string()); } }
        }
        None
    } else {
        let end = rest.find(|c| matches!(c, ',' | '}' | ']' | ' ' | '\n' | '\t')).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

fn json_array_get(json: &str, target_idx: usize) -> Option<String> {
    let json = json.trim();
    if !json.starts_with('[') { return None; }
    let inner = &json[1..json.len() - 1].trim();
    if inner.is_empty() { return None; }
    
    let mut depth = 0;
    let mut idx = 0;
    let mut start = 0;
    let mut in_string = false;
    let mut escape = false;

    for (i, c) in inner.char_indices() {
        if escape { escape = false; continue; }
        if c == '\\' { escape = true; continue; }
        if c == '"' { in_string = !in_string; continue; }
        if in_string { continue; }

        if c == '{' || c == '[' { depth += 1; }
        else if c == '}' || c == ']' { depth -= 1; }
        else if c == ',' && depth == 0 {
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
    if !json.starts_with('[') { return false; }
    let inner = &json[1..json.len() - 1].trim();
    if inner.is_empty() { return false; }

    let mut depth = 0;
    let mut start = 0;
    let mut in_string = false;
    let mut escape = false;

    for (i, c) in inner.char_indices() {
        if escape { escape = false; continue; }
        if c == '\\' { escape = true; continue; }
        if c == '"' { in_string = !in_string; continue; }
        if in_string { continue; }

        if c == '{' || c == '[' { depth += 1; }
        else if c == '}' || c == ']' { depth -= 1; }
        else if c == ',' && depth == 0 {
            let elem = inner[start..i].trim();
            if elem == target_val { return true; }
            start = i + 1;
        }
    }
    if start < inner.len() {
        let elem = inner[start..].trim();
        if elem == target_val { return true; }
    }
    false
}
