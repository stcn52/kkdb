use crate::error::Result;
use crate::sql::ast as kk;
use sqlparser::ast as sa;

use super::common::{parse_blob_hex, parse_number_literal, unsupported};
use super::query::convert_query_to_select;

pub(crate) fn convert_expr(expr: sa::Expr) -> Result<kk::Expr> {
    match expr {
        sa::Expr::Identifier(id) => Ok(kk::Expr::ColumnRef {
            table: None,
            column: id.value,
        }),
        sa::Expr::CompoundIdentifier(ids) => convert_compound_identifier(ids),
        sa::Expr::Value(value) => convert_value(value.value),
        sa::Expr::UnaryOp { op, expr } => {
            // Handle operators that can't map to kk::UnaryOperator directly
            match &op {
                // Plus is a no-op: +x = x
                sa::UnaryOperator::Plus => return convert_expr(*expr),
                // PG bitwise NOT ~x → BITWISE_NOT(x)
                sa::UnaryOperator::BitwiseNot => {
                    return Ok(kk::Expr::Function {
                        name: "BITWISE_NOT".to_string(),
                        args: vec![convert_expr(*expr)?],
                        distinct: false,
                    })
                }
                // PG absolute value @x → ABS(x)
                sa::UnaryOperator::PGAbs => {
                    return Ok(kk::Expr::Function {
                        name: "ABS".to_string(),
                        args: vec![convert_expr(*expr)?],
                        distinct: false,
                    })
                }
                // PG square root |/x → SQRT(x)
                sa::UnaryOperator::PGSquareRoot => {
                    return Ok(kk::Expr::Function {
                        name: "SQRT".to_string(),
                        args: vec![convert_expr(*expr)?],
                        distinct: false,
                    })
                }
                // PG cube root ||/x → CBRT(x)
                sa::UnaryOperator::PGCubeRoot => {
                    return Ok(kk::Expr::Function {
                        name: "CBRT".to_string(),
                        args: vec![convert_expr(*expr)?],
                        distinct: false,
                    })
                }
                // PG factorial: !!x or x! → FACTORIAL(x)
                sa::UnaryOperator::PGPrefixFactorial | sa::UnaryOperator::PGPostfixFactorial => {
                    return Ok(kk::Expr::Function {
                        name: "FACTORIAL".to_string(),
                        args: vec![convert_expr(*expr)?],
                        distinct: false,
                    })
                }
                _ => {}
            }
            let op = convert_unary_operator(op)?;
            Ok(kk::Expr::UnaryOp {
                op,
                expr: Box::new(convert_expr(*expr)?),
            })
        }
        sa::Expr::BinaryOp { left, op, right } => {
            // Operators that cannot map to kk::BinaryOperator — rewrite as function calls
            match &op {
                // <=> (MySQL NULL-safe equal) → __IS_NOT_DISTINCT_FROM__(l, r)
                sa::BinaryOperator::Spaceship => {
                    return Ok(kk::Expr::Function {
                        name: "__IS_NOT_DISTINCT_FROM__".to_string(),
                        args: vec![convert_expr(*left)?, convert_expr(*right)?],
                        distinct: false,
                    });
                }
                // a ^ b (PG exponentiation) → POWER(a, b)
                sa::BinaryOperator::PGExp => {
                    return Ok(kk::Expr::Function {
                        name: "POWER".to_string(),
                        args: vec![convert_expr(*left)?, convert_expr(*right)?],
                        distinct: false,
                    });
                }
                // -> or ->> (JSON field access) → JSON_EXTRACT(l, r)
                sa::BinaryOperator::Arrow | sa::BinaryOperator::LongArrow => {
                    return Ok(kk::Expr::Function {
                        name: "JSON_EXTRACT".to_string(),
                        args: vec![convert_expr(*left)?, convert_expr(*right)?],
                        distinct: false,
                    });
                }
                // ^@ (PG starts-with text operator) → STARTS_WITH(l, r)
                sa::BinaryOperator::PGStartsWith => {
                    return Ok(kk::Expr::Function {
                        name: "STARTS_WITH".to_string(),
                        args: vec![convert_expr(*left)?, convert_expr(*right)?],
                        distinct: false,
                    });
                }
                // Regex operators: ~ and ~* → REGEXP_LIKE(l, r)
                sa::BinaryOperator::PGRegexMatch | sa::BinaryOperator::PGRegexIMatch => {
                    return Ok(kk::Expr::Function {
                        name: "REGEXP_LIKE".to_string(),
                        args: vec![convert_expr(*left)?, convert_expr(*right)?],
                        distinct: false,
                    });
                }
                // Regex not-match operators: !~ and !~* → NOT REGEXP_LIKE(l, r)
                sa::BinaryOperator::PGRegexNotMatch | sa::BinaryOperator::PGRegexNotIMatch => {
                    return Ok(kk::Expr::Function {
                        name: "NOT".to_string(),
                        args: vec![kk::Expr::Function {
                            name: "REGEXP_LIKE".to_string(),
                            args: vec![convert_expr(*left)?, convert_expr(*right)?],
                            distinct: false,
                        }],
                        distinct: false,
                    });
                }
                _ => {}
            }
            Ok(kk::Expr::BinaryOp {
                left: Box::new(convert_expr(*left)?),
                op: convert_binary_operator(op)?,
                right: Box::new(convert_expr(*right)?),
            })
        }
        sa::Expr::IsNull(expr) => Ok(kk::Expr::IsNull {
            expr: Box::new(convert_expr(*expr)?),
            negated: false,
        }),
        sa::Expr::IsNotNull(expr) => Ok(kk::Expr::IsNull {
            expr: Box::new(convert_expr(*expr)?),
            negated: true,
        }),
        // IS TRUE / IS FALSE → compare with 1 / 0
        sa::Expr::IsTrue(expr) => Ok(kk::Expr::BinaryOp {
            left: Box::new(convert_expr(*expr)?),
            op: kk::BinaryOperator::Equal,
            right: Box::new(kk::Expr::IntegerLiteral(1)),
        }),
        sa::Expr::IsNotTrue(expr) => Ok(kk::Expr::BinaryOp {
            left: Box::new(convert_expr(*expr)?),
            op: kk::BinaryOperator::NotEqual,
            right: Box::new(kk::Expr::IntegerLiteral(1)),
        }),
        sa::Expr::IsFalse(expr) => Ok(kk::Expr::BinaryOp {
            left: Box::new(convert_expr(*expr)?),
            op: kk::BinaryOperator::Equal,
            right: Box::new(kk::Expr::IntegerLiteral(0)),
        }),
        sa::Expr::IsNotFalse(expr) => Ok(kk::Expr::BinaryOp {
            left: Box::new(convert_expr(*expr)?),
            op: kk::BinaryOperator::NotEqual,
            right: Box::new(kk::Expr::IntegerLiteral(0)),
        }),
        // IS UNKNOWN / IS NOT UNKNOWN → IS NULL / IS NOT NULL
        sa::Expr::IsUnknown(expr) => Ok(kk::Expr::IsNull {
            expr: Box::new(convert_expr(*expr)?),
            negated: false,
        }),
        sa::Expr::IsNotUnknown(expr) => Ok(kk::Expr::IsNull {
            expr: Box::new(convert_expr(*expr)?),
            negated: true,
        }),
        sa::Expr::InList {
            expr,
            list,
            negated,
        } => {
            let mut out = Vec::with_capacity(list.len());
            for item in list {
                out.push(convert_expr(item)?);
            }
            Ok(kk::Expr::InList {
                expr: Box::new(convert_expr(*expr)?),
                list: out,
                negated,
            })
        }
        sa::Expr::InSubquery {
            expr,
            subquery,
            negated,
        } => Ok(kk::Expr::InSubquery {
            expr: Box::new(convert_expr(*expr)?),
            subquery: Box::new(convert_query_to_select(*subquery)?),
            negated,
        }),
        sa::Expr::Like {
            expr,
            pattern,
            escape_char,
            negated,
            ..
        } => {
            let escape = match escape_char {
                Some(sa::Value::SingleQuotedString(s)) if !s.is_empty() => {
                    Some(s.chars().next().unwrap())
                }
                Some(sa::Value::DoubleQuotedString(s)) if !s.is_empty() => {
                    Some(s.chars().next().unwrap())
                }
                _ => None,
            };
            Ok(kk::Expr::Like {
                expr: Box::new(convert_expr(*expr)?),
                pattern: Box::new(convert_expr(*pattern)?),
                escape_char: escape,
                case_insensitive: false,
                negated,
            })
        }
        sa::Expr::ILike {
            expr,
            pattern,
            escape_char,
            negated,
            ..
        } => {
            let escape = match escape_char {
                Some(sa::Value::SingleQuotedString(s)) if !s.is_empty() => {
                    Some(s.chars().next().unwrap())
                }
                Some(sa::Value::DoubleQuotedString(s)) if !s.is_empty() => {
                    Some(s.chars().next().unwrap())
                }
                _ => None,
            };
            Ok(kk::Expr::Like {
                expr: Box::new(convert_expr(*expr)?),
                pattern: Box::new(convert_expr(*pattern)?),
                escape_char: escape,
                case_insensitive: true,
                negated,
            })
        }
        sa::Expr::SimilarTo {
            expr,
            pattern,
            negated,
            ..
        }
        | sa::Expr::RLike {
            expr,
            pattern,
            negated,
            ..
        } => {
            let func = kk::Expr::Function {
                name: "REGEXP_LIKE".to_string(),
                args: vec![convert_expr(*expr)?, convert_expr(*pattern)?],
                distinct: false,
            };
            if negated {
                Ok(kk::Expr::Function {
                    name: "NOT".to_string(),
                    args: vec![func],
                    distinct: false,
                })
            } else {
                Ok(func)
            }
        }
        sa::Expr::Between {
            expr,
            low,
            high,
            negated,
        } => Ok(kk::Expr::Between {
            expr: Box::new(convert_expr(*expr)?),
            low: Box::new(convert_expr(*low)?),
            high: Box::new(convert_expr(*high)?),
            negated,
        }),
        sa::Expr::Substring {
            expr,
            substring_from,
            substring_for,
            shorthand,
            ..
        } => {
            let mut args = Vec::with_capacity(3);
            args.push(convert_expr(*expr)?);
            if let Some(from) = substring_from {
                args.push(convert_expr(*from)?);
            }
            if let Some(for_len) = substring_for {
                args.push(convert_expr(*for_len)?);
            }
            Ok(kk::Expr::Function {
                name: if shorthand {
                    "SUBSTR".to_string()
                } else {
                    "SUBSTRING".to_string()
                },
                args,
                distinct: false,
            })
        }
        // TRIM / LTRIM / RTRIM — unified handler (supports LEADING 'x' FROM s)
        sa::Expr::Trim {
            expr,
            trim_where,
            trim_what,
            trim_characters: _,
        } => {
            let func_name = match trim_where {
                Some(sa::TrimWhereField::Leading) => "LTRIM",
                Some(sa::TrimWhereField::Trailing) => "RTRIM",
                _ => "TRIM",
            };
            let mut args = vec![convert_expr(*expr)?];
            if let Some(what) = trim_what {
                args.push(convert_expr(*what)?);
            }
            Ok(kk::Expr::Function {
                name: func_name.to_string(),
                args,
                distinct: false,
            })
        }
        sa::Expr::Function(function) => convert_function(function),
        sa::Expr::Subquery(query) => Ok(kk::Expr::Subquery(Box::new(convert_query_to_select(
            *query,
        )?))),
        sa::Expr::Exists { subquery, negated } => {
            let exists = kk::Expr::Exists(Box::new(convert_query_to_select(*subquery)?));
            if negated {
                Ok(kk::Expr::UnaryOp {
                    op: kk::UnaryOperator::Not,
                    expr: Box::new(exists),
                })
            } else {
                Ok(exists)
            }
        }
        sa::Expr::Nested(expr) => Ok(kk::Expr::Nested(Box::new(convert_expr(*expr)?))),
        sa::Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let operand = operand
                .map(|e| convert_expr(*e).map(Box::new))
                .transpose()?;
            let when_clauses = conditions
                .into_iter()
                .map(|cw| Ok((convert_expr(cw.condition)?, convert_expr(cw.result)?)))
                .collect::<Result<Vec<_>>>()?;
            let else_clause = else_result
                .map(|e| convert_expr(*e).map(Box::new))
                .transpose()?;
            Ok(kk::Expr::Case {
                operand,
                when_clauses,
                else_clause,
            })
        }
        // Cast: CAST(expr AS type), TRY_CAST, SAFE_CAST, x::type (DoubleColon)
        sa::Expr::Cast {
            kind,
            expr,
            data_type,
            ..
        } => {
            let try_cast = matches!(kind, sa::CastKind::TryCast | sa::CastKind::SafeCast);
            Ok(kk::Expr::Cast {
                expr: Box::new(convert_expr(*expr)?),
                to_type: convert_cast_type(data_type),
                try_cast,
            })
        }
        // POSITION(needle IN haystack) → INSTR(haystack, needle) — note reversed args
        sa::Expr::Position { expr, r#in } => Ok(kk::Expr::Function {
            name: "INSTR".to_string(),
            args: vec![convert_expr(*r#in)?, convert_expr(*expr)?],
            distinct: false,
        }),
        // EXTRACT(field FROM expr) → DATE_EXTRACT(field_str, expr)
        sa::Expr::Extract {
            field,
            expr,
            syntax: _,
        } => {
            let field_str = kk::Expr::StringLiteral(format!("{field}"));
            Ok(kk::Expr::Function {
                name: "DATE_EXTRACT".to_string(),
                args: vec![field_str, convert_expr(*expr)?],
                distinct: false,
            })
        }
        // CEIL / FLOOR — sqlparser-rs 0.61+ native AST nodes
        sa::Expr::Ceil { expr, .. } => Ok(kk::Expr::Function {
            name: "CEIL".to_string(),
            args: vec![convert_expr(*expr)?],
            distinct: false,
        }),
        sa::Expr::Floor { expr, .. } => Ok(kk::Expr::Function {
            name: "FLOOR".to_string(),
            args: vec![convert_expr(*expr)?],
            distinct: false,
        }),

        // ── Batch A new arms ──────────────────────────────────────────────────

        // A1. COLLATE: capture internal expression and collation properties natively
        sa::Expr::Collate { expr, collation } => Ok(kk::Expr::Collate {
            expr: Box::new(convert_expr(*expr)?),
            collation: crate::sql::sqlparser_adapter::common::object_name_to_string(&collation),
        }),

        // A2. CONVERT(x, type) → Cast
        sa::Expr::Convert {
            expr,
            data_type: Some(dt),
            ..
        } => Ok(kk::Expr::Cast {
            expr: Box::new(convert_expr(*expr)?),
            to_type: convert_cast_type(dt),
            try_cast: false,
        }),
        sa::Expr::Convert { expr, .. } => convert_expr(*expr),

        // A3. OVERLAY(s PLACING repl FROM pos [FOR len]) → OVERLAY function
        sa::Expr::Overlay {
            expr,
            overlay_what,
            overlay_from,
            overlay_for,
        } => {
            let mut args = vec![
                convert_expr(*expr)?,
                convert_expr(*overlay_what)?,
                convert_expr(*overlay_from)?,
            ];
            if let Some(len) = overlay_for {
                args.push(convert_expr(*len)?);
            }
            Ok(kk::Expr::Function {
                name: "OVERLAY".to_string(),
                args,
                distinct: false,
            })
        }

        // A4. INTERVAL 'n' unit → Native Interval construction
        sa::Expr::Interval(sa::Interval {
            value,
            leading_field,
            ..
        }) => {
            let field_str = leading_field.as_ref().map(|f| f.to_string());
            Ok(kk::Expr::Interval {
                value: Box::new(convert_expr(*value)?),
                leading_field: field_str,
            })
        }

        // A5. TIMESTAMP AT TIME ZONE 'tz' → strip time zone, return inner expression
        sa::Expr::AtTimeZone { timestamp, .. } => convert_expr(*timestamp),

        // IS DISTINCT FROM / IS NOT DISTINCT FROM → NULL-safe equality via CASE WHEN
        sa::Expr::IsDistinctFrom(left, right) => {
            let l = convert_expr(*left)?;
            let r = convert_expr(*right)?;
            // IS DISTINCT FROM: CASE WHEN l IS NULL AND r IS NULL THEN 0
            //                        WHEN l IS NULL OR r IS NULL THEN 1
            //                        WHEN l = r THEN 0 ELSE 1 END
            // Simplified: NOT (l IS NOT DISTINCT FROM r)
            // Use: (l IS NULL AND r IS NOT NULL) OR (l IS NOT NULL AND r IS NULL) OR (l <> r)
            Ok(kk::Expr::Function {
                name: "__IS_DISTINCT_FROM__".to_string(),
                args: vec![l, r],
                distinct: false,
            })
        }
        sa::Expr::IsNotDistinctFrom(left, right) => {
            let l = convert_expr(*left)?;
            let r = convert_expr(*right)?;
            Ok(kk::Expr::Function {
                name: "__IS_NOT_DISTINCT_FROM__".to_string(),
                args: vec![l, r],
                distinct: false,
            })
        }

        // CompoundFieldAccess: a.b.c chain → take outermost two segments as table.column
        sa::Expr::CompoundFieldAccess { root, access_chain } => {
            // Collect the chain into a flat identifier list
            let root_str = match *root {
                sa::Expr::Identifier(id) => id.value,
                sa::Expr::CompoundIdentifier(ids) => ids
                    .into_iter()
                    .map(|i| i.value)
                    .collect::<Vec<_>>()
                    .join("."),
                other => return Err(unsupported(format!("complex field access root `{other}`"))),
            };
            let last = access_chain.last().map(|a| format!("{a}"));
            match last {
                Some(col) => Ok(kk::Expr::ColumnRef {
                    table: Some(root_str),
                    column: col,
                }),
                None => Ok(kk::Expr::ColumnRef {
                    table: None,
                    column: root_str,
                }),
            }
        }

        // TypedString: TIMESTAMP '...', DATE '...', etc. → strip type, keep string value
        // TypedString is a tuple-like variant wrapping a TypedString struct
        sa::Expr::TypedString(ts) => {
            if let Some(s) = ts.value.value.into_string() {
                Ok(kk::Expr::StringLiteral(s))
            } else {
                Ok(kk::Expr::Null)
            }
        }

        // Named expression: SOME_EXPR AS name (dialect-specific) → pass through inner
        sa::Expr::Named { expr, .. } => convert_expr(*expr),

        // IsNormalized → always 1 (assume already normalized)
        sa::Expr::IsNormalized { .. } => Ok(kk::Expr::IntegerLiteral(1)),

        // Prefixed: N'text', E'text' → strip prefix marker, treat as string
        sa::Expr::Prefixed { value, .. } => convert_expr(*value),

        // AnyOp: x = ANY(subq) → InSubquery when op is Eq; others unsupported
        sa::Expr::AnyOp {
            left,
            compare_op,
            right,
            ..
        } => {
            let subquery = match *right {
                sa::Expr::Subquery(q) => convert_query_to_select(*q)?,
                other => {
                    return Err(unsupported(format!(
                        "ANY with non-subquery operand `{other}`"
                    )))
                }
            };
            if compare_op == sa::BinaryOperator::Eq {
                Ok(kk::Expr::InSubquery {
                    expr: Box::new(convert_expr(*left)?),
                    subquery: Box::new(subquery),
                    negated: false,
                })
            } else {
                Ok(kk::Expr::AnyOp {
                    expr: Box::new(convert_expr(*left)?),
                    op: convert_binary_operator(compare_op)?,
                    subquery: Box::new(subquery),
                })
            }
        }

        // Array: ARRAY[1,2,3] → function JSON_ARRAY(1,2,3)
        sa::Expr::Array(sa::Array { elem, .. }) => {
            let mut args = Vec::with_capacity(elem.len());
            for e in elem {
                args.push(convert_expr(e)?);
            }
            Ok(kk::Expr::Function {
                name: "JSON_ARRAY".to_string(),
                args,
                distinct: false,
            })
        }
        // AllOp: x > ALL(subq)
        sa::Expr::AllOp {
            left,
            compare_op,
            right,
            ..
        } => {
            let subquery = match *right {
                sa::Expr::Subquery(q) => convert_query_to_select(*q)?,
                other => {
                    return Err(unsupported(format!(
                        "ALL with non-subquery operand `{other}`"
                    )))
                }
            };
            Ok(kk::Expr::AllOp {
                expr: Box::new(convert_expr(*left)?),
                op: convert_binary_operator(compare_op)?,
                subquery: Box::new(subquery),
            })
        }

        // InUnnest: col IN UNNEST(arr) → JSON_MEMBER_OF
        sa::Expr::InUnnest {
            expr,
            array_expr,
            negated,
        } => {
            let func = kk::Expr::Function {
                name: "JSON_MEMBER_OF".to_string(),
                args: vec![convert_expr(*expr)?, convert_expr(*array_expr)?],
                distinct: false,
            };
            if negated {
                Ok(kk::Expr::Function {
                    name: "NOT".to_string(),
                    args: vec![func],
                    distinct: false,
                })
            } else {
                Ok(func)
            }
        }

        // JsonAccess: col->'path' → JSON_EXTRACT(col, 'path_str')
        sa::Expr::JsonAccess { value, path } => Ok(kk::Expr::Function {
            name: "JSON_EXTRACT".to_string(),
            args: vec![
                convert_expr(*value)?,
                kk::Expr::StringLiteral(format!("{path}")),
            ],
            distinct: false,
        }),

        // arr[idx] → ARRAY_GET placeholder (no Expr::Index in sqlparser-rs 0.61; this is handled via CompoundFieldAccess or JsonAccess)
        // Tuple: (a, b, c) → pass through if single element, else unsupported
        sa::Expr::Tuple(elements) => {
            if elements.len() == 1 {
                convert_expr(elements.into_iter().next().unwrap())
            } else {
                Err(unsupported("tuple constructor expression"))
            }
        }

        // MemberOf: x MEMBER OF(arr) → placeholder function
        sa::Expr::MemberOf(member_of) => Ok(kk::Expr::Function {
            name: "JSON_MEMBER_OF".to_string(),
            args: vec![
                convert_expr(*member_of.value)?,
                convert_expr(*member_of.array)?,
            ],
            distinct: false,
        }),

        // Wildcard in expression context (rare) → treat as 1
        sa::Expr::Wildcard(..) => Ok(kk::Expr::IntegerLiteral(1)),
        sa::Expr::QualifiedWildcard(..) => Err(unsupported("qualified wildcard in expression")),

        // Oracle-specific, MySQL FTS, other dialect-specific
        sa::Expr::OuterJoin(expr) => convert_expr(*expr), // Ignore (+) safely
        sa::Expr::Prior(_expr) => Err(unsupported("Oracle CONNECT BY PRIOR")),
        sa::Expr::Lambda(..) => Err(unsupported("Lambda expressions (x -> y)")),
        sa::Expr::MatchAgainst {
            columns,
            match_value,
            ..
        } => {
            // Extract column names (MySQL: ObjectName list)
            let col_names = columns
                .iter()
                .map(|c| super::common::object_name_to_string(c))
                .collect::<Vec<_>>();

            // Extract the search string from the match_value (single-quoted string)
            let query_str = match match_value {
                sa::Value::SingleQuotedString(s) | sa::Value::DoubleQuotedString(s) => s,
                other => format!("{other}"),
            };

            Ok(kk::Expr::MatchAgainst {
                columns: col_names,
                query: query_str,
            })
        }
        sa::Expr::GroupingSets(..) | sa::Expr::Cube(..) | sa::Expr::Rollup(..) => Err(unsupported(
            "GROUPING SETS / CUBE / ROLLUP modifiers in GROUP BY",
        )),
        sa::Expr::Struct { .. } => Err(unsupported("STRUCT expression")),
        sa::Expr::Dictionary(values) => {
            let mut args = Vec::with_capacity(values.len() * 2);
            for d in values {
                let k_str = d.key.value.clone();
                args.push(kk::Expr::StringLiteral(k_str));
                args.push(convert_expr(*d.value.clone())?);
            }
            Ok(kk::Expr::Function {
                name: "JSON_OBJECT".to_string(),
                args,
                distinct: false,
            })
        }
        sa::Expr::Map(..) => Err(unsupported("MAP expression")),
    }
}

fn convert_cast_type(data_type: sqlparser::ast::DataType) -> kk::CastTargetType {
    let raw = data_type.to_string();
    let head: String = raw
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    match head.as_str() {
        "INT" | "INTEGER" | "INT2" | "INT4" | "INT8" | "BIGINT" | "SMALLINT" | "TINYINT"
        | "BOOLEAN" | "BOOL" => kk::CastTargetType::Integer,
        "REAL" | "FLOAT" | "FLOAT4" | "FLOAT8" | "DOUBLE" => kk::CastTargetType::Real,
        "TEXT" | "VARCHAR" | "CHAR" | "CHARACTER" | "CLOB" | "STRING" | "NCHAR" | "NVARCHAR" => {
            kk::CastTargetType::Text
        }
        "BLOB" | "BYTES" | "BYTEA" | "BINARY" | "VARBINARY" => kk::CastTargetType::Blob,
        "NUMERIC" | "DECIMAL" | "DEC" | "NUMBER" => kk::CastTargetType::Numeric,
        // Temporal types — stored as Text (ISO 8601), exposed as distinct cast types
        "DATE" => kk::CastTargetType::Date,
        "TIME" => kk::CastTargetType::Time,
        "TIMESTAMP" | "DATETIME" => kk::CastTargetType::Timestamp,
        // JSON — stored as Text
        "JSON" | "JSONB" => kk::CastTargetType::Json,
        _ => kk::CastTargetType::Text,
    }
}

fn convert_function(function: sa::Function) -> Result<kk::Expr> {
    let sa::Function {
        name,
        parameters,
        args,
        filter,
        null_treatment,
        over,
        within_group,
        ..
    } = function;

    if !matches!(parameters, sa::FunctionArguments::None) {
        return Err(unsupported("function parameters"));
    }
    // A6: filter is handled below (inline CASE WHEN rewrite)
    if null_treatment.is_some() {
        return Err(unsupported("function NULL treatment clause"));
    }
    // Bug 3 fix: OVER clause → convert to WindowFunction
    if let Some(window_type) = over {
        let func_name_str = name.to_string();
        let distinct_flag = matches!(
            &args,
            sa::FunctionArguments::List(l) if matches!(l.duplicate_treatment, Some(sa::DuplicateTreatment::Distinct))
        );
        let mut out_args = Vec::new();
        if let sa::FunctionArguments::List(ref list) = args {
            for arg in &list.args {
                if let sa::FunctionArg::Unnamed(sa::FunctionArgExpr::Expr(e)) = arg {
                    if let Ok(converted) = convert_expr(e.clone()) {
                        out_args.push(converted);
                    }
                }
            }
        }
        // Extract partition_by and order_by from window spec
        let (partition_by, order_by, frame) = match &window_type {
            sa::WindowType::WindowSpec(spec) => {
                let pb: Vec<kk::Expr> = spec
                    .partition_by
                    .iter()
                    .filter_map(|e| convert_expr(e.clone()).ok())
                    .collect();
                let ob: Vec<kk::OrderByItem> = spec
                    .order_by
                    .iter()
                    .map(|item| kk::OrderByItem {
                        expr: convert_expr(item.expr.clone()).unwrap_or(kk::Expr::Null),
                        ascending: item.options.asc.unwrap_or(true),
                        nulls_first: item.options.nulls_first,
                    })
                    .collect();
                let frame = if let Some(wf) = &spec.window_frame {
                    convert_window_frame(wf).ok()
                } else {
                    None
                };
                (pb, ob, frame)
            }
            sa::WindowType::NamedWindow(name) => {
                // Return a placeholder for named windows (resolved in eval_expr later)
                // For now we just store the name as a dummy column ref in partition_by
                let dummy_pb = vec![kk::Expr::ColumnRef {
                    table: None,
                    column: format!("__named_window_{}", name),
                }];
                (dummy_pb, Vec::new(), None)
            }
        };
        let func_variant = match func_name_str.to_uppercase().as_str() {
            "ROW_NUMBER" => kk::WindowFunc::RowNumber,
            "RANK" => kk::WindowFunc::Rank,
            "DENSE_RANK" => kk::WindowFunc::DenseRank,
            "NTILE" => kk::WindowFunc::Ntile(Box::new(
                out_args.first().cloned().unwrap_or(kk::Expr::Null),
            )),
            "LAG" => kk::WindowFunc::Lag {
                expr: Box::new(out_args.first().cloned().unwrap_or(kk::Expr::Null)),
                offset: out_args.get(1).cloned().map(Box::new),
                default: out_args.get(2).cloned().map(Box::new),
            },
            "LEAD" => kk::WindowFunc::Lead {
                expr: Box::new(out_args.first().cloned().unwrap_or(kk::Expr::Null)),
                offset: out_args.get(1).cloned().map(Box::new),
                default: out_args.get(2).cloned().map(Box::new),
            },
            "FIRST_VALUE" => kk::WindowFunc::FirstValue(Box::new(
                out_args.first().cloned().unwrap_or(kk::Expr::Null),
            )),
            "LAST_VALUE" => kk::WindowFunc::LastValue(Box::new(
                out_args.first().cloned().unwrap_or(kk::Expr::Null),
            )),
            "NTH_VALUE" => kk::WindowFunc::NthValue(
                Box::new(out_args.first().cloned().unwrap_or(kk::Expr::Null)),
                Box::new(out_args.get(1).cloned().unwrap_or(kk::Expr::Null)),
            ),
            _ => kk::WindowFunc::Aggregate {
                name: func_name_str.clone(),
                args: out_args,
                distinct: distinct_flag,
            },
        };
        return Ok(kk::Expr::WindowFunction {
            func: func_variant,
            partition_by,
            order_by,
            frame,
        });
    }
    if !within_group.is_empty() {
        return Err(unsupported("WITHIN GROUP"));
    }

    let func_name = name.to_string();
    let mut distinct = false;
    let mut out_args = Vec::new();

    match args {
        sa::FunctionArguments::None => {}
        sa::FunctionArguments::Subquery(query) => {
            out_args.push(kk::Expr::Subquery(Box::new(convert_query_to_select(
                *query,
            )?)));
        }
        sa::FunctionArguments::List(list) => {
            distinct = matches!(
                list.duplicate_treatment,
                Some(sa::DuplicateTreatment::Distinct)
            );
            if !list.clauses.is_empty() {
                return Err(unsupported("function argument clauses"));
            }
            for arg in list.args {
                convert_function_arg(arg, &func_name, &mut out_args)?;
            }
        }
    }

    // A6. FILTER (WHERE cond): rewrite each non-wildcard arg as CASE WHEN cond THEN arg ELSE NULL END
    // This makes COUNT(*) FILTER (WHERE x > 0) work as COUNT(CASE WHEN x > 0 THEN 1 ELSE NULL END)
    if let Some(filter_clause) = filter {
        let cond = convert_expr(*filter_clause)?;
        out_args = out_args
            .into_iter()
            .map(|arg| kk::Expr::Case {
                operand: None,
                when_clauses: vec![(cond.clone(), arg)],
                else_clause: Some(Box::new(kk::Expr::Null)),
            })
            .collect();
        // Handle COUNT(*) / COUNT(1) special case: if no args, use CASE WHEN cond THEN 1 ELSE NULL
        if out_args.is_empty() && func_name.eq_ignore_ascii_case("COUNT") {
            out_args.push(kk::Expr::Case {
                operand: None,
                when_clauses: vec![(cond, kk::Expr::IntegerLiteral(1))],
                else_clause: Some(Box::new(kk::Expr::Null)),
            });
        }
    }

    Ok(kk::Expr::Function {
        name: func_name,
        args: out_args,
        distinct,
    })
}

pub(crate) fn convert_window_frame(wf: &sa::WindowFrame) -> Result<kk::WindowFrame> {
    let unit = match wf.units {
        sa::WindowFrameUnits::Rows => kk::WindowFrameUnit::Rows,
        sa::WindowFrameUnits::Range => kk::WindowFrameUnit::Range,
        sa::WindowFrameUnits::Groups => kk::WindowFrameUnit::Groups,
    };
    let start = convert_window_bound(&wf.start_bound)?;
    let end = if let Some(eb) = &wf.end_bound {
        Some(convert_window_bound(eb)?)
    } else {
        None
    };
    Ok(kk::WindowFrame { unit, start, end })
}

pub(crate) fn convert_window_bound(wb: &sa::WindowFrameBound) -> Result<kk::WindowBound> {
    match wb {
        sa::WindowFrameBound::CurrentRow => Ok(kk::WindowBound::CurrentRow),
        sa::WindowFrameBound::Preceding(Some(e)) => Ok(kk::WindowBound::Preceding(Box::new(
            convert_expr(*e.clone())?,
        ))),
        sa::WindowFrameBound::Preceding(None) => Ok(kk::WindowBound::UnboundedPreceding),
        sa::WindowFrameBound::Following(Some(e)) => Ok(kk::WindowBound::Following(Box::new(
            convert_expr(*e.clone())?,
        ))),
        sa::WindowFrameBound::Following(None) => Ok(kk::WindowBound::UnboundedFollowing),
    }
}

fn convert_function_arg(
    arg: sa::FunctionArg,
    function_name: &str,
    out: &mut Vec<kk::Expr>,
) -> Result<()> {
    let inner = match arg {
        sa::FunctionArg::Unnamed(a)
        | sa::FunctionArg::Named { arg: a, .. }
        | sa::FunctionArg::ExprNamed { arg: a, .. } => a,
    };
    convert_function_arg_expr(inner, function_name, out)
}

fn convert_function_arg_expr(
    arg: sa::FunctionArgExpr,
    function_name: &str,
    out: &mut Vec<kk::Expr>,
) -> Result<()> {
    match arg {
        sa::FunctionArgExpr::Expr(expr) => {
            out.push(convert_expr(expr)?);
            Ok(())
        }
        sa::FunctionArgExpr::Wildcard | sa::FunctionArgExpr::QualifiedWildcard(_) => {
            if function_name.eq_ignore_ascii_case("COUNT") {
                out.push(kk::Expr::IntegerLiteral(1));
                Ok(())
            } else {
                Err(unsupported(format!(
                    "wildcard argument in function `{function_name}`"
                )))
            }
        }
    }
}

fn convert_value(value: sa::Value) -> Result<kk::Expr> {
    match value {
        sa::Value::Number(raw, _) => parse_number_literal(&raw),
        sa::Value::HexStringLiteral(hex) => Ok(kk::Expr::BlobLiteral(parse_blob_hex(&hex)?)),
        sa::Value::SingleQuotedByteStringLiteral(bytes)
        | sa::Value::DoubleQuotedByteStringLiteral(bytes)
        | sa::Value::TripleSingleQuotedByteStringLiteral(bytes)
        | sa::Value::TripleDoubleQuotedByteStringLiteral(bytes) => {
            Ok(kk::Expr::BlobLiteral(bytes.into_bytes()))
        }
        sa::Value::Null => Ok(kk::Expr::Null),
        sa::Value::Boolean(v) => Ok(kk::Expr::IntegerLiteral(if v { 1 } else { 0 })),
        sa::Value::Placeholder(p) => Err(unsupported(format!("placeholder `{p}`"))),
        other => {
            if let Some(s) = other.into_string() {
                Ok(kk::Expr::StringLiteral(s))
            } else {
                Err(unsupported("literal value"))
            }
        }
    }
}

fn convert_compound_identifier(ids: Vec<sa::Ident>) -> Result<kk::Expr> {
    if ids.is_empty() {
        return Err(unsupported("empty compound identifier"));
    }
    let mut parts: Vec<String> = ids.into_iter().map(|i| i.value).collect();
    let column = parts.pop().unwrap();
    if parts.is_empty() {
        Ok(kk::Expr::ColumnRef {
            table: None,
            column,
        })
    } else {
        Ok(kk::Expr::ColumnRef {
            table: Some(parts.join(".")),
            column,
        })
    }
}

fn convert_binary_operator(op: sa::BinaryOperator) -> Result<kk::BinaryOperator> {
    match op {
        sa::BinaryOperator::Plus => Ok(kk::BinaryOperator::Add),
        sa::BinaryOperator::Minus => Ok(kk::BinaryOperator::Subtract),
        sa::BinaryOperator::Multiply => Ok(kk::BinaryOperator::Multiply),
        sa::BinaryOperator::Divide => Ok(kk::BinaryOperator::Divide),
        sa::BinaryOperator::Modulo => Ok(kk::BinaryOperator::Modulo),
        sa::BinaryOperator::Eq => Ok(kk::BinaryOperator::Equal),
        sa::BinaryOperator::NotEq => Ok(kk::BinaryOperator::NotEqual),
        sa::BinaryOperator::Lt => Ok(kk::BinaryOperator::LessThan),
        sa::BinaryOperator::LtEq => Ok(kk::BinaryOperator::LessThanOrEqual),
        sa::BinaryOperator::Gt => Ok(kk::BinaryOperator::GreaterThan),
        sa::BinaryOperator::GtEq => Ok(kk::BinaryOperator::GreaterThanOrEqual),
        sa::BinaryOperator::And => Ok(kk::BinaryOperator::And),
        sa::BinaryOperator::Or => Ok(kk::BinaryOperator::Or),
        sa::BinaryOperator::StringConcat => Ok(kk::BinaryOperator::Concat),
        // Logical XOR
        sa::BinaryOperator::Xor => Ok(kk::BinaryOperator::Xor),
        // Bitwise operators
        sa::BinaryOperator::BitwiseOr => Ok(kk::BinaryOperator::BitwiseOr),
        sa::BinaryOperator::BitwiseAnd => Ok(kk::BinaryOperator::BitwiseAnd),
        sa::BinaryOperator::BitwiseXor | sa::BinaryOperator::PGBitwiseXor => {
            Ok(kk::BinaryOperator::BitwiseXor)
        }
        sa::BinaryOperator::PGBitwiseShiftLeft => Ok(kk::BinaryOperator::ShiftLeft),
        sa::BinaryOperator::PGBitwiseShiftRight => Ok(kk::BinaryOperator::ShiftRight),
        // Integer divide → wrap as function (div by zero → NULL)
        sa::BinaryOperator::DuckIntegerDivide | sa::BinaryOperator::MyIntegerDivide => {
            // This path is hit only when the op is not wrapped in an Expr variant
            // In practice the outer handler will wrap it with a Cast; return Divide as best effort
            Ok(kk::BinaryOperator::Divide)
        }
        // L4: FTS MATCH operator
        sa::BinaryOperator::Match => Ok(kk::BinaryOperator::FtsMatch),
        // Regex / LIKE-variant operators → unsupported (would silently return wrong results if mapped to Equal)
        sa::BinaryOperator::PGRegexMatch
        | sa::BinaryOperator::PGILikeMatch
        | sa::BinaryOperator::Regexp
        | sa::BinaryOperator::PGRegexIMatch
        | sa::BinaryOperator::PGRegexNotMatch
        | sa::BinaryOperator::PGRegexNotIMatch
        | sa::BinaryOperator::PGLikeMatch
        | sa::BinaryOperator::PGNotLikeMatch
        | sa::BinaryOperator::PGNotILikeMatch => {
            Err(unsupported(format!("regex/LIKE binary operator `{op}`")))
        }
        other => Err(unsupported(format!("binary operator `{other}`"))),
    }
}

fn convert_unary_operator(op: sa::UnaryOperator) -> Result<kk::UnaryOperator> {
    match op {
        sa::UnaryOperator::Minus => Ok(kk::UnaryOperator::Minus),
        sa::UnaryOperator::Not | sa::UnaryOperator::BangNot => Ok(kk::UnaryOperator::Not),
        other => Err(unsupported(format!("unary operator `{other}`"))),
    }
}
