# KKDB

KKDB is a small SQLite-style database engine written in Rust.

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
