use crate::error::Result;
use crate::sql::ast as kk;
use sqlparser::ast as sa;

use super::common::{
    object_name_last_ident, object_name_to_string, table_alias_to_string, unsupported,
};
use super::expr::convert_expr;

pub(crate) fn convert_query_to_select(query: sa::Query) -> Result<kk::SelectStmt> {
    if query.fetch.is_some() {
        return Err(unsupported("FETCH"));
    }
    if !query.locks.is_empty() {
        return Err(unsupported("FOR UPDATE/SHARE"));
    }
    if query.for_clause.is_some() {
        return Err(unsupported("FOR clause"));
    }
    if query.settings.is_some() {
        return Err(unsupported("SETTINGS"));
    }
    if query.format_clause.is_some() {
        return Err(unsupported("FORMAT"));
    }
    if !query.pipe_operators.is_empty() {
        return Err(unsupported("pipe operator"));
    }

    // Batch D / L7: extract CTEs from WITH clause
    let ctes = if let Some(with) = query.with {
        let is_recursive = with.recursive;
        let mut out = Vec::with_capacity(with.cte_tables.len());
        for cte in with.cte_tables {
            out.push(kk::CteDefinition {
                name: cte.alias.name.value.clone(),
                columns: cte
                    .alias
                    .columns
                    .iter()
                    .map(|c| c.name.value.clone())
                    .collect(),
                query: Box::new(convert_query_to_select(*cte.query)?),
                is_recursive,
            });
        }
        out
    } else {
        Vec::new()
    };

    let mut select = match *query.body {
        sa::SetExpr::Select(select) => convert_select(*select)?,
        sa::SetExpr::Query(inner) => convert_query_to_select(*inner)?,
        // Batch B: nested set operation (A UNION B UNION C) — wrap in SetOp FROM clause
        sa::SetExpr::SetOperation {
            op,
            set_quantifier,
            left,
            right,
        } => {
            use super::statement::convert_set_expr_to_setop;
            let stmt = convert_set_expr_to_setop(op, set_quantifier, left, right)?;
            // Extract ORDER BY / LIMIT from the query and put them on the SetOpStmt
            let (limit, offset) = if let Some(lc) = query.limit_clause {
                match lc {
                    sa::LimitClause::LimitOffset { limit, offset, .. } => (
                        limit.map(|e| super::expr::convert_expr(e)).transpose()?,
                        offset
                            .map(|o| super::expr::convert_expr(o.value))
                            .transpose()?,
                    ),
                    sa::LimitClause::OffsetCommaLimit { offset, limit } => (
                        Some(super::expr::convert_expr(limit)?),
                        Some(super::expr::convert_expr(offset)?),
                    ),
                }
            } else {
                (None, None)
            };
            let order_by = if let Some(ob) = query.order_by {
                convert_order_by(ob)?
            } else {
                Vec::new()
            };
            let full_stmt = kk::SetOpStmt {
                kind: stmt.kind,
                left: stmt.left,
                right: stmt.right,
                order_by,
                limit,
                offset,
            };
            // Return a synthetic SelectStmt that has SetOp as its FROM source
            return Ok(kk::SelectStmt {
                distinct: false,
                columns: vec![kk::SelectColumn::AllColumns],
                from: Some(kk::FromClause::SetOp {
                    stmt: Box::new(full_stmt),
                    alias: "__setop__".to_string(),
                }),
                where_clause: None,
                group_by: Vec::new(),
                having: None,
                order_by: Vec::new(),
                limit: None,
                offset: None,
                ctes: Vec::new(),
                window_defs: Vec::new(),
            });
        }
        other => return Err(unsupported(format!("query body `{other}`"))),
    };

    if let Some(order_by) = query.order_by {
        select.order_by = convert_order_by(order_by)?;
    }
    if let Some(limit_clause) = query.limit_clause {
        let (limit, offset) = convert_limit_clause(limit_clause)?;
        select.limit = limit;
        select.offset = offset;
    }
    select.ctes = ctes;
    Ok(select)
}

fn convert_select(select: sa::Select) -> Result<kk::SelectStmt> {
    if select.flavor != sa::SelectFlavor::Standard {
        return Err(unsupported("non-standard SELECT flavor"));
    }
    if select.into.is_some() {
        return Err(unsupported("SELECT INTO"));
    }
    if select.top.is_some() {
        return Err(unsupported("SELECT TOP"));
    }
    if !select.lateral_views.is_empty() {
        return Err(unsupported("LATERAL VIEW"));
    }
    if select.prewhere.is_some() {
        return Err(unsupported("PREWHERE"));
    }
    if !select.connect_by.is_empty() {
        return Err(unsupported("CONNECT BY"));
    }
    if !select.cluster_by.is_empty() {
        return Err(unsupported("CLUSTER BY"));
    }
    if !select.distribute_by.is_empty() {
        return Err(unsupported("DISTRIBUTE BY"));
    }
    if !select.sort_by.is_empty() {
        return Err(unsupported("SORT BY"));
    }
    let mut window_defs = Vec::new();
    for curr_def in select.named_window {
        let name = curr_def.0.value;
        let spec = match curr_def.1 {
            sa::NamedWindowExpr::WindowSpec(spec) => spec,
            sa::NamedWindowExpr::NamedWindow(_) => {
                return Err(unsupported("reference to another named window"))
            }
        };
        let partition_by: Vec<kk::Expr> = spec
            .partition_by
            .iter()
            .filter_map(|e| super::expr::convert_expr(e.clone()).ok())
            .collect();
        let order_by: Vec<kk::OrderByItem> = spec
            .order_by
            .iter()
            .map(|item| kk::OrderByItem {
                expr: super::expr::convert_expr(item.expr.clone()).unwrap_or(kk::Expr::Null),
                ascending: item.options.asc.unwrap_or(true),
                nulls_first: item.options.nulls_first,
            })
            .collect();
        let frame = if let Some(wf) = &spec.window_frame {
            super::expr::convert_window_frame(wf).ok()
        } else {
            None
        };
        window_defs.push(kk::NamedWindowDefinition {
            name,
            partition_by,
            order_by,
            frame,
        });
    }
    if select.qualify.is_some() {
        return Err(unsupported("QUALIFY"));
    }
    if select.value_table_mode.is_some() {
        return Err(unsupported("value-table mode"));
    }

    let mut columns = Vec::with_capacity(select.projection.len());
    for item in select.projection {
        columns.push(convert_select_item(item)?);
    }

    let from = convert_from_clause(select.from)?;
    let where_clause = select.selection.map(convert_expr).transpose()?;
    let group_by = convert_group_by(select.group_by)?;
    let having = select.having.map(convert_expr).transpose()?;

    Ok(kk::SelectStmt {
        distinct: select.distinct.is_some(),
        columns,
        from,
        where_clause,
        group_by,
        having,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        ctes: Vec::new(),
        window_defs,
    })
}

fn convert_select_item(item: sa::SelectItem) -> Result<kk::SelectColumn> {
    match item {
        sa::SelectItem::Wildcard(_) => Ok(kk::SelectColumn::AllColumns),
        sa::SelectItem::QualifiedWildcard(kind, _) => match kind {
            sa::SelectItemQualifiedWildcardKind::ObjectName(obj) => Ok(
                kk::SelectColumn::TableAllColumns(object_name_to_string(&obj)),
            ),
            sa::SelectItemQualifiedWildcardKind::Expr(expr) => Err(unsupported(format!(
                "projection `{expr}.*` is not supported"
            ))),
        },
        sa::SelectItem::UnnamedExpr(expr) => Ok(kk::SelectColumn::Expr {
            expr: convert_expr(expr)?,
            alias: None,
        }),
        sa::SelectItem::ExprWithAlias { expr, alias } => Ok(kk::SelectColumn::Expr {
            expr: convert_expr(expr)?,
            alias: Some(alias.value),
        }),
    }
}

fn convert_group_by(group_by: sa::GroupByExpr) -> Result<Vec<kk::Expr>> {
    match group_by {
        sa::GroupByExpr::Expressions(exprs, modifiers) => {
            if !modifiers.is_empty() {
                return Err(unsupported("GROUP BY modifiers"));
            }
            let mut out = Vec::with_capacity(exprs.len());
            for expr in exprs {
                out.push(convert_expr(expr)?);
            }
            Ok(out)
        }
        sa::GroupByExpr::All(_) => Err(unsupported("GROUP BY ALL")),
    }
}

fn convert_order_by(order_by: sa::OrderBy) -> Result<Vec<kk::OrderByItem>> {
    if order_by.interpolate.is_some() {
        return Err(unsupported("ORDER BY INTERPOLATE"));
    }

    let exprs = match order_by.kind {
        sa::OrderByKind::Expressions(exprs) => exprs,
        sa::OrderByKind::All(_) => return Err(unsupported("ORDER BY ALL")),
    };

    let mut out = Vec::with_capacity(exprs.len());
    for item in exprs {
        out.push(kk::OrderByItem {
            expr: convert_expr(item.expr)?,
            ascending: item.options.asc.unwrap_or(true),
            nulls_first: item.options.nulls_first,
        });
    }
    Ok(out)
}

fn convert_limit_clause(
    limit_clause: sa::LimitClause,
) -> Result<(Option<kk::Expr>, Option<kk::Expr>)> {
    match limit_clause {
        sa::LimitClause::LimitOffset {
            limit,
            offset,
            limit_by,
        } => {
            if !limit_by.is_empty() {
                return Err(unsupported("LIMIT BY"));
            }
            let limit = limit.map(convert_expr).transpose()?;
            let offset = offset.map(|o| convert_expr(o.value)).transpose()?;
            Ok((limit, offset))
        }
        sa::LimitClause::OffsetCommaLimit { offset, limit } => {
            let limit = Some(convert_expr(limit)?);
            let offset = Some(convert_expr(offset)?);
            Ok((limit, offset))
        }
    }
}

fn convert_from_clause(from: Vec<sa::TableWithJoins>) -> Result<Option<kk::FromClause>> {
    if from.is_empty() {
        return Ok(None);
    }

    let mut iter = from.into_iter();
    let mut current = convert_table_with_joins(iter.next().unwrap())?;
    for table in iter {
        let right = convert_table_with_joins(table)?;
        current = kk::FromClause::Join {
            left: Box::new(current),
            join_type: kk::JoinType::Cross,
            right: Box::new(right),
            on: None,
        };
    }
    Ok(Some(current))
}

fn convert_table_with_joins(table: sa::TableWithJoins) -> Result<kk::FromClause> {
    let mut left = convert_table_factor(table.relation)?;
    for join in table.joins {
        let right = convert_table_factor(join.relation)?;
        let (join_type, on) = convert_join_operator(join.join_operator, &left, &right)?;
        left = kk::FromClause::Join {
            left: Box::new(left),
            join_type,
            right: Box::new(right),
            on,
        };
    }
    Ok(left)
}

fn convert_table_factor(factor: sa::TableFactor) -> Result<kk::FromClause> {
    match factor {
        sa::TableFactor::Table {
            name, alias, args, ..
        } => {
            if let Some(func_args) = args {
                // sqlparser parses GENERATE_SERIES(1,5) and UNNEST('...') as
                // TableFactor::Table with a `args: TableFunctionArgs` field
                let func_name = object_name_to_string(&name);
                let mut kk_args = Vec::new();
                for arg in &func_args.args {
                    if let sa::FunctionArg::Unnamed(sa::FunctionArgExpr::Expr(e)) = arg {
                        kk_args.push(super::expr::convert_expr(e.clone())?);
                    }
                }
                // Extract alias and optional column alias from TableAlias
                let (table_alias, col_alias) = if let Some(ta) = alias {
                    let col = ta.columns.into_iter().next().map(|c| c.name.value);
                    (Some(ta.name.value), col)
                } else {
                    (None, None)
                };
                return Ok(kk::FromClause::TableFunction {
                    name: func_name,
                    args: kk_args,
                    alias: table_alias,
                    column: col_alias,
                });
            }
            Ok(kk::FromClause::Table {
                name: object_name_to_string(&name),
                alias: table_alias_to_string(alias),
            })
        }
        sa::TableFactor::Derived {
            subquery, alias, ..
        } => {
            let alias = alias
                .map(|a| a.name.value)
                .ok_or_else(|| unsupported("subquery in FROM requires alias"))?;
            Ok(kk::FromClause::Subquery {
                query: Box::new(convert_query_to_select(*subquery)?),
                alias,
            })
        }
        sa::TableFactor::NestedJoin {
            table_with_joins,
            alias,
        } => {
            if alias.is_some() {
                return Err(unsupported("alias on parenthesized join"));
            }
            convert_table_with_joins(*table_with_joins)
        }
        // Table-valued functions: UNNEST(arr), generate_series(start, stop[, step])
        sa::TableFactor::TableFunction { expr, alias } => {
            // The expr must be a function call
            match expr {
                sa::Expr::Function(func) => {
                    let func_name = func.name.to_string();
                    // Extract positional arguments
                    let mut args = Vec::new();
                    if let sa::FunctionArguments::List(ref list) = func.args {
                        for arg in &list.args {
                            if let sa::FunctionArg::Unnamed(sa::FunctionArgExpr::Expr(e)) = arg {
                                args.push(super::expr::convert_expr(e.clone())?);
                            }
                        }
                    }
                    // Alias: from `TableAlias { name, columns }` — name is the table alias,
                    // columns[0] (if present) is the column alias for the generated column
                    let (table_alias, col_alias) = if let Some(ta) = alias {
                        let col = ta.columns.into_iter().next().map(|c| c.name.value);
                        (Some(ta.name.value), col)
                    } else {
                        (None, None)
                    };
                    Ok(kk::FromClause::TableFunction {
                        name: func_name,
                        args,
                        alias: table_alias,
                        column: col_alias,
                    })
                }
                other => Err(unsupported(format!(
                    "FROM TABLE FUNCTION with non-function expression `{other}`"
                ))),
            }
        }
        other => Err(unsupported(format!("FROM table factor `{other}`"))),
    }
}

fn convert_join_operator(
    op: sa::JoinOperator,
    left_rel: &kk::FromClause,
    right_rel: &kk::FromClause,
) -> Result<(kk::JoinType, Option<kk::Expr>)> {
    // Helper: check if a constraint is Natural before consuming it
    let is_natural = |c: &sa::JoinConstraint| matches!(c, sa::JoinConstraint::Natural);

    match op {
        sa::JoinOperator::Join(c)
        | sa::JoinOperator::Inner(c)
        | sa::JoinOperator::StraightJoin(c) => {
            // Batch C: NATURAL JOIN is Inner with JoinConstraint::Natural
            if is_natural(&c) {
                Ok((kk::JoinType::Natural, None))
            } else {
                Ok((
                    kk::JoinType::Inner,
                    convert_join_constraint(c, left_rel, right_rel)?,
                ))
            }
        }
        sa::JoinOperator::Left(c) | sa::JoinOperator::LeftOuter(c) => {
            if is_natural(&c) {
                Ok((kk::JoinType::Natural, None))
            } else {
                Ok((
                    kk::JoinType::Left,
                    convert_join_constraint(c, left_rel, right_rel)?,
                ))
            }
        }
        sa::JoinOperator::Right(c) | sa::JoinOperator::RightOuter(c) => {
            if is_natural(&c) {
                Ok((kk::JoinType::Natural, None))
            } else {
                Ok((
                    kk::JoinType::Right,
                    convert_join_constraint(c, left_rel, right_rel)?,
                ))
            }
        }
        sa::JoinOperator::LeftSemi(c) => Ok((
            kk::JoinType::LeftSemi,
            convert_join_constraint(c, left_rel, right_rel)?,
        )),
        sa::JoinOperator::RightSemi(c) => Ok((
            kk::JoinType::RightSemi,
            convert_join_constraint(c, left_rel, right_rel)?,
        )),
        // Batch C: FULL OUTER JOIN
        sa::JoinOperator::FullOuter(c) => Ok((
            kk::JoinType::Full,
            convert_join_constraint(c, left_rel, right_rel)?,
        )),
        sa::JoinOperator::CrossJoin(c) => {
            if is_natural(&c) {
                Ok((kk::JoinType::Natural, None))
            } else {
                let on = convert_join_constraint(c, left_rel, right_rel)?;
                if on.is_some() {
                    return Err(unsupported("CROSS JOIN with ON/USING"));
                }
                Ok((kk::JoinType::Cross, None))
            }
        }
        // LeftSemi / RightSemi — approximate as INNER JOIN
        // (semi-join eliminates duplicate right rows; INNER JOIN may produce more rows for
        // LeftAnti / RightAnti — anti-join (NOT IN subquery), complex, unsupported
        sa::JoinOperator::LeftAnti(c) | sa::JoinOperator::RightAnti(c) => {
            let _ = c; // consume to avoid unused warning
            Err(unsupported("ANTI JOIN is not supported"))
        }
        other => Err(unsupported(format!("join operator `{:?}`", other))),
    }
}

fn convert_join_constraint(
    constraint: sa::JoinConstraint,
    left_rel: &kk::FromClause,
    right_rel: &kk::FromClause,
) -> Result<Option<kk::Expr>> {
    match constraint {
        sa::JoinConstraint::None => Ok(None),
        sa::JoinConstraint::On(expr) => Ok(Some(convert_expr(expr)?)),
        sa::JoinConstraint::Using(columns) => {
            Ok(Some(convert_join_using(columns, left_rel, right_rel)?))
        }
        // Batch C: NATURAL JOIN — defer ON computation to exec time
        sa::JoinConstraint::Natural => Ok(None), // executor will resolve from schema
    }
}

fn convert_join_using(
    columns: Vec<sa::ObjectName>,
    left_rel: &kk::FromClause,
    right_rel: &kk::FromClause,
) -> Result<kk::Expr> {
    if columns.is_empty() {
        return Err(unsupported("JOIN USING without columns"));
    }

    let mut iter = columns.into_iter();
    let mut expr = using_column_eq(left_rel, right_rel, iter.next().unwrap())?;
    for col in iter {
        let next = using_column_eq(left_rel, right_rel, col)?;
        expr = kk::Expr::BinaryOp {
            left: Box::new(expr),
            op: kk::BinaryOperator::And,
            right: Box::new(next),
        };
    }
    Ok(expr)
}

fn using_side_expr(from: &kk::FromClause, side: &str, column: &str) -> Result<kk::Expr> {
    match from {
        kk::FromClause::Table { name, alias } => Ok(kk::Expr::ColumnRef {
            table: Some(alias.clone().unwrap_or_else(|| name.clone())),
            column: column.to_string(),
        }),
        kk::FromClause::Subquery { alias, .. } => Ok(kk::Expr::ColumnRef {
            table: Some(alias.clone()),
            column: column.to_string(),
        }),
        kk::FromClause::Join {
            join_type,
            left,
            right,
            ..
        } => match join_type {
            kk::JoinType::Inner | kk::JoinType::Cross => Ok(kk::Expr::Function {
                // SQL USING semantics use a merged key column; COALESCE approximates this on join trees.
                name: "COALESCE".to_string(),
                args: vec![
                    using_side_expr(left, side, column)?,
                    using_side_expr(right, side, column)?,
                ],
                distinct: false,
            }),
            kk::JoinType::Left | kk::JoinType::LeftSemi => using_side_expr(left, side, column),
            kk::JoinType::Right | kk::JoinType::RightSemi => using_side_expr(right, side, column),
            // Batch C: FULL/NATURAL JOIN — treat like INNER for USING purposes
            kk::JoinType::Full | kk::JoinType::Natural => Ok(kk::Expr::Function {
                name: "COALESCE".to_string(),
                args: vec![
                    using_side_expr(left, side, column)?,
                    using_side_expr(right, side, column)?,
                ],
                distinct: false,
            }),
        },
        // Batch B: SetOp — treat as anonymous subquery with given side
        kk::FromClause::SetOp { alias, .. } => Ok(kk::Expr::ColumnRef {
            table: Some(alias.clone()),
            column: column.to_string(),
        }),
        // TableFunction — treat output column as unqualified (table-valued functions produce flat rows)
        kk::FromClause::TableFunction { alias, name, .. } => Ok(kk::Expr::ColumnRef {
            table: alias.clone().or_else(|| Some(name.clone())),
            column: column.to_string(),
        }),
    }
}

fn using_column_eq(
    left_rel: &kk::FromClause,
    right_rel: &kk::FromClause,
    column: sa::ObjectName,
) -> Result<kk::Expr> {
    let col = object_name_last_ident(&column)?;
    Ok(kk::Expr::BinaryOp {
        left: Box::new(using_side_expr(left_rel, "left", &col)?),
        op: kk::BinaryOperator::Equal,
        right: Box::new(using_side_expr(right_rel, "right", &col)?),
    })
}
