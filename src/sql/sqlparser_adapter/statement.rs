use crate::error::{KkdbError, Result};
use crate::sql::ast as kk;
use sqlparser::ast as sa;

use super::common::{
    convert_data_type, object_name_last_ident, object_name_to_string, unsupported,
};
use super::expr::convert_expr;
use super::query::convert_query_to_select;

pub(crate) fn convert_statement(stmt: sa::Statement) -> Result<kk::Statement> {
    match stmt {
        sa::Statement::Query(query) => {
            convert_query_statement(*query)
        }
        sa::Statement::Insert(insert) => convert_insert(insert),
        sa::Statement::Update(update) => convert_update(update),
        sa::Statement::Delete(delete) => convert_delete(delete),
        sa::Statement::CreateTable(create) => convert_create_table(create),
        sa::Statement::CreateIndex(create) => convert_create_index(create),
        sa::Statement::AlterTable(alter) => convert_alter_table(alter),
        sa::Statement::Drop {
            object_type,
            if_exists,
            names,
            ..
        } => convert_drop(object_type, if_exists, names),
        sa::Statement::StartTransaction { .. } => Ok(kk::Statement::Begin),
        sa::Statement::Commit { .. } => Ok(kk::Statement::Commit),
        sa::Statement::Rollback { savepoint, .. } => {
            if let Some(sp) = savepoint {
                Ok(kk::Statement::RollbackToSavepoint(sp.value))
            } else {
                Ok(kk::Statement::Rollback)
            }
        }
        sa::Statement::Savepoint { name } => Ok(kk::Statement::Savepoint(name.value)),
        sa::Statement::ReleaseSavepoint { name } => {
            Ok(kk::Statement::ReleaseSavepoint(name.value))
        }
        sa::Statement::ShowTables { .. } => Ok(kk::Statement::ShowTables),
        sa::Statement::Vacuum { .. } => Ok(kk::Statement::Vacuum),
        // O1: ANALYZE TABLE t
        sa::Statement::Analyze(a) => {
            let table_name = a.table_name
                .map(|n| object_name_to_string(&n))
                .ok_or_else(|| unsupported("ANALYZE without table name"))?;
            Ok(kk::Statement::AnalyzeTable(table_name))
        }
        // Batch E: CREATE VIEW
        sa::Statement::CreateView(cv) => {
            let view_query = convert_query_to_select(*cv.query)?;
            Ok(kk::Statement::CreateView(kk::CreateViewStmt {
                name: object_name_to_string(&cv.name),
                columns: cv.columns.into_iter().map(|c| c.name.value).collect(),
                query: Box::new(view_query),
                or_replace: cv.or_replace,
                if_not_exists: cv.if_not_exists,
            }))
        }
        // L4: CREATE VIRTUAL TABLE
        sa::Statement::CreateVirtualTable {
            name,
            module_name,
            module_args,
            if_not_exists,
            ..
        } => {
            if module_name.value.to_lowercase() != "fts5" {
                return Err(unsupported(format!("virtual table module {}", module_name.value)));
            }
            let mut columns = Vec::new();
            for arg in module_args {
                columns.push(kk::ColumnDef {
                    name: arg.value,
                    data_type: crate::types::DataType::Text,
                    primary_key: false,
                    autoincrement: false,
                    not_null: false,
                    unique: false,
                    default: None,
                    references: None,
                    check_expr: None,
                });
            }
            Ok(kk::Statement::CreateTable(kk::CreateTableStmt {
                table_name: object_name_to_string(&name),
                columns,
                if_not_exists,
                is_fts: true,
                source: None,
                checks: Vec::new(),
            }))
        }
        // L3: CREATE TRIGGER
        sa::Statement::CreateTrigger(ct) => {
            let trig_name = object_name_to_string(&ct.name);
            let timing = match ct.period.as_ref().map(|p| p.to_string().to_uppercase()) {
                Some(ref p) if p == "BEFORE" => kk::TriggerTiming::Before,
                Some(ref p) if p == "AFTER" => kk::TriggerTiming::After,
                _ => return Err(unsupported(format!("trigger timing {:?}", ct.period))),
            };
            let event = match ct.events.first() {
                Some(sqlparser::ast::TriggerEvent::Insert) => kk::TriggerEvent::Insert,
                Some(sqlparser::ast::TriggerEvent::Delete) => kk::TriggerEvent::Delete,
                Some(sqlparser::ast::TriggerEvent::Update { .. }) => kk::TriggerEvent::Update,
                _ => return Err(unsupported(format!("trigger event {:?}", ct.events))),
            };
            let t_name = object_name_to_string(&ct.table_name);
            let body_sql = ct.statements.as_ref().map(|cs| format!("{cs}")).unwrap_or_default();
            Ok(kk::Statement::CreateTrigger(kk::CreateTriggerStmt {
                name: trig_name,
                timing,
                event,
                table_name: t_name,
                body_sql,
                or_replace: ct.or_replace,
            }))
        }
        // L3: DROP TRIGGER
        sa::Statement::DropTrigger(dt) => {
            Ok(kk::Statement::DropTrigger {
                name: object_name_to_string(&dt.trigger_name),
                if_exists: dt.if_exists,
            })
        }
        sa::Statement::Explain { statement, .. } => {
            let inner = convert_statement(*statement)?;
            Ok(kk::Statement::Explain(Box::new(inner)))
        }
        // R4: TRUNCATE → DELETE without WHERE (removes all rows)
        sa::Statement::Truncate(t) => {
            let table_name = if let Some(target) = t.table_names.into_iter().next() {
                object_name_to_string(&target.name)
            } else {
                return Err(unsupported("TRUNCATE with no table"));
            };
            Ok(kk::Statement::Delete(kk::DeleteStmt {
                table_name,
                where_clause: None,
            }))
        }
        // User Management
        sa::Statement::CreateRole(sa::CreateRole { names, login: _, password, .. }) => {
            if names.len() != 1 {
                return Err(unsupported("CREATE USER with multiple names"));
            }
            Ok(kk::Statement::CreateUser(kk::CreateUserStmt {
                username: object_name_to_string(&names[0]),
                password: password.as_ref().map(|pw| match pw {
                    sa::Password::Password(sa::Expr::Identifier(ident)) => ident.value.clone(),
                    sa::Password::Password(sa::Expr::Value(sa::ValueWithSpan { value: sa::Value::SingleQuotedString(s), .. })) => s.clone(),
                    sa::Password::Password(sa::Expr::Value(sa::ValueWithSpan { value: sa::Value::DoubleQuotedString(s), .. })) => s.clone(),
                    _ => "".to_string(), // Error handling skipped for this demo, just map safely
                }),
            }))
        }
        sa::Statement::AlterRole { name, operation: _ } => {
            // Simplified handling for AlterRole
            Ok(kk::Statement::AlterUser(kk::AlterUserStmt {
                username: name.value.clone(),
                password: None, // Simplified until full password map logic is needed from Operation
            }))
        }

        sa::Statement::Grant(sa::Grant { privileges, objects, grantees, .. }) => {
            let kk_privs = convert_privileges(privileges)?;
            let kk_obj = objects.map(|o| convert_grant_object(o)).unwrap_or_else(|| Err(unsupported("GRANT without object")))?; 
            Ok(kk::Statement::Grant(kk::GrantStmt {
                privileges: kk_privs,
                object: kk_obj,
                grantees: grantees.into_iter().filter_map(|n| match n.name {
                    Some(sa::GranteeName::ObjectName(name)) => Some(object_name_to_string(&name)),
                    Some(sa::GranteeName::UserHost { user, host }) => Some(format!("{}@{}", user.value, host.value)),
                    None => None,
                }).collect(),
            }))
        }
        sa::Statement::Revoke(sa::Revoke { privileges, objects, grantees, .. }) => {
            let kk_privs = convert_privileges(privileges)?;
            let kk_obj = objects.map(|o| convert_grant_object(o)).unwrap_or_else(|| Err(unsupported("REVOKE without object")))?; 
            Ok(kk::Statement::Revoke(kk::RevokeStmt {
                privileges: kk_privs,
                object: kk_obj,
                grantees: grantees.into_iter().filter_map(|n| match n.name {
                    Some(sa::GranteeName::ObjectName(name)) => Some(object_name_to_string(&name)),
                    Some(sa::GranteeName::UserHost { user, host }) => Some(format!("{}@{}", user.value, host.value)),
                    None => None,
                }).collect(),
            }))
        }
        // SET kkdb.key = 'value' (session variables for RLS / multi-tenant)
        sa::Statement::Set(sa::Set::SingleAssignment { variable, values, .. }) => {
            // variable is an ObjectName; join parts with '.'
            let key = variable.0.iter().map(|i| format!("{i}")).collect::<Vec<_>>().join(".");
            // take the first value expression and coerce to string
            let value = values.into_iter().next().map(|expr| match expr {
                sa::Expr::Value(sa::ValueWithSpan { value: sa::Value::SingleQuotedString(s), .. }) => s,
                sa::Expr::Value(sa::ValueWithSpan { value: sa::Value::DoubleQuotedString(s), .. }) => s,
                other => format!("{other}"),
            }).unwrap_or_default();
            Ok(kk::Statement::SetSessionVar { key, value })
        }
        // Quietly absorb other SET variants (e.g. SET NAMES, SET @@global...)
        sa::Statement::Set(_) => Err(unsupported("SET statement variant")),
        // RLS: CREATE POLICY / DROP POLICY
        sa::Statement::CreatePolicy(cp) => {
            Ok(kk::Statement::CreatePolicy(kk::CreatePolicyStmt {
                name: cp.name.value,
                table_name: object_name_to_string(&cp.table_name),
                role: cp.to.as_ref().and_then(|v| v.first()).map(|o| format!("{o}")),
                using_expr: cp.using.map(|e| convert_expr(e)).transpose()?,
                check_expr: cp.with_check.map(|e| convert_expr(e)).transpose()?,
            }))
        }
        sa::Statement::DropPolicy(dp) => {
            Ok(kk::Statement::DropPolicy(kk::DropPolicyStmt {
                name: dp.name.value,
                table_name: object_name_to_string(&dp.table_name),
                if_exists: dp.if_exists,
            }))
        }
        sa::Statement::AlterView { .. } => Err(unsupported("ALTER VIEW")),
        sa::Statement::AlterIndex { .. } => Err(unsupported("ALTER INDEX")),
        sa::Statement::AlterSchema(..) => Err(unsupported("ALTER SCHEMA")),
        sa::Statement::AlterType(..) => Err(unsupported("ALTER TYPE")),
        sa::Statement::CreateFunction(..) => Err(unsupported("CREATE FUNCTION")),
        sa::Statement::CreateProcedure { .. } => Err(unsupported("CREATE PROCEDURE")),
        sa::Statement::DropFunction(..) => Err(unsupported("DROP FUNCTION")),
        sa::Statement::DropProcedure { .. } => Err(unsupported("DROP PROCEDURE")),
        sa::Statement::DropExtension(..) => Err(unsupported("DROP EXTENSION")),
        sa::Statement::CreateExtension(..) => Err(unsupported("CREATE EXTENSION")),
        sa::Statement::Declare { .. } => Err(unsupported("DECLARE cursor")),
        sa::Statement::Fetch { .. } => Err(unsupported("FETCH cursor")),
        sa::Statement::Close { .. } => Err(unsupported("CLOSE cursor")),
        sa::Statement::Open(..) => Err(unsupported("OPEN cursor")),
        sa::Statement::Call(..) => Err(unsupported("CALL stored procedure")),
        sa::Statement::Install { .. } => Err(unsupported("INSTALL")),
        sa::Statement::Load { .. } => Err(unsupported("LOAD")),
        sa::Statement::CreateSecret { .. } => Err(unsupported("CREATE SECRET")),
        sa::Statement::DropSecret { .. } => Err(unsupported("DROP SECRET")),
        sa::Statement::Msck(..) => Err(unsupported("MSCK")),
        sa::Statement::AttachDatabase { .. } | sa::Statement::AttachDuckDBDatabase { .. }
        | sa::Statement::DetachDuckDBDatabase { .. } => Err(unsupported("ATTACH/DETACH DATABASE")),
        other => Err(unsupported(format!("statement `{other}`"))),
    }
}

fn convert_drop(
    object_type: sa::ObjectType,
    if_exists: bool,
    names: Vec<sa::ObjectName>,
) -> Result<kk::Statement> {
    match object_type {
        sa::ObjectType::User | sa::ObjectType::Role => {
            if names.is_empty() {
                return Err(unsupported("DROP USER/ROLE without names"));
            }
            Ok(kk::Statement::DropUser(kk::DropUserStmt {
                usernames: names.into_iter().map(|n| object_name_to_string(&n)).collect(),
                if_exists,
            }))
        }
        sa::ObjectType::Table => {
            if names.len() != 1 {
                return Err(unsupported("DROP TABLE with multiple names"));
            }
            Ok(kk::Statement::DropTable(kk::DropTableStmt {
                table_name: object_name_to_string(&names[0]),
                if_exists,
            }))
        }
        sa::ObjectType::Index => {
            if names.len() != 1 {
                return Err(unsupported("DROP INDEX with multiple names"));
            }
            Ok(kk::Statement::DropIndex(kk::DropIndexStmt {
                index_name: object_name_to_string(&names[0]),
                if_exists,
            }))
        }
        sa::ObjectType::View => {
            // DROP VIEW treated like DROP TABLE — views stored in same schema map
            if names.len() != 1 {
                return Err(unsupported("DROP VIEW with multiple names"));
            }
            Ok(kk::Statement::DropTable(kk::DropTableStmt {
                table_name: object_name_to_string(&names[0]),
                if_exists,
            }))
        }
        other => Err(unsupported(format!("DROP {other}"))),
    }
}

fn convert_create_table(create: sa::CreateTable) -> Result<kk::Statement> {
    if create.columns.is_empty() && create.query.is_none() {
        return Err(KkdbError::ParseError(
            "CREATE TABLE requires at least one column".into(),
        ));
    }

    // CREATE TABLE AS SELECT
    if let Some(query) = create.query {
        let select = convert_query_to_select(*query)?;
        return Ok(kk::Statement::CreateTable(kk::CreateTableStmt {
            table_name: object_name_to_string(&create.name),
            columns: Vec::new(),
            if_not_exists: create.if_not_exists,
            is_fts: false,
            source: Some(Box::new(select)),
            checks: Vec::new(),
        }));
    }

    let mut columns = Vec::with_capacity(create.columns.len());
    for col in create.columns {
        columns.push(convert_column_def(col)?);
    }

    // Parse table-level CHECK constraints
    let mut checks: Vec<(Option<String>, kk::Expr)> = Vec::new();
    for tc in &create.constraints {
        if let sa::TableConstraint::Check(cc) = tc {
            let name = cc.name.as_ref().map(|i| i.value.clone());
            if let Ok(expr) = convert_expr(*cc.expr.clone()) {
                checks.push((name, expr));
            }
        }
    }

    Ok(kk::Statement::CreateTable(kk::CreateTableStmt {
        table_name: object_name_to_string(&create.name),
        columns,
        if_not_exists: create.if_not_exists,
        is_fts: false,
        source: None,
        checks,
    }))
}

fn convert_create_index(create: sa::CreateIndex) -> Result<kk::Statement> {
    let index_name = create
        .name
        .as_ref()
        .map(object_name_to_string)
        .ok_or_else(|| unsupported("CREATE INDEX without index name"))?;

    let mut columns = Vec::with_capacity(create.columns.len());
    for col in create.columns {
        columns.push(convert_index_column(col)?);
    }

    Ok(kk::Statement::CreateIndex(kk::CreateIndexStmt {
        index_name,
        table_name: object_name_to_string(&create.table_name),
        columns,
        unique: create.unique,
        if_not_exists: create.if_not_exists,
    }))
}

fn convert_index_column(col: sa::IndexColumn) -> Result<String> {
    match col.column.expr {
        sa::Expr::Identifier(id) => Ok(id.value),
        sa::Expr::CompoundIdentifier(ids) => ids
            .last()
            .map(|id| id.value.clone())
            .ok_or_else(|| unsupported("empty compound identifier in index column")),
        other => Err(unsupported(format!(
            "index expression `{other}` is not supported"
        ))),
    }
}

fn convert_alter_table(alter: sa::AlterTable) -> Result<kk::Statement> {
    if alter.operations.len() != 1 {
        return Err(unsupported("ALTER TABLE with multiple operations"));
    }

    let table_name = object_name_to_string(&alter.name);
    let action = match alter.operations.into_iter().next().unwrap() {
        sa::AlterTableOperation::AddColumn { column_def, .. } => {
            kk::AlterTableAction::AddColumn(convert_column_def(column_def)?)
        }
        sa::AlterTableOperation::DropColumn { column_names, .. } => {
            if column_names.len() != 1 {
                return Err(unsupported("ALTER TABLE DROP COLUMN with multiple columns"));
            }
            kk::AlterTableAction::DropColumn(column_names[0].value.clone())
        }
        sa::AlterTableOperation::RenameTable { table_name } => {
            let name = match table_name {
                sa::RenameTableNameKind::As(n) | sa::RenameTableNameKind::To(n) => {
                    object_name_to_string(&n)
                }
            };
            kk::AlterTableAction::RenameTable(name)
        }
        sa::AlterTableOperation::RenameColumn {
            old_column_name,
            new_column_name,
        } => kk::AlterTableAction::RenameColumn {
            old_name: old_column_name.value,
            new_name: new_column_name.value,
        },
        sa::AlterTableOperation::EnableRowLevelSecurity => {
            kk::AlterTableAction::EnableRowLevelSecurity
        }
        sa::AlterTableOperation::DisableRowLevelSecurity => {
            // Best-effort: treat as a no-op for now (could add DisableRLS later)
            return Err(unsupported("ALTER TABLE DISABLE ROW LEVEL SECURITY"))
        }
        other => {
            return Err(unsupported(format!(
                "ALTER TABLE operation `{other}` is not supported"
            )))
        }
    };

    Ok(kk::Statement::AlterTable(kk::AlterTableStmt {
        table_name,
        action,
    }))
}

fn convert_insert(insert: sa::Insert) -> Result<kk::Statement> {
    if !insert.into {
        return Err(unsupported("INSERT without INTO"));
    }
    if !insert.assignments.is_empty() {
        return Err(unsupported("INSERT ... SET"));
    }
    if insert.on.is_some() {
        return Err(unsupported("INSERT ... ON CONFLICT/ON DUPLICATE"));
    }
    if insert.returning.is_some() {
        return Err(unsupported("INSERT ... RETURNING"));
    }

    // Determine conflict policy from OR clause (SQLite) or ON CONFLICT clause (standard)
    let conflict = if let Some(on_conflict) = insert.on {
        get_conflict_policy_from_on(on_conflict)?
    } else if insert.or.is_some() {
        match insert.or.as_ref().map(|o| o) {
            Some(sa::SqliteOnConflict::Replace) => kk::ConflictPolicy::Replace,
            Some(sa::SqliteOnConflict::Ignore) => kk::ConflictPolicy::Ignore,
            _ => kk::ConflictPolicy::Error,
        }
    } else {
        kk::ConflictPolicy::Error
    };

    let table_name = match insert.table {
        sa::TableObject::TableName(name) => object_name_to_string(&name),
        sa::TableObject::TableFunction(_) => {
            return Err(unsupported("INSERT INTO TABLE FUNCTION"));
        }
    };

    let source = insert
        .source
        .ok_or_else(|| unsupported("INSERT without VALUES source"))?;

    let source = match *source.body {
        sa::SetExpr::Values(values) => {
            let mut rows = Vec::with_capacity(values.rows.len());
            for row in values.rows {
                let mut out = Vec::with_capacity(row.len());
                for expr in row {
                    out.push(convert_expr(expr)?);
                }
                rows.push(out);
            }
            kk::InsertSource::Values(rows)
        }
        sa::SetExpr::Select(select) => {
            let stmt = convert_select_body(*select)?;
            kk::InsertSource::Select(Box::new(stmt))
        }
        _ => return Err(unsupported("INSERT source must be VALUES or SELECT")),
    };

    Ok(kk::Statement::Insert(kk::InsertStmt {
        table_name,
        columns: if insert.columns.is_empty() {
            None
        } else {
            Some(insert.columns.into_iter().map(|c| c.value).collect())
        },
        source,
        conflict,
    }))
}

/// Parse `INSERT ... ON CONFLICT ...` into a ConflictPolicy
fn get_conflict_policy_from_on(on: sa::OnInsert) -> Result<kk::ConflictPolicy> {
    match on {
        sa::OnInsert::OnConflict(oc) => {
            match oc.action {
                sa::OnConflictAction::DoNothing => Ok(kk::ConflictPolicy::Ignore),
                sa::OnConflictAction::DoUpdate(dou) => {
                    let mut assignments = Vec::new();
                    for assign in dou.assignments {
                        let col_name = match assign.target {
                            sa::AssignmentTarget::ColumnName(name) => {
                                super::common::object_name_last_ident(&name)?
                            }
                            sa::AssignmentTarget::Tuple(_) => {
                                return Err(unsupported("tuple assignment in ON CONFLICT DO UPDATE"));
                            }
                        };
                        assignments.push((col_name, convert_expr(assign.value)?));
                    }
                    Ok(kk::ConflictPolicy::Update(assignments))
                }
            }
        }
        sa::OnInsert::DuplicateKeyUpdate(assigns) => {
            // MySQL ON DUPLICATE KEY UPDATE col = val ...
            let mut assignments = Vec::new();
            for assign in assigns {
                let col_name = match assign.target {
                    sa::AssignmentTarget::ColumnName(name) => {
                        super::common::object_name_last_ident(&name)?
                    }
                    sa::AssignmentTarget::Tuple(_) => {
                        return Err(unsupported("tuple in ON DUPLICATE KEY UPDATE"));
                    }
                };
                assignments.push((col_name, convert_expr(assign.value)?));
            }
            Ok(kk::ConflictPolicy::Update(assignments))
        }
        // Handle any future #[non_exhaustive] variants
        _ => Err(unsupported("unsupported ON INSERT variant")),
    }
}

fn convert_select_body(select: sa::Select) -> Result<kk::SelectStmt> {
    use super::query::convert_query_to_select;
    let query = sa::Query {
        with: None,
        body: Box::new(sa::SetExpr::Select(Box::new(select))),
        order_by: None,
        limit_clause: None,
        fetch: None,
        locks: Vec::new(),
        for_clause: None,
        settings: None,
        format_clause: None,
        pipe_operators: Vec::new(),
    };
    convert_query_to_select(query)
}

fn convert_update(update: sa::Update) -> Result<kk::Statement> {
    if update.from.is_some() {
        return Err(unsupported("UPDATE ... FROM"));
    }
    if update.returning.is_some() {
        return Err(unsupported("UPDATE ... RETURNING"));
    }
    if update.limit.is_some() {
        return Err(unsupported("UPDATE ... LIMIT"));
    }

    let table_name = extract_simple_table_name(update.table, "UPDATE target")?;

    let mut assignments = Vec::with_capacity(update.assignments.len());
    for assignment in update.assignments {
        let col = match assignment.target {
            sa::AssignmentTarget::ColumnName(name) => object_name_last_ident(&name)?,
            sa::AssignmentTarget::Tuple(_) => {
                return Err(unsupported("tuple assignment in UPDATE"));
            }
        };
        assignments.push((col, convert_expr(assignment.value)?));
    }

    let where_clause = update.selection.map(convert_expr).transpose()?;
    Ok(kk::Statement::Update(kk::UpdateStmt {
        table_name,
        assignments,
        where_clause,
    }))
}

fn convert_delete(delete: sa::Delete) -> Result<kk::Statement> {
    if delete.using.is_some() {
        return Err(unsupported("DELETE ... USING"));
    }
    if delete.returning.is_some() {
        return Err(unsupported("DELETE ... RETURNING"));
    }
    if !delete.order_by.is_empty() {
        return Err(unsupported("DELETE ... ORDER BY"));
    }
    if delete.limit.is_some() {
        return Err(unsupported("DELETE ... LIMIT"));
    }

    let mut tables = match delete.from {
        sa::FromTable::WithFromKeyword(tables) | sa::FromTable::WithoutKeyword(tables) => tables,
    };
    if tables.len() != 1 {
        return Err(unsupported("DELETE from multiple tables"));
    }

    let table_name = extract_simple_table_name(tables.remove(0), "DELETE target")?;
    let where_clause = delete.selection.map(convert_expr).transpose()?;
    Ok(kk::Statement::Delete(kk::DeleteStmt {
        table_name,
        where_clause,
    }))
}

fn extract_simple_table_name(table: sa::TableWithJoins, context: &str) -> Result<String> {
    if !table.joins.is_empty() {
        return Err(unsupported(format!("{context} with JOIN")));
    }
    match table.relation {
        sa::TableFactor::Table { name, args, .. } => {
            if args.is_some() {
                return Err(unsupported(format!("{context} table function args")));
            }
            Ok(object_name_to_string(&name))
        }
        other => Err(unsupported(format!("{context} table factor `{other}`"))),
    }
}

fn convert_column_def(col: sa::ColumnDef) -> Result<kk::ColumnDef> {
    let mut out = kk::ColumnDef {
        name: col.name.value,
        data_type: convert_data_type(col.data_type),
        primary_key: false,
        autoincrement: false,
        not_null: false,
        unique: false,
        default: None,
        references: None,
        check_expr: None,
    };

    for option in col.options {
        match option.option {
            sa::ColumnOption::NotNull => out.not_null = true,
            sa::ColumnOption::PrimaryKey(_) => out.primary_key = true,
            sa::ColumnOption::Unique(_) => out.unique = true,
            sa::ColumnOption::Default(expr) => out.default = Some(convert_expr(expr)?),
            // L1: REFERENCES table(col) column-level constraint
            sa::ColumnOption::ForeignKey(fk) => {
                let table_name = fk.foreign_table
                    .0
                    .iter()
                    .filter_map(|part| match part {
                        sa::ObjectNamePart::Identifier(ident) => Some(ident.value.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                let ref_col = fk.referred_columns
                    .into_iter()
                    .next()
                    .map(|ident| ident.value);
                out.references = Some(kk::ForeignKeyRef {
                    table: table_name,
                    column: ref_col,
                });
            }
            // L2: CHECK (expr) column-level constraint
            sa::ColumnOption::Check(cc) => {
                if let Ok(expr) = convert_expr(*cc.expr) {
                    out.check_expr = Some(expr);
                }
            }
            sa::ColumnOption::DialectSpecific(tokens) => {
                for tok in tokens {
                    if tok.to_string().eq_ignore_ascii_case("AUTOINCREMENT") {
                        out.autoincrement = true;
                    }
                }
            }
            sa::ColumnOption::Identity(sa::IdentityPropertyKind::Autoincrement(_)) => {
                out.autoincrement = true;
            }
            sa::ColumnOption::Null
            | sa::ColumnOption::OnConflict(_)
            | sa::ColumnOption::Comment(_)
            | sa::ColumnOption::CharacterSet(_)
            | sa::ColumnOption::Collation(_) => {}
            _ => {}
        }
    }
    Ok(out)
}

/// Convert a top-level Query AST node to either a simple Select or a SetOp statement.
/// Handles UNION / INTERSECT / EXCEPT and correctly places ORDER BY / LIMIT on the
/// combined result (not on the right-side query).
fn convert_query_statement(query: sa::Query) -> Result<kk::Statement> {
    use super::query::convert_query_to_select;

    if query.fetch.is_some() {
        return Err(unsupported("FETCH at query level"));
    }
    if !query.locks.is_empty() {
        return Err(unsupported("FOR UPDATE/SHARE"));
    }
    if query.for_clause.is_some() {
        return Err(unsupported("FOR clause"));
    }
    // WITH CTE is handled in convert_query_to_select — do NOT block it here

    // Peel off ORDER BY / LIMIT / WITH that belong to the whole statement
    let top_order_by = query.order_by;
    let top_limit = query.limit_clause;
    let top_with = query.with;

    match *query.body {
        sa::SetExpr::SetOperation {
            op,
            set_quantifier,
            left,
            right,
        } => {
            // Bug #3 fix: ByName means UNION DISTINCT by column name — treat as DISTINCT
            let all = matches!(set_quantifier, sa::SetQuantifier::All);
            let kind = match op {
                sa::SetOperator::Union => {
                    if all { kk::SetOpKind::UnionAll } else { kk::SetOpKind::UnionDistinct }
                }
                sa::SetOperator::Intersect => {
                    if all { kk::SetOpKind::IntersectAll } else { kk::SetOpKind::IntersectDistinct }
                }
                sa::SetOperator::Except | sa::SetOperator::Minus => {
                    if all { kk::SetOpKind::ExceptAll } else { kk::SetOpKind::ExceptDistinct }
                }
            };

            // Bug #2 fix: left/right may themselves be SetOperations — recurse via
            // convert_query_statement if needed, then unwrap to SelectStmt.
            // For simplicity we flatten each side: if a side is a SetOperation it will
            // produce a nested SetOp Statement; we call exec_set_op recursively.
            // The left side never has ORDER BY / LIMIT.
            let left_query = sa::Query {
                with: None,
                body: left,
                order_by: None,
                limit_clause: None,
                fetch: None,
                locks: Vec::new(),
                for_clause: None,
                settings: None,
                format_clause: None,
                pipe_operators: Vec::new(),
            };
            // Bug #2 fix: right side also never has ORDER BY / LIMIT (those are on top)
            let right_query = sa::Query {
                with: None,
                body: right,
                order_by: None,
                limit_clause: None,
                fetch: None,
                locks: Vec::new(),
                for_clause: None,
                settings: None,
                format_clause: None,
                pipe_operators: Vec::new(),
            };

            let left_select = Box::new(convert_query_to_select(left_query)?);
            let right_select = Box::new(convert_query_to_select(right_query)?);

            // Bug #1 fix: ORDER BY / LIMIT go on SetOpStmt, applied after combining
            let (limit, offset) = if let Some(lc) = top_limit {
                match lc {
                    sa::LimitClause::LimitOffset { limit, offset, .. } => (
                        limit.map(|e| convert_expr(e)).transpose()?,
                        offset.map(|o| convert_expr(o.value)).transpose()?,
                    ),
                    sa::LimitClause::OffsetCommaLimit { offset, limit } => (
                        Some(convert_expr(limit)?),
                        Some(convert_expr(offset)?),
                    ),
                }
            } else {
                (None, None)
            };

            let order_by = if let Some(ob) = top_order_by {
                convert_order_by_items(ob)?
            } else {
                Vec::new()
            };

            Ok(kk::Statement::SetOp(kk::SetOpStmt {
                kind,
                left: left_select,
                right: right_select,
                order_by,
                limit,
                offset,
            }))
        }
        body => {
            // Simple SELECT with top-level ORDER BY / LIMIT / WITH (CTE)
            let assembled = sa::Query {
                with: top_with,  // preserve CTE definitions
                body: Box::new(body),
                order_by: top_order_by,
                limit_clause: top_limit,
                fetch: None,
                locks: Vec::new(),
                for_clause: None,
                settings: None,
                format_clause: None,
                pipe_operators: Vec::new(),
            };
            let select = convert_query_to_select(assembled)?;
            Ok(kk::Statement::Select(select))
        }
    }
}

// Helper: convert ORDER BY expressions for SetOpStmt
fn convert_order_by_items(order_by: sa::OrderBy) -> Result<Vec<kk::OrderByItem>> {
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

/// Public helper: convert a set expression (UNION/INTERSECT/EXCEPT) into a SetOpStmt.
/// Used by query.rs when a nested set operation appears as a sub-query body.
pub(crate) fn convert_set_expr_to_setop(
    op: sa::SetOperator,
    set_quantifier: sa::SetQuantifier,
    left: Box<sa::SetExpr>,
    right: Box<sa::SetExpr>,
) -> Result<kk::SetOpStmt> {
    let all = matches!(set_quantifier, sa::SetQuantifier::All);
    let kind = match op {
        sa::SetOperator::Union => {
            if all { kk::SetOpKind::UnionAll } else { kk::SetOpKind::UnionDistinct }
        }
        sa::SetOperator::Intersect => {
            if all { kk::SetOpKind::IntersectAll } else { kk::SetOpKind::IntersectDistinct }
        }
        sa::SetOperator::Except | sa::SetOperator::Minus => {
            if all { kk::SetOpKind::ExceptAll } else { kk::SetOpKind::ExceptDistinct }
        }
    };
    use super::query::convert_query_to_select;
    let left_select = Box::new(convert_query_to_select(sa::Query {
        with: None, body: left, order_by: None, limit_clause: None,
        fetch: None, locks: Vec::new(), for_clause: None,
        settings: None, format_clause: None, pipe_operators: Vec::new(),
    })?);
    let right_select = Box::new(convert_query_to_select(sa::Query {
        with: None, body: right, order_by: None, limit_clause: None,
        fetch: None, locks: Vec::new(), for_clause: None,
        settings: None, format_clause: None, pipe_operators: Vec::new(),
    })?);
    Ok(kk::SetOpStmt {
        kind,
        left: left_select,
        right: right_select,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}

fn convert_privileges(privs: sa::Privileges) -> Result<kk::PrivilegeList> {
    match privs {
        sa::Privileges::All { .. } => Ok(kk::PrivilegeList::All),
        sa::Privileges::Actions(actions) => {
            let mut lst = Vec::new();
            for act in actions {
                match act {
                    sa::Action::Select { .. } => lst.push("SELECT".to_string()),
                    sa::Action::Insert { .. } => lst.push("INSERT".to_string()),
                    sa::Action::Update { .. } => lst.push("UPDATE".to_string()),
                    sa::Action::Delete => lst.push("DELETE".to_string()),
                    sa::Action::References { .. } => lst.push("REFERENCES".to_string()),
                    sa::Action::Trigger => lst.push("TRIGGER".to_string()),
                    sa::Action::Truncate => lst.push("TRUNCATE".to_string()),
                    sa::Action::Connect => lst.push("CONNECT".to_string()),
                    sa::Action::Create { .. } => lst.push("CREATE".to_string()),
                    sa::Action::Execute { .. } => lst.push("EXECUTE".to_string()),
                    sa::Action::Usage => lst.push("USAGE".to_string()),
                    // Catch-all
                    _ => lst.push(format!("{:?}", act).to_uppercase()),
                }
            }
            Ok(kk::PrivilegeList::Specific(lst))
        }
    }
}

fn convert_grant_object(obj: sa::GrantObjects) -> Result<kk::GrantObject> {
    match obj {
        sa::GrantObjects::Tables(tables) => {
            let first = tables.first().ok_or_else(|| unsupported("GRANT without object"))?;
            Ok(kk::GrantObject::Table(object_name_to_string(first)))
        }
        sa::GrantObjects::Sequences(_) => Err(unsupported("GRANT ON SEQUENCE")),
        sa::GrantObjects::Schemas(schemas) => {
            let first = schemas.first().ok_or_else(|| unsupported("GRANT without object"))?;
            Ok(kk::GrantObject::Database(object_name_to_string(first)))
        }
        sa::GrantObjects::Databases(dbs) => {
            let first = dbs.first().ok_or_else(|| unsupported("GRANT without object"))?;
            Ok(kk::GrantObject::Database(object_name_to_string(first)))
        }
        _ => Err(unsupported("Unknown GRANT Object format")),
    }
}

