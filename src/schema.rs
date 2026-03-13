use crate::error::{KkdbError, Result};
use crate::sql::ast::{ColumnDef, Expr, UnaryOperator};
use crate::storage::btree::BTree;
use crate::storage::pager::Pager;
use crate::types::{DataType, Row, Value};
use std::collections::{HashMap, HashSet};

/// Helper for zero-alloc lowercase key lookup (fast path: already lowercase 鈫?borrow)
enum LowercaseKey<'a> {
    Borrowed(&'a str),
    Owned(String),
}

impl<'a> AsRef<str> for LowercaseKey<'a> {
    #[inline]
    fn as_ref(&self) -> &str {
        match self {
            LowercaseKey::Borrowed(s) => s,
            LowercaseKey::Owned(s) => s.as_str(),
        }
    }
}

/// Schema table root is tracked by the pager (v2 superblock schema_root)
/// Each row in the schema table represents a database object (table, index, etc.)
///
/// Schema table columns:
///   type TEXT      - "table" or "index"
///   name TEXT      - name of the object
///   tbl_name TEXT  - table name (same as name for tables)
///   rootpage INT   - root page number of the B-tree
///   sql TEXT       - CREATE statement
///
/// Cached table schema info
#[derive(Debug, Clone)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    pub col_names: Vec<String>,
    pub root_page: u32,
    pub next_rowid: i64,
    /// Non-None for views: holds the SELECT AST to expand at query time (Batch E)
    pub view_select: Option<crate::sql::ast::SelectStmt>,
    /// L1: Foreign key constraints defined on this table
    pub foreign_keys: Vec<ForeignKey>,
    /// L2: CHECK constraints: (optional constraint name, expression AST)
    pub check_constraints: Vec<(Option<String>, crate::sql::ast::Expr)>,
    /// L4: True if this table is an FTS virtual table
    pub is_fts: bool,
    /// RLS: whether row-level security is enabled on this table
    pub rls_enabled: bool,
    /// RLS: list of policies defined on this table
    pub policies: Vec<PolicySchema>,
    /// InnoDB-style flag: true when data B-Tree is ordered by the primary key (clustered).
    /// Always true for KKDB tables — data is stored in rowid order, and
    /// INTEGER PRIMARY KEY is mapped directly to the internal rowid.
    pub clustered_index: bool,
}

impl TableSchema {
    /// Return the name of the primary key column, if exactly one exists.
    pub fn primary_key_column(&self) -> Option<&str> {
        let mut pk = None;
        for col in &self.columns {
            if col.primary_key {
                if pk.is_some() {
                    return None; // composite PK — not a single column
                }
                pk = Some(col.name.as_str());
            }
        }
        pk
    }

    /// Return the column index of the primary key, if exactly one PK column exists.
    pub fn primary_key_col_index(&self) -> Option<usize> {
        let mut pk_idx = None;
        for col in &self.columns {
            if col.primary_key {
                if pk_idx.is_some() {
                    return None;
                }
                pk_idx = Some(col.col_index);
            }
        }
        pk_idx
    }

    /// Check if the primary key is an INTEGER type (InnoDB-style clustered by rowid).
    pub fn pk_is_integer_clustered(&self) -> bool {
        self.clustered_index
            && self
                .columns
                .iter()
                .any(|c| c.primary_key && matches!(c.data_type, DataType::Integer))
    }
}

/// RLS policy stored in memory alongside the TableSchema
#[derive(Debug, Clone)]
pub struct PolicySchema {
    /// Policy name
    pub name: String,
    /// Optional role restriction (None = all users)
    pub role: Option<String>,
    /// USING expression (evaluated at SELECT time for row filtering)
    pub using_expr: Option<crate::sql::ast::Expr>,
    /// WITH CHECK expression (evaluated at INSERT/UPDATE time)
    pub check_expr: Option<crate::sql::ast::Expr>,
}

/// 表列的完整元数据信息。
///
/// 包含列名、数据类型、约束（PRIMARY KEY / NOT NULL / UNIQUE）、
/// 自增标记、列索引位置及统计信息。
#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: DataType,
    pub primary_key: bool,
    pub autoincrement: bool,
    pub not_null: bool,
    pub unique: bool,
    pub col_index: usize,
    /// O1: column-level statistics (populated by ANALYZE TABLE)
    pub stats: Option<ColumnStats>,
}

/// O1: Column statistics for cost-based optimizer hints.
#[derive(Debug, Clone)]
pub struct ColumnStats {
    /// Total rows scanned
    pub total_count: i64,
    /// Number of NULL values
    pub null_count: i64,
    /// Number of distinct non-null values
    pub ndv: i64,
    /// Minimum value (None if all NULL or empty)
    pub min: Option<crate::types::Value>,
    /// Maximum value (None if all NULL or empty)
    pub max: Option<crate::types::Value>,
    /// O2: Equi-depth histogram buckets for range selectivity estimation.
    /// Each bucket stores (upper_bound, cumulative_count).
    /// `cumulative_count` is the number of rows with value ≤ `upper_bound`.
    pub histogram: Option<Vec<HistogramBucket>>,
}

/// O2: A single equi-depth histogram bucket for CBO range estimation.
#[derive(Debug, Clone)]
pub struct HistogramBucket {
    /// Upper bound value of this bucket (inclusive)
    pub upper_bound: crate::types::Value,
    /// Cumulative row count: total rows with value ≤ upper_bound
    pub cumulative_count: i64,
    /// Number of distinct values in this bucket
    pub ndv_in_bucket: i64,
}

impl ColumnStats {
    /// R11: Estimate the selectivity of `value = constant` using histogram if available.
    ///
    /// Returns an estimated fraction [0.0, 1.0] of rows matching the equality predicate.
    /// Falls back to 1/ndv if no histogram is available.
    pub fn selectivity_eq(&self, _val: &crate::types::Value) -> f64 {
        if self.total_count == 0 {
            return 0.0;
        }
        if self.ndv == 0 {
            return 0.0;
        }
        // Simple uniform assumption: 1 / NDV
        1.0 / self.ndv as f64
    }

    /// R11: Estimate the selectivity of `value < constant` using histogram.
    ///
    /// Returns an estimated fraction [0.0, 1.0] of rows satisfying the range predicate.
    pub fn selectivity_lt(&self, val: &crate::types::Value) -> f64 {
        if self.total_count == 0 {
            return 0.0;
        }
        if let Some(ref buckets) = self.histogram {
            if buckets.is_empty() {
                return 0.5; // no information, assume 50%
            }
            // Find the bucket whose upper_bound >= val
            for bucket in buckets {
                if &bucket.upper_bound >= val {
                    // Linear interpolation within the bucket
                    let bucket_rows = bucket.cumulative_count;
                    return (bucket_rows as f64) / (self.total_count as f64);
                }
            }
            // val > all bucket upper bounds → selectivity ≈ 1.0
            return 1.0;
        }
        // No histogram: use linear interpolation between min and max
        if let (Some(ref min), Some(ref max)) = (&self.min, &self.max) {
            if min >= max {
                return 0.5;
            }
            if val <= min {
                return 0.0;
            }
            if val >= max {
                return 1.0;
            }
            // Simple linear interpolation using integer values
            match (min, max, val) {
                (
                    crate::types::Value::Integer(mn),
                    crate::types::Value::Integer(mx),
                    crate::types::Value::Integer(v),
                ) => (*v - mn) as f64 / (*mx - mn) as f64,
                _ => 0.5,
            }
        } else {
            0.5
        }
    }

    /// R11: Estimate the selectivity of `value BETWEEN lo AND hi`.
    pub fn selectivity_between(&self, lo: &crate::types::Value, hi: &crate::types::Value) -> f64 {
        let s_hi = self.selectivity_lt(hi);
        let s_lo = self.selectivity_lt(lo);
        (s_hi - s_lo).clamp(0.0, 1.0)
    }

    /// R11: Null fraction.
    pub fn null_fraction(&self) -> f64 {
        if self.total_count == 0 {
            return 0.0;
        }
        self.null_count as f64 / self.total_count as f64
    }
}

impl HistogramBucket {
    /// Create a new bucket.
    pub fn new(
        upper_bound: crate::types::Value,
        cumulative_count: i64,
        ndv_in_bucket: i64,
    ) -> Self {
        Self {
            upper_bound,
            cumulative_count,
            ndv_in_bucket,
        }
    }
}

/// L1: Represents a FOREIGN KEY constraint stored in the schema.
#[derive(Debug, Clone)]
pub struct ForeignKey {
    /// The column in this table that holds the FK value
    pub col_name: String,
    /// The column index in this table
    pub col_index: usize,
    /// Referenced table name (lowercase)
    pub ref_table: String,
    /// Referenced column name (or the PK if empty)
    pub ref_col: Option<String>,
    /// Action on parent DELETE
    pub on_delete: crate::sql::ast::FkAction,
    /// Action on parent UPDATE
    pub on_update: crate::sql::ast::FkAction,
}

/// L3: Trigger definition cached in schema
#[derive(Debug, Clone)]
pub struct TriggerSchema {
    pub name: String,
    pub timing: crate::sql::ast::TriggerTiming,
    pub event: crate::sql::ast::TriggerEvent,
    pub table_name: String,
    pub body_sql: String,
    pub rowid: i64,
}

/// Cached index schema info
#[derive(Debug, Clone)]
pub struct IndexSchema {
    pub name: String,
    pub table_name: String,
    pub columns: Vec<String>,
    pub root_page: u32,
    pub unique: bool,
    /// True for FULLTEXT INDEX entries (stored in same schema table as type="fulltext_index")
    pub is_fts: bool,
}

/// Schema manager - maintains the catalog of tables
#[derive(Clone)]
pub struct Schema {
    pub tables: HashMap<String, TableSchema>,
    pub indexes: HashMap<String, IndexSchema>,
    indexes_by_table: HashMap<String, Vec<String>>,
    /// L3: Triggers grouped by table name (lowercase)
    pub triggers: HashMap<String, Vec<TriggerSchema>>,
    /// Vector indexes: in-memory HNSW graphs keyed by index name
    pub vector_indexes: crate::vector::VectorIndexRegistry,
}

impl Default for Schema {
    fn default() -> Self {
        Self::new()
    }
}

impl Schema {
    /// 创建一个空的 Schema。
    ///
    /// 所有集合（tables / indexes / triggers / vector_indexes）均初始为空。
    /// 调用 [`Schema::load_from_pager`] 可从磁盘加载已持久化的元数据。
    pub fn new() -> Self {
        Schema {
            tables: HashMap::new(),
            indexes: HashMap::new(),
            indexes_by_table: HashMap::new(),
            triggers: HashMap::new(),
            vector_indexes: crate::vector::VectorIndexRegistry::new(),
        }
    }

    /// Load all table schemas from the schema table root page.
    pub fn load_from_pager(&mut self, pager: &mut Pager) -> Result<()> {
        self.tables.clear();
        self.indexes.clear();
        self.indexes_by_table.clear();

        let schema_root = pager.schema_root_page();
        let mut btree = BTree::new(pager);
        let rows = btree.scan_all(schema_root)?;

        for (_rowid, row) in rows {
            if row.len() < 5 {
                continue;
            }

            let obj_type = match &row[0] {
                Value::Text(s) => s.to_string(),
                _ => continue,
            };
            let name = match &row[1] {
                Value::Text(s) => s.to_string(),
                _ => continue,
            };
            let root_page = match &row[3] {
                Value::Integer(v) => *v as u32,
                _ => continue,
            };
            let sql = match &row[4] {
                Value::Text(s) => s.to_string(),
                _ => continue,
            };

            if obj_type == "table" || obj_type == "fts_table" {
                let is_fts = obj_type == "fts_table";
                // Parse the CREATE TABLE SQL to get column info
                match crate::sql::parser::parse_sql(&sql) {
                    Ok(crate::sql::ast::Statement::CreateTable(create)) => {
                        let mut columns = Vec::new();
                        let mut check_constraints = Vec::new();
                        // C1 fix: reconstruct FK list from the persisted CREATE TABLE SQL
                        let mut foreign_keys = Vec::new();
                        for (i, col_def) in create.columns.iter().enumerate() {
                            columns.push(ColumnInfo {
                                name: col_def.name.clone(),
                                data_type: col_def.data_type.clone(),
                                primary_key: col_def.primary_key,
                                autoincrement: col_def.autoincrement,
                                not_null: col_def.not_null,
                                unique: col_def.unique,
                                col_index: i,
                                stats: None,
                            });
                            if let Some(ref check) = col_def.check_expr {
                                check_constraints.push((None, check.clone()));
                            }
                            // Restore FK from column-level REFERENCES clause
                            if let Some(ref fkref) = col_def.references {
                                foreign_keys.push(ForeignKey {
                                    col_name: col_def.name.clone(),
                                    col_index: i,
                                    ref_table: fkref.table.to_ascii_lowercase(),
                                    ref_col: fkref.column.clone(),
                                    on_delete: fkref.on_delete.clone(),
                                    on_update: fkref.on_update.clone(),
                                });
                            }
                        }

                        for tc in &create.checks {
                            check_constraints.push(tc.clone());
                        }

                        // Get max rowid for autoincrement.
                        // NOTE: In multi-file mode, root_page belongs to the table's own pager,
                        // not the catalog pager. Silently default to 1; VM::open will fix it up.
                        let next_rowid = btree.max_rowid(root_page).unwrap_or(0) + 1;

                        let col_names: Vec<String> =
                            columns.iter().map(|c| c.name.clone()).collect();
                        self.tables.insert(
                            name.to_lowercase(),
                            TableSchema {
                                name,
                                columns,
                                col_names,
                                root_page,
                                next_rowid,
                                view_select: None,
                                foreign_keys,
                                check_constraints,
                                is_fts,
                                rls_enabled: false,
                                policies: Vec::new(),
                                clustered_index: true, // InnoDB-style: data in PK order
                            },
                        );
                    }
                    _ => {
                        // Can't parse - store minimal info
                        self.tables.insert(
                            name.to_lowercase(),
                            TableSchema {
                                name,
                                columns: Vec::new(),
                                col_names: Vec::new(),
                                root_page,
                                next_rowid: 1,
                                view_select: None,
                                foreign_keys: Vec::new(),
                                check_constraints: Vec::new(),
                                is_fts,
                                rls_enabled: false,
                                policies: Vec::new(),
                                clustered_index: true,
                            },
                        );
                    }
                }
            } else if obj_type == "index" || obj_type == "fulltext_index" {
                // Parse the CREATE INDEX SQL to get index info
                let is_fts_idx = obj_type == "fulltext_index";
                let tbl_name = match &row[2] {
                    Value::Text(s) => s.to_string(),
                    _ => continue,
                };
                // Determine columns: for fulltext_index parse from sql pattern
                let columns: Vec<String> = if is_fts_idx {
                    // Extract columns from persisted SQL: "CREATE FULLTEXT INDEX name ON tbl (col1,col2)"
                    let upper = sql.to_ascii_uppercase();
                    let rest = &sql[upper.find("ON").unwrap_or(0)..];
                    let op = rest.find('(');
                    let cp = rest.rfind(')');
                    match (op, cp) {
                        (Some(o), Some(c)) if c > o => rest[o + 1..c]
                            .split(',')
                            .map(|s| s.trim().trim_matches('`').to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                        _ => Vec::new(),
                    }
                } else {
                    match crate::sql::parser::parse_sql(&sql) {
                        Ok(crate::sql::ast::Statement::CreateIndex(ci)) => ci.columns,
                        _ => Vec::new(),
                    }
                };
                // C2 fix: restore the unique flag from the persisted CREATE [UNIQUE] INDEX SQL
                let unique = if is_fts_idx {
                    false
                } else {
                    match crate::sql::parser::parse_sql(&sql) {
                        Ok(crate::sql::ast::Statement::CreateIndex(ci)) => ci.unique,
                        _ => false,
                    }
                };
                let tbl_lower = tbl_name.to_lowercase();
                let idx_name_lower = name.to_lowercase();
                self.indexes_by_table
                    .entry(tbl_lower)
                    .or_default()
                    .push(idx_name_lower.clone());
                self.indexes.insert(
                    idx_name_lower,
                    IndexSchema {
                        name: name.clone(),
                        table_name: tbl_name,
                        columns,
                        root_page,
                        unique,
                        is_fts: is_fts_idx,
                    },
                );
            } else if obj_type == "trigger" {
                // L3: Restore trigger schema from serialized metadata in sql column.
                let tbl_name = match &row[2] {
                    Value::Text(s) => s.to_string(),
                    _ => continue,
                };
                let rowid_val = match &row[3] {
                    Value::Integer(v) => *v,
                    _ => 0,
                };
                let parts: Vec<&str> = sql.splitn(3, '|').collect();
                if parts.len() == 3 {
                    use crate::sql::ast::{TriggerEvent, TriggerTiming};
                    let timing = match parts[0] {
                        "BEFORE" => TriggerTiming::Before,
                        _ => TriggerTiming::After,
                    };
                    let event = match parts[1] {
                        "INSERT" => TriggerEvent::Insert,
                        "UPDATE" => TriggerEvent::Update,
                        _ => TriggerEvent::Delete,
                    };
                    let tbl_lower = tbl_name.to_lowercase();
                    self.triggers
                        .entry(tbl_lower)
                        .or_default()
                        .push(TriggerSchema {
                            name,
                            timing,
                            event,
                            table_name: tbl_name,
                            body_sql: parts[2].to_string(),
                            rowid: rowid_val,
                        });
                }
            } else if obj_type == "rls_enabled" {
                // M5 fix: re-apply persisted RLS flag to the table on reload
                let tbl_key = name.to_lowercase();
                if let Some(tbl) = self.tables.get_mut(&tbl_key) {
                    tbl.rls_enabled = true;
                }
            } else if obj_type == "vector_index" {
                // Phase 2/3: restore HNSW vector index registration from catalog.
                // The sql column holds the full CREATE VECTOR INDEX DDL, which we re-parse.
                // The HNSW graph is initialized empty here; VM::open backfills it from table rows.
                let tbl_name = match &row[2] {
                    Value::Text(s) => s.to_string(),
                    _ => continue,
                };
                // Re-parse the stored DDL to recover dim / distance / column.
                match crate::sql::parser::parse_sql(&sql) {
                    Ok(crate::sql::ast::Statement::CreateVectorIndex(vi_stmt)) => {
                        use crate::vector::{distance::DistanceMetric, VectorIndex};
                        // Look up the column index from the already-loaded table schema.
                        let col_idx = if let Some(tbl) = self.tables.get(&tbl_name.to_lowercase()) {
                            tbl.columns
                                .iter()
                                .find(|c| c.name.eq_ignore_ascii_case(&vi_stmt.column))
                                .map(|c| c.col_index)
                                .unwrap_or(0)
                        } else {
                            0
                        };
                        let metric = match vi_stmt.distance {
                            crate::sql::ast::VecDistanceType::Cosine => DistanceMetric::Cosine,
                            crate::sql::ast::VecDistanceType::L2 => DistanceMetric::L2,
                        };
                        let index_id = self.vector_indexes.alloc_index_id();
                        let vi = VectorIndex::new(
                            vi_stmt.index_name.clone(),
                            tbl_name.clone(),
                            vi_stmt.column.clone(),
                            col_idx,
                            vi_stmt.dim,
                            metric,
                            index_id,
                        );
                        self.vector_indexes.register(vi);
                    }
                    _ => {
                        // Couldn't re-parse DDL — skip (non-fatal, index will be absent until
                        // manually recreated; this can happen during format migrations)
                    }
                }
            }
        }

        Ok(())
    }

    /// L3: Persist a trigger and add to memory schema.
    pub fn save_trigger(&mut self, pager: &mut Pager, trigger: TriggerSchema) -> Result<()> {
        use crate::sql::ast::{TriggerEvent, TriggerTiming};
        let timing_str = match trigger.timing {
            TriggerTiming::Before => "BEFORE",
            TriggerTiming::After => "AFTER",
        };
        let event_str = match trigger.event {
            TriggerEvent::Insert => "INSERT",
            TriggerEvent::Update => "UPDATE",
            TriggerEvent::Delete => "DELETE",
        };
        // Encode: type | name | tbl_name | 0 | "timing|event|body_sql"
        let encoded = format!("{}|{}|{}", timing_str, event_str, trigger.body_sql);
        let schema_row: Row = vec![
            Value::Text("trigger".into()),
            Value::Text(trigger.name.clone().into()),
            Value::Text(trigger.table_name.clone().into()),
            Value::Integer(0),
            Value::Text(encoded.into()),
        ];
        let schema_root = pager.schema_root_page();
        let mut btree = BTree::new(pager);
        let next_rowid = btree.max_rowid(schema_root).unwrap_or(0) + 1;
        let new_root = btree.insert(schema_root, next_rowid, &schema_row)?;
        if new_root != schema_root {
            pager.set_schema_root_page(new_root)?;
        }
        let tbl_lower = trigger.table_name.to_lowercase();
        let mut trigger_mut = trigger;
        trigger_mut.rowid = next_rowid;
        self.triggers
            .entry(tbl_lower)
            .or_default()
            .push(trigger_mut);
        Ok(())
    }

    /// L3: Remove a trigger by name from catalog and memory.
    pub fn drop_trigger_by_name(
        &mut self,
        pager: &mut Pager,
        name: &str,
        if_exists: bool,
    ) -> Result<()> {
        let mut found_rowid = None;
        let mut found_tbl = None;
        let mut found_idx = None;
        'outer: for (tbl, vec) in &self.triggers {
            for (i, t) in vec.iter().enumerate() {
                if t.name.eq_ignore_ascii_case(name) {
                    found_rowid = Some(t.rowid);
                    found_tbl = Some(tbl.clone());
                    found_idx = Some(i);
                    break 'outer;
                }
            }
        }
        match (found_rowid, found_tbl, found_idx) {
            (Some(rowid), Some(tbl), Some(idx)) => {
                let schema_root = pager.schema_root_page();
                let mut btree = BTree::new(pager);
                let (_found, new_root) = btree.delete_by_rowid(schema_root, rowid)?;
                if new_root != schema_root {
                    pager.set_schema_root_page(new_root)?;
                }
                if let Some(vec) = self.triggers.get_mut(&tbl) {
                    vec.remove(idx);
                }
                Ok(())
            }
            _ => {
                if if_exists {
                    Ok(())
                } else {
                    Err(crate::error::KkdbError::RuntimeError(format!(
                        "trigger '{}' does not exist",
                        name
                    )))
                }
            }
        }
    }

    /// L3: Get all triggers for a table matching timing + event.
    pub fn get_triggers(
        &self,
        table_name: &str,
        timing: &crate::sql::ast::TriggerTiming,
        event: &crate::sql::ast::TriggerEvent,
    ) -> Vec<&TriggerSchema> {
        self.triggers
            .get(&table_name.to_lowercase())
            .map(|v| {
                v.iter()
                    .filter(|t| &t.timing == timing && &t.event == event)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Register a new table in the schema.
    ///
    /// `catalog_pager`: holds the schema B-Tree (pages 1-3).
    /// `table_pager`: the pager where this table's data B-Tree will live
    ///   (same as `catalog_pager` in single-file/memory mode, separate file in multi-file mode).
    #[allow(clippy::too_many_arguments)]
    pub fn create_table(
        &mut self,
        catalog_pager: &mut Pager,
        table_pager: &mut Pager,
        name: &str,
        column_defs: &[ColumnDef],
        if_not_exists: bool,
        original_sql: &str,
        extra_checks: &[(Option<String>, crate::sql::ast::Expr)],
        is_fts: bool,
    ) -> Result<()> {
        let name_lower = name.to_lowercase();
        if self.tables.contains_key(&name_lower) {
            if if_not_exists {
                return Ok(());
            }
            return Err(KkdbError::TableAlreadyExists(name.to_string()));
        }

        // Create a new B-tree for the table (in the table's own pager).
        let root_page = {
            let mut btree = BTree::new(table_pager);
            btree.create_table()?
        };

        // Build column info + FK list + CHECK constraints
        let mut columns = Vec::new();
        let mut foreign_keys = Vec::new();
        let mut check_constraints: Vec<(Option<String>, crate::sql::ast::Expr)> = Vec::new();
        for (i, col_def) in column_defs.iter().enumerate() {
            columns.push(ColumnInfo {
                name: col_def.name.clone(),
                data_type: col_def.data_type.clone(),
                primary_key: col_def.primary_key,
                autoincrement: col_def.autoincrement,
                not_null: col_def.not_null,
                unique: col_def.unique,
                col_index: i,
                stats: None,
            });
            // L1: collect FK references
            if let Some(ref fkref) = col_def.references {
                foreign_keys.push(ForeignKey {
                    col_name: col_def.name.clone(),
                    col_index: i,
                    ref_table: fkref.table.to_ascii_lowercase(),
                    ref_col: fkref.column.clone(),
                    on_delete: fkref.on_delete.clone(),
                    on_update: fkref.on_update.clone(),
                });
            }
            // L2: collect column-level CHECK constraints
            if let Some(ref check) = col_def.check_expr {
                check_constraints.push((None, check.clone()));
            }
        }
        // L2: add table-level CHECK constraints
        for tc in extra_checks {
            check_constraints.push(tc.clone());
        }

        // Insert into schema table (catalog pager).
        let type_val = if is_fts { "fts_table" } else { "table" };
        let schema_row: Row = vec![
            Value::Text(type_val.into()),
            Value::Text(name.to_string().into()),
            Value::Text(name.to_string().into()),
            Value::Integer(root_page as i64),
            Value::Text(original_sql.to_string().into()),
        ];

        let schema_root = catalog_pager.schema_root_page();
        let schema_rowid = {
            let mut btree = BTree::new(catalog_pager);
            let max_id = btree.max_rowid(schema_root).unwrap_or(0);
            max_id + 1
        };

        {
            let mut btree = BTree::new(catalog_pager);
            let new_root = btree.insert(schema_root, schema_rowid, &schema_row)?;
            if new_root != schema_root {
                btree.pager.set_schema_root_page(new_root)?;
            }
        };

        let col_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        self.tables.insert(
            name_lower,
            TableSchema {
                name: name.to_string(),
                columns,
                col_names,
                root_page,
                next_rowid: 1,
                view_select: None,
                foreign_keys,
                check_constraints,
                is_fts,
                rls_enabled: false,
                policies: Vec::new(),
                clustered_index: true, // InnoDB-style: data in PK order
            },
        );

        Ok(())
    }

    /// Drop a table
    pub fn drop_table(&mut self, pager: &mut Pager, name: &str, if_exists: bool) -> Result<()> {
        let name_lower = name.to_lowercase();
        if !self.tables.contains_key(&name_lower) {
            if if_exists {
                return Ok(());
            }
            return Err(KkdbError::TableNotFound(name.to_string()));
        }

        // Collect index names for this table
        let idx_names: Vec<String> = self
            .indexes
            .values()
            .filter(|idx| idx.table_name.eq_ignore_ascii_case(name))
            .map(|idx| idx.name.clone())
            .collect();

        // Remove table and associated indexes from schema table.
        {
            let mut schema_root = pager.schema_root_page();
            let mut btree = BTree::new(pager);
            let rows = btree.scan_all(schema_root)?;
            for (rowid, row) in rows {
                if row.len() >= 2 {
                    if let Value::Text(ref n) = row[1] {
                        if n.eq_ignore_ascii_case(name)
                            || idx_names.iter().any(|i| i.eq_ignore_ascii_case(n))
                        {
                            let (_, new_root) = btree.delete_by_rowid(schema_root, rowid)?;
                            if new_root != schema_root {
                                btree.pager.set_schema_root_page(new_root)?;
                                schema_root = new_root;
                            }
                        }
                    }
                }
            }
        }

        // Remove from in-memory cache
        for idx_name in &idx_names {
            self.indexes.remove(&idx_name.to_lowercase());
        }
        self.indexes_by_table.remove(&name_lower);
        self.tables.remove(&name_lower);
        Ok(())
    }

    /// Drop an index
    pub fn drop_index(&mut self, pager: &mut Pager, name: &str, if_exists: bool) -> Result<()> {
        let idx_lower = name.to_lowercase();
        let idx = match self.indexes.get(&idx_lower) {
            Some(i) => i.clone(),
            None => {
                if if_exists {
                    return Ok(());
                }
                return Err(KkdbError::Internal(format!("index '{}' not found", name)));
            }
        };

        // Remove from schema table
        let mut schema_root = pager.schema_root_page();
        let mut btree = BTree::new(pager);
        let rows = btree.scan_all(schema_root)?;
        for (rowid, row) in rows {
            if row.len() >= 2 {
                if let Value::Text(ref n) = row[1] {
                    if n.eq_ignore_ascii_case(name) {
                        let (_, new_root) = btree.delete_by_rowid(schema_root, rowid)?;
                        if new_root != schema_root {
                            btree.pager.set_schema_root_page(new_root)?;
                            schema_root = new_root; // keep var in sync before break
                            let _ = schema_root; // suppress unused_assignments
                        }
                        break;
                    }
                }
            }
        }

        // Remove from in-memory caches
        let tbl_lower = idx.table_name.to_lowercase();
        if let Some(list) = self.indexes_by_table.get_mut(&tbl_lower) {
            list.retain(|n| !n.eq_ignore_ascii_case(name));
        }
        self.indexes.remove(&idx_lower);
        Ok(())
    }

    /// Create a new index.
    /// `catalog_pager`: holds the schema B-Tree catalog.
    /// `table_pager`: the pager where this table's data and indexes live.
    #[allow(clippy::too_many_arguments)]
    pub fn create_index(
        &mut self,
        catalog_pager: &mut Pager,
        table_pager: &mut Pager,
        index_name: &str,
        table_name: &str,
        columns: &[String],
        unique: bool,
        if_not_exists: bool,
        original_sql: &str,
    ) -> Result<()> {
        let idx_lower = index_name.to_lowercase();
        if self.indexes.contains_key(&idx_lower) {
            if if_not_exists {
                return Ok(());
            }
            return Err(KkdbError::Internal(format!(
                "index '{}' already exists",
                index_name
            )));
        }

        // Verify table exists and columns are valid
        let table = self.get_table(table_name)?.clone();
        let mut col_indices = Vec::with_capacity(columns.len());
        for col_name in columns {
            let mut found = false;
            for col in &table.columns {
                if col.name.eq_ignore_ascii_case(col_name) {
                    col_indices.push(col.col_index);
                    found = true;
                    break;
                }
            }
            if !found {
                return Err(KkdbError::ColumnNotFound(format!(
                    "{}.{}",
                    table_name, col_name
                )));
            }
        }

        // Create a new B-tree for the index (in the table's own pager).
        let index_root = {
            let mut btree = BTree::new(table_pager);
            btree.create_table()?
        };

        // Populate the index with existing table data (scan from the table pager).
        let mut current_index_root = index_root;
        {
            let mut btree = BTree::new(table_pager);
            let table_rows = btree.scan_all(table.root_page)?;
            let mut idx_rowid = 1i64;
            let mut unique_seen: HashSet<Vec<u8>> = HashSet::new();
            for (tbl_rowid, row) in &table_rows {
                let mut index_row: Row = Vec::with_capacity(col_indices.len() + 1);
                for &ci in &col_indices {
                    index_row.push(if ci < row.len() {
                        row[ci].clone()
                    } else {
                        Value::Null
                    });
                }

                if unique && !index_row.iter().any(|v| matches!(v, Value::Null)) {
                    let key = Self::index_key(&index_row);
                    if !unique_seen.insert(key) {
                        return Err(KkdbError::ConstraintViolation(format!(
                            "UNIQUE constraint failed: {}.{}",
                            table_name,
                            columns.join(", ")
                        )));
                    }
                }

                index_row.push(Value::Integer(*tbl_rowid));
                current_index_root = btree.insert(current_index_root, idx_rowid, &index_row)?;
                idx_rowid += 1;
            }
        }

        // Insert index metadata into schema table (catalog pager).
        let schema_row: Row = vec![
            Value::Text("index".into()),
            Value::Text(index_name.to_string().into()),
            Value::Text(table_name.to_string().into()),
            Value::Integer(current_index_root as i64),
            Value::Text(original_sql.to_string().into()),
        ];
        {
            let schema_root = catalog_pager.schema_root_page();
            let mut btree = BTree::new(catalog_pager);
            let max_id = btree.max_rowid(schema_root).unwrap_or(0);
            let new_root = btree.insert(schema_root, max_id + 1, &schema_row)?;
            if new_root != schema_root {
                btree.pager.set_schema_root_page(new_root)?;
            }
        }

        self.indexes_by_table
            .entry(table_name.to_lowercase())
            .or_default()
            .push(idx_lower.clone());
        self.indexes.insert(
            idx_lower,
            IndexSchema {
                name: index_name.to_string(),
                table_name: table_name.to_string(),
                columns: columns.to_vec(),
                root_page: current_index_root,
                unique,
                is_fts: false,
            },
        );

        Ok(())
    }

    /// Check if any indexes exist for a given table 鈥?O(1)
    #[inline]
    pub fn has_indexes_for_table(&self, table_name: &str) -> bool {
        let key = Self::lowercase_key(table_name);
        self.indexes_by_table
            .get(key.as_ref())
            .is_some_and(|v| !v.is_empty())
    }

    /// Get all indexes for a given table 鈥?O(1) lookup
    pub fn indexes_for_table(&self, table_name: &str) -> Vec<&IndexSchema> {
        let key = Self::lowercase_key(table_name);
        match self.indexes_by_table.get(key.as_ref()) {
            Some(names) => names.iter().filter_map(|n| self.indexes.get(n)).collect(),
            None => Vec::new(),
        }
    }

    /// Register a FTS index schema in memory (without creating a B-Tree page).
    /// Used by exec_create_fulltext_index after persisting the catalog row.
    pub fn register_fts_index(
        &mut self,
        index_name: &str,
        table_name: &str,
        columns: Vec<String>,
        index_id: u32,
    ) {
        let idx_lower = index_name.to_lowercase();
        let tbl_lower = table_name.to_lowercase();
        self.indexes_by_table
            .entry(tbl_lower)
            .or_default()
            .push(idx_lower.clone());
        self.indexes.insert(
            idx_lower,
            IndexSchema {
                name: index_name.to_string(),
                table_name: table_name.to_string(),
                columns,
                root_page: index_id, // repurposed to hold FTS index_id
                unique: false,
                is_fts: true,
            },
        );
    }

    /// Get table schema (avoids heap alloc for names 鈮?28 bytes)
    pub fn get_table(&self, name: &str) -> Result<&TableSchema> {
        let key = Self::lowercase_key(name);
        self.tables
            .get(key.as_ref())
            .ok_or_else(|| KkdbError::TableNotFound(name.to_string()))
    }

    /// Get mutable table schema
    pub fn get_table_mut(&mut self, name: &str) -> Result<&mut TableSchema> {
        let key = Self::lowercase_key(name);
        self.tables
            .get_mut(key.as_ref())
            .ok_or_else(|| KkdbError::TableNotFound(name.to_string()))
    }

    /// Return all user table names
    pub fn list_tables(&self) -> Vec<String> {
        self.tables.keys().cloned().collect()
    }

    /// Add a view (Batch E): store it in memory only (no B-tree, root_page=0)
    pub fn add_view(&mut self, schema: TableSchema) {
        self.tables.insert(schema.name.to_lowercase(), schema);
    }

    /// Produce a lowercase key without heap allocation for short names.
    /// Returns a stack-backed str ref for names 鈮?28 bytes, else heap String.
    #[inline]
    fn lowercase_key(name: &str) -> LowercaseKey<'_> {
        // Fast path: already all-lowercase
        if !name.bytes().any(|b| b.is_ascii_uppercase()) {
            return LowercaseKey::Borrowed(name);
        }
        if name.len() <= 128 {
            let mut buf = [0u8; 128];
            for (i, b) in name.bytes().enumerate() {
                buf[i] = b.to_ascii_lowercase();
            }
            let s = std::str::from_utf8(&buf[..name.len()])
                .unwrap_or(name)
                .to_string();
            LowercaseKey::Owned(s)
        } else {
            LowercaseKey::Owned(name.to_lowercase())
        }
    }

    /// Find column index by name in a table
    pub fn find_column(&self, table_name: &str, col_name: &str) -> Result<usize> {
        let table = self.get_table(table_name)?;
        for col in &table.columns {
            if col.name.eq_ignore_ascii_case(col_name) {
                return Ok(col.col_index);
            }
        }
        Err(KkdbError::ColumnNotFound(format!(
            "{}.{}",
            table_name, col_name
        )))
    }

    // ---- ALTER TABLE operations ----

    /// Add a column to a table. Existing rows get NULL for the new column.
    pub fn alter_add_column(
        &mut self,
        pager: &mut Pager,
        table_name: &str,
        col: &ColumnDef,
    ) -> Result<()> {
        let table = self.get_table(table_name)?.clone();

        // Check for duplicate column name
        if table
            .columns
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(&col.name))
        {
            return Err(KkdbError::Internal(format!(
                "column '{}' already exists in table '{}'",
                col.name, table_name
            )));
        }

        // Cannot add a NOT NULL column without DEFAULT (existing rows would violate)
        if col.not_null && col.default.is_none() {
            return Err(KkdbError::ConstraintViolation(format!(
                "cannot add NOT NULL column '{}' without DEFAULT",
                col.name
            )));
        }

        // Append NULL (or default) value to every existing row
        let default_value = match &col.default {
            Some(expr) => Self::eval_default_expr(expr)?,
            None => Value::Null,
        };
        {
            let mut btree = BTree::new(pager);
            let rows = btree.scan_all(table.root_page)?;
            for (rowid, mut row) in rows {
                row.push(default_value.clone());
                btree.update_row(table.root_page, rowid, &row)?;
            }
        }

        // Update in-memory schema
        let new_col_index = table.columns.len();
        let tbl = self.get_table_mut(table_name)?;
        tbl.columns.push(ColumnInfo {
            name: col.name.clone(),
            data_type: col.data_type.clone(),
            primary_key: false,
            autoincrement: false,
            not_null: col.not_null,
            unique: col.unique,
            col_index: new_col_index,
            stats: None,
        });
        tbl.col_names.push(col.name.clone());

        // Update schema table SQL
        self.update_schema_sql(pager, table_name)?;
        Ok(())
    }

    /// Drop a column from a table.
    pub fn alter_drop_column(
        &mut self,
        pager: &mut Pager,
        table_name: &str,
        col_name: &str,
    ) -> Result<()> {
        let table = self.get_table(table_name)?.clone();

        let col_idx = table
            .columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(col_name))
            .map(|c| c.col_index)
            .ok_or_else(|| KkdbError::ColumnNotFound(format!("{}.{}", table_name, col_name)))?;

        // Cannot drop primary key column
        if table.columns[col_idx].primary_key {
            return Err(KkdbError::Internal(format!(
                "cannot drop PRIMARY KEY column '{}'",
                col_name
            )));
        }

        // Must have at least 1 column remaining
        if table.columns.len() <= 1 {
            return Err(KkdbError::Internal(
                "cannot drop the only column in a table".into(),
            ));
        }

        // Remove the column value from every row
        {
            let mut btree = BTree::new(pager);
            let rows = btree.scan_all(table.root_page)?;
            for (rowid, row) in rows {
                let mut new_row: Row = Vec::with_capacity(row.len() - 1);
                for (i, val) in row.into_iter().enumerate() {
                    if i != col_idx {
                        new_row.push(val);
                    }
                }
                btree.update_row(table.root_page, rowid, &new_row)?;
            }
        }

        // Drop any indexes that reference this column
        let idx_to_drop: Vec<String> = self
            .indexes
            .values()
            .filter(|idx| {
                idx.table_name.eq_ignore_ascii_case(table_name)
                    && idx.columns.iter().any(|c| c.eq_ignore_ascii_case(col_name))
            })
            .map(|idx| idx.name.clone())
            .collect();
        for idx_name in &idx_to_drop {
            // Remove from schema table
            let schema_root = pager.schema_root_page();
            let mut btree = BTree::new(pager);
            let schema_rows = btree.scan_all(schema_root)?;
            for (rowid, row) in schema_rows {
                if row.len() >= 2 {
                    if let Value::Text(ref n) = row[1] {
                        if n.eq_ignore_ascii_case(idx_name) {
                            let (_, new_root) = btree.delete_by_rowid(schema_root, rowid)?;
                            if new_root != schema_root {
                                btree.pager.set_schema_root_page(new_root)?;
                            }
                            break;
                        }
                    }
                }
            }
            self.indexes.remove(&idx_name.to_lowercase());
        }

        // Update in-memory column list
        let tbl = self.get_table_mut(table_name)?;
        tbl.columns
            .retain(|c| !c.name.eq_ignore_ascii_case(col_name));
        // Re-index columns
        for (i, c) in tbl.columns.iter_mut().enumerate() {
            c.col_index = i;
        }
        tbl.col_names = tbl.columns.iter().map(|c| c.name.clone()).collect();

        // Update indexes_by_table: remove dropped indexes
        for idx_name in &idx_to_drop {
            let tbl_lower = table_name.to_lowercase();
            if let Some(names) = self.indexes_by_table.get_mut(&tbl_lower) {
                names.retain(|n| !n.eq_ignore_ascii_case(idx_name));
            }
        }

        // Update schema table SQL
        self.update_schema_sql(pager, table_name)?;
        Ok(())
    }

    /// Rename a table.
    pub fn alter_rename_table(
        &mut self,
        pager: &mut Pager,
        old_name: &str,
        new_name: &str,
    ) -> Result<()> {
        let old_lower = old_name.to_lowercase();
        let new_lower = new_name.to_lowercase();

        if !self.tables.contains_key(&old_lower) {
            return Err(KkdbError::TableNotFound(old_name.to_string()));
        }
        if self.tables.contains_key(&new_lower) {
            return Err(KkdbError::TableAlreadyExists(new_name.to_string()));
        }

        // Update in-memory table
        let mut table = self.tables.remove(&old_lower).unwrap();
        table.name = new_name.to_string();
        self.tables.insert(new_lower.clone(), table);

        // Update index table_name references
        for idx in self.indexes.values_mut() {
            if idx.table_name.eq_ignore_ascii_case(old_name) {
                idx.table_name = new_name.to_string();
            }
        }

        // Move indexes_by_table entry from old to new table name
        if let Some(idx_names) = self.indexes_by_table.remove(&old_lower) {
            self.indexes_by_table.insert(new_lower, idx_names);
        }

        // Update schema table: change name and tbl_name in the row
        {
            let schema_root = pager.schema_root_page();
            let mut btree = BTree::new(pager);
            let rows = btree.scan_all(schema_root)?;
            for (rowid, row) in rows {
                if row.len() >= 5 {
                    if let (Value::Text(ref typ), Value::Text(ref n)) = (&row[0], &row[1]) {
                        if &**typ == "table" && n.eq_ignore_ascii_case(old_name) {
                            let mut new_row = row.clone();
                            new_row[1] = Value::Text(new_name.to_string().into());
                            new_row[2] = Value::Text(new_name.to_string().into());
                            // I28 fix: replace the table name in context-specific positions
                            // (after TABLE keyword) to avoid corrupting column names or
                            // literal values that happen to share the same string as old_name.
                            if let Value::Text(ref sql) = row[4] {
                                // Replace occurrences of old_name that follow a TABLE/INTO/UPDATE
                                // keyword (case-insensitive), bounded by whitespace, paren, or `;`.
                                // This is safer than bare replacen which can hit column names.
                                let mut new_sql = sql.as_ref().to_string();
                                for kw in &["TABLE ", "table ", "Table "] {
                                    let pat = format!("{}{}", kw, old_name);
                                    let repl = format!("{}{}", kw, new_name);
                                    new_sql = new_sql.replacen(&pat, &repl, 1);
                                }
                                new_row[4] = Value::Text(new_sql.into());
                            }
                            let new_root = btree.update_row(schema_root, rowid, &new_row)?;
                            if new_root != schema_root {
                                btree.pager.set_schema_root_page(new_root)?;
                            }
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Rename a column in a table.
    pub fn alter_rename_column(
        &mut self,
        pager: &mut Pager,
        table_name: &str,
        old_col: &str,
        new_col: &str,
    ) -> Result<()> {
        let table = self.get_table(table_name)?.clone();

        // Verify old column exists
        if !table
            .columns
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(old_col))
        {
            return Err(KkdbError::ColumnNotFound(format!(
                "{}.{}",
                table_name, old_col
            )));
        }

        // Verify new column name doesn't conflict
        if table
            .columns
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(new_col))
        {
            return Err(KkdbError::Internal(format!(
                "column '{}' already exists in table '{}'",
                new_col, table_name
            )));
        }

        // Update in-memory column name
        let tbl = self.get_table_mut(table_name)?;
        for col in &mut tbl.columns {
            if col.name.eq_ignore_ascii_case(old_col) {
                col.name = new_col.to_string();
                break;
            }
        }
        tbl.col_names = tbl.columns.iter().map(|c| c.name.clone()).collect();

        // Update index column references
        for idx in self.indexes.values_mut() {
            if idx.table_name.eq_ignore_ascii_case(table_name) {
                for c in &mut idx.columns {
                    if c.eq_ignore_ascii_case(old_col) {
                        *c = new_col.to_string();
                    }
                }
            }
        }

        // Update schema table SQL
        self.update_schema_sql(pager, table_name)?;
        Ok(())
    }

    /// Helper: rebuild and update the SQL stored in the schema table for a given table
    fn update_schema_sql(&self, pager: &mut Pager, table_name: &str) -> Result<()> {
        let table = self.get_table(table_name)?;
        let new_sql = self.rebuild_create_sql(table);

        let schema_root = pager.schema_root_page();
        let mut btree = BTree::new(pager);
        let rows = btree.scan_all(schema_root)?;
        for (rowid, row) in rows {
            if row.len() >= 5 {
                if let (Value::Text(ref typ), Value::Text(ref n)) = (&row[0], &row[1]) {
                    if &**typ == "table" && n.eq_ignore_ascii_case(table_name) {
                        let mut new_row = row.clone();
                        new_row[4] = Value::Text(new_sql.into());
                        let new_root = btree.update_row(schema_root, rowid, &new_row)?;
                        if new_root != schema_root {
                            btree.pager.set_schema_root_page(new_root)?;
                        }
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    /// Rebuild CREATE TABLE SQL from in-memory schema
    fn rebuild_create_sql(&self, table: &TableSchema) -> String {
        let mut sql = format!("CREATE TABLE {} (", table.name);
        for (i, col) in table.columns.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&col.name);
            sql.push(' ');
            sql.push_str(match col.data_type {
                DataType::Null => "BLOB",
                DataType::Integer => "INTEGER",
                DataType::Real => "REAL",
                DataType::Text => "TEXT",
                DataType::Blob => "BLOB",
                DataType::Timestamp => "TIMESTAMP",
            });
            if col.primary_key {
                sql.push_str(" PRIMARY KEY");
            }
            if col.autoincrement {
                sql.push_str(" AUTOINCREMENT");
            }
            if col.not_null && !col.primary_key {
                sql.push_str(" NOT NULL");
            }
            if col.unique && !col.primary_key {
                sql.push_str(" UNIQUE");
            }
        }
        sql.push(')');
        sql
    }

    #[inline]
    pub(crate) fn index_key(values: &[Value]) -> Vec<u8> {
        let mut key = Vec::new();
        for v in values {
            let encoded = v.serialize();
            key.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            key.extend_from_slice(&encoded);
        }
        key
    }

    fn eval_default_expr(expr: &Expr) -> Result<Value> {
        match expr {
            Expr::Null => Ok(Value::Null),
            Expr::IntegerLiteral(v) => Ok(Value::Integer(*v)),
            Expr::RealLiteral(v) => Ok(Value::Real(*v)),
            Expr::StringLiteral(v) => Ok(Value::Text(v.clone().into())),
            Expr::BlobLiteral(v) => Ok(Value::Blob(v.clone())),
            Expr::Nested(inner) => Self::eval_default_expr(inner),
            Expr::UnaryOp {
                op: UnaryOperator::Minus,
                expr: inner,
            } => match Self::eval_default_expr(inner)? {
                Value::Integer(v) => Ok(Value::Integer(v.wrapping_neg())),
                Value::Real(v) => Ok(Value::Real(-v)),
                _ => Err(KkdbError::ConstraintViolation(
                    "ALTER TABLE ADD COLUMN DEFAULT supports only numeric unary minus".into(),
                )),
            },
            _ => Err(KkdbError::ConstraintViolation(
                "ALTER TABLE ADD COLUMN DEFAULT supports only constant expressions".into(),
            )),
        }
    }
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
