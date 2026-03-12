// ── Audit Log Module ─────────────────────────────────────────────────────────
//
// Provides SQL operation auditing for security compliance.
// Records every SQL statement executed along with metadata:
//   - timestamp
//   - user identity (if set)
//   - SQL text (optionally sanitised)
//   - result status (success/failure)
//   - affected row count
//
// The audit log is in-memory by default; production deployments can
// flush it to disk via `drain()`.

use std::time::SystemTime;

/// A single audit record.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    /// Monotonically increasing sequence number.
    pub seq: u64,
    /// When the statement was executed.
    pub timestamp: SystemTime,
    /// User who issued the statement (empty if unauthenticated).
    pub user: String,
    /// SQL statement text.
    pub sql: String,
    /// Whether the statement succeeded.
    pub success: bool,
    /// Number of rows affected (0 for queries / DDL).
    pub rows_affected: usize,
    /// Optional error message on failure.
    pub error: Option<String>,
    /// Statement category.
    pub category: AuditCategory,
}

/// Broad classification of audited operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditCategory {
    /// DDL: CREATE, DROP, ALTER
    Ddl,
    /// DML: INSERT, UPDATE, DELETE
    Dml,
    /// Query: SELECT
    Query,
    /// Transaction control: BEGIN, COMMIT, ROLLBACK
    Txn,
    /// System: SET, ANALYZE, VACUUM, EXPLAIN
    System,
    /// Auth: CREATE USER, GRANT, REVOKE
    Auth,
}

impl AuditCategory {
    /// Categorize a SQL statement by its first keyword.
    pub fn from_sql(sql: &str) -> Self {
        let upper = sql.trim_start().to_ascii_uppercase();
        if upper.starts_with("SELECT") || upper.starts_with("EXECUTE") {
            AuditCategory::Query
        } else if upper.starts_with("INSERT")
            || upper.starts_with("UPDATE")
            || upper.starts_with("DELETE")
        {
            AuditCategory::Dml
        } else if upper.starts_with("CREATE TABLE")
            || upper.starts_with("DROP")
            || upper.starts_with("ALTER")
        {
            AuditCategory::Ddl
        } else if upper.starts_with("BEGIN")
            || upper.starts_with("COMMIT")
            || upper.starts_with("ROLLBACK")
            || upper.starts_with("SAVEPOINT")
        {
            AuditCategory::Txn
        } else if upper.starts_with("CREATE USER")
            || upper.starts_with("GRANT")
            || upper.starts_with("REVOKE")
            || upper.starts_with("CREATE POLICY")
        {
            AuditCategory::Auth
        } else {
            AuditCategory::System
        }
    }
}

impl std::fmt::Display for AuditCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditCategory::Ddl => write!(f, "DDL"),
            AuditCategory::Dml => write!(f, "DML"),
            AuditCategory::Query => write!(f, "QUERY"),
            AuditCategory::Txn => write!(f, "TXN"),
            AuditCategory::System => write!(f, "SYSTEM"),
            AuditCategory::Auth => write!(f, "AUTH"),
        }
    }
}

/// In-memory audit log with configurable capacity.
#[derive(Debug)]
pub struct AuditLog {
    entries: Vec<AuditEntry>,
    next_seq: u64,
    /// Maximum entries kept in memory before oldest are discarded.
    max_entries: usize,
    /// Whether audit logging is enabled.
    enabled: bool,
    /// Categories to audit (empty = audit all).
    filter: Vec<AuditCategory>,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            next_seq: 1,
            max_entries: 10_000,
            enabled: false, // off by default for performance
            filter: Vec::new(),
        }
    }
}

impl AuditLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an audit log with a custom capacity.
    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            max_entries,
            ..Self::default()
        }
    }

    /// Enable audit logging.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Disable audit logging.
    pub fn disable(&mut self) {
        self.enabled = false;
    }

    /// Check if audit logging is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Set categories to audit (empty = audit all).
    pub fn set_filter(&mut self, categories: Vec<AuditCategory>) {
        self.filter = categories;
    }

    /// Record a SQL execution result.
    pub fn record(
        &mut self,
        user: &str,
        sql: &str,
        success: bool,
        rows_affected: usize,
        error: Option<&str>,
    ) {
        if !self.enabled {
            return;
        }
        let category = AuditCategory::from_sql(sql);
        if !self.filter.is_empty() && !self.filter.contains(&category) {
            return;
        }
        let entry = AuditEntry {
            seq: self.next_seq,
            timestamp: SystemTime::now(),
            user: user.to_string(),
            sql: sql.to_string(),
            success,
            rows_affected,
            error: error.map(|s| s.to_string()),
            category,
        };
        self.next_seq += 1;
        self.entries.push(entry);
        // Evict oldest if over capacity
        if self.entries.len() > self.max_entries {
            let excess = self.entries.len() - self.max_entries;
            self.entries.drain(0..excess);
        }
    }

    /// Total number of entries currently stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get all entries (read-only).
    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }

    /// Get the last N entries.
    pub fn last_n(&self, n: usize) -> &[AuditEntry] {
        let start = self.entries.len().saturating_sub(n);
        &self.entries[start..]
    }

    /// Drain all entries and return them (for flushing to disk).
    pub fn drain(&mut self) -> Vec<AuditEntry> {
        std::mem::take(&mut self.entries)
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Search entries by SQL text substring (case-insensitive).
    pub fn search(&self, pattern: &str) -> Vec<&AuditEntry> {
        let pat = pattern.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|e| e.sql.to_ascii_lowercase().contains(&pat))
            .collect()
    }

    /// Count entries by category.
    pub fn count_by_category(&self, category: AuditCategory) -> usize {
        self.entries.iter().filter(|e| e.category == category).count()
    }

    /// Count failed statements.
    pub fn failure_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.success).count()
    }
}

/// R11: Simple SQL injection detection heuristics.
///
/// Returns `true` if the SQL appears to contain common injection patterns.
/// This is a best-effort heuristic, NOT a substitute for parameterised queries.
pub fn detect_sql_injection(sql: &str) -> bool {
    let upper = sql.to_ascii_uppercase();
    // Pattern 1: classic comment injection
    if upper.contains("--") && upper.contains("OR") && upper.contains("1=1") {
        return true;
    }
    // Pattern 2: UNION-based injection
    if upper.contains("UNION") && upper.contains("SELECT") && upper.contains("FROM") {
        // Allow legitimate UNION queries — flag only if there's a suspicious pattern
        // like quotes followed by UNION
        if upper.contains("' UNION") || upper.contains("\" UNION") {
            return true;
        }
    }
    // Pattern 3: stacked queries via semicolons with DROP/DELETE
    if sql.contains(';') {
        let parts: Vec<&str> = sql.split(';').collect();
        if parts.len() > 1 {
            for part in &parts[1..] {
                let trimmed = part.trim().to_ascii_uppercase();
                if trimmed.starts_with("DROP") || trimmed.starts_with("DELETE") {
                    return true;
                }
            }
        }
    }
    // Pattern 4: hex/char encoding bypass attempts
    if upper.contains("CHAR(") && (upper.contains("DROP") || upper.contains("DELETE")) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_log_disabled_by_default() {
        let log = AuditLog::new();
        assert!(!log.is_enabled());
    }

    #[test]
    fn audit_log_record_when_disabled() {
        let mut log = AuditLog::new();
        log.record("admin", "SELECT 1", true, 0, None);
        assert_eq!(log.len(), 0); // not stored because disabled
    }

    #[test]
    fn audit_log_record_when_enabled() {
        let mut log = AuditLog::new();
        log.enable();
        log.record("admin", "SELECT 1", true, 0, None);
        assert_eq!(log.len(), 1);
        assert_eq!(log.entries()[0].user, "admin");
        assert_eq!(log.entries()[0].category, AuditCategory::Query);
    }

    #[test]
    fn audit_log_capacity() {
        let mut log = AuditLog::with_capacity(3);
        log.enable();
        for i in 0..5 {
            log.record("u", &format!("SELECT {i}"), true, 0, None);
        }
        assert_eq!(log.len(), 3);
        // Oldest should be evicted
        assert_eq!(log.entries()[0].seq, 3);
    }

    #[test]
    fn audit_log_drain() {
        let mut log = AuditLog::new();
        log.enable();
        log.record("u", "INSERT INTO t VALUES (1)", true, 1, None);
        let drained = log.drain();
        assert_eq!(drained.len(), 1);
        assert!(log.is_empty());
    }

    #[test]
    fn audit_log_category_filter() {
        let mut log = AuditLog::new();
        log.enable();
        log.set_filter(vec![AuditCategory::Dml]);
        log.record("u", "SELECT 1", true, 0, None); // filtered out
        log.record("u", "INSERT INTO t VALUES (1)", true, 1, None); // kept
        assert_eq!(log.len(), 1);
        assert_eq!(log.entries()[0].category, AuditCategory::Dml);
    }

    #[test]
    fn audit_log_search() {
        let mut log = AuditLog::new();
        log.enable();
        log.record("u", "SELECT * FROM users", true, 0, None);
        log.record("u", "DELETE FROM orders", true, 5, None);
        let found = log.search("users");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn audit_log_failure_count() {
        let mut log = AuditLog::new();
        log.enable();
        log.record("u", "SELECT 1", true, 0, None);
        log.record("u", "SELECT bad", false, 0, Some("syntax error"));
        assert_eq!(log.failure_count(), 1);
    }

    #[test]
    fn audit_log_count_by_category() {
        let mut log = AuditLog::new();
        log.enable();
        log.record("u", "SELECT 1", true, 0, None);
        log.record("u", "SELECT 2", true, 0, None);
        log.record("u", "INSERT INTO t VALUES (1)", true, 1, None);
        assert_eq!(log.count_by_category(AuditCategory::Query), 2);
        assert_eq!(log.count_by_category(AuditCategory::Dml), 1);
    }

    #[test]
    fn audit_log_last_n() {
        let mut log = AuditLog::new();
        log.enable();
        for i in 0..10 {
            log.record("u", &format!("SELECT {i}"), true, 0, None);
        }
        let last3 = log.last_n(3);
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0].seq, 8);
    }

    #[test]
    fn audit_category_from_sql() {
        assert_eq!(AuditCategory::from_sql("SELECT 1"), AuditCategory::Query);
        assert_eq!(AuditCategory::from_sql("INSERT INTO t VALUES (1)"), AuditCategory::Dml);
        assert_eq!(AuditCategory::from_sql("CREATE TABLE t (id INT)"), AuditCategory::Ddl);
        assert_eq!(AuditCategory::from_sql("BEGIN"), AuditCategory::Txn);
        assert_eq!(AuditCategory::from_sql("GRANT SELECT ON t TO u"), AuditCategory::Auth);
        assert_eq!(AuditCategory::from_sql("VACUUM"), AuditCategory::System);
    }

    #[test]
    fn audit_category_display() {
        assert_eq!(format!("{}", AuditCategory::Ddl), "DDL");
        assert_eq!(format!("{}", AuditCategory::Query), "QUERY");
    }

    #[test]
    fn sql_injection_detection() {
        assert!(detect_sql_injection("' OR 1=1 --"));
        assert!(detect_sql_injection("SELECT 1; DROP TABLE users"));
        assert!(detect_sql_injection("SELECT CHAR(68) DROP"));
        assert!(!detect_sql_injection("SELECT * FROM users WHERE id = 1"));
        assert!(!detect_sql_injection("INSERT INTO t VALUES (1, 'hello')"));
    }
}
