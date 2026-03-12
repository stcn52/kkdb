#!/usr/bin/env bash
# ============================================================
# KKDB 快速构建脚本
# ============================================================
# 用法:
#   ./scripts/build.sh                    # Debug 构建
#   ./scripts/build.sh release            # Release 构建
#   ./scripts/build.sh release 8192       # Release + 自定义页大小
#   ./scripts/build.sh docker             # Docker 镜像构建
#   ./scripts/build.sh docker-compose     # Docker Compose 构建+启动
# ============================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

PROFILE="${1:-debug}"
PAGE_SIZE="${2:-4096}"

# ANSI colors
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info()  { echo -e "${CYAN}[KKDB]${NC} $*"; }
ok()    { echo -e "${GREEN}[KKDB]${NC} $*"; }
warn()  { echo -e "${YELLOW}[KKDB]${NC} $*"; }

case "$PROFILE" in
  docker)
    info "Building Docker image..."
    docker build -t kkdb:latest \
      --build-arg PROFILE=release \
      --build-arg PAGE_SIZE="$PAGE_SIZE" \
      .
    ok "Docker image 'kkdb:latest' built successfully"
    echo ""
    info "Run with:"
    echo "  docker run -p 3306:3306 -p 3307:3307 -p 6543:6543 kkdb:latest"
    echo "  docker run -v kkdb-data:/data kkdb:latest"
    ;;

  docker-compose)
    info "Building and starting with Docker Compose..."
    docker compose up -d --build
    ok "KKDB is running"
    echo ""
    info "Services:"
    docker compose ps
    echo ""
    info "Logs: docker compose logs -f"
    info "Stop: docker compose down"
    ;;

  cluster)
    info "Starting 3-node Raft cluster with Docker Compose..."
    docker compose --profile cluster up -d --build
    ok "Raft cluster is running"
    echo ""
    docker compose --profile cluster ps
    ;;

  debug|release)
    info "Building KKDB (profile=$PROFILE, page_size=$PAGE_SIZE)..."

    if [[ "$PROFILE" == "release" ]]; then
      KKDB_PAGE_SIZE="$PAGE_SIZE" cargo build --release
      BINARY="target/release/kkdb"
    else
      KKDB_PAGE_SIZE="$PAGE_SIZE" cargo build
      BINARY="target/debug/kkdb"
    fi

    SIZE=$(du -h "$BINARY" | cut -f1)
    ok "Build complete: $BINARY ($SIZE)"
    echo ""
    info "Run REPL:     $BINARY"
    info "Open file:    $BINARY path/to/mydb"
    info "Server mode:  $BINARY --server path/to/mydb --port 3306 --http-port 6543"
    ;;

  test)
    info "Running all tests..."
    KKDB_PAGE_SIZE="$PAGE_SIZE" cargo test --release 2>&1 | tail -5
    ok "Tests complete"
    ;;

  clean)
    info "Cleaning build artifacts..."
    cargo clean
    ok "Clean complete"
    ;;

  *)
    warn "Unknown profile: $PROFILE"
    echo ""
    echo "Usage: $0 [debug|release|docker|docker-compose|cluster|test|clean] [page_size]"
    echo ""
    echo "Profiles:"
    echo "  debug           Debug 构建 (默认)"
    echo "  release         Release 优化构建"
    echo "  docker          构建 Docker 镜像"
    echo "  docker-compose  Docker Compose 构建+启动 (单节点)"
    echo "  cluster         Docker Compose 3 节点 Raft 集群"
    echo "  test            运行全部测试"
    echo "  clean           清理构建产物"
    echo ""
    echo "Options:"
    echo "  page_size       B-Tree 页大小 (512-65536, 默认 4096)"
    exit 1
    ;;
esac
