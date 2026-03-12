// R15 – Developer toolchain enhancement: SQL lint checker,
//       query plan visualization tree, index recommendation engine,
//       schema migration version management.
//
// Provides:
//   - `SqlLintChecker`: static analysis rules for SQL anti-patterns
//   - `PlanVisualizer`: text/tree rendering of query plan nodes
//   - `IndexAdvisor`: workload-based index recommendation
//   - `SchemaMigrationManager`: versioned schema migration tracking

// ── SQL Lint Checker ──────────────────────────────────────────────────

/// Severity level for SQL lint issues.
#[derive(Debug, Clone, PartialEq)]
pub enum LintSeverity {
    Info,
    Warning,
    Error,
}

/// A detected lint issue.
#[derive(Debug, Clone)]
pub struct LintIssue {
    pub rule: String,
    pub severity: LintSeverity,
    pub message: String,
    pub suggestion: Option<String>,
}

/// SQL lint rules.
#[derive(Debug, Clone, PartialEq)]
pub enum LintRule {
    /// SELECT * is discouraged.
    NoSelectStar,
    /// Missing WHERE clause in UPDATE/DELETE.
    MissingWhereClause,
    /// Implicit type coercion.
    ImplicitCoercion,
    /// Using != instead of <>.
    NonStandardNotEqual,
    /// Column not in GROUP BY or aggregate.
    AmbiguousGroupBy,
    /// Subquery in WHERE could be JOIN.
    SubqueryToJoin,
}

/// Static analysis checker for SQL queries.
pub struct SqlLintChecker {
    enabled_rules: Vec<LintRule>,
}

impl Default for SqlLintChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl SqlLintChecker {
    pub fn new() -> Self {
        Self {
            enabled_rules: vec![
                LintRule::NoSelectStar,
                LintRule::MissingWhereClause,
                LintRule::ImplicitCoercion,
                LintRule::NonStandardNotEqual,
                LintRule::AmbiguousGroupBy,
                LintRule::SubqueryToJoin,
            ],
        }
    }

    pub fn with_rules(rules: Vec<LintRule>) -> Self {
        Self {
            enabled_rules: rules,
        }
    }

    /// Lint a SQL string for common anti-patterns.
    pub fn check(&self, sql: &str) -> Vec<LintIssue> {
        let mut issues = Vec::new();
        let upper = sql.to_uppercase();

        if self.enabled_rules.contains(&LintRule::NoSelectStar)
            && (upper.contains("SELECT *") || upper.contains("SELECT  *"))
        {
            issues.push(LintIssue {
                rule: "no-select-star".to_string(),
                severity: LintSeverity::Warning,
                message: "Avoid SELECT *; specify columns explicitly".to_string(),
                suggestion: Some("List specific columns".to_string()),
            });
        }

        if self.enabled_rules.contains(&LintRule::MissingWhereClause) {
            let has_update_delete = upper.contains("UPDATE ") || upper.contains("DELETE ");
            let has_where = upper.contains("WHERE ");
            if has_update_delete && !has_where {
                issues.push(LintIssue {
                    rule: "missing-where".to_string(),
                    severity: LintSeverity::Error,
                    message: "UPDATE/DELETE without WHERE clause affects all rows".to_string(),
                    suggestion: Some("Add a WHERE clause".to_string()),
                });
            }
        }

        if self.enabled_rules.contains(&LintRule::NonStandardNotEqual) && sql.contains("!=") {
            issues.push(LintIssue {
                rule: "non-standard-not-equal".to_string(),
                severity: LintSeverity::Info,
                message: "Use <> instead of != for SQL standard compliance".to_string(),
                suggestion: Some("Replace != with <>".to_string()),
            });
        }

        issues
    }

    pub fn enabled_rule_count(&self) -> usize {
        self.enabled_rules.len()
    }

    pub fn enable_rule(&mut self, rule: LintRule) {
        if !self.enabled_rules.contains(&rule) {
            self.enabled_rules.push(rule);
        }
    }

    pub fn disable_rule(&mut self, rule: &LintRule) {
        self.enabled_rules.retain(|r| r != rule);
    }
}

// ── Query Plan Visualizer ─────────────────────────────────────────────

/// A node in the plan visualization tree.
#[derive(Debug, Clone)]
pub struct PlanNode {
    pub operation: String,
    pub table: Option<String>,
    pub cost: f64,
    pub rows: usize,
    pub extra: Vec<String>,
    pub children: Vec<PlanNode>,
}

impl PlanNode {
    pub fn new(operation: &str, cost: f64, rows: usize) -> Self {
        Self {
            operation: operation.to_string(),
            table: None,
            cost,
            rows,
            extra: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn with_table(mut self, table: &str) -> Self {
        self.table = Some(table.to_string());
        self
    }

    pub fn with_extra(mut self, extra: &str) -> Self {
        self.extra.push(extra.to_string());
        self
    }

    pub fn add_child(&mut self, child: PlanNode) {
        self.children.push(child);
    }

    /// Total cost including children.
    pub fn total_cost(&self) -> f64 {
        self.cost + self.children.iter().map(|c| c.total_cost()).sum::<f64>()
    }

    /// Total estimated rows.
    pub fn total_rows(&self) -> usize {
        self.rows + self.children.iter().map(|c| c.total_rows()).sum::<usize>()
    }
}

/// Renders a plan tree as formatted text.
pub struct PlanVisualizer;

impl PlanVisualizer {
    /// Render a plan tree as a list of text lines.
    pub fn render(node: &PlanNode) -> Vec<String> {
        let mut lines = Vec::new();
        Self::render_node(node, &mut lines, "", true);
        lines
    }

    fn render_node(node: &PlanNode, lines: &mut Vec<String>, prefix: &str, is_last: bool) {
        let connector = if prefix.is_empty() {
            ""
        } else if is_last {
            "└── "
        } else {
            "├── "
        };

        let table_info = node
            .table
            .as_deref()
            .map(|t| format!(" on {}", t))
            .unwrap_or_default();
        let line = format!(
            "{}{}{}{} (cost={:.1}, rows={})",
            prefix, connector, node.operation, table_info, node.cost, node.rows
        );
        lines.push(line);

        for extra in &node.extra {
            let extra_prefix = if prefix.is_empty() {
                "    ".to_string()
            } else if is_last {
                format!("{}    ", prefix)
            } else {
                format!("{}│   ", prefix)
            };
            lines.push(format!("{}» {}", extra_prefix, extra));
        }

        let child_prefix = if prefix.is_empty() {
            "".to_string()
        } else if is_last {
            format!("{}    ", prefix)
        } else {
            format!("{}│   ", prefix)
        };

        for (i, child) in node.children.iter().enumerate() {
            let last = i == node.children.len() - 1;
            Self::render_node(child, lines, &child_prefix, last);
        }
    }

    /// Render as a single string.
    pub fn render_string(node: &PlanNode) -> String {
        Self::render(node).join("\n")
    }
}

// ── Index Advisor ─────────────────────────────────────────────────────

/// A query access pattern observed.
#[derive(Debug, Clone)]
pub struct AccessPattern {
    pub table: String,
    pub columns: Vec<String>,
    pub frequency: usize,
    pub selectivity: f64,
}

/// A recommended index.
#[derive(Debug, Clone)]
pub struct IndexRecommendation {
    pub table: String,
    pub columns: Vec<String>,
    pub score: f64,
    pub estimated_speedup: f64,
    pub reason: String,
}

/// Workload-based index recommendation engine.
pub struct IndexAdvisor {
    patterns: Vec<AccessPattern>,
    existing_indexes: Vec<(String, Vec<String>)>,
}

impl Default for IndexAdvisor {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexAdvisor {
    pub fn new() -> Self {
        Self {
            patterns: Vec::new(),
            existing_indexes: Vec::new(),
        }
    }

    /// Record an observed access pattern.
    pub fn observe(&mut self, table: &str, columns: &[&str], selectivity: f64) {
        // Merge with existing pattern if same table+columns
        let cols: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
        for p in &mut self.patterns {
            if p.table == table && p.columns == cols {
                p.frequency += 1;
                p.selectivity = (p.selectivity + selectivity) / 2.0;
                return;
            }
        }
        self.patterns.push(AccessPattern {
            table: table.to_string(),
            columns: cols,
            frequency: 1,
            selectivity,
        });
    }

    /// Register an existing index.
    pub fn add_existing_index(&mut self, table: &str, columns: &[&str]) {
        self.existing_indexes.push((
            table.to_string(),
            columns.iter().map(|c| c.to_string()).collect(),
        ));
    }

    /// Check if an index already exists for the given pattern.
    fn is_covered(&self, table: &str, columns: &[String]) -> bool {
        self.existing_indexes
            .iter()
            .any(|(t, cols)| t == table && columns.iter().all(|c| cols.contains(c)))
    }

    /// Generate index recommendations.
    pub fn recommend(&self) -> Vec<IndexRecommendation> {
        let mut recs = Vec::new();

        for pattern in &self.patterns {
            if self.is_covered(&pattern.table, &pattern.columns) {
                continue;
            }

            let score = pattern.frequency as f64 * (1.0 - pattern.selectivity);
            if score > 0.5 {
                let speedup = if pattern.selectivity < 0.1 {
                    10.0
                } else if pattern.selectivity < 0.5 {
                    3.0
                } else {
                    1.5
                };

                recs.push(IndexRecommendation {
                    table: pattern.table.clone(),
                    columns: pattern.columns.clone(),
                    score,
                    estimated_speedup: speedup,
                    reason: format!(
                        "Accessed {} times with {:.0}% selectivity",
                        pattern.frequency,
                        pattern.selectivity * 100.0
                    ),
                });
            }
        }

        recs.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        recs
    }

    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }
}

// ── Schema Migration Manager ──────────────────────────────────────────

/// Migration status.
#[derive(Debug, Clone, PartialEq)]
pub enum MigrationStatus {
    Pending,
    Applied,
    RolledBack,
    Failed(String),
}

/// A schema migration entry.
#[derive(Debug, Clone)]
pub struct Migration {
    pub version: u64,
    pub name: String,
    pub up_sql: String,
    pub down_sql: String,
    pub status: MigrationStatus,
    pub applied_at: Option<u64>,
}

/// Manages versioned schema migrations.
pub struct SchemaMigrationManager {
    migrations: Vec<Migration>,
    current_version: u64,
}

impl Default for SchemaMigrationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaMigrationManager {
    pub fn new() -> Self {
        Self {
            migrations: Vec::new(),
            current_version: 0,
        }
    }

    /// Register a migration.
    pub fn add_migration(&mut self, version: u64, name: &str, up_sql: &str, down_sql: &str) {
        self.migrations.push(Migration {
            version,
            name: name.to_string(),
            up_sql: up_sql.to_string(),
            down_sql: down_sql.to_string(),
            status: MigrationStatus::Pending,
            applied_at: None,
        });
        self.migrations.sort_by_key(|m| m.version);
    }

    /// Get pending migrations (not yet applied).
    pub fn pending(&self) -> Vec<&Migration> {
        self.migrations
            .iter()
            .filter(|m| m.status == MigrationStatus::Pending && m.version > self.current_version)
            .collect()
    }

    /// Apply a specific migration version.
    pub fn apply(&mut self, version: u64, timestamp: u64) -> Option<&str> {
        for m in &mut self.migrations {
            if m.version == version && m.status == MigrationStatus::Pending {
                m.status = MigrationStatus::Applied;
                m.applied_at = Some(timestamp);
                if version > self.current_version {
                    self.current_version = version;
                }
                return Some(&m.up_sql);
            }
        }
        None
    }

    /// Roll back a specific migration version.
    pub fn rollback(&mut self, version: u64) -> Option<String> {
        let mut found_sql = None;
        for m in &mut self.migrations {
            if m.version == version && m.status == MigrationStatus::Applied {
                m.status = MigrationStatus::RolledBack;
                found_sql = Some(m.down_sql.clone());
                break;
            }
        }
        if found_sql.is_some() && version == self.current_version {
            self.current_version = self
                .migrations
                .iter()
                .filter(|m| m.status == MigrationStatus::Applied)
                .map(|m| m.version)
                .max()
                .unwrap_or(0);
        }
        found_sql
    }

    pub fn current_version(&self) -> u64 {
        self.current_version
    }

    pub fn migration_count(&self) -> usize {
        self.migrations.len()
    }

    /// Get history of applied migrations.
    pub fn applied_history(&self) -> Vec<&Migration> {
        self.migrations
            .iter()
            .filter(|m| m.status == MigrationStatus::Applied)
            .collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_select_star() {
        let checker = SqlLintChecker::new();
        let issues = checker.check("SELECT * FROM users");
        assert!(issues.iter().any(|i| i.rule == "no-select-star"));
    }

    #[test]
    fn lint_missing_where() {
        let checker = SqlLintChecker::new();
        let issues = checker.check("DELETE FROM users");
        assert!(issues.iter().any(|i| i.rule == "missing-where"));
    }

    #[test]
    fn lint_no_issues() {
        let checker = SqlLintChecker::new();
        let issues = checker.check("SELECT id, name FROM users WHERE id = 1");
        assert!(issues.is_empty());
    }

    #[test]
    fn plan_visualizer_simple() {
        let mut root = PlanNode::new("Seq Scan", 100.0, 1000).with_table("users");
        let idx = PlanNode::new("Index Scan", 10.0, 50)
            .with_table("orders")
            .with_extra("Using index: idx_user_id");
        root.add_child(idx);

        let lines = PlanVisualizer::render(&root);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("Seq Scan"));
        assert!(root.total_cost() > 100.0);
    }

    #[test]
    fn plan_visualizer_total_cost() {
        let mut root = PlanNode::new("Hash Join", 50.0, 500);
        root.add_child(PlanNode::new("Scan A", 20.0, 200));
        root.add_child(PlanNode::new("Scan B", 30.0, 300));
        assert!((root.total_cost() - 100.0).abs() < 0.01);
    }

    #[test]
    fn index_advisor_recommendation() {
        let mut advisor = IndexAdvisor::new();
        advisor.observe("users", &["email"], 0.05);
        advisor.observe("users", &["email"], 0.05);
        advisor.observe("users", &["email"], 0.05);

        let recs = advisor.recommend();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].table, "users");
        assert!(recs[0].estimated_speedup > 5.0);
    }

    #[test]
    fn index_advisor_covered() {
        let mut advisor = IndexAdvisor::new();
        advisor.add_existing_index("users", &["email"]);
        advisor.observe("users", &["email"], 0.05);
        advisor.observe("users", &["email"], 0.05);

        let recs = advisor.recommend();
        assert!(recs.is_empty()); // already indexed
    }

    #[test]
    fn schema_migration_apply() {
        let mut mgr = SchemaMigrationManager::new();
        mgr.add_migration(
            1,
            "create_users",
            "CREATE TABLE users (id INT)",
            "DROP TABLE users",
        );
        mgr.add_migration(
            2,
            "add_email",
            "ALTER TABLE users ADD email TEXT",
            "ALTER TABLE users DROP email",
        );

        assert_eq!(mgr.pending().len(), 2);
        let sql = mgr.apply(1, 1000);
        assert!(sql.is_some());
        assert_eq!(mgr.current_version(), 1);
        assert_eq!(mgr.pending().len(), 1);
    }

    #[test]
    fn schema_migration_rollback() {
        let mut mgr = SchemaMigrationManager::new();
        mgr.add_migration(1, "v1", "CREATE TABLE t1 (id INT)", "DROP TABLE t1");
        mgr.apply(1, 1000);
        assert_eq!(mgr.current_version(), 1);

        let down_sql = mgr.rollback(1);
        assert!(down_sql.is_some());
        assert_eq!(mgr.current_version(), 0);
    }

    #[test]
    fn lint_disable_rule() {
        let mut checker = SqlLintChecker::new();
        checker.disable_rule(&LintRule::NoSelectStar);
        let issues = checker.check("SELECT * FROM users");
        assert!(!issues.iter().any(|i| i.rule == "no-select-star"));
    }
}
