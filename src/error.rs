use thiserror::Error;

#[derive(Error, Debug)]
pub enum KkdbError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Syntax error: {0}")]
    SyntaxError(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Runtime error: {0}")]
    RuntimeError(String),

    #[error("Type error: {0}")]
    TypeError(String),

    #[error("Table '{0}' already exists")]
    TableAlreadyExists(String),

    #[error("Table '{0}' not found")]
    TableNotFound(String),

    #[error("Column '{0}' not found")]
    ColumnNotFound(String),

    #[error("Column count mismatch: expected {expected}, got {got}")]
    ColumnCountMismatch { expected: usize, got: usize },

    #[error("Page {0} out of range")]
    PageOutOfRange(u32),

    #[error("B-tree error: {0}")]
    BTreeError(String),

    #[error("Database is full")]
    DatabaseFull,

    #[error("Corrupt database: {0}")]
    CorruptDatabase(String),

    #[error("Constraint violation: {0}")]
    ConstraintViolation(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Not implemented: {0}")]
    NotImplemented(String),
}

pub type Result<T> = std::result::Result<T, KkdbError>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
