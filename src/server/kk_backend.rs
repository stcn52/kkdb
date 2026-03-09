use crate::types::Value;
use crate::vm::execute::{ExecResult, VM};
use msql_srv::{
    Column, ColumnFlags, ColumnType, ErrorKind, InitWriter, MysqlShim as KkShim, ParamParser,
    QueryResultWriter, StatementMetaWriter,
};
use std::sync::{Arc, Mutex};

pub struct KkdbBackend {
    pub vm: Arc<Mutex<VM>>,
    pub current_user: Option<String>,
}

impl<W: std::io::Read + std::io::Write> KkShim<W> for KkdbBackend {
    type Error = std::io::Error;

    fn on_prepare<'a>(
        &mut self,
        _query: &'a str,
        info: StatementMetaWriter<'a, W>,
    ) -> std::io::Result<()> {
        info.reply(0, &[], &[])?;
        Ok(())
    }

    fn on_execute<'a>(
        &mut self,
        _id: u32,
        _params: ParamParser<'a>,
        results: QueryResultWriter<'a, W>,
    ) -> std::io::Result<()> {
        results.completed(0, 0)?;
        Ok(())
    }

    fn on_close(&mut self, _id: u32) {}

    fn on_query<'a>(
        &mut self,
        query: &'a str,
        results: QueryResultWriter<'a, W>,
    ) -> std::io::Result<()> {
        let mut vm = self.vm.lock().unwrap();
        match vm.execute_sql(query) {
            Ok(ExecResult::QueryResult { columns, rows }) => {
                let cols: Vec<Column> = columns
                    .iter()
                    .map(|c| Column {
                        table: "".into(),
                        column: c.clone().into(),
                        coltype: ColumnType::MYSQL_TYPE_VAR_STRING,
                        colflags: ColumnFlags::empty(),
                    })
                    .collect();

                let mut rw = results.start(&cols)?;
                for row in rows {
                    for val in &row {
                        match val {
                            Value::Integer(i) => rw.write_col(*i)?,
                            Value::Real(f) => rw.write_col(*f)?,
                            Value::Text(t) => rw.write_col(&**t)?,
                            Value::Blob(b) => rw.write_col(b.as_slice())?,
                            Value::Null => rw.write_col(None::<i32>)?, // simplified
                        }
                    }
                    rw.end_row()?;
                }
                rw.finish()?;
            }
            Ok(ExecResult::RowsAffected { count, .. }) => {
                results.completed(count as u64, 0)?;
            }
            Ok(ExecResult::Ok { .. }) => {
                results.completed(0, 0)?;
            }
            Ok(ExecResult::Explain { plan }) => {
                let cols = vec![Column {
                    table: "".into(),
                    column: "plan".into(),
                    coltype: ColumnType::MYSQL_TYPE_VAR_STRING,
                    colflags: ColumnFlags::empty(),
                }];
                let mut rw = results.start(&cols)?;
                rw.write_col(plan)?;
                rw.end_row()?;
                rw.finish()?;
            }
            Err(e) => {
                results.error(ErrorKind::ER_UNKNOWN_ERROR, format!("{}", e).as_bytes())?;
            }
        }
        Ok(())
    }

    fn on_init<'a>(
        &mut self,
        _database: &'a str,
        writer: InitWriter<'a, W>,
    ) -> std::io::Result<()> {
        writer.ok()?;
        Ok(())
    }

    fn after_authentication(
        &mut self,
        context: &msql_srv::AuthenticationContext<'_>,
    ) -> std::io::Result<()> {
        if let Some(user_bytes) = &context.username {
            let username = String::from_utf8_lossy(user_bytes).into_owned();
            let mut vm = self.vm.lock().unwrap();

            // Allow root by default, otherwise check `kkdb_users` table
            let mut is_valid = username == "root";
            if !is_valid {
                let query = format!(
                    "SELECT username FROM kkdb_users WHERE username = '{}'",
                    username
                );
                if let Ok(ExecResult::QueryResult { rows, .. }) = vm.execute_sql(&query) {
                    if !rows.is_empty() {
                        is_valid = true;
                    }
                }
            }

            if is_valid {
                self.current_user = Some(username.clone());
                // Inject into session_vars so auth.uid() / current_user() work
                // for MySQL-protocol clients, mirroring what the HTTP JWT path does.
                vm.session_vars
                    .insert("kkdb.current_user".to_string(), username.clone());
                vm.session_vars
                    .insert("request.jwt.sub".to_string(), username);
                return Ok(());
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("Access denied for user '{}'", username),
                ));
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "No username provided",
        ))
    }
}
