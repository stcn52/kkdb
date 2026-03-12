//! # kkdb — 轻量级嵌入式关系型数据库引擎
//!
//! kkdb (KiloKilo DB) 是一个用纯 Rust 编写的嵌入式数据库引擎，提供：
//!
//! - **PostgreSQL 风格的 SQL** — 通过 `sqlparser` 解析，支持 DDL / DML / 事务 / CTE / 窗口函数
//! - **Copy-on-Write B-Tree 存储** — 双 Superblock 原子提交，WAL 可选
//! - **MVCC 事务** — 支持 ReadCommitted / RepeatableRead / Serializable 隔离级别
//! - **全文搜索** — BM25 评分 + 倒排索引
//! - **向量搜索** — HNSW 近邻索引，支持余弦 / 欧氏 / 点积距离
//! - **Raft 分布式共识** — 基于 openraft 的多节点复制
//! - **MySQL 协议兼容** — 可通过 MySQL 客户端连接
//!
//! ## 快速开始
//!
//! ```rust,no_run
//! use kkdb::vm::execute::VM;
//!
//! let mut vm = VM::new_memory();
//! vm.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
//! vm.execute_sql("INSERT INTO users VALUES (1, 'Alice')").unwrap();
//! let result = vm.execute_sql("SELECT * FROM users").unwrap();
//! println!("{:?}", result);
//! ```

/// Binary log (binlog) 复制与变更捕获模块。
pub mod binlog;
/// 统一错误类型 [`error::KkdbError`] 及 `Result` 别名。
pub mod error;
/// Raft 分布式共识协议实现（基于 openraft）。
pub mod raft;
/// 数据库元数据：表结构 ([`schema::TableSchema`])、列信息、索引、触发器、视图定义。
pub mod schema;
/// 网络服务层：MySQL 协议服务器、HTTP API、TLS 支持。
pub mod server;
/// SQL 解析与 AST 转换：将 SQL 文本转为内部 [`sql::ast`] 表示。
pub mod sql;
/// 存储引擎：[`storage::pager::Pager`] (COW v2)、WAL、B-Tree、缓冲池、页压缩。
pub mod storage;
/// 动态类型系统：[`types::Value`]、[`types::DataType`]、行序列化 / 反序列化。
pub mod types;
/// 变长整数 (varint) 编解码，兼容 SQLite 格式。
pub mod varint;
/// 虚拟机层：[`vm::execute::VM`] 是数据库的核心执行入口。
pub mod vm;

/// BM25 全文搜索引擎：分词、倒排索引、相关性评分。
pub mod fulltext;
/// HNSW 向量近邻搜索：高维向量索引与 KNN 查询。
pub mod vector;
