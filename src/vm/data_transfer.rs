use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use crate::error::{KkdbError, Result};
use crate::storage::btree::BTree;
use crate::types::Value;
use crate::vm::execute::{ExecResult, VM};

impl VM {
    /// Backup the entire database to a SQL dump file.
    pub fn backup(&mut self, file_path: &str) -> Result<()> {
        let mut file = BufWriter::new(File::create(file_path).map_err(KkdbError::Io)?);

        writeln!(file, "BEGIN TRANSACTION;").map_err(KkdbError::Io)?;

        // 1. Scan catalog for all DDL definitions
        let mut obj_sqls = Vec::new();
        {
            let schema_root = self.pager.schema_root_page();
            let mut btree = BTree::new(&mut self.pager);
            let rows = btree.scan_all(schema_root)?;
            for (_, row) in rows {
                if row.len() >= 5 {
                    if let Value::Text(sql) = &row[4] {
                        let obj_type = match &row[0] {
                            Value::Text(t) => t.to_string(),
                            _ => continue,
                        };
                        let name = match &row[1] {
                            Value::Text(n) => n.to_string(),
                            _ => continue,
                        };
                        
                        obj_sqls.push((obj_type, name, sql.to_string()));
                    }
                }
            }
        }

        // 2. Write tables first, then data, then indexes/views
        for (obj_type, name, sql) in &obj_sqls {
            if obj_type == "table" {
                writeln!(file, "{};", sql).map_err(KkdbError::Io)?;

                // Dump data for this table
                let query = format!("SELECT * FROM {};", name);
                let result = self.execute_sql(&query)?;
                if let ExecResult::QueryResult { rows, .. } = result {
                    for row in rows {
                        let values_str = row
                            .iter()
                            .map(|v| match v {
                                Value::Null => "NULL".to_string(),
                                Value::Integer(i) => i.to_string(),
                                Value::Real(f) => format!("{}", f),
                                Value::Text(t) => {
                                    // Escape single quotes
                                    format!("'{}'", t.replace("'", "''"))
                                }
                                Value::Blob(b) => {
                                    let hex: String =
                                        b.iter().map(|byte| format!("{:02X}", byte)).collect();
                                    format!("X'{}'", hex)
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        writeln!(file, "INSERT INTO {} VALUES ({});", name, values_str)
                            .map_err(KkdbError::Io)?;
                    }
                }
            }
        }

        // Write indexes and views
        for (obj_type, _name, sql) in &obj_sqls {
            if obj_type == "index" || obj_type == "view" {
                writeln!(file, "{};", sql).map_err(KkdbError::Io)?;
            }
        }

        writeln!(file, "COMMIT;").map_err(KkdbError::Io)?;
        file.flush().map_err(KkdbError::Io)?;

        Ok(())
    }

    /// Restore the database from a SQL dump file.
    pub fn restore(&mut self, file_path: &str) -> Result<()> {
        let file = File::open(file_path).map_err(KkdbError::Io)?;
        let reader = BufReader::new(file);

        let mut current_stmt = String::new();
        for line in reader.lines() {
            let line = line.map_err(KkdbError::Io)?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("--") {
                continue;
            }

            current_stmt.push_str(&line);
            current_stmt.push('\n');

            if trimmed.ends_with(';') {
                self.execute_sql(&current_stmt)?;
                current_stmt.clear();
            }
        }

        Ok(())
    }

    /// Export a table to a CSV file.
    pub fn export_csv(&mut self, table_name: &str, file_path: &str) -> Result<()> {
        let query = format!("SELECT * FROM {};", table_name);
        if let ExecResult::QueryResult { columns, rows } = self.execute_sql(&query)? {
            let mut file = BufWriter::new(File::create(file_path).map_err(KkdbError::Io)?);

            // Write Header
            writeln!(file, "{}", columns.join(",")).map_err(KkdbError::Io)?; // simplified, assumes no comma in col names

            // Write Rows
            for row in rows {
                let csv_line = row
                    .iter()
                    .map(|v| match v {
                        Value::Null => "".to_string(),
                        Value::Integer(i) => i.to_string(),
                        Value::Real(f) => format!("{}", f),
                        Value::Text(t) => {
                            if t.contains(',') || t.contains('"') || t.contains('\n') {
                                format!("\"{}\"", t.replace("\"", "\"\""))
                            } else {
                                t.to_string()
                            }
                        }
                        Value::Blob(b) => {
                            let hex: String =
                                b.iter().map(|byte| format!("{:02X}", byte)).collect();
                            format!("X'{}'", hex)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                writeln!(file, "{}", csv_line).map_err(KkdbError::Io)?;
            }
            file.flush().map_err(KkdbError::Io)?;
            Ok(())
        } else {
            Err(KkdbError::Internal("Export query failed".into()))
        }
    }

    /// Import a CSV file into a table.
    /// Very basic CSV parser: assumes comma-delimited and double-quote escaping.
    pub fn import_csv(&mut self, file_path: &str, table_name: &str) -> Result<()> {
        let file = File::open(file_path).map_err(KkdbError::Io)?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        let _header_line = match lines.next() {
            Some(Ok(l)) => l,
            _ => return Err(KkdbError::Internal("Empty CSV file".into())),
        };

        // For simplicity, we assume the CSV header exactly matches the table schema columns in order.
        // We will insert by positional values.
        self.execute_sql("BEGIN TRANSACTION;")?;

        let mut insert_sql = format!("INSERT INTO {} VALUES (", table_name);
        let schema = self.schema.get_table(table_name)?;
        let expected_cols = schema.columns.len();
        
        for _ in 0..expected_cols {
             insert_sql.push_str("?, ");
        }
        
        for line in lines {
            let line = line.map_err(KkdbError::Io)?;
            if line.trim().is_empty() {
                continue;
            }
            
            // Very naive split. 
            let mut fields = Vec::new();
            let mut current = String::new();
            let mut in_quotes = false;
            let mut chars = line.chars().peekable();
            
            while let Some(c) = chars.next() {
                if c == '"' {
                    if in_quotes && chars.peek() == Some(&'"') {
                        current.push('"');
                        chars.next();
                    } else {
                        in_quotes = !in_quotes;
                    }
                } else if c == ',' && !in_quotes {
                    fields.push(current.clone());
                    current.clear();
                } else {
                    current.push(c);
                }
            }
            fields.push(current);

            // Construct VALUES clause literal
            let val_literals = fields.into_iter().map(|f| {
                if f.is_empty() {
                    "NULL".to_string()
                } else if f.parse::<i64>().is_ok() || f.parse::<f64>().is_ok() {
                    f
                } else {
                    format!("'{}'", f.replace("'", "''"))
                }
            }).collect::<Vec<_>>().join(", ");

            let stmt = format!("INSERT INTO {} VALUES ({});", table_name, val_literals);
            if let Err(e) = self.execute_sql(&stmt) {
                let _ = self.execute_sql("ROLLBACK;");
                return Err(e);
            }
        }

        self.execute_sql("COMMIT;")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::vm::execute::{ExecResult, VM};
    use std::fs;

    #[test]
    fn test_csv_export_import_edge_cases() {
        let mut vm = VM::new_memory();
        vm.execute_sql("CREATE TABLE test_csv (id INTEGER PRIMARY KEY, msg TEXT, amt REAL);").unwrap();
        
        // CSV Edge cases
        vm.execute_sql("INSERT INTO test_csv VALUES (1, 'Hello, world', 10.5);").unwrap();
        vm.execute_sql("INSERT INTO test_csv VALUES (2, 'Quotes \"inside\" correctly', 20.0);").unwrap();
        vm.execute_sql("INSERT INTO test_csv VALUES (3, NULL, NULL);").unwrap();

        let csv_path = "unit_test_edge_cases.csv";
        let _ = fs::remove_file(csv_path);

        assert!(vm.export_csv("test_csv", csv_path).is_ok());

        let mut vm2 = VM::new_memory();
        vm2.execute_sql("CREATE TABLE test_csv (id INTEGER PRIMARY KEY, msg TEXT, amt REAL);").unwrap();
        assert!(vm2.import_csv(csv_path, "test_csv").is_ok());

        let res = vm2.execute_sql("SELECT msg, amt FROM test_csv ORDER BY id;").unwrap();
        if let ExecResult::QueryResult { rows, .. } = res {
            assert_eq!(rows.len(), 3);
            assert!(matches!(&rows[0][0], crate::types::Value::Text(t) if t.as_ref() == "Hello, world"));
            assert!(matches!(&rows[1][0], crate::types::Value::Text(t) if t.as_ref() == "Quotes \"inside\" correctly"));
            assert!(matches!(&rows[2][0], crate::types::Value::Null));
        } else {
            panic!("Expected query result");
        }

        let _ = fs::remove_file(csv_path);
    }

    #[test]
    fn test_backup_restore_in_memory() {
        let mut vm = VM::new_memory();
        vm.execute_sql("CREATE TABLE units (val INTEGER);").unwrap();
        vm.execute_sql("INSERT INTO units VALUES (999);").unwrap();

        let backup_path = "unit_test_backup.sql";
        let _ = fs::remove_file(backup_path);

        assert!(vm.backup(backup_path).is_ok());

        let mut vm2 = VM::new_memory();
        assert!(vm2.restore(backup_path).is_ok());

        let res = vm2.execute_sql("SELECT * FROM units;").unwrap();
        if let ExecResult::QueryResult { rows, .. } = res {
            assert_eq!(rows.len(), 1);
            assert!(matches!(&rows[0][0], crate::types::Value::Integer(999)));
        } else {
            panic!("Expected query result");
        }

        let _ = fs::remove_file(backup_path);
    }
}
