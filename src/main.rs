use kkdb::storage::btree::BTree;
use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::env;

fn print_query_result(columns: &[String], rows: &[Vec<kkdb::types::Value>]) {
    if columns.is_empty() && rows.is_empty() {
        return;
    }

    // Calculate column widths
    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    for row in rows {
        for (i, val) in row.iter().enumerate() {
            if i < widths.len() {
                let w = format!("{}", val).len();
                if w > widths[i] {
                    widths[i] = w;
                }
            }
        }
    }

    // Print header
    let header: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{:width$}", c, width = widths[i]))
        .collect();
    println!("{}", header.join(" | "));

    // Print separator
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    println!("{}", sep.join("-+-"));

    // Print rows
    for row in rows {
        let vals: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let s = format!("{}", v);
                if i < widths.len() {
                    format!("{:width$}", s, width = widths[i])
                } else {
                    s
                }
            })
            .collect();
        println!("{}", vals.join(" | "));
    }

    println!("({} rows)", rows.len());
}

fn print_help() {
    println!("KKDB - A SQLite implementation in Rust");
    println!();
    println!("Commands:");
    println!("  .help              Show this help message");
    println!("  .quit / .exit      Exit the program");
    println!("  .tables            List all tables");
    println!("  .schema [TABLE]    Show CREATE statement for tables");
    println!("  .open FILE         Open a database file");
    println!("  .memory            Switch to in-memory database");
    println!();
    println!("SQL Statements:");
    println!("  CREATE TABLE name (col1 TYPE, col2 TYPE, ...)");
    println!("  DROP TABLE [IF EXISTS] name");
    println!("  INSERT INTO name [(cols)] VALUES (vals), ...");
    println!("  SELECT cols FROM table [WHERE ...] [ORDER BY ...] [LIMIT n]");
    println!("  UPDATE name SET col=val [WHERE ...]");
    println!("  DELETE FROM name [WHERE ...]");
    println!("  EXPLAIN statement");
}

fn handle_dot_command(vm: &mut VM, cmd: &str) -> Option<VM> {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let command = parts[0].to_lowercase();

    match command.as_str() {
        ".help" => {
            print_help();
            None
        }
        ".quit" | ".exit" => {
            std::process::exit(0);
        }
        ".tables" => {
            let mut names: Vec<&String> = vm.schema.tables.keys().collect();
            names.sort();
            for name in names {
                println!("{}", name);
            }
            None
        }
        ".schema" => {
            let filter = parts.get(1).map(|s| s.trim());
            let mut entries: Vec<(String, String)> = Vec::new();

            // Prefer persisted schema catalog so output includes exact SQL.
            let schema_root = vm.pager.schema_root_page();
            let mut btree = BTree::new(&mut vm.pager);
            match btree.scan_all(schema_root) {
                Ok(rows) => {
                    for (_rowid, row) in rows {
                        if row.len() < 5 {
                            continue;
                        }
                        let name = match &row[1] {
                            Value::Text(v) => v.to_string(),
                            _ => continue,
                        };
                        let sql = match &row[4] {
                            Value::Text(v) => v.to_string(),
                            _ => continue,
                        };
                        if let Some(f) = filter {
                            if !name.eq_ignore_ascii_case(f) {
                                continue;
                            }
                        }
                        entries.push((name, sql));
                    }
                }
                Err(_) => {
                    // Fallback: reconstruct from in-memory schema cache.
                    for (name, table) in &vm.schema.tables {
                        if let Some(f) = filter {
                            if !name.eq_ignore_ascii_case(f) {
                                continue;
                            }
                        }
                        let cols: Vec<String> = table
                            .columns
                            .iter()
                            .map(|c| {
                                let mut s = format!("{} {}", c.name, c.data_type);
                                if c.primary_key {
                                    s.push_str(" PRIMARY KEY");
                                }
                                if c.autoincrement {
                                    s.push_str(" AUTOINCREMENT");
                                }
                                if c.not_null {
                                    s.push_str(" NOT NULL");
                                }
                                if c.unique {
                                    s.push_str(" UNIQUE");
                                }
                                s
                            })
                            .collect();
                        entries.push((
                            name.clone(),
                            format!("CREATE TABLE {} ({})", name, cols.join(", ")),
                        ));
                    }
                }
            }

            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (_name, sql) in entries {
                let text = sql.trim();
                if text.ends_with(';') {
                    println!("{}", text);
                } else {
                    println!("{};", text);
                }
            }
            None
        }
        ".open" => {
            if let Some(path) = parts.get(1) {
                match VM::open(path.trim()) {
                    Ok(new_vm) => {
                        println!("Opened database: {}", path.trim());
                        Some(new_vm)
                    }
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        None
                    }
                }
            } else {
                eprintln!("Usage: .open FILENAME");
                None
            }
        }
        ".memory" => {
            println!("Switched to in-memory database");
            Some(VM::new_memory())
        }
        _ => {
            eprintln!("Unknown command: {}. Use .help for help.", command);
            None
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut vm = if args.len() > 1 {
        match VM::open(&args[1]) {
            Ok(vm) => {
                println!("KKDB v0.1.0 - opened '{}'", args[1]);
                vm
            }
            Err(e) => {
                eprintln!("Error opening '{}': {}", args[1], e);
                std::process::exit(1);
            }
        }
    } else {
        println!("KKDB v0.1.0 - in-memory database");
        VM::new_memory()
    };

    println!("Type .help for usage hints, .quit to exit.");
    println!();

    let mut rl = DefaultEditor::new().expect("Failed to create line editor");
    let mut multi_line_buf = String::new();

    loop {
        let prompt = if multi_line_buf.is_empty() {
            "kkdb> "
        } else {
            "  ... "
        };

        match rl.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim();

                if trimmed.is_empty() {
                    continue;
                }

                // Dot commands
                if trimmed.starts_with('.') && multi_line_buf.is_empty() {
                    let _ = rl.add_history_entry(trimmed);
                    if let Some(new_vm) = handle_dot_command(&mut vm, trimmed) {
                        vm = new_vm;
                    }
                    continue;
                }

                multi_line_buf.push_str(trimmed);
                multi_line_buf.push(' ');

                // Check if statement is complete (ends with semicolon)
                if !trimmed.ends_with(';') {
                    continue;
                }

                let sql = multi_line_buf.trim().to_string();
                multi_line_buf.clear();

                let _ = rl.add_history_entry(&sql);

                match vm.execute_sql(&sql) {
                    Ok(result) => match result {
                        ExecResult::Ok { message } => println!("{}", message),
                        ExecResult::RowsAffected { message, .. } => println!("{}", message),
                        ExecResult::QueryResult { columns, rows } => {
                            print_query_result(&columns, &rows);
                        }
                        ExecResult::Explain { plan } => print!("{}", plan),
                    },
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
            Err(ReadlineError::Interrupted) => {
                multi_line_buf.clear();
                println!("^C");
            }
            Err(ReadlineError::Eof) => {
                println!("Bye!");
                break;
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        }
    }
}
