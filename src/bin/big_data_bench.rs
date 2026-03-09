use kkdb::types::Value;
use kkdb::vm::execute::VM;
use std::time::Instant;

fn main() {
    let db_path = "big_bench.db";
    // Setup clean environment
    let _ = std::fs::remove_file(db_path);
    let _ = std::fs::remove_file(format!("{}.binlog", db_path));

    let mut vm = VM::open(db_path).expect("Failed to open DB");

    println!("Creating table `huge_table`...");
    vm.execute_sql("CREATE TABLE huge_table (id INTEGER PRIMARY KEY, category TEXT, payload TEXT, value INTEGER)")
        .unwrap();

    let total_rows = 10_000_000;
    let batch_size = 10_000;

    println!(
        "Starting ingestion of {} rows in batches of {}...",
        total_rows, batch_size
    );
    let start = Instant::now();

    // 10M rows takes some time. Using transactions minimizes `fsync` overhead!
    for batch in 0..(total_rows / batch_size) {
        vm.execute_sql("BEGIN").unwrap();

        let start_id = batch * batch_size + 1;
        let end_id = (batch + 1) * batch_size;

        let mut batch_rows = Vec::with_capacity(batch_size);
        for i in start_id..=end_id {
            batch_rows.push(vec![
                Value::Integer(i as i64),
                Value::Text(format!("cat_{}", i % 100).into()),
                Value::Text(format!("payload_data_{}", i).into()),
                Value::Integer((i * 3 % 1000) as i64),
            ]);
        }
        vm.insert_batch_raw("huge_table", batch_rows).unwrap();

        vm.execute_sql("COMMIT").unwrap();

        if (batch + 1) % 100 == 0 {
            let elapsed = start.elapsed().as_secs_f64();
            let rows_inserted = (batch + 1) * batch_size;
            let rows_per_sec = rows_inserted as f64 / elapsed;
            println!(
                "  Inserted {:>8} / {} rows ({:>8.0} rows/sec)",
                rows_inserted, total_rows, rows_per_sec
            );
        }
    }

    let total_time = start.elapsed().as_secs_f64();
    println!("========================================");
    println!("Ingestion Complete!");
    println!("Total time: {:.2} seconds", total_time);
    println!(
        "Average speed: {:.0} rows/sec",
        total_rows as f64 / total_time
    );

    // Query phase
    println!("\nStarting Query Phase...");

    // We execute some queries to ensure looking up in a 10M row table works efficiently via index.
    let q_start = Instant::now();
    vm.execute_sql("SELECT * FROM huge_table WHERE id = 5000000")
        .unwrap();
    println!("Point lookup (id=5000000) took: {:.2?}", q_start.elapsed());

    let q_start = Instant::now();
    vm.execute_sql("SELECT * FROM huge_table WHERE id = 9999999")
        .unwrap();
    println!("Point lookup (id=9999999) took: {:.2?}", q_start.elapsed());

    // Get file sizes
    if let Ok(metadata) = std::fs::metadata(db_path) {
        println!(
            "Final DB file size: {:.2} MB",
            metadata.len() as f64 / 1_048_576.0
        );
    }
    if let Ok(metadata) = std::fs::metadata(format!("{}.binlog", db_path)) {
        println!(
            "Final Binlog file size: {:.2} MB",
            metadata.len() as f64 / 1_048_576.0
        );
    }

    println!("Done.");
}
