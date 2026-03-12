# ============================================================
# KKDB Multi-Stage Dockerfile
# ============================================================
# 产物: ~30MB 静态链接可执行文件 (musl)
#
# 构建:
#   docker build -t kkdb .
#   docker build -t kkdb --build-arg PROFILE=release .
#   docker build -t kkdb --build-arg PAGE_SIZE=8192 .
#
# 运行:
#   docker run -p 3306:3306 -p 6543:6543 -p 3307:3307 kkdb
#   docker run -v kkdb-data:/data kkdb --server /data/mydb --port 3306
# ============================================================

# ---------- Stage 1: Builder ----------
FROM rust:1.87-bookworm AS builder

# 构建参数
ARG PROFILE=release
ARG PAGE_SIZE=4096

WORKDIR /build

# 1) 先复制依赖清单，利用 Docker 缓存
COPY Cargo.toml Cargo.lock* build.rs ./

# 2) 创建空 src 骨架，让 cargo fetch 预下载依赖
RUN mkdir -p src/bin src/binlog src/fulltext src/raft src/server src/sql \
             src/storage src/vector src/vm && \
    echo 'fn main() {}' > src/main.rs && \
    echo 'pub fn lib() {}' > src/lib.rs && \
    echo 'fn main() {}' > src/bin/kkdb-cli.rs && \
    echo 'fn main() {}' > src/bin/big_data_bench.rs && \
    KKDB_PAGE_SIZE=${PAGE_SIZE} cargo fetch

# 3) 复制全部源码
COPY src/ src/

# 4) 编译
RUN KKDB_PAGE_SIZE=${PAGE_SIZE} cargo build --profile ${PROFILE} --bin kkdb && \
    cp target/${PROFILE}/kkdb /build/kkdb-bin

# ---------- Stage 2: Runtime ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# 非 root 用户
RUN groupadd -r kkdb && useradd -r -g kkdb -m -d /home/kkdb kkdb

# 数据与配置目录
RUN mkdir -p /data /etc/kkdb && chown kkdb:kkdb /data

COPY --from=builder /build/kkdb-bin /usr/local/bin/kkdb
RUN chmod +x /usr/local/bin/kkdb

# 默认端口:
#   3306 - Legacy MySQL Protocol
#   3307 - Async MySQL Wire Protocol
#   6543 - HTTP REST API
EXPOSE 3306 3307 6543

# 数据卷
VOLUME ["/data"]

USER kkdb
WORKDIR /data

# 默认以 server 模式启动
ENTRYPOINT ["kkdb"]
CMD ["--server", "/data/kkdb", "--port", "3306", "--http-port", "6543", "--mysql-port", "3307", "--data-dir", "/data/users"]
