#!/usr/bin/env bash
# ============================================================
# KKDB 快速启动脚本
# ============================================================
# 用法:
#   ./scripts/run.sh                      # 内存模式 REPL
#   ./scripts/run.sh mydb                 # 打开/创建数据库文件
#   ./scripts/run.sh server               # 服务器模式 (全部协议)
#   ./scripts/run.sh server mydb          # 服务器模式 + 数据库文件
#   ./scripts/run.sh server mydb 3306 6543 3307  # 自定义端口
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

# 查找二进制文件 (release 优先)
if [[ -x "target/release/kkdb" ]]; then
  BINARY="target/release/kkdb"
elif [[ -x "target/debug/kkdb" ]]; then
  BINARY="target/debug/kkdb"
else
  echo "[KKDB] Binary not found. Building..."
  cargo build --release
  BINARY="target/release/kkdb"
fi

MODE="${1:-repl}"

case "$MODE" in
  server)
    DB_PATH="${2:-/tmp/kkdb_data}"
    MYSQL_PORT="${3:-3306}"
    HTTP_PORT="${4:-6543}"
    ASYNC_MYSQL_PORT="${5:-3307}"
    DATA_DIR="${DB_PATH}/users"

    mkdir -p "$DATA_DIR"

    echo "[KKDB] Starting server mode"
    echo "  Database:    $DB_PATH"
    echo "  MySQL:       0.0.0.0:$MYSQL_PORT"
    echo "  HTTP API:    0.0.0.0:$HTTP_PORT"
    echo "  MySQL Wire:  0.0.0.0:$ASYNC_MYSQL_PORT"
    echo "  Data Dir:    $DATA_DIR"
    echo ""

    exec "$BINARY" --server "$DB_PATH" \
      --port "$MYSQL_PORT" \
      --http-port "$HTTP_PORT" \
      --mysql-port "$ASYNC_MYSQL_PORT" \
      --data-dir "$DATA_DIR"
    ;;

  repl)
    echo "[KKDB] Starting in-memory REPL"
    exec "$BINARY"
    ;;

  *)
    # 如果参数不是 server，当作数据库路径
    echo "[KKDB] Opening database: $MODE"
    exec "$BINARY" "$MODE"
    ;;
esac
