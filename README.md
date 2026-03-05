# KKDB

KKDB is a small SQLite-style database engine written in Rust.

## Documentation

- Project doc: [`docs/PROJECT.md`](docs/PROJECT.md)
- API doc: [`docs/API.md`](docs/API.md)
- Upgrade plan: [`docs/UPGRADE_PLAN.md`](docs/UPGRADE_PLAN.md)
- Storage reliability design: [`docs/COW_DOUBLE_SUPERBLOCK_DESIGN.md`](docs/COW_DOUBLE_SUPERBLOCK_DESIGN.md)
- Binlog design: [`docs/BINLOG_DESIGN.md`](docs/BINLOG_DESIGN.md)

## What it includes

- SQL tokenizer, parser, and AST
- VM executor for DDL, DML, and SELECT
- Pager + B-Tree storage engine
- In-memory and file-backed database modes
- REPL CLI

## Build and run

```bash
# build
cargo build

# run (in-memory)
cargo run

# run with a file database
cargo run -- mydb.db
```

## Library quick start

```rust
use kkdb::vm::execute::{ExecResult, VM};

let mut vm = VM::new_memory();
vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")?;
vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")?;

if let ExecResult::QueryResult { columns, rows } =
    vm.execute_sql("SELECT id, name FROM t1")?
{
    println!("{:?}", columns);
    println!("{:?}", rows);
}
```

## Test

```bash
cargo test
```

On some Windows environments, file-locking can make repeated test runs unstable.
Use the provided check script to isolate build output per run:

```powershell
.\scripts\check.ps1
```

## Repository layout

- `src/sql`: tokenizer, parser, AST
- `src/storage`: pager, B-Tree, cursor
- `src/schema.rs`: catalog and schema operations
- `src/vm`: SQL execution engine
- `src/main.rs`: REPL CLI
- `tests`: integration and perf-style tests

## Notes

- Page size is 4096 bytes.
- Page 1 stores schema metadata.
- Transactions currently support `BEGIN` / `COMMIT` / `ROLLBACK`.
