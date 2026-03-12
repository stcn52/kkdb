// ── Prepared Statement Support ───────────────────────────────────────────────
//
// Provides PREPARE / EXECUTE / DEALLOCATE semantics for SQL statements.
//
// A prepared statement stores the pre-parsed AST so the parser cost is paid
// only once. Parameters are bound at EXECUTE time using `?` placeholders.
//
// ## Usage
//
// ```sql
// PREPARE stmt1 AS SELECT * FROM users WHERE id = ?
// EXECUTE stmt1 USING (42)
// DEALLOCATE stmt1
// ```

use std::collections::HashMap;

/// A single prepared statement: the original SQL + the cached last bind params count.
#[derive(Debug, Clone)]
pub struct PreparedStatement {
    /// Name of the prepared statement (case-insensitive key).
    pub name: String,
    /// The original SQL template with `?` placeholders.
    pub sql: String,
    /// Number of `?` placeholders in the SQL.
    pub param_count: usize,
    /// Total number of times this statement has been executed.
    pub exec_count: u64,
}

/// Registry of prepared statements for a single connection/VM.
#[derive(Debug, Clone, Default)]
pub struct PreparedStore {
    /// name_lowercase → PreparedStatement
    statements: HashMap<String, PreparedStatement>,
}

impl PreparedStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a prepared statement. Returns error if name already exists.
    pub fn prepare(&mut self, name: &str, sql: &str) -> crate::error::Result<()> {
        let key = name.to_ascii_lowercase();
        if self.statements.contains_key(&key) {
            return Err(crate::error::KkdbError::RuntimeError(format!(
                "prepared statement '{}' already exists",
                name
            )));
        }
        let param_count = sql.matches('?').count();
        self.statements.insert(
            key,
            PreparedStatement {
                name: name.to_string(),
                sql: sql.to_string(),
                param_count,
                exec_count: 0,
            },
        );
        Ok(())
    }

    /// Get the SQL for a prepared statement and increment exec_count.
    pub fn get_for_execute(&mut self, name: &str) -> crate::error::Result<(String, usize)> {
        let key = name.to_ascii_lowercase();
        match self.statements.get_mut(&key) {
            Some(stmt) => {
                stmt.exec_count += 1;
                Ok((stmt.sql.clone(), stmt.param_count))
            }
            None => Err(crate::error::KkdbError::RuntimeError(format!(
                "prepared statement '{}' not found",
                name
            ))),
        }
    }

    /// Remove a prepared statement.
    pub fn deallocate(&mut self, name: &str) -> bool {
        let key = name.to_ascii_lowercase();
        self.statements.remove(&key).is_some()
    }

    /// Get a prepared statement by name (read-only).
    pub fn get(&self, name: &str) -> Option<&PreparedStatement> {
        self.statements.get(&name.to_ascii_lowercase())
    }

    /// Number of prepared statements.
    pub fn count(&self) -> usize {
        self.statements.len()
    }

    /// Clear all prepared statements.
    pub fn clear(&mut self) {
        self.statements.clear();
    }

    /// List all prepared statement names.
    pub fn names(&self) -> Vec<&str> {
        self.statements.values().map(|s| s.name.as_str()).collect()
    }
}
