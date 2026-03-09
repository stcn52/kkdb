use kkdb::vm::execute::VM;
use std::env;
use std::process;

fn print_usage() {
    println!("kkdb-cli - Command line utility for KKDB data transfer");
    println!("\nUsage:");
    println!("  kkdb-cli backup <db_path> <output.sql>");
    println!("  kkdb-cli restore <db_path> <input.sql>");
    println!("  kkdb-cli export <db_path> <table> <output.csv>");
    println!("  kkdb-cli import <db_path> <table> <input.csv>");
    process::exit(1);
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        print_usage();
    }

    let command = args[1].as_str();
    let db_path = args[2].as_str();

    // Initialize VM
    let mut vm = match VM::open(db_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error opening database at '{}': {:?}", db_path, e);
            process::exit(1);
        }
    };

    match command {
        "backup" => {
            if args.len() != 4 {
                print_usage();
            }
            let output_file = args[3].as_str();
            println!("Backing up database to {}...", output_file);
            if let Err(e) = vm.backup(output_file) {
                eprintln!("Failed to backup: {:?}", e);
                process::exit(1);
            }
            println!("Backup completed successfully.");
        }
        "restore" => {
            if args.len() != 4 {
                print_usage();
            }
            let input_file = args[3].as_str();
            println!("Restoring database from {}...", input_file);
            if let Err(e) = vm.restore(input_file) {
                eprintln!("Failed to restore: {:?}", e);
                process::exit(1);
            }
            println!("Restore completed successfully.");
        }
        "export" => {
            if args.len() != 5 {
                print_usage();
            }
            let table_name = args[3].as_str();
            let output_file = args[4].as_str();
            println!("Exporting table '{}' to {}...", table_name, output_file);
            if let Err(e) = vm.export_csv(table_name, output_file) {
                eprintln!("Failed to export: {:?}", e);
                process::exit(1);
            }
            println!("Export completed successfully.");
        }
        "import" => {
            if args.len() != 5 {
                print_usage();
            }
            let table_name = args[3].as_str();
            let input_file = args[4].as_str();
            println!(
                "Importing data from {} into table '{}'...",
                input_file, table_name
            );
            if let Err(e) = vm.import_csv(input_file, table_name) {
                eprintln!("Failed to import: {:?}", e);
                process::exit(1);
            }
            println!("Import completed successfully.");
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            print_usage();
        }
    }
}
