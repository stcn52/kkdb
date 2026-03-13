# KKDB

<p align="center">
  <strong>A fully-featured relational database engine written in Rust</strong>
</p>

<p align="center">
  <em>~57,000 lines of Rust · 91 modules · 5,000+ tests · Zero external DB dependencies</em>
</p>

<p align="center">
  <a href="README.md">English</a> ·
  <a href="README.zh-CN.md">中文</a>
</p>

---

## Highlights

| Category | Features |
|----------|----------|
| **SQL** | Full DDL/DML/SELECT · 70+ built-in functions · Window functions · CTEs (recursive) · Subqueries · Set operations · JSON functions |
| **Storage** | COW dual-superblock Pager · B-Tree (SQLite-compatible format) · WAL · Buffer Pool (LRU-K) · Bloom Filter · LZ4/Zstd compression |
| **Transactions** | MVCC snapshot isolation · Table/row-level locking · Deadlock detection · SAVEPOINT · 2PC/3PC distributed transactions |
| **Full-Text Search** | BM25 inverted index · jieba-rs Chinese tokenizer · Fuzzy search · Synonym expansion · Faceted search |
| **Vector Search** | HNSW approximate nearest neighbors · Cosine/L2 distance · Multi-index management · Quantization |
| **Security** | RBAC permissions · Row-Level Security (RLS) · Column-level encryption (AES/ChaCha20) · Audit log · Data masking |
| **Network** | MySQL Wire Protocol v10 · Supabase-style HTTP REST API (JWT auth + multi-tenancy) |
| **Distributed** | openraft v0.9 Raft consensus · Automatic failover · Consistent-hash sharding · Node discovery · Service mesh |

## Quick Start

```bash
# Build
cargo build --release

# In-memory REPL
cargo run --release

# File-persistent mode
cargo run --release -- mydb

# Start MySQL + HTTP server
cargo run --release -- --server mydb --port 3306 --http-port 6543 --mysql-port 3307
```

### Docker

```bash
# Build & run
docker build -t kkdb .
docker run -d -p 3306:3306 -p 3307:3307 -p 6543:6543 -v kkdb-data:/data kkdb

# Docker Compose
docker compose up -d

# 3-node Raft cluster
docker compose --profile cluster up -d
```

### Minimal Rust API

```rust
use kkdb::vm::execute::{ExecResult, VM};

let mut vm = VM::new_memory();
vm.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")?;
vm.execute_sql("INSERT INTO users VALUES (1, 'Alice')")?;

if let ExecResult::QueryResult { columns, rows } =
    vm.execute_sql("SELECT * FROM users")?
{
    println!("{columns:?}"); // ["id", "name"]
    for row in &rows { println!("{row:?}"); }
}
```

### Minimal SQL

```sql
CREATE TABLE products (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    price REAL DEFAULT 0.0,
    category TEXT
);

INSERT INTO products (name, price, category) VALUES ('Widget', 29.99, 'tools');

SELECT category, COUNT(*), AVG(price)
FROM products
GROUP BY category
HAVING COUNT(*) > 0;
```

### MySQL Client

```bash
mysql -h 127.0.0.1 -P 3307
```

### HTTP REST API

```bash
curl -X POST http://localhost:8080/rest/query \
  -H 'Authorization: Bearer <token>' \
  -H 'Content-Type: application/json' \
  -d '{"sql": "SELECT * FROM users"}'
```

## Documentation

### 📖 User Guides

| Document | Description |
|----------|-------------|
| **[Complete Manual](docs/USAGE.md)** | **Full reference for all features (31 chapters, 1800 lines)** |
| [Advanced SQL](docs/ADVANCED_SQL.md) | Window functions, CTEs, subqueries, RLS, full-text search |
| [Built-in Functions](docs/FUNCTIONS.md) | 70+ functions: aggregate, string, math, date, JSON |
| [Rust API Reference](docs/API.md) | Public crate interface, VM usage, type system |
| [HTTP REST API](docs/HTTP_API.md) | Supabase-style endpoints, JWT auth, multi-tenancy |
| [MySQL Protocol Server](docs/MYSQL_SERVER.md) | Wire Protocol v10, COM commands, compatibility |
| [Distributed Cluster (Raft)](docs/DISTRIBUTED.md) | Cluster setup, Raft HTTP API, snapshots, membership changes |
| [Use Cases](docs/EXAMPLES.md) | E-commerce, CMS, log analytics, AI vector search, multi-tenant SaaS, IoT |
| [Deployment Guide](docs/DEPLOYMENT.md) | Standalone/cluster deployment, Docker, systemd, production config |

### 🏗️ Architecture & Design

| Document | Description |
|----------|-------------|
| [Architecture Deep Dive](docs/ARCHITECTURE.md) | B-Tree, COW, MVCC, BM25, HNSW, Raft internals |
| [Project Overview](docs/PROJECT.md) | Architecture, module structure, design decisions |
| [COW Dual-Superblock Design](docs/COW_DOUBLE_SUPERBLOCK_DESIGN.md) | Crash-safe storage engine design |
| [Vector Search Design](docs/VECTOR_SEARCH_DESIGN.md) | HNSW storage model and SQL interface |
| [Binlog Design](docs/BINLOG_DESIGN.md) | PITR / replication / audit log format |
| [SQL Parser Refactor](docs/SQLPARSER_REFACTOR_ANALYSIS.md) | sqlparser-rs migration analysis |

### 🗺️ Roadmap

| Document | Description |
|----------|-------------|
| [Upgrade Plan](docs/UPGRADE_PLAN.md) | CoW + dual-superblock + Binlog upgrade phases |
| [Optimization Roadmap](docs/optimization_roadmap.md) | Performance optimization checklist |
| [Task Checklist](docs/task.md) | Pending / completed feature checklist |

## Testing

```bash
# Run all tests (~5000 tests)
cargo test

# Library tests only
cargo test --lib

# Windows
.\scripts\check.ps1
```

## Project Structure

```
src/
├── main.rs                   # Interactive REPL entry point
├── lib.rs                    # Crate root
├── types.rs                  # DataType / Value / Row
├── schema.rs                 # TableSchema / ColumnInfo
├── error.rs                  # KkdbError (17 variants)
├── varint.rs                 # LEB128 / ZigZag encoding
├── sql/                      # SQL Parser
│   ├── ast.rs                #   AST node definitions
│   ├── parser.rs             #   parse_sql() entry
│   └── sqlparser_adapter/    #   sqlparser crate adapter
├── storage/                  # Storage Engine
│   ├── pager.rs              #   COW v2 dual-superblock Pager
│   ├── btree.rs              #   B-Tree (SQLite format)
│   ├── wal.rs                #   Write-Ahead Log
│   ├── cursor.rs             #   B-Tree cursor
│   ├── buffer_pool.rs        #   LRU-K(2) buffer pool
│   ├── bloom.rs              #   Bloom Filter
│   └── ext/                  #   Storage extensions
├── vm/                       # Virtual Machine
│   ├── execute.rs            #   VM core (new_memory / open)
│   ├── exec_ddl.rs           #   DDL executor
│   ├── exec_dml.rs           #   DML executor (+ FK + MVCC)
│   ├── exec_select.rs        #   SELECT pipeline (JOIN/CTE/Window)
│   ├── eval_expr.rs          #   Expression evaluator + 70+ functions
│   ├── mvcc.rs               #   MVCC Undo Log
│   ├── optimizer/            #   Query optimizer
│   ├── engine/               #   Execution engine extensions
│   ├── auth/                 #   RBAC / audit / security
│   └── monitor/              #   Monitoring / observability
├── fulltext/                 # Full-Text Search (BM25 + jieba-rs)
├── vector/                   # Vector Search (HNSW)
├── raft/                     # Raft Distributed Consensus
│   ├── node.rs               #   KkdbNode wrapper
│   ├── log_store.rs          #   WAL-persisted Raft log
│   └── features/             #   HA / 2PC / sharding / service mesh
├── server/                   # Network Servers
│   ├── mysql.rs              #   MySQL Wire Protocol v10
│   └── http_api.rs           #   HTTP REST API (axum)
├── binlog/                   # Binlog replication log
└── bin/                      # CLI tools
    ├── kkdb-cli.rs           #   Backup/restore/import/export
    └── big_data_bench.rs     #   Benchmarks
tests/                        # Integration tests
docs/                         # Documentation
scripts/                      # Build/check scripts
```

## On-Disk Layout

```
mydb/
  catalog.kkdb   ← Schema metadata (all CREATE statements + root pages)
  users.kkdb     ← users table data B-Tree
  binlog.bin     ← Binlog replication log
```

- Page size 4096 bytes (configurable 512–65536)
- Each table has its own `.kkdb` file; schema stored in `catalog.kkdb`
- Legacy single-file format (`.db`) backwards-compatible; `VM::open` auto-detects
- LZ4/Zstd page-level compression supported

## License

MIT
