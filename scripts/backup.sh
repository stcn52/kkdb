#!/usr/bin/env bash
# ============================================================
# KKDB 数据库备份脚本
# ============================================================
# 用法:
#   ./scripts/backup.sh /data/kkdb                    # 备份到默认位置
#   ./scripts/backup.sh /data/kkdb /backups/kkdb      # 备份到指定位置
#   ./scripts/backup.sh /data/kkdb /backups/kkdb 7    # 保留最近 7 天
# ============================================================
set -euo pipefail

DB_PATH="${1:?Usage: $0 <db_path> [backup_dir] [retention_days]}"
BACKUP_DIR="${2:-${DB_PATH}_backups}"
RETENTION_DAYS="${3:-30}"

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_NAME="kkdb_backup_${TIMESTAMP}"
BACKUP_PATH="${BACKUP_DIR}/${BACKUP_NAME}"

GREEN='\033[0;32m'
CYAN='\033[0;36m'
NC='\033[0m'
info() { echo -e "${CYAN}[BACKUP]${NC} $*"; }
ok()   { echo -e "${GREEN}[BACKUP]${NC} $*"; }

# 检查源数据库
if [[ ! -d "$DB_PATH" ]] && [[ ! -f "$DB_PATH" ]]; then
  echo "Error: Database path '$DB_PATH' does not exist"
  exit 1
fi

# 创建备份目录
mkdir -p "$BACKUP_DIR"

info "Starting backup..."
info "  Source:    $DB_PATH"
info "  Target:    $BACKUP_PATH"

# 复制数据库文件/目录
cp -r "$DB_PATH" "$BACKUP_PATH"

# 压缩
info "Compressing..."
tar -czf "${BACKUP_PATH}.tar.gz" -C "$BACKUP_DIR" "$BACKUP_NAME"
rm -rf "$BACKUP_PATH"

BACKUP_SIZE=$(du -h "${BACKUP_PATH}.tar.gz" | cut -f1)
ok "Backup complete: ${BACKUP_PATH}.tar.gz ($BACKUP_SIZE)"

# 清理旧备份
if [[ "$RETENTION_DAYS" -gt 0 ]]; then
  info "Cleaning backups older than $RETENTION_DAYS days..."
  DELETED=$(find "$BACKUP_DIR" -name "kkdb_backup_*.tar.gz" -mtime +${RETENTION_DAYS} -delete -print | wc -l)
  if [[ "$DELETED" -gt 0 ]]; then
    info "Deleted $DELETED old backup(s)"
  fi
fi

# 列出当前备份
echo ""
info "Current backups in $BACKUP_DIR:"
ls -lhS "$BACKUP_DIR"/kkdb_backup_*.tar.gz 2>/dev/null || echo "  (none)"
