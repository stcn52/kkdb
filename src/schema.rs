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
}

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
}

/// Cached index schema info
#[derive(Debug, Clone)]
pub struct IndexSchema {
    pub name: String,
    pub table_name: String,
    pub columns: Vec<String>,
    pub root_page: u32,
    pub unique: bool,
}

/// Schema manager - maintains the catalog of tables
#[derive(Clone)]
pub struct Schema {
    pub tables: HashMap<String, TableSchema>,
    pub indexes: HashMap<String, IndexSchema>,
    /// Index names grouped by table name (lowercase) for O(1) lookup
    indexes_by_table: HashMap<String, Vec<String>>,
}

impl Schema {
    pub fn new() -> Self {
        Schema {
            tables: HashMap::new(),
            indexes: HashMap::new(),
            indexes_by_table: HashMap::new(),
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

            if obj_type == "table" {
                // Parse the CREATE TABLE SQL to get column info
                match crate::sql::parser::parse_sql(&sql) {
                    Ok(crate::sql::ast::Statement::CreateTable(create)) => {
                        let mut columns = Vec::new();
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
                                foreign_keys: Vec::new(),
                                check_constraints: Vec::new(),
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
                            },
                        );
                    }
                }
            } else if obj_type == "index" {
                // Parse the CREATE INDEX SQL to get index info
                let tbl_name = match &row[2] {
                    Value::Text(s) => s.to_string(),
                    _ => continue,
                };
                match crate::sql::parser::parse_sql(&sql) {
                    Ok(crate::sql::ast::Statement::CreateIndex(ci)) => {
                        let tbl_lower = tbl_name.to_lowercase();
                        let idx_name_lower = name.to_lowercase();
                        self.indexes_by_table
                            .entry(tbl_lower)
                            .or_insert_with(Vec::new)
                            .push(idx_name_lower.clone());
                        self.indexes.insert(
                            idx_name_lower,
                            IndexSchema {
                                name: name.clone(),
                                table_name: tbl_name,
                                columns: ci.columns,
                                root_page,
                                unique: ci.unique,
                            },
                        );
                    }
                    _ => {
                        // Can't parse - skip
                    }
                }
            }
        }

        Ok(())
    }

    /// Register a new table in the schema.
    /// `catalog_pager`: holds the schema B-Tree (pages 1-3).
    /// `table_pager`: the pager where this table's data B-Tree will live
    ///   (same as `catalog_pager` in single-file/memory mode, separate file in multi-file mode).
    pub fn create_table(
        &mut self,
        catalog_pager: &mut Pager,
        table_pager: &mut Pager,
        name: &str,
        column_defs: &[ColumnDef],
        if_not_exists: bool,
        original_sql: &str,
        extra_checks: &[(Option<String>, crate::sql::ast::Expr)],
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
        let schema_row: Row = vec![
            Value::Text("table".into()),
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
            .or_insert_with(Vec::new)
            .push(idx_lower.clone());
        self.indexes.insert(
            idx_lower,
            IndexSchema {
                name: index_name.to_string(),
                table_name: table_name.to_string(),
                columns: columns.to_vec(),
                root_page: current_index_root,
                unique,
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
            .map_or(false, |v| !v.is_empty())
    }

    /// Get all indexes for a given table 鈥?O(1) lookup
    pub fn indexes_for_table(&self, table_name: &str) -> Vec<&IndexSchema> {
        let key = Self::lowercase_key(table_name);
        match self.indexes_by_table.get(key.as_ref()) {
            Some(names) => names.iter().filter_map(|n| self.indexes.get(n)).collect(),
            None => Vec::new(),
        }
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

    /// Remove a table or view from the in-memory schema
    pub fn remove_table(&mut self, name: &str) {
        self.tables.remove(&name.to_lowercase());
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
            let mut schema_root = pager.schema_root_page();
            let mut btree = BTree::new(pager);
            let schema_rows = btree.scan_all(schema_root)?;
            for (rowid, row) in schema_rows {
                if row.len() >= 2 {
                    if let Value::Text(ref n) = row[1] {
                        if n.eq_ignore_ascii_case(idx_name) {
                            let (_, new_root) = btree.delete_by_rowid(schema_root, rowid)?;
                            if new_root != schema_root {
                                btree.pager.set_schema_root_page(new_root)?;
                                schema_root = new_root;
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
                            // Rebuild the SQL with the new table name
                            if let Value::Text(ref sql) = row[4] {
                                // Simple replacement of old table name with new
                                let new_sql = sql.replacen(old_name, new_name, 1);
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
