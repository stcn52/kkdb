// R19 – Test catalog & classification system
//
// Provides:
//   - `TestCategory`: enum of test categories matching directory structure
//   - `TestEntry`: metadata for a test file
//   - `TestCatalog`: registry of all test files with search/filter

use std::collections::HashMap;

/// Test category matching directory structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestCategory {
    /// Core SQL: basic ops, SELECT, operators, functions, JOIN, DDL, params
    Core,
    /// Expression evaluation, sqlparser coverage
    Expression,
    /// Transaction & MVCC: visibility, row locks, read committed
    Transaction,
    /// Feature-specific: R5, emoji, query cache, binlog, raft
    Feature,
    /// Legacy coverage pushes (R1-R6)
    CoverageLegacy,
    /// R7 coverage push80 series
    CoveragePush80,
    /// Round-based coverage (R8+)
    CoverageRounds,
    /// Inline module tests
    InlineUnit,
}

impl TestCategory {
    pub fn directory(&self) -> &'static str {
        match self {
            TestCategory::Core => "execute_tests/core",
            TestCategory::Expression => "execute_tests/expr",
            TestCategory::Transaction => "execute_tests/txn",
            TestCategory::Feature => "execute_tests/feature",
            TestCategory::CoverageLegacy => "execute_tests/coverage",
            TestCategory::CoveragePush80 => "execute_tests/push80",
            TestCategory::CoverageRounds => "execute_tests/rounds",
            TestCategory::InlineUnit => "(inline)",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            TestCategory::Core => "Core SQL operations",
            TestCategory::Expression => "Expression evaluation & parser",
            TestCategory::Transaction => "Transaction & MVCC",
            TestCategory::Feature => "Feature-specific tests",
            TestCategory::CoverageLegacy => "Legacy coverage push (R1-R6)",
            TestCategory::CoveragePush80 => "R7 coverage push80",
            TestCategory::CoverageRounds => "Round-based coverage (R8+)",
            TestCategory::InlineUnit => "Inline unit tests",
        }
    }
}

/// Tag for filtering tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestTag {
    Sql,
    Storage,
    Raft,
    Security,
    Performance,
    Optimizer,
    FullText,
    Vector,
    Mvcc,
    Ddl,
    Dml,
    Integration,
    Unit,
}

/// Metadata for a test file.
#[derive(Debug, Clone)]
pub struct TestEntry {
    pub file_name: String,
    pub category: TestCategory,
    pub tags: Vec<TestTag>,
    pub test_count: usize,
    pub description: String,
}

/// Registry of all test files with search/filter capabilities.
pub struct TestCatalog {
    entries: Vec<TestEntry>,
    tag_index: HashMap<TestTag, Vec<usize>>,
}

impl TestCatalog {
    pub fn new() -> Self {
        Self { entries: Vec::new(), tag_index: HashMap::new() }
    }

    /// Build the full catalog with all known test files.
    pub fn build_full() -> Self {
        let mut cat = Self::new();

        // Core SQL tests
        cat.add("basic.rs", TestCategory::Core, vec![TestTag::Sql, TestTag::Unit],
            "VM construction, CREATE/DROP, INSERT basics");
        cat.add("select.rs", TestCategory::Core, vec![TestTag::Sql, TestTag::Dml],
            "SELECT expressions, UPDATE, DELETE");
        cat.add("operators.rs", TestCategory::Core, vec![TestTag::Sql, TestTag::Unit],
            "Binary/unary operators, IS NULL, LIKE, BETWEEN");
        cat.add("functions.rs", TestCategory::Core, vec![TestTag::Sql, TestTag::Unit],
            "Scalar and aggregate functions");
        cat.add("join.rs", TestCategory::Core, vec![TestTag::Sql, TestTag::Unit],
            "INNER, LEFT, RIGHT JOIN");
        cat.add("ddl.rs", TestCategory::Core, vec![TestTag::Sql, TestTag::Ddl],
            "EXPLAIN, CREATE INDEX, aliases, subqueries");
        cat.add("params.rs", TestCategory::Core, vec![TestTag::Sql, TestTag::Unit],
            "Parameterized queries");

        // Expression tests
        cat.add("expressions.rs", TestCategory::Expression, vec![TestTag::Sql, TestTag::Unit],
            "Fine-grained expression coverage");
        cat.add("eval_expr_coverage.rs", TestCategory::Expression, vec![TestTag::Sql, TestTag::Unit],
            "Expression evaluator coverage");
        cat.add("sqlparser_expr_coverage.rs", TestCategory::Expression, vec![TestTag::Sql, TestTag::Unit],
            "SQL parser expression coverage");
        cat.add("eval_expr_r5.rs", TestCategory::Expression, vec![TestTag::Sql, TestTag::Unit],
            "R5 expression enhancements");

        // Transaction tests
        cat.add("transactions.rs", TestCategory::Transaction, vec![TestTag::Mvcc, TestTag::Integration],
            "Transaction commit/rollback, isolation");
        cat.add("mvcc_visibility.rs", TestCategory::Transaction, vec![TestTag::Mvcc, TestTag::Unit],
            "MVCC visibility rules");
        cat.add("mvcc_row_lock.rs", TestCategory::Transaction, vec![TestTag::Mvcc, TestTag::Unit],
            "Row-level locking");
        cat.add("select_for_update.rs", TestCategory::Transaction, vec![TestTag::Mvcc, TestTag::Sql],
            "SELECT FOR UPDATE");
        cat.add("read_committed.rs", TestCategory::Transaction, vec![TestTag::Mvcc, TestTag::Unit],
            "Read committed isolation level");

        // Feature tests
        cat.add("r5_features.rs", TestCategory::Feature, vec![TestTag::Sql, TestTag::Unit],
            "R5 features: UNNEST, GROUP BY alias");
        cat.add("emoji_compat.rs", TestCategory::Feature, vec![TestTag::Sql, TestTag::Unit],
            "Unicode/emoji compatibility");
        cat.add("query_cache_integration.rs", TestCategory::Feature, vec![TestTag::Performance, TestTag::Integration],
            "Query cache integration");
        cat.add("binlog_coverage.rs", TestCategory::Feature, vec![TestTag::Storage, TestTag::Unit],
            "Binlog coverage");
        cat.add("raft_coverage.rs", TestCategory::Feature, vec![TestTag::Raft, TestTag::Integration],
            "Raft consensus coverage");

        // Round coverage R8-R18
        cat.add("coverage_r8_optimizer_wal.rs", TestCategory::CoverageRounds,
            vec![TestTag::Optimizer, TestTag::Storage], "R8: optimizer + WAL");
        cat.add("coverage_r9_mvcc_fts_raft.rs", TestCategory::CoverageRounds,
            vec![TestTag::Mvcc, TestTag::FullText, TestTag::Raft], "R9: MVCC + FTS + Raft");
        cat.add("coverage_r10_prepared_bloom_wf.rs", TestCategory::CoverageRounds,
            vec![TestTag::Sql, TestTag::Storage], "R10: prepared statements + bloom + WF");
        cat.add("coverage_r18_adv_query.rs", TestCategory::CoverageRounds,
            vec![TestTag::Sql, TestTag::Optimizer], "R18: advanced query processing");

        cat.rebuild_index();
        cat
    }

    pub fn add(&mut self, file_name: &str, category: TestCategory, tags: Vec<TestTag>, desc: &str) {
        self.entries.push(TestEntry {
            file_name: file_name.to_string(),
            category,
            tags,
            test_count: 0,
            description: desc.to_string(),
        });
    }

    fn rebuild_index(&mut self) {
        self.tag_index.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            for tag in &entry.tags {
                self.tag_index.entry(*tag).or_default().push(i);
            }
        }
    }

    /// Filter by category.
    pub fn by_category(&self, cat: TestCategory) -> Vec<&TestEntry> {
        self.entries.iter().filter(|e| e.category == cat).collect()
    }

    /// Filter by tag.
    pub fn by_tag(&self, tag: TestTag) -> Vec<&TestEntry> {
        self.tag_index.get(&tag)
            .map(|indices| indices.iter().map(|&i| &self.entries[i]).collect())
            .unwrap_or_default()
    }

    pub fn total_files(&self) -> usize {
        self.entries.len()
    }

    pub fn categories_summary(&self) -> HashMap<TestCategory, usize> {
        let mut map = HashMap::new();
        for e in &self.entries {
            *map.entry(e.category).or_insert(0) += 1;
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalog_build() {
        let cat = TestCatalog::build_full();
        assert!(cat.total_files() > 20);
    }

    #[test]
    fn test_catalog_by_category() {
        let cat = TestCatalog::build_full();
        let core = cat.by_category(TestCategory::Core);
        assert_eq!(core.len(), 7); // 7 core files
    }

    #[test]
    fn test_catalog_by_tag() {
        let cat = TestCatalog::build_full();
        let sql_tests = cat.by_tag(TestTag::Sql);
        assert!(sql_tests.len() > 5);
    }

    #[test]
    fn test_category_directory() {
        assert_eq!(TestCategory::Core.directory(), "execute_tests/core");
        assert_eq!(TestCategory::CoverageRounds.directory(), "execute_tests/rounds");
    }

    #[test]
    fn test_catalog_summary() {
        let cat = TestCatalog::build_full();
        let summary = cat.categories_summary();
        assert!(summary.contains_key(&TestCategory::Core));
        assert!(summary.contains_key(&TestCategory::Transaction));
    }
}
