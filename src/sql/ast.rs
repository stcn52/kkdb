use crate::types::DataType;

/// Top-level SQL statement
#[derive(Debug, Clone)]
pub enum Statement {
    CreateTable(CreateTableStmt),
    DropTable(DropTableStmt),
    DropIndex(DropIndexStmt),
    Insert(InsertStmt),
    Select(SelectStmt),
    Update(UpdateStmt),
    Delete(DeleteStmt),
    CreateIndex(CreateIndexStmt),
    AlterTable(AlterTableStmt),
    Begin,
    Commit,
    Rollback,
    Savepoint(String),
    ReleaseSavepoint(String),
    RollbackToSavepoint(String),
    SetOp(SetOpStmt),
    ShowTables,
    /// VACUUM — reclaim free pages and truncate file
    Vacuum,
    /// CREATE VIEW
    CreateView(CreateViewStmt),
    Explain(Box<Statement>),
}

/// Set operation: UNION / INTERSECT / EXCEPT
#[derive(Debug, Clone)]
pub struct SetOpStmt {
    pub kind: SetOpKind,
    pub left: Box<SelectStmt>,
    pub right: Box<SelectStmt>,
    /// ORDER BY applied to the combined result
    pub order_by: Vec<OrderByItem>,
    /// LIMIT applied to the combined result
    pub limit: Option<Expr>,
    /// OFFSET applied to the combined result
    pub offset: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetOpKind {
    UnionAll,
    UnionDistinct,
    IntersectAll,
    IntersectDistinct,
    ExceptAll,
    ExceptDistinct,
}

#[derive(Debug, Clone)]
pub struct AlterTableStmt {
    pub table_name: String,
    pub action: AlterTableAction,
}

#[derive(Debug, Clone)]
pub enum AlterTableAction {
    AddColumn(ColumnDef),
    DropColumn(String),
    RenameTable(String),
    RenameColumn { old_name: String, new_name: String },
}

#[derive(Debug, Clone)]
pub struct CreateTableStmt {
    pub table_name: String,
    pub columns: Vec<ColumnDef>,
    pub if_not_exists: bool,
    /// Source SELECT for CREATE TABLE AS SELECT; None for regular CREATE TABLE
    pub source: Option<Box<SelectStmt>>,
}

#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub primary_key: bool,
    pub autoincrement: bool,
    pub not_null: bool,
    pub unique: bool,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct DropTableStmt {
    pub table_name: String,
    pub if_exists: bool,
}

#[derive(Debug, Clone)]
pub struct DropIndexStmt {
    pub index_name: String,
    pub if_exists: bool,
}

/// Conflict resolution policy for INSERT statements
#[derive(Debug, Clone)]
pub enum ConflictPolicy {
    /// Default: error on conflict
    Error,
    /// INSERT OR REPLACE: delete conflicting row then insert
    Replace,
    /// INSERT OR IGNORE: silently skip conflicting row
    Ignore,
    /// ON CONFLICT DO UPDATE SET ...: update existing row (Batch G)
    Update(Vec<(String, Expr)>),
}

#[derive(Debug, Clone)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    Select(Box<SelectStmt>),
}

#[derive(Debug, Clone)]
pub struct InsertStmt {
    pub table_name: String,
    pub columns: Option<Vec<String>>,
    pub source: InsertSource,
    pub conflict: ConflictPolicy,
}

#[derive(Debug, Clone)]
pub struct SelectStmt {
    pub distinct: bool,
    pub columns: Vec<SelectColumn>,
    pub from: Option<FromClause>,
    pub where_clause: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub order_by: Vec<OrderByItem>,
    pub limit: Option<Expr>,
    pub offset: Option<Expr>,
    /// Common Table Expressions (WITH clause) — Batch D
    pub ctes: Vec<CteDefinition>,
    /// Named Windows (WINDOW clause) — Batch F
    pub window_defs: Vec<NamedWindowDefinition>,
}

#[derive(Debug, Clone)]
pub struct NamedWindowDefinition {
    pub name: String,
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<OrderByItem>,
    pub frame: Option<WindowFrame>,
}

/// One CTE definition: `name [(cols)] AS (subquery)`
#[derive(Debug, Clone)]
pub struct CteDefinition {
    pub name: String,
    pub columns: Vec<String>,
    pub query: Box<SelectStmt>,
}

/// CREATE VIEW stub for Batch E
#[derive(Debug, Clone)]
pub struct CreateViewStmt {
    pub name: String,
    pub columns: Vec<String>,
    pub query: Box<SelectStmt>,
    pub or_replace: bool,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub enum SelectColumn {
    AllColumns,              // *
    TableAllColumns(String), // table.*
    Expr { expr: Expr, alias: Option<String> },
}

#[derive(Debug, Clone)]
pub enum FromClause {
    Table {
        name: String,
        alias: Option<String>,
    },
    Join {
        left: Box<FromClause>,
        join_type: JoinType,
        right: Box<FromClause>,
        on: Option<Expr>,
    },
    Subquery {
        query: Box<SelectStmt>,
        alias: String,
    },
    /// Nested set operation used as a row source (for nested UNION/INTERSECT inside FROM)
    SetOp {
        stmt: Box<SetOpStmt>,
        alias: String,
    },
    /// Table-valued function in FROM clause: UNNEST(expr), generate_series(start, stop[, step])
    /// The function is identified by `name` and called with `args` expressions.
    /// `alias` names the result set; `column` optionally names the output column (else defaults to `name`).
    TableFunction {
        name: String,
        args: Vec<Expr>,
        alias: Option<String>,
        /// Optional explicit output column name (e.g. from `AS t(col)`)
        column: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Cross,
    LeftSemi,
    RightSemi,
    /// FULL [OUTER] JOIN
    Full,
    /// NATURAL JOIN — join columns determined at runtime from schema
    Natural,
}

#[derive(Debug, Clone)]
pub struct OrderByItem {
    pub expr: Expr,
    pub ascending: bool,
    /// NULLS FIRST (true) / NULLS LAST (false) / None = default (NULLs sort first for ASC, last for DESC)
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct UpdateStmt {
    pub table_name: String,
    pub assignments: Vec<(String, Expr)>,
    pub where_clause: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct DeleteStmt {
    pub table_name: String,
    pub where_clause: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct CreateIndexStmt {
    pub index_name: String,
    pub table_name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub if_not_exists: bool,
}

/// Expression node
#[derive(Debug, Clone)]
pub enum Expr {
    // Literals
    IntegerLiteral(i64),
    RealLiteral(f64),
    StringLiteral(String),
    BlobLiteral(Vec<u8>),
    Null,

    // References
    ColumnRef {
        table: Option<String>,
        column: String,
    },

    // Interval literal (e.g., INTERVAL '1' DAY)
    Interval {
        value: Box<Expr>,
        leading_field: Option<String>,
    },

    // COLLATE expression
    Collate {
        expr: Box<Expr>,
        collation: String,
    },

    // Binary operators
    BinaryOp {
        left: Box<Expr>,
        op: BinaryOperator,
        right: Box<Expr>,
    },

    // Unary operators
    UnaryOp {
        op: UnaryOperator,
        expr: Box<Expr>,
    },

    // IS NULL / IS NOT NULL
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },

    // IN (list)
    InList {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },

    Like {
        expr: Box<Expr>,
        pattern: Box<Expr>,
        escape_char: Option<char>,
        case_insensitive: bool,
        negated: bool,
    },

    // BETWEEN
    Between {
        expr: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
        negated: bool,
    },

    // Function call
    Function {
        name: String,
        args: Vec<Expr>,
        distinct: bool,
    },

    // Window function: func() OVER (PARTITION BY ... ORDER BY ...) — Batch F
    WindowFunction {
        func: WindowFunc,
        partition_by: Vec<Expr>,
        order_by: Vec<OrderByItem>,
        frame: Option<WindowFrame>,
    },

    // Subquery (scalar)
    Subquery(Box<SelectStmt>),

    // IN (SELECT ...)
    InSubquery {
        expr: Box<Expr>,
        subquery: Box<SelectStmt>,
        negated: bool,
    },

    // EXISTS (SELECT ...)
    Exists(Box<SelectStmt>),

    // AnyOp: x > ANY (SELECT ...)
    AnyOp {
        expr: Box<Expr>,
        op: BinaryOperator,
        subquery: Box<SelectStmt>,
    },

    // AllOp: x > ALL (SELECT ...)
    AllOp {
        expr: Box<Expr>,
        op: BinaryOperator,
        subquery: Box<SelectStmt>,
    },

    // Parenthesized expression
    Nested(Box<Expr>),

    // CASE WHEN ... THEN ... [ELSE ...] END
    Case {
        /// Simple CASE: CASE <operand> WHEN val THEN result ...
        /// Searched CASE: operand is None, each when clause is a boolean predicate
        operand: Option<Box<Expr>>,
        when_clauses: Vec<(Expr, Expr)>, // (condition_or_value, result)
        else_clause: Option<Box<Expr>>,
    },

    // CAST(expr AS type)
    Cast {
        expr: Box<Expr>,
        to_type: CastTargetType,
        try_cast: bool,
    },
}

/// Target type for CAST expressions
#[derive(Debug, Clone, PartialEq)]
pub enum CastTargetType {
    Integer,
    Real,
    Text,
    Blob,
    Numeric, // prefer integer, fall back to real
    /// DATE / TIME / TIMESTAMP — stored as Text (ISO format), semantics tracked for future
    Date,
    Time,
    Timestamp,
    /// JSON — stored as Text, preserved for JSON function interop
    Json,
}

impl Expr {
    /// Convert a runtime `Value` into its corresponding literal `Expr`.
    pub fn from_value(val: crate::types::Value) -> Self {
        match val {
            crate::types::Value::Null => Expr::Null,
            crate::types::Value::Integer(v) => Expr::IntegerLiteral(v),
            crate::types::Value::Real(v) => Expr::RealLiteral(v),
            crate::types::Value::Text(s) => Expr::StringLiteral(s.to_string()),
            crate::types::Value::Blob(b) => Expr::BlobLiteral(b),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    And,
    Or,
    Concat,
    /// Logical XOR: 1 XOR 0 = 1
    Xor,
    /// Bitwise OR: a | b
    BitwiseOr,
    /// Bitwise AND: a & b
    BitwiseAnd,
    /// Bitwise XOR: a ^ b
    BitwiseXor,
    /// Bitwise shift left: a << b
    ShiftLeft,
    /// Bitwise shift right: a >> b
    ShiftRight,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOperator {
    Minus,
    Not,
}

/// Window function variant — Batch F
#[derive(Debug, Clone)]
pub enum WindowFunc {
    RowNumber,
    Rank,
    DenseRank,
    PercentRank,
    CumeDist,
    Ntile(Box<Expr>),
    Lag { expr: Box<Expr>, offset: Option<Box<Expr>>, default: Option<Box<Expr>> },
    Lead { expr: Box<Expr>, offset: Option<Box<Expr>>, default: Option<Box<Expr>> },
    FirstValue(Box<Expr>),
    LastValue(Box<Expr>),
    NthValue(Box<Expr>, Box<Expr>),
    /// Aggregate used as window (SUM, COUNT, AVG, MIN, MAX over a window)
    Aggregate { name: String, args: Vec<Expr>, distinct: bool },
}

#[derive(Debug, Clone)]
pub struct WindowFrame {
    pub unit: WindowFrameUnit,
    pub start: WindowBound,
    pub end: Option<WindowBound>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WindowFrameUnit { Rows, Range, Groups }

#[derive(Debug, Clone)]
pub enum WindowBound {
    UnboundedPreceding,
    Preceding(Box<Expr>),
    CurrentRow,
    Following(Box<Expr>),
    UnboundedFollowing,
}
