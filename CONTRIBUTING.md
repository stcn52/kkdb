# 贡献指南

感谢你对 KKDB 的关注！以下是参与贡献的指南。

## 开发环境

### 前置要求

- **Rust** 1.75+ (stable)
- **Git**
- Linux / macOS / Windows (WSL 推荐)

### 设置

```bash
# 克隆仓库
git clone https://github.com/your-org/kkdb.git
cd kkdb

# 编译
cargo build

# 运行全部测试
cargo test

# 仅库内测试（更快）
cargo test --lib
```

## 项目结构

```
src/
├── sql/           # SQL 解析器（sqlparser-rs 适配层）
├── storage/       # 存储引擎（COW Pager + B-Tree + WAL + Buffer Pool）
├── vm/            # 虚拟机（DDL/DML/SELECT 执行 + 优化器 + 安全 + 监控）
├── fulltext/      # 全文检索（BM25 + jieba-rs）
├── vector/        # 向量搜索（HNSW）
├── raft/          # 分布式共识（openraft v0.9）
├── server/        # 网络服务器（MySQL 协议 + HTTP REST API）
├── binlog/        # Binlog 复制日志
└── bin/           # CLI 工具
tests/             # 集成测试
docs/              # 文档
```

详细模块说明参见 [完全使用手册 · 模块架构全景](docs/USAGE.md#31-模块架构全景)。

## 代码规范

### Rust 风格

- 遵循 `rustfmt` 默认格式
- 使用 `clippy` 消除警告：`cargo clippy -- -D warnings`
- 公共 API 必须有 `///` 文档注释
- 错误处理使用 `KkdbError`(定义在 `src/error.rs`)，不使用 `unwrap()` / `panic!()`（测试代码除外）

### 命名约定

| 类型 | 约定 | 示例 |
|------|------|------|
| 结构体 / 枚举 | `PascalCase` | `ExecResult`, `DataType` |
| 函数 / 方法 | `snake_case` | `execute_sql()`, `new_memory()` |
| 常量 | `UPPER_SNAKE_CASE` | `PAGE_SIZE`, `MAX_PAGES` |
| 模块文件 | `snake_case` | `exec_engine.rs`, `query_cache.rs` |

### 关键 API 注意事项

- `VM::execute_sql()` 只接受**单条** SQL 语句
- `VM::new_memory()` 创建内存实例（不是 `new_in_memory()`）
- `VM::open(path)` 创建**目录**（不是文件），清理时需 `remove_dir_all`
- `Value::Text` 使用 `Arc<str>`，构造时用 `.into()`
- crate 内测试使用 `crate::` 路径（不是 `kkdb::`）
- SQL 解析使用 **SQLiteDialect**（不是 PostgreSQL）
- `ExecResult::RowsAffected` 是 struct variant：`{ count: usize, message: String }`

## 测试

### 测试组织

- **内联测试** (`#[cfg(test)] mod tests`): 每个源文件底部，测试模块内部逻辑
- **集成测试** (`src/vm/execute_tests/`): 通过 `exec()` / `query_rows()` 辅助函数执行端到端 SQL 测试
- **外部集成测试** (`tests/`): 跨模块集成测试

### 运行测试

```bash
# 全部测试
cargo test

# 仅库内测试（推荐日常使用）
cargo test --lib

# 运行特定测试
cargo test test_name

# 显示输出
cargo test -- --nocapture
```

### 编写测试

新功能模块应包含：
1. 源文件底部至少 5 个内联测试
2. `src/vm/execute_tests/rounds/` 中的集成测试文件（如果涉及 SQL 层）

集成测试模板：
```rust
use crate::raft::features::your_module::*;

#[test]
fn test_your_feature() {
    // 测试逻辑
}
```

### 测试数据目录

持久化测试使用 `testdata/test_*` 目录存放数据库文件，`cleanup_test_dir()` 会自动清理。

## 提交规范

使用语义化提交消息：

```
类型: 简短描述

可选的详细说明
```

### 类型

| 类型 | 说明 |
|------|------|
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `docs` | 仅文档变更 |
| `refactor` | 重构（不改变行为） |
| `test` | 添加或修改测试 |
| `perf` | 性能优化 |
| `chore` | 构建/工具链变更 |

### 示例

```
feat: 添加向量索引量化压缩支持

- 实现 ScalarQuantizer 将 f32 量化为 u8
- 添加 QuantizedCompressor 批量压缩接口
- 包含 5 个内联单元测试
```

## Pull Request 流程

1. Fork 仓库并创建特性分支
2. 编写代码和测试
3. 确保 `cargo test --lib` 全部通过
4. 确保 `cargo clippy` 无警告
5. 提交 PR，描述变更内容和动机

## 文档

- 用户文档放在 `docs/` 目录
- 设计/架构文档使用 `UPPER_SNAKE_CASE.md` 命名
- 修改公共 API 时同步更新 [USAGE.md](docs/USAGE.md) 和 [API.md](docs/API.md)
- 文档之间应包含交叉引用（`## 相关文档` 章节）

## 相关文档

- [完全使用手册](docs/USAGE.md) — 全部功能的综合参考
- [Rust API 参考](docs/API.md) — Crate 公开接口
- [项目总览](docs/PROJECT.md) — 架构概述与设计决策
