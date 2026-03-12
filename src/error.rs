use thiserror::Error;

/// kkdb 统一错误类型。
///
/// 所有可能的数据库操作错误都通过此枚举表示，涵盖：
/// I/O 错误、SQL 解析错误、运行时错误、类型错误、约束违反、存储引擎错误等。
///
/// # 示例
/// ```rust
/// use kkdb::error::KkdbError;
///
/// let err = KkdbError::TableNotFound("users".to_string());
/// assert_eq!(err.to_string(), "Table 'users' not found");
/// ```
#[derive(Error, Debug)]
pub enum KkdbError {
    /// 底层 I/O 操作失败（文件读写、fsync 等）。
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// SQL 语法错误（解析阶段检测到的非法语法）。
    #[error("Syntax error: {0}")]
    SyntaxError(String),

    /// SQL 解析错误（词法或结构层面的解析失败）。
    #[error("Parse error: {0}")]
    ParseError(String),

    /// 运行时执行错误（如除零、函数参数不合法等）。
    #[error("Runtime error: {0}")]
    RuntimeError(String),

    /// 类型不匹配错误（如将文本插入整数列时的隐式转换失败）。
    #[error("Type error: {0}")]
    TypeError(String),

    /// 尝试创建已存在的表。
    #[error("Table '{0}' already exists")]
    TableAlreadyExists(String),

    /// 引用的表不存在。
    #[error("Table '{0}' not found")]
    TableNotFound(String),

    /// 引用的列不存在（在指定表的 schema 中未找到）。
    #[error("Column '{0}' not found")]
    ColumnNotFound(String),

    /// INSERT / UPDATE 提供的列数量与表定义不匹配。
    #[error("Column count mismatch: expected {expected}, got {got}")]
    ColumnCountMismatch { expected: usize, got: usize },

    /// 请求的页号超出数据库文件的有效范围。
    #[error("Page {0} out of range")]
    PageOutOfRange(u32),

    /// B-Tree 层面的内部错误（如节点损坏、分裂失败）。
    #[error("B-tree error: {0}")]
    BTreeError(String),

    /// 数据库已满（页数量达到 `MAX_PAGES` 上限）。
    #[error("Database is full")]
    DatabaseFull,

    /// 数据库文件损坏（CRC 校验失败、Superblock 不一致等）。
    #[error("Corrupt database: {0}")]
    CorruptDatabase(String),

    /// 约束违反（UNIQUE、NOT NULL、CHECK、FOREIGN KEY 等）。
    #[error("Constraint violation: {0}")]
    ConstraintViolation(String),

    /// 内部实现错误（不应由用户触发的逻辑漏洞）。
    #[error("Internal error: {0}")]
    Internal(String),

    /// 功能尚未实现。
    #[error("Not implemented: {0}")]
    NotImplemented(String),

    /// MVCC 锁冲突（并发事务写-写冲突）。
    #[error("Lock conflict: {0}")]
    LockConflict(String),
}

/// kkdb 操作的 `Result` 类型别名。
///
/// 等价于 `std::result::Result<T, KkdbError>`。
pub type Result<T> = std::result::Result<T, KkdbError>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
