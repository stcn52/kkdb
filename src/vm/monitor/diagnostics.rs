// R13 – Developer experience enhancement: EXPLAIN ANALYZE structures,
//       system-table metadata queries, error diagnostic context.
//
// Provides:
//   - `ExplainNode` + `ExplainAnalyze`: tree-structured plan nodes with runtime stats
//   - `SystemCatalog`: virtual system-table metadata (tables, columns, indexes)
//   - `DiagnosticContext`: rich error context with suggestions

use std::collections::HashMap;
use std::time::Duration;

// ── EXPLAIN ANALYZE ───────────────────────────────────────────────────

/// A node in the EXPLAIN plan tree.
#[derive(Debug, Clone)]
pub struct ExplainNode {
    pub op: String, // e.g. "SeqScan", "IndexScan", "HashJoin"
    pub table: Option<String>,
    pub estimated_rows: u64,
    pub estimated_cost: f64,
    pub actual_rows: Option<u64>,
    pub actual_time_us: Option<u64>, // microseconds
    pub children: Vec<ExplainNode>,
    pub extra: HashMap<String, String>,
}

impl ExplainNode {
    pub fn new(op: &str) -> Self {
        Self {
            op: op.to_string(),
            table: None,
            estimated_rows: 0,
            estimated_cost: 0.0,
            actual_rows: None,
            actual_time_us: None,
            children: Vec::new(),
            extra: HashMap::new(),
        }
    }

    pub fn with_table(mut self, table: &str) -> Self {
        self.table = Some(table.to_string());
        self
    }

    pub fn with_estimates(mut self, rows: u64, cost: f64) -> Self {
        self.estimated_rows = rows;
        self.estimated_cost = cost;
        self
    }

    pub fn with_actuals(mut self, rows: u64, time_us: u64) -> Self {
        self.actual_rows = Some(rows);
        self.actual_time_us = Some(time_us);
        self
    }

    pub fn add_child(&mut self, child: ExplainNode) {
        self.children.push(child);
    }

    pub fn set_extra(&mut self, key: &str, value: &str) {
        self.extra.insert(key.to_string(), value.to_string());
    }

    /// Compute the total actual time for this node and all descendants.
    pub fn total_time_us(&self) -> u64 {
        let self_time = self.actual_time_us.unwrap_or(0);
        let children_time: u64 = self.children.iter().map(|c| c.total_time_us()).sum();
        self_time + children_time
    }

    /// Find the most expensive node in the tree.
    pub fn bottleneck(&self) -> &ExplainNode {
        let mut worst = self;
        for child in &self.children {
            let child_worst = child.bottleneck();
            if child_worst.actual_time_us.unwrap_or(0) > worst.actual_time_us.unwrap_or(0) {
                worst = child_worst;
            }
        }
        worst
    }

    /// Pretty-print the plan tree as text lines.
    pub fn format_lines(&self, indent: usize) -> Vec<String> {
        let prefix = "  ".repeat(indent);
        let mut line = format!("{}{}", prefix, self.op);
        if let Some(ref t) = self.table {
            line.push_str(&format!(" on {}", t));
        }
        line.push_str(&format!(
            " (est. rows={}, cost={:.1}",
            self.estimated_rows, self.estimated_cost
        ));
        if let Some(ar) = self.actual_rows {
            line.push_str(&format!(", actual rows={}", ar));
        }
        if let Some(at) = self.actual_time_us {
            line.push_str(&format!(", time={}µs", at));
        }
        line.push(')');
        let mut lines = vec![line];
        for child in &self.children {
            lines.extend(child.format_lines(indent + 1));
        }
        lines
    }
}

/// Top-level EXPLAIN ANALYZE result.
#[derive(Debug, Clone)]
pub struct ExplainAnalyze {
    pub root: ExplainNode,
    pub planning_time: Duration,
    pub execution_time: Duration,
}

impl ExplainAnalyze {
    pub fn new(root: ExplainNode, planning_time: Duration, execution_time: Duration) -> Self {
        Self {
            root,
            planning_time,
            execution_time,
        }
    }

    /// Total wall-clock time (planning + execution).
    pub fn total_time(&self) -> Duration {
        self.planning_time + self.execution_time
    }

    /// Get formatted plan.
    pub fn format(&self) -> String {
        let mut lines = self.root.format_lines(0);
        lines.push(format!(
            "Planning time: {:.3}ms",
            self.planning_time.as_secs_f64() * 1000.0
        ));
        lines.push(format!(
            "Execution time: {:.3}ms",
            self.execution_time.as_secs_f64() * 1000.0
        ));
        lines.join("\n")
    }
}

// ── System Catalog ────────────────────────────────────────────────────

/// Metadata about a column in the system catalog.
#[derive(Debug, Clone)]
pub struct SysCatalogColumn {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub ordinal_position: usize,
}

/// Metadata about an index.
#[derive(Debug, Clone)]
pub struct SysCatalogIndex {
    pub name: String,
    pub table_name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
    pub is_primary: bool,
}

/// Metadata about a table.
#[derive(Debug, Clone)]
pub struct SysCatalogTable {
    pub name: String,
    pub row_count_estimate: u64,
    pub columns: Vec<SysCatalogColumn>,
    pub indexes: Vec<SysCatalogIndex>,
    pub created_at: Option<String>,
}

/// Virtual system catalog that can be queried for metadata.
pub struct SystemCatalog {
    tables: HashMap<String, SysCatalogTable>,
}

impl Default for SystemCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemCatalog {
    pub fn new() -> Self {
        Self {
            tables: HashMap::new(),
        }
    }

    /// Register a table in the catalog.
    pub fn register_table(&mut self, table: SysCatalogTable) {
        self.tables.insert(table.name.clone(), table);
    }

    /// Remove a table from the catalog.
    pub fn unregister_table(&mut self, name: &str) -> bool {
        self.tables.remove(name).is_some()
    }

    /// Lookup a table by name.
    pub fn get_table(&self, name: &str) -> Option<&SysCatalogTable> {
        self.tables.get(name)
    }

    /// List all table names.
    pub fn table_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.tables.keys().cloned().collect();
        names.sort();
        names
    }

    /// Find columns by name pattern (substring match).
    pub fn find_columns(&self, pattern: &str) -> Vec<(String, String)> {
        let lower_pattern = pattern.to_lowercase();
        let mut result = Vec::new();
        for table in self.tables.values() {
            for col in &table.columns {
                if col.name.to_lowercase().contains(&lower_pattern) {
                    result.push((table.name.clone(), col.name.clone()));
                }
            }
        }
        result
    }

    /// Get all indexes for a table.
    pub fn indexes_for_table(&self, table_name: &str) -> Vec<&SysCatalogIndex> {
        self.tables
            .get(table_name)
            .map(|t| t.indexes.iter().collect())
            .unwrap_or_default()
    }

    /// Total number of tables.
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }
}

// ── Diagnostic Context ────────────────────────────────────────────────

/// Severity level for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

/// A suggestion for resolving an error.
#[derive(Debug, Clone)]
pub struct DiagnosticSuggestion {
    pub message: String,
    pub fix: Option<String>, // optional auto-fix SQL
}

/// Rich error context attached to SQL errors.
#[derive(Debug, Clone)]
pub struct DiagnosticContext {
    pub severity: DiagnosticSeverity,
    pub error_code: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<usize>, // character position in SQL
    pub sql: Option<String>,
    pub suggestions: Vec<DiagnosticSuggestion>,
}

impl DiagnosticContext {
    pub fn error(code: &str, message: &str) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            error_code: code.to_string(),
            message: message.to_string(),
            detail: None,
            hint: None,
            position: None,
            sql: None,
            suggestions: Vec::new(),
        }
    }

    pub fn warning(code: &str, message: &str) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            error_code: code.to_string(),
            message: message.to_string(),
            detail: None,
            hint: None,
            position: None,
            sql: None,
            suggestions: Vec::new(),
        }
    }

    pub fn with_detail(mut self, detail: &str) -> Self {
        self.detail = Some(detail.to_string());
        self
    }

    pub fn with_hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.to_string());
        self
    }

    pub fn with_position(mut self, pos: usize) -> Self {
        self.position = Some(pos);
        self
    }

    pub fn with_sql(mut self, sql: &str) -> Self {
        self.sql = Some(sql.to_string());
        self
    }

    pub fn add_suggestion(&mut self, message: &str, fix: Option<&str>) {
        self.suggestions.push(DiagnosticSuggestion {
            message: message.to_string(),
            fix: fix.map(|s| s.to_string()),
        });
    }

    /// Format the diagnostic for display.
    pub fn format(&self) -> String {
        let sev = match self.severity {
            DiagnosticSeverity::Info => "INFO",
            DiagnosticSeverity::Warning => "WARNING",
            DiagnosticSeverity::Error => "ERROR",
        };
        let mut parts = vec![format!("{} [{}]: {}", sev, self.error_code, self.message)];
        if let Some(ref d) = self.detail {
            parts.push(format!("  Detail: {}", d));
        }
        if let Some(ref h) = self.hint {
            parts.push(format!("  Hint: {}", h));
        }
        if let Some(ref sql) = self.sql {
            parts.push(format!("  SQL: {}", sql));
            if let Some(pos) = self.position {
                let arrow = " ".repeat(7 + pos) + "^";
                parts.push(arrow);
            }
        }
        for (i, s) in self.suggestions.iter().enumerate() {
            parts.push(format!("  Suggestion {}: {}", i + 1, s.message));
            if let Some(ref fix) = s.fix {
                parts.push(format!("    Fix: {}", fix));
            }
        }
        parts.join("\n")
    }
}

/// Build common diagnostic contexts for frequent errors.
pub struct DiagnosticBuilder;

impl DiagnosticBuilder {
    /// Table not found.
    pub fn table_not_found(table: &str) -> DiagnosticContext {
        let mut ctx =
            DiagnosticContext::error("42P01", &format!("table \"{}\" does not exist", table));
        ctx.add_suggestion(
            &format!(
                "Did you mean to create it? Try: CREATE TABLE {} (...)",
                table
            ),
            None,
        );
        ctx
    }

    /// Column not found.
    pub fn column_not_found(column: &str, table: &str) -> DiagnosticContext {
        DiagnosticContext::error(
            "42703",
            &format!(
                "column \"{}\" does not exist in table \"{}\"",
                column, table
            ),
        )
        .with_hint("Check column names with: SELECT * FROM information_schema.columns")
    }

    /// Syntax error.
    pub fn syntax_error(message: &str, sql: &str, position: usize) -> DiagnosticContext {
        DiagnosticContext::error("42601", message)
            .with_sql(sql)
            .with_position(position)
    }

    /// Type mismatch.
    pub fn type_mismatch(expected: &str, got: &str) -> DiagnosticContext {
        DiagnosticContext::error(
            "42804",
            &format!("type mismatch: expected {}, got {}", expected, got),
        )
        .with_hint(&format!("Try casting with CAST(expr AS {})", expected))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_node_format() {
        let mut root = ExplainNode::new("HashJoin").with_estimates(100, 50.0);
        let child = ExplainNode::new("SeqScan")
            .with_table("users")
            .with_estimates(1000, 30.0)
            .with_actuals(980, 1200);
        root.add_child(child);
        let lines = root.format_lines(0);
        assert!(lines[0].contains("HashJoin"));
        assert!(lines[1].contains("SeqScan"));
        assert!(lines[1].contains("users"));
    }

    #[test]
    fn explain_node_total_time() {
        let mut root = ExplainNode::new("Sort").with_actuals(0, 500);
        root.add_child(ExplainNode::new("Scan").with_actuals(0, 300));
        assert_eq!(root.total_time_us(), 800);
    }

    #[test]
    fn explain_node_bottleneck() {
        let mut root = ExplainNode::new("Sort").with_actuals(0, 100);
        root.add_child(ExplainNode::new("Scan").with_actuals(0, 5000));
        let bn = root.bottleneck();
        assert_eq!(bn.op, "Scan");
    }

    #[test]
    fn explain_analyze_format() {
        let root = ExplainNode::new("SeqScan")
            .with_table("t1")
            .with_estimates(100, 10.0)
            .with_actuals(95, 2000);
        let ea = ExplainAnalyze::new(
            root,
            Duration::from_micros(500),
            Duration::from_micros(2000),
        );
        let text = ea.format();
        assert!(text.contains("SeqScan on t1"));
        assert!(text.contains("Planning time"));
        assert!(text.contains("Execution time"));
    }

    #[test]
    fn system_catalog_register_and_query() {
        let mut cat = SystemCatalog::new();
        cat.register_table(SysCatalogTable {
            name: "users".into(),
            row_count_estimate: 1000,
            columns: vec![
                SysCatalogColumn {
                    name: "id".into(),
                    data_type: "INTEGER".into(),
                    nullable: false,
                    default_value: None,
                    ordinal_position: 0,
                },
                SysCatalogColumn {
                    name: "user_name".into(),
                    data_type: "TEXT".into(),
                    nullable: true,
                    default_value: None,
                    ordinal_position: 1,
                },
            ],
            indexes: vec![],
            created_at: None,
        });
        assert_eq!(cat.table_count(), 1);
        assert!(cat.get_table("users").is_some());
        assert_eq!(cat.table_names(), vec!["users".to_string()]);
    }

    #[test]
    fn system_catalog_find_columns() {
        let mut cat = SystemCatalog::new();
        cat.register_table(SysCatalogTable {
            name: "orders".into(),
            row_count_estimate: 500,
            columns: vec![
                SysCatalogColumn {
                    name: "order_id".into(),
                    data_type: "INT".into(),
                    nullable: false,
                    default_value: None,
                    ordinal_position: 0,
                },
                SysCatalogColumn {
                    name: "user_id".into(),
                    data_type: "INT".into(),
                    nullable: false,
                    default_value: None,
                    ordinal_position: 1,
                },
            ],
            indexes: vec![],
            created_at: None,
        });
        let matches = cat.find_columns("user");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], ("orders".to_string(), "user_id".to_string()));
    }

    #[test]
    fn diagnostic_error_format() {
        let ctx = DiagnosticBuilder::table_not_found("missing_tbl");
        let formatted = ctx.format();
        assert!(formatted.contains("ERROR"));
        assert!(formatted.contains("42P01"));
        assert!(formatted.contains("missing_tbl"));
    }

    #[test]
    fn diagnostic_syntax_error_with_position() {
        let ctx = DiagnosticBuilder::syntax_error("unexpected token", "SELECT * FORM t1", 9);
        let formatted = ctx.format();
        assert!(formatted.contains("42601"));
        assert!(formatted.contains("FORM"));
        assert!(formatted.contains("^"));
    }

    #[test]
    fn diagnostic_type_mismatch() {
        let ctx = DiagnosticBuilder::type_mismatch("INTEGER", "TEXT");
        assert!(ctx.message.contains("INTEGER"));
        assert!(ctx.hint.unwrap().contains("CAST"));
    }

    #[test]
    fn diagnostic_column_not_found() {
        let ctx = DiagnosticBuilder::column_not_found("naem", "users");
        assert!(ctx.message.contains("naem"));
        assert!(ctx.hint.is_some());
    }

    #[test]
    fn diagnostic_warning() {
        let ctx = DiagnosticContext::warning("01000", "index unused");
        assert_eq!(ctx.severity, DiagnosticSeverity::Warning);
        let formatted = ctx.format();
        assert!(formatted.contains("WARNING"));
    }
}
