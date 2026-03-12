# KKDB 部署指南

本文档覆盖 KKDB 在各种环境下的部署方案，包括单机、集群、容器化和生产环境最佳实践。

---

## 目录

1. [单机部署](#1-单机部署)
2. [MySQL 协议服务器部署](#2-mysql-协议服务器部署)
3. [HTTP REST API 服务器部署](#3-http-rest-api-服务器部署)
4. [Raft 分布式集群部署](#4-raft-分布式集群部署)
5. [Docker 容器化部署](#5-docker-容器化部署)
6. [systemd 服务配置](#6-systemd-服务配置)
7. [生产环境配置](#7-生产环境配置)
8. [备份与恢复](#8-备份与恢复)
9. [监控与告警](#9-监控与告警)
10. [故障排查](#10-故障排查)

---

## 1. 单机部署

### 1.1 编译

```bash
# Release 编译（推荐生产使用）
cargo build --release

# 编译产物
ls -la target/release/kkdb
```

### 1.2 以内嵌库方式使用

将 KKDB 作为 Rust crate 嵌入你的应用：

```toml
# Cargo.toml
[dependencies]
kkdb = { path = "/path/to/kkdb" }
```

```rust
use kkdb::vm::execute::VM;

let mut vm = VM::open("data/mydb")?;
vm.execute_sql("SELECT 1")?;
```

### 1.3 交互式 REPL

```bash
# 内存模式（临时数据，关闭即丢失）
./target/release/kkdb

# 持久化模式（创建数据目录）
./target/release/kkdb /var/lib/kkdb/mydata
```

数据目录结构：

```
/var/lib/kkdb/mydata/
├── catalog.kkdb     # Schema 元数据
├── users.kkdb       # users 表数据
├── orders.kkdb      # orders 表数据
└── binlog.bin       # Binlog
```

---

## 2. MySQL 协议服务器部署

### 2.1 启动

```bash
# 单独启动 MySQL 服务
./target/release/kkdb --mysql-port 3307

# 同时启动 MySQL + HTTP
./target/release/kkdb --mysql-port 3307 --http-port 8080

# 指定数据目录
./target/release/kkdb /var/lib/kkdb/data --mysql-port 3307
```

### 2.2 客户端连接

```bash
# MySQL CLI
mysql -h 127.0.0.1 -P 3307 -u root

# Python (mysql-connector)
import mysql.connector
conn = mysql.connector.connect(host='127.0.0.1', port=3307, user='root')

# Node.js (mysql2)
const mysql = require('mysql2/promise');
const conn = await mysql.createConnection({host: '127.0.0.1', port: 3307, user: 'root'});

# Java (JDBC)
Connection conn = DriverManager.getConnection("jdbc:mysql://127.0.0.1:3307/", "root", "");

# Go (go-sql-driver/mysql)
db, _ := sql.Open("mysql", "root@tcp(127.0.0.1:3307)/")
```

### 2.3 兼容性说明

- 支持 MySQL Wire Protocol v10 完整握手
- 支持 COM_QUERY、COM_STMT_PREPARE/EXECUTE、COM_PING 等
- 认证方式：`mysql_native_password`
- 详细兼容性矩阵参见 [MySQL 协议服务器文档](MYSQL_SERVER.md)

---

## 3. HTTP REST API 服务器部署

### 3.1 启动

```bash
# 启动 HTTP 服务
./target/release/kkdb --http-port 8080

# 指定数据目录
./target/release/kkdb /var/lib/kkdb/data --http-port 8080
```

### 3.2 反向代理（Nginx）

```nginx
upstream kkdb_api {
    server 127.0.0.1:8080;
}

server {
    listen 443 ssl;
    server_name api.example.com;

    ssl_certificate     /etc/ssl/certs/api.pem;
    ssl_certificate_key /etc/ssl/private/api.key;

    location / {
        proxy_pass http://kkdb_api;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # WebSocket / 长连接支持
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
    }

    # 健康检查（不需要认证）
    location /health {
        proxy_pass http://kkdb_api/health;
    }
}
```

### 3.3 API 端点

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/auth/signup` | 注册 |
| POST | `/auth/signin` | 登录（JWT） |
| POST | `/auth/refresh` | 刷新 Token |
| POST | `/rest/query` | 执行 SELECT |
| POST | `/rest/execute` | 执行 DML |
| POST | `/rest/batch` | 批量 SQL |
| POST | `/rest/bulk` | 批量数据操作 |
| GET | `/health` | 健康检查 |

详细 API 文档参见 [HTTP REST API 文档](HTTP_API.md)。

---

## 4. Raft 分布式集群部署

### 4.1 3 节点集群

```bash
# 节点 1（Leader 候选）
./target/release/kkdb /var/lib/kkdb/node1 \
  --raft-id 1 \
  --raft-port 9001 \
  --http-port 8001

# 节点 2
./target/release/kkdb /var/lib/kkdb/node2 \
  --raft-id 2 \
  --raft-port 9002 \
  --http-port 8002

# 节点 3
./target/release/kkdb /var/lib/kkdb/node3 \
  --raft-id 3 \
  --raft-port 9003 \
  --http-port 8003
```

### 4.2 集群初始化

```bash
# 初始化集群（在任一节点上执行）
curl -X POST http://localhost:8001/raft/init \
  -H 'Content-Type: application/json' \
  -d '{"members": [1, 2, 3]}'

# 添加 Learner 节点
curl -X POST http://localhost:8001/raft/add-learner \
  -H 'Content-Type: application/json' \
  -d '{"node_id": 4, "address": "10.0.0.4:9004"}'

# 查看集群状态
curl http://localhost:8001/raft/status

# 查看 Raft 指标
curl http://localhost:8001/raft/metrics
```

### 4.3 Raft 数据目录

```
/var/lib/kkdb/node1/
├── catalog.kkdb    # Schema 元数据
├── *.kkdb          # 表数据
├── wal.log         # Raft WAL 日志
├── vote.json       # Raft 投票持久化
└── purge.json      # 日志清理标记
```

### 4.4 高可用策略

- **最少 3 节点**：容忍 1 节点故障（Quorum = 2）
- **5 节点推荐**：容忍 2 节点故障（Quorum = 3）
- **跨可用区部署**：节点分布在不同 AZ 以防单点故障
- **Leader 自动故障转移**：Leader 失效后自动选举

### 4.5 网络要求

| 用途 | 端口 | 协议 |
|------|------|------|
| Raft 共识 | 9001-9005 | HTTP/JSON |
| 客户端 HTTP | 8001-8005 | HTTP REST |
| MySQL 协议 | 3307+ | MySQL Wire |

节点间网络延迟建议 < 10ms（同城），跨区域延迟建议 < 100ms。

---

## 5. Docker 容器化部署

项目根目录已包含完整的 Dockerfile、docker-compose.yml 和构建脚本。

### 5.1 使用 Dockerfile

```bash
# 默认 Release 构建
docker build -t kkdb .

# 自定义页大小
docker build -t kkdb --build-arg PAGE_SIZE=8192 .

# 运行（单节点，全部协议）
docker run -d --name kkdb \
  -p 3306:3306 -p 3307:3307 -p 6543:6543 \
  -v kkdb-data:/data \
  kkdb:latest

# 仅 REPL 模式（交互式）
docker run -it --rm kkdb:latest /bin/sh -c "kkdb"
```

> Dockerfile 使用多阶段构建：Builder (rust:1.87-bookworm) + Runtime (debian:bookworm-slim)，
> 最终镜像不含编译工具链，体积小巧。

### 5.2 使用 Docker Compose（单节点）

```bash
# 构建+启动
docker compose up -d

# 查看状态
docker compose ps

# 查看日志
docker compose logs -f

# 停止（保留数据）
docker compose down

# 停止并删除数据
docker compose down -v
```

### 5.3 Docker Compose（3 节点 Raft 集群）

```bash
# 启动集群
docker compose --profile cluster up -d

# 查看集群状态
docker compose --profile cluster ps

# 各节点端口映射:
#   Node 1: MySQL=13306, HTTP=16543, Raft=21001
#   Node 2: MySQL=23306, HTTP=26543, Raft=21002
#   Node 3: MySQL=33306, HTTP=36543, Raft=21003

# 停止集群
docker compose --profile cluster down
```

### 5.4 使用构建脚本

```bash
# 构建 Docker 镜像
./scripts/build.sh docker

# Docker Compose 单节点
./scripts/build.sh docker-compose

# Docker Compose Raft 集群
./scripts/build.sh cluster

# 本地 Release 构建
./scripts/build.sh release

# 运行服务器
./scripts/run.sh server /data/mydb

# 数据库备份
./scripts/backup.sh /data/kkdb /backups 7
```

---

## 6. systemd 服务配置

### 6.1 服务文件

```ini
# /etc/systemd/system/kkdb.service
[Unit]
Description=KKDB Database Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=kkdb
Group=kkdb
ExecStart=/usr/local/bin/kkdb /var/lib/kkdb/data --mysql-port 3307 --http-port 8080
WorkingDirectory=/var/lib/kkdb
Restart=on-failure
RestartSec=5s
LimitNOFILE=65536
LimitMEMLOCK=infinity

# 安全加固
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/kkdb

[Install]
WantedBy=multi-user.target
```

### 6.2 安装与管理

```bash
# 创建系统用户
sudo useradd -r -s /sbin/nologin kkdb
sudo mkdir -p /var/lib/kkdb/data
sudo chown -R kkdb:kkdb /var/lib/kkdb

# 安装二进制
sudo cp target/release/kkdb /usr/local/bin/

# 启用并启动服务
sudo systemctl daemon-reload
sudo systemctl enable kkdb
sudo systemctl start kkdb

# 查看状态
sudo systemctl status kkdb
sudo journalctl -u kkdb -f
```

---

## 7. 生产环境配置

### 7.1 存储引擎调优

```rust
use kkdb::storage::pager::EngineConfig;
use kkdb::storage::pager::FlushMethod;

let config = EngineConfig {
    buffer_pool_pages: 4096,        // 16MB buffer pool (4096 * 4KB)
    wal_auto_checkpoint: 10000,     // 每 10000 帧自动 checkpoint
    wal_enabled: true,              // 启用 WAL
    use_lz4: true,                  // 启用 LZ4 压缩
    flush_method: FlushMethod::Fsync, // 安全刷盘
};
```

### 7.2 SQL 会话变量

```sql
-- 查看引擎状态
SHOW ENGINE STATUS;

-- 设置会话变量
SET innodb_buffer_pool_size = 16384;  -- 页数
SET innodb_flush_method = 'fsync';
```

### 7.3 性能建议

| 配置项 | 开发环境 | 生产环境 |
|--------|---------|---------|
| Buffer Pool | 256 页 (1MB) | 4096+ 页 (16MB+) |
| WAL Checkpoint | 1000 帧 | 10000+ 帧 |
| LZ4 压缩 | 可选 | 推荐 |
| 刷盘方式 | 默认 | Fsync |
| 文件描述符限制 | 默认 | 65536+ |

### 7.4 安全加固

```sql
-- 创建管理员用户
CREATE USER admin WITH PASSWORD 'strong_password_here';
GRANT ALL ON * TO admin;

-- 创建只读用户
CREATE USER reader WITH PASSWORD 'reader_password';
GRANT SELECT ON * TO reader;

-- 启用行级安全
ALTER TABLE sensitive_data ENABLE ROW LEVEL SECURITY;
```

---

## 8. 备份与恢复

### 8.1 SQL Dump 备份

```bash
# 完整备份
./target/release/kkdb-cli backup /var/lib/kkdb/data backup.sql

# 恢复
./target/release/kkdb-cli restore /var/lib/kkdb/data backup.sql
```

### 8.2 文件级备份

```bash
# 停服备份（最安全）
sudo systemctl stop kkdb
cp -r /var/lib/kkdb/data /backup/kkdb-$(date +%Y%m%d)
sudo systemctl start kkdb

# 热备份（利用 COW 快照）
cp /var/lib/kkdb/data/*.kkdb /backup/hot/
```

### 8.3 定时备份（Cron）

```bash
# /etc/cron.d/kkdb-backup
0 2 * * * kkdb /usr/local/bin/kkdb-cli backup /var/lib/kkdb/data /backup/kkdb-$(date +\%Y\%m\%d).sql 2>&1 | logger -t kkdb-backup
```

### 8.4 CSV 导出

```bash
# 导出单表
./target/release/kkdb-cli export /var/lib/kkdb/data users /backup/users.csv

# 导入
./target/release/kkdb-cli import /var/lib/kkdb/data users /backup/users.csv
```

---

## 9. 监控与告警

### 9.1 健康检查

```bash
# HTTP 健康检查端点
curl http://localhost:8080/health
# 返回: {"status": "ok"}

# MySQL Ping
mysqladmin -h 127.0.0.1 -P 3307 ping
```

### 9.2 Raft 指标（Prometheus）

```bash
# Prometheus 格式指标
curl http://localhost:8001/raft/metrics/prometheus

# JSON 格式指标
curl http://localhost:8001/raft/metrics
```

### 9.3 存储引擎状态

```sql
SHOW ENGINE STATUS;
-- 返回: 页面总数、已用页、空闲页、WAL 帧数、Buffer Pool 命中率等
```

### 9.4 监控脚本示例

```bash
#!/bin/bash
# kkdb-healthcheck.sh
HEALTH=$(curl -s -o /dev/null -w "%{http_code}" http://localhost:8080/health)
if [ "$HEALTH" != "200" ]; then
    echo "KKDB health check failed! HTTP status: $HEALTH"
    # 发送告警（邮件/Slack/PagerDuty）
fi
```

---

## 10. 故障排查

### 10.1 常见问题

| 问题 | 原因 | 解决方案 |
|------|------|----------|
| 启动失败 "CorruptDatabase" | 数据库文件损坏 | 使用备份恢复；COW 双超块通常可自动恢复 |
| 连接超时 | 端口未开放 / 防火墙 | 检查 `firewall-cmd` 或 `ufw` 规则 |
| "TableNotFound" | 数据文件缺失 | 检查数据目录完整性 |
| WAL 文件过大 | 长时间未 Checkpoint | 执行 `VACUUM` 或调整 `wal_auto_checkpoint` |
| 内存使用过高 | Buffer Pool 过大 | 减少 `buffer_pool_pages` 配置 |
| Raft 选举失败 | 节点间网络不通 | 检查 Raft 端口连通性 |

### 10.2 日志查看

```bash
# systemd 服务日志
sudo journalctl -u kkdb -f

# 查看最近 100 行日志
sudo journalctl -u kkdb -n 100 --no-pager
```

### 10.3 数据完整性检查

```sql
-- 检查页面校验和
SHOW ENGINE STATUS;

-- 验证表结构
.schema users

-- 重建索引（如果数据不一致）
DROP INDEX idx_users_name;
CREATE INDEX idx_users_name ON users(name);
```

---

## 相关文档

- [完全使用手册](USAGE.md) — 全部功能的综合参考
- [分布式集群](DISTRIBUTED.md) — Raft 共识详细文档
- [HTTP REST API](HTTP_API.md) — HTTP 接口文档
- [MySQL 协议服务器](MYSQL_SERVER.md) — MySQL Wire Protocol 文档
- [COW 双超块设计](COW_DOUBLE_SUPERBLOCK_DESIGN.md) — 存储引擎设计
- [技术架构详解](ARCHITECTURE.md) — 底层算法与数据结构
- [应用案例](EXAMPLES.md) — 完整业务场景示例
