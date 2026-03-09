#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# KKDB 向量搜索基准测试脚本
# 测试 VEC_SEARCH 在不同规模下的插入/搜索吞吐量
# ─────────────────────────────────────────────────────────────────────────────

set -euo pipefail

DB_DIR="$(mktemp -d /tmp/kkdb_bench_XXXXXX)"
BIN="./target/release/kkdb"
DIM=128
trap 'rm -rf "$DB_DIR"' EXIT

echo "=== KKDB Vector Search Benchmark ==="
echo "DIM=${DIM}  DB=${DB_DIR}"
echo

# Build release binary if not present
if [ ! -f "$BIN" ]; then
  echo "[build] cargo build --release ..."
  cargo build --release -q
fi

# Generate a random f32 vector literal for SQL
random_vec() {
  python3 -c "
import random, sys
n = int(sys.argv[1])
vals = [f'{random.uniform(-1,1):.4f}' for _ in range(n)]
print(\"VEC('[\" + ','.join(vals) + \"]')\")
" "$1"
}

# ── Phase 1: INSERT benchmark ──────────────────────────────────────────────────
echo "--- INSERT benchmark ---"
ROWS="${1:-1000}"
echo "Inserting ${ROWS} rows (DIM=${DIM})..."

# Build a batch INSERT SQL file
TMPFILE="$(mktemp /tmp/kkdb_bench_sql_XXXXXX.sql)"

{
  echo "CREATE TABLE vecs (id INTEGER PRIMARY KEY, emb BLOB);"
  for i in $(seq 1 "$ROWS"); do
    VEC="$(random_vec "$DIM")"
    echo "INSERT INTO vecs VALUES ($i, $VEC);"
  done
  echo "CREATE VECTOR INDEX idx_emb ON vecs(emb) DIM ${DIM} DISTANCE COSINE;"
} > "$TMPFILE"

INSERT_START=$(date +%s%N)
"$BIN" exec --db "$DB_DIR" --file "$TMPFILE" > /dev/null
INSERT_END=$(date +%s%N)

INSERT_MS=$(( (INSERT_END - INSERT_START) / 1000000 ))
echo "  Inserted ${ROWS} rows + CREATE VECTOR INDEX: ${INSERT_MS} ms  ($(( ROWS * 1000 / (INSERT_MS + 1) )) rows/s)"

rm -f "$TMPFILE"

# ── Phase 2: VEC_SEARCH benchmark ─────────────────────────────────────────────
echo
echo "--- VEC_SEARCH benchmark (top-5) ---"
QUERIES="${2:-100}"
echo "Running ${QUERIES} queries..."

SEARCH_START=$(date +%s%N)
for _ in $(seq 1 "$QUERIES"); do
  QV="$(random_vec "$DIM")"
  "$BIN" exec --db "$DB_DIR" \
    --sql "SELECT id, ${QV/VEC/VEC_SEARCH('vecs', 'idx_emb',})" \
    --sql "SELECT id FROM vecs ORDER BY VEC_SEARCH('vecs','idx_emb', $QV) DESC LIMIT 5" \
    > /dev/null 2>&1 || true
done
SEARCH_END=$(date +%s%N)

SEARCH_MS=$(( (SEARCH_END - SEARCH_START) / 1000000 ))
echo "  ${QUERIES} VEC_SEARCH queries: ${SEARCH_MS} ms  ($(( QUERIES * 1000 / (SEARCH_MS + 1) )) qps)"

# ── Summary ────────────────────────────────────────────────────────────────────
echo
echo "=== Summary ==="
echo "  Rows: ${ROWS}   Dim: ${DIM}"
echo "  INSERT+INDEX: ${INSERT_MS} ms"
echo "  SEARCH (${QUERIES} queries): ${SEARCH_MS} ms"
