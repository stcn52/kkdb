//! DDL statement execution for KKDB.
//!
//! This module implements all Data Definition Language (DDL) operations on the
//! [`VM`]: `CREATE TABLE`, `DROP TABLE`, `ALTER TABLE`, `CREATE INDEX`,
//! `DROP INDEX`, `CREATE VIEW`, `CREATE TRIGGER`, `CREATE FULLTEXT INDEX`,
//! `VACUUM`, `ANALYZE TABLE`, and `EXPLAIN`.
//!
//! ## Multi-file vs. single-file (in-memory) mode
//!
//! KKDB can store each table in its own `.kkdb` file (multi-file mode) or keep
//! all data in a single pager (single-file / in-memory mode).  Most DDL helpers
//! branch on `self.table_pagers.contains_key(...)` to handle both cases.
//!
//! ## Unsafe double-borrow pattern
//!
//! Several helpers need **two `&mut` borrows** of the same `Pager` simultaneously
//! (one for the schema catalog B-tree, one for the table data B-tree).  Because
//! Rust's borrow checker does not allow this, we use a raw-pointer alias:
//!
//! ```ignore
//! let p  = &mut self.pager as *mut _;
//! let p2 = unsafe { &mut *p }; // disjoint logical access, same physical object
//! ```
//!
//! This is safe because the two B-tree operations never touch the same page
//! simultaneously; the schema B-tree root and the data B-tree root are distinct.

use super::execute::{ExecResult, VM};
use crate::error::Result;
use crate::sql::ast::*;
use crate::storage::btree::BTree;
use crate::types::Value;

/// Helper struct for EXPLAIN (FORMAT TREE) rendering.
struct TreeNode {
    label: String,
    children: Vec<TreeNode>,
}

impl TreeNode {
    fn new(label: &str) -> Self {
        Self { label: label.to_string(), children: Vec::new() }
    }
}

impl VM {
    // ---- CREATE TABLE ----

    /// Execute a `CREATE TABLE` statement.
    ///
    /// Supports three variants:
    /// - **Regular `CREATE TABLE`**: allocates a schema catalog entry and a data
    ///   B-tree root page.  In multi-file mode a dedicated `.kkdb` file is
    ///   created for the table.
    /// - **`CREATE TABLE AS SELECT`**: delegates to [`Self::exec_create_table_as_select`]
    ///   which first executes the SELECT, infers column types, then creates the
    ///   table and bulk-inserts the rows inside an implicit transaction.
    /// - **`CREATE TABLE IF NOT EXISTS`**: silently succeeds when the table
    ///   already exists (handled inside [`crate::schema::Schema::create_table`]).
    ///
    /// After creation the index caches are invalidated and the pager is
    /// auto-flushed to persist the change.
    pub(crate) fn exec_create_table(
        &mut self,
        create: &CreateTableStmt,
        original_sql: &str,
    ) -> Result<ExecResult> {
        // CREATE TABLE AS SELECT
        if let Some(ref query) = create.source {
            return self.exec_create_table_as_select(create, query.as_ref().clone());
        }

        // In multi-file mode: open/create a separate pager file for this table.
        if let Some(ref db_dir) = self.db_dir.clone() {
            if VM::is_safe_table_name(&create.table_name) {
                VM::open_or_create_table_pager(
                    &mut self.table_pagers,
                    db_dir,
                    &create.table_name,
                    self.pager.in_transaction(),
                )?;
            }
        }

        // Get the two pagers: catalog (schema B-tree) and table (data B-tree).
        let table_name = create.table_name.clone();
        let tbl_key = table_name.to_ascii_lowercase();
        if self.table_pagers.contains_key(&tbl_key) {
            // Multi-file: temporarily split borrow by extracting table_pager.
            // SAFETY: contains_key check above guarantees the key exists
            let table_pager = self.table_pagers.get_mut(&tbl_key).unwrap() as *mut _;
            // SAFETY: table_pager and self.pager are disjoint fields.
            let table_pager: &mut crate::storage::pager::Pager = unsafe { &mut *table_pager };
            self.schema.create_table(
                &mut self.pager,
                table_pager,
                &create.table_name,
                &create.columns,
                create.if_not_exists,
                original_sql,
                &create.checks,
                create.is_fts,
            )?;
        } else {
            // Single-file / memory mode: catalog pager is also table pager.
            // We need two &mut borrows of self.pager 鈥?split via raw ptr.
            let p = &mut self.pager as *mut _;
            let p2: &mut crate::storage::pager::Pager = unsafe { &mut *p };
            self.schema.create_table(
                &mut self.pager,
                p2,
                &create.table_name,
                &create.columns,
                create.if_not_exists,
                original_sql,
                &create.checks,
                create.is_fts,
            )?;
        }

        self.clear_index_caches();
        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!("Table '{}' created", create.table_name),
        })
    }

    fn exec_create_table_as_select(
        &mut self,
        create: &CreateTableStmt,
        query: crate::sql::ast::SelectStmt,
    ) -> Result<ExecResult> {
        use crate::sql::ast::ColumnDef;
        use crate::types::{DataType, Value};

        // Phase 1: Execute SELECT and materialise all rows + column names.
        let (col_names, rows) = match self.exec_select(&query)? {
            crate::vm::execute::ExecResult::QueryResult { columns, rows } => (columns, rows),
            _ => {
                return Err(crate::error::KkdbError::Internal(
                    "CTAS: exec_select did not return rows".into(),
                ))
            }
        };

        // Infer column types from the first non-NULL value in each column.
        // Falls back to Text if column is all-NULL or result set is empty.
        let mut col_types: Vec<DataType> = vec![DataType::Text; col_names.len()];
        'outer: for row in &rows {
            let mut all_inferred = true;
            for (i, val) in row.iter().enumerate() {
                if matches!(col_types[i], DataType::Text) {
                    if !matches!(val, Value::Null) {
                        col_types[i] = match val {
                            Value::Integer(_) => DataType::Integer,
                            Value::Real(_) => DataType::Real,
                            Value::Blob(_) => DataType::Blob,
                            _ => DataType::Text,
                        };
                    } else {
                        all_inferred = false;
                    }
                }
            }
            if all_inferred {
                break 'outer;
            }
        }

        // Build column definitions with auto-names for expressions without alias.
        let columns: Vec<ColumnDef> = col_names
            .iter()
            .zip(col_types)
            .enumerate()
            .map(|(i, (name, dt))| {
                let col_name = if name.is_empty() || name == "?" {
                    format!("col_{}", i + 1)
                } else {
                    name.to_ascii_lowercase()
                };
                ColumnDef {
                    name: col_name,
                    data_type: dt,
                    primary_key: false,
                    autoincrement: false,
                    not_null: false,
                    unique: false,
                    default: None,
                    references: None,
                    check_expr: None,
                }
            })
            .collect();

        // Generate DDL SQL string to store in schema.
        let cols_sql: String = columns
            .iter()
            .map(|c| format!("{} {}", c.name, c.data_type))
            .collect::<Vec<_>>()
            .join(", ");
        let ddl_sql = format!("CREATE TABLE {} ({})", create.table_name, cols_sql);

        if create.if_not_exists
            && self
                .schema
                .tables
                .contains_key(&create.table_name.to_lowercase())
        {
            return Ok(ExecResult::Ok {
                message: format!("Table '{}' already exists", create.table_name),
            });
        }

        // Phase 2: Create table + bulk-insert with implicit transaction (atomicity).
        let need_auto_txn = !self.pager.in_transaction();
        if need_auto_txn {
            self.pager.begin_transaction()?;
            self.schema_snapshot = Some(self.schema.clone());
        }

        let result = (|| -> Result<ExecResult> {
            // In multi-file mode: open/create a separate pager file for this table.
            if let Some(ref db_dir) = self.db_dir.clone() {
                if VM::is_safe_table_name(&create.table_name) {
                    VM::open_or_create_table_pager(
                        &mut self.table_pagers,
                        db_dir,
                        &create.table_name,
                        self.pager.in_transaction(),
                    )?;
                }
            }
            let tbl_key = create.table_name.to_ascii_lowercase();
            if self.table_pagers.contains_key(&tbl_key) {
                // SAFETY: contains_key check above guarantees the key exists
                let table_pager = self.table_pagers.get_mut(&tbl_key).unwrap() as *mut _;
                let table_pager: &mut crate::storage::pager::Pager = unsafe { &mut *table_pager };
                self.schema.create_table(
                    &mut self.pager,
                    table_pager,
                    &create.table_name,
                    &columns,
                    create.if_not_exists,
                    &ddl_sql,
                    &[],
                    create.is_fts,
                )?;
            } else {
                let p = &mut self.pager as *mut _;
                let p2: &mut crate::storage::pager::Pager = unsafe { &mut *p };
                self.schema.create_table(
                    &mut self.pager,
                    p2,
                    &create.table_name,
                    &columns,
                    create.if_not_exists,
                    &ddl_sql,
                    &[],
                    create.is_fts,
                )?;
            }
            self.clear_index_caches();

            // Insert every row from the SELECT result.
            let insert_stmt = crate::sql::ast::InsertStmt {
                table_name: create.table_name.clone(),
                columns: None,
                source: crate::sql::ast::InsertSource::Values(
                    rows.iter()
                        .map(|r| {
                            r.iter()
                                .map(|v| crate::sql::ast::Expr::from_value(v.clone()))
                                .collect()
                        })
                        .collect(),
                ),
                conflict: crate::sql::ast::ConflictPolicy::Error,
                returning: None,
            };
            self.exec_insert(&insert_stmt)?;

            Ok(ExecResult::Ok {
                message: format!(
                    "Table '{}' created with {} row(s)",
                    create.table_name,
                    rows.len()
                ),
            })
        })();

        if need_auto_txn {
            match result {
                Ok(r) => {
                    // B13-1 fix: if commit fails, rollback and return error (same as B12-6)
                    match self.pager.commit_transaction() {
                        Ok(()) => {
                            self.schema_snapshot = None;
                            Ok(r)
                        }
                        Err(e) => {
                            let _ = self.pager.rollback_transaction();
                            for tbl_pager in self.table_pagers.values_mut() {
                                if tbl_pager.in_transaction() {
                                    let _ = tbl_pager.rollback_transaction();
                                }
                            }
                            if let Some(snap) = self.schema_snapshot.take() {
                                self.schema = snap;
                            }
                            self.clear_index_caches();
                            Err(e)
                        }
                    }
                }
                Err(e) => {
                    let _ = self.pager.rollback_transaction();
                    if let Some(snap) = self.schema_snapshot.take() {
                        self.schema = snap;
                    }
                    self.clear_index_caches();
                    Err(e)
                }
            }
        } else {
            result
        }
    }

    // ---- DROP TABLE ----

    /// Execute a `DROP TABLE [IF EXISTS]` statement.
    ///
    /// In addition to removing the table's schema entry and data B-tree, this
    /// method performs a **cascade drop** for the associated FTS internal table
    /// (`<name>_fts_idx`) when the table was created with `CREATE FULLTEXT INDEX`.
    ///
    /// If `drop.if_exists` is `true` and the table does not exist, the method
    /// succeeds silently.
    pub(crate) fn exec_drop_table(&mut self, drop: &DropTableStmt) -> Result<ExecResult> {
        let is_fts = self
            .schema
            .tables
            .get(&drop.table_name.to_lowercase())
            .map(|t| t.is_fts)
            .unwrap_or(false);
        self.schema
            .drop_table(&mut self.pager, &drop.table_name, drop.if_exists)?;

        // L4: Cascade drop for FTS internal table
        if is_fts {
            let fts_tbl = format!("{}_fts_idx", drop.table_name);
            let _ = self.exec_drop_table(&DropTableStmt {
                table_name: fts_tbl,
                if_exists: true,
            });
        }

        self.clear_index_caches();
        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!("Table '{}' dropped", drop.table_name),
        })
    }

    // ---- ALTER TABLE ----

    /// Execute an `ALTER TABLE` statement.
    ///
    /// Supported actions:
    /// - `ADD COLUMN col_def` — appends a new column to the schema; existing
    ///   rows implicitly get `NULL` for the new column.
    /// - `DROP COLUMN col_name` — removes the column metadata; physical row
    ///   data is not rewritten (lazy schema evolution).
    /// - `RENAME TO new_name` — renames the table in both the in-memory schema
    ///   and the catalog B-tree.
    /// - `RENAME COLUMN old TO new` — renames a single column.
    /// - `ENABLE ROW LEVEL SECURITY` — sets `rls_enabled = true` on the table
    ///   and persists an `rls_enabled` marker row to the schema catalog so the
    ///   flag survives a process restart.
    pub(crate) fn exec_alter_table(&mut self, alter: &AlterTableStmt) -> Result<ExecResult> {
        let msg = match &alter.action {
            AlterTableAction::AddColumn(col_def) => {
                self.schema
                    .alter_add_column(&mut self.pager, &alter.table_name, col_def)?;
                format!(
                    "Column '{}' added to table '{}'",
                    col_def.name, alter.table_name
                )
            }
            AlterTableAction::DropColumn(col_name) => {
                self.schema
                    .alter_drop_column(&mut self.pager, &alter.table_name, col_name)?;
                format!(
                    "Column '{}' dropped from table '{}'",
                    col_name, alter.table_name
                )
            }
            AlterTableAction::RenameTable(new_name) => {
                self.schema
                    .alter_rename_table(&mut self.pager, &alter.table_name, new_name)?;
                format!("Table '{}' renamed to '{}'", alter.table_name, new_name)
            }
            AlterTableAction::RenameColumn { old_name, new_name } => {
                self.schema.alter_rename_column(
                    &mut self.pager,
                    &alter.table_name,
                    old_name,
                    new_name,
                )?;
                format!(
                    "Column '{}' renamed to '{}' in table '{}'",
                    old_name, new_name, alter.table_name
                )
            }
            AlterTableAction::EnableRowLevelSecurity => {
                let tbl_key = alter.table_name.to_ascii_lowercase();
                if let Some(tbl) = self.schema.tables.get_mut(&tbl_key) {
                    tbl.rls_enabled = true;
                } else {
                    return Err(crate::error::KkdbError::RuntimeError(format!(
                        "table '{}' not found",
                        alter.table_name
                    )));
                }
                // M5 fix: persist the RLS flag to the catalog so it survives a restart
                let schema_root = self.pager.schema_root_page();
                let next_rowid = {
                    let mut bt = BTree::new(&mut self.pager);
                    bt.max_rowid(schema_root).unwrap_or(0) + 1
                };
                let rls_row = vec![
                    Value::Text("rls_enabled".into()),   // type
                    Value::Text(tbl_key.clone().into()), // name
                    Value::Text(tbl_key.clone().into()), // tbl_name
                    Value::Integer(0),                   // root_page (unused)
                    Value::Text("ALTER TABLE ... ENABLE ROW LEVEL SECURITY".into()), // sql
                ];
                let mut btree = BTree::new(&mut self.pager);
                let new_schema_root = btree.insert(schema_root, next_rowid, &rls_row)?;
                if new_schema_root != schema_root {
                    self.pager.set_schema_root_page(new_schema_root)?;
                }
                format!("RLS enabled on table '{}'", alter.table_name)
            }
        };

        self.clear_index_caches();
        self.auto_flush()?;
        Ok(ExecResult::Ok { message: msg })
    }

    // ---- CREATE INDEX ----

    /// Execute a `CREATE [UNIQUE] INDEX` statement.
    ///
    /// Allocates a new B-tree root page for the index, scans the parent table
    /// to build the initial index entries, and persists both the index schema
    /// row and the root page to disk.
    ///
    /// In multi-file mode the index B-tree lives in the same pager file as its
    /// parent table.  In single-file / in-memory mode the catalog pager doubles
    /// as the index pager (safe double-borrow via raw pointer, see module docs).
    pub(crate) fn exec_create_index(&mut self, create_idx: &CreateIndexStmt) -> Result<ExecResult> {
        // Reconstruct the original SQL for schema storage
        let unique_str = if create_idx.unique { "UNIQUE " } else { "" };
        let cols_str = create_idx.columns.join(", ");
        let sql = format!(
            "CREATE {}INDEX {} ON {} ({})",
            unique_str, create_idx.index_name, create_idx.table_name, cols_str
        );

        let tbl_key = create_idx.table_name.to_ascii_lowercase();
        if self.table_pagers.contains_key(&tbl_key) {
            // SAFETY: contains_key check above guarantees the key exists
            let table_pager = self.table_pagers.get_mut(&tbl_key).unwrap() as *mut _;
            let table_pager: &mut crate::storage::pager::Pager = unsafe { &mut *table_pager };
            self.schema.create_index(
                &mut self.pager,
                table_pager,
                &create_idx.index_name,
                &create_idx.table_name,
                &create_idx.columns,
                create_idx.unique,
                create_idx.if_not_exists,
                &sql,
            )?;
        } else {
            let p = &mut self.pager as *mut _;
            let p2: &mut crate::storage::pager::Pager = unsafe { &mut *p };
            self.schema.create_index(
                &mut self.pager,
                p2,
                &create_idx.index_name,
                &create_idx.table_name,
                &create_idx.columns,
                create_idx.unique,
                create_idx.if_not_exists,
                &sql,
            )?;
        }
        self.clear_index_caches();
        self.auto_flush()?;

        Ok(ExecResult::Ok {
            message: format!("Index created: {}", create_idx.index_name),
        })
    }

    // ---- DROP INDEX ----

    /// Execute a `DROP INDEX [IF EXISTS]` statement.
    ///
    /// Removes the index from the schema catalog and frees the associated B-tree
    /// root page.  The index cache is invalidated so subsequent queries do not
    /// attempt to use the stale entry.
    pub(crate) fn exec_drop_index(&mut self, drop: &DropIndexStmt) -> Result<ExecResult> {
        self.schema
            .drop_index(&mut self.pager, &drop.index_name, drop.if_exists)?;
        self.clear_index_caches();
        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!("Index '{}' dropped", drop.index_name),
        })
    }

    // ---- CREATE FULLTEXT INDEX (BM25) ----
    /// Phase 4: Register a full-text index in the schema and backfill existing rows.
    pub(crate) fn exec_create_fulltext_index(
        &mut self,
        stmt: &CreateFulltextIndexStmt,
    ) -> Result<ExecResult> {
        use crate::storage::btree::BTree;
        use std::collections::HashMap;

        // 1. Validate table exists
        let tbl = self.schema.get_table(&stmt.table_name)?.clone();

        // 2. Validate columns exist in table
        let col_indices: Vec<usize> = stmt
            .columns
            .iter()
            .map(|col_name| {
                tbl.columns
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(col_name))
                    .map(|c| c.col_index)
                    .ok_or_else(|| {
                        crate::error::KkdbError::ColumnNotFound(format!(
                            "{}.{}",
                            stmt.table_name, col_name
                        ))
                    })
            })
            .collect::<Result<Vec<_>>>()?;

        // 3. Check IF NOT EXISTS
        let idx_lower = stmt.index_name.to_lowercase();
        if self.schema.indexes.contains_key(&idx_lower) {
            if stmt.if_not_exists {
                return Ok(ExecResult::Ok {
                    message: format!("FULLTEXT INDEX '{}' already exists", stmt.index_name),
                });
            }
            return Err(crate::error::KkdbError::Internal(format!(
                "FULLTEXT INDEX '{}' already exists",
                stmt.index_name
            )));
        }

        // 4. Allocate a real BTree root page for FTS postings storage.
        // This avoids execute_sql (schema.create_table unsafe double-borrow) entirely.
        let fts_root = {
            let mut btree = BTree::new(&mut self.pager);
            btree.create_table()?
        };

        // 5. Persist FTS index metadata to schema catalog.
        // Store fts_root (a small valid page number) as the root_page field.
        let original_sql = format!(
            "CREATE FULLTEXT INDEX {} ON {} ({})",
            stmt.index_name,
            stmt.table_name,
            stmt.columns.join(", ")
        );
        let schema_row: crate::types::Row = vec![
            crate::types::Value::Text("fulltext_index".into()),
            crate::types::Value::Text(stmt.index_name.clone().into()),
            crate::types::Value::Text(stmt.table_name.clone().into()),
            crate::types::Value::Integer(fts_root as i64), // valid page number, no UB
            crate::types::Value::Text(original_sql.into()),
        ];
        let schema_root = self.pager.schema_root_page();
        {
            let mut btree = BTree::new(&mut self.pager);
            let max_id = btree.max_rowid(schema_root).unwrap_or(0);
            let new_root = btree.insert(schema_root, max_id + 1, &schema_row)?;
            if new_root != schema_root {
                self.pager.set_schema_root_page(new_root)?;
            }
        }

        // 6. Register in in-memory schema (root_page = fts_root, the real BTree page)
        self.schema.register_fts_index(
            &stmt.index_name,
            &stmt.table_name,
            stmt.columns.clone(),
            fts_root, // root_page IS the FTS BTree root page
        );

        // 7. Backfill: scan existing rows and build inverted index directly via BTree
        let rows = {
            let pager = self.get_table_pager_mut(&stmt.table_name);
            let mut btree = BTree::new(pager);
            btree.scan_all(tbl.root_page)?
        };

        let mut total_docs: u64 = 0;
        let mut total_field_len: u64 = 0;
        let mut postings: HashMap<String, HashMap<u64, (u32, u32)>> = HashMap::new();
        let mut doc_freq: HashMap<String, u64> = HashMap::new();

        for (rowid, row) in &rows {
            total_docs += 1;
            let mut field_tokens: Vec<String> = Vec::new();
            for &ci in &col_indices {
                if let Some(crate::types::Value::Text(s)) = row.get(ci) {
                    let tok = crate::fulltext::tokenizer::simple_tokenize(s);
                    field_tokens.extend(tok);
                }
            }
            let field_len = field_tokens.len() as u32;
            total_field_len += field_len as u64;
            let mut tf_map: HashMap<String, u32> = HashMap::new();
            for token in field_tokens {
                *tf_map.entry(token).or_insert(0) += 1;
            }
            let rid = *rowid as u64;
            for (token, tf) in tf_map {
                *doc_freq.entry(token.clone()).or_insert(0) += 1;
                postings
                    .entry(token)
                    .or_default()
                    .insert(rid, (tf, field_len));
            }
        }

        // Write postings directly to the FTS BTree root page (no execute_sql)
        self.write_fts_postings_raw(fts_root, &postings, &doc_freq, total_docs, total_field_len)?;

        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!(
                "FULLTEXT INDEX '{}' created on {}.({}) with {} tokens across {} rows",
                stmt.index_name,
                stmt.table_name,
                stmt.columns.join(", "),
                doc_freq.len(),
                total_docs
            ),
        })
    }

    // ---- CREATE VECTOR INDEX (HNSW) ----

    /// Execute `CREATE VECTOR INDEX name ON table(col) DIM N [DISTANCE COSINE|L2]`.
    ///
    /// Phase 2 implementation:
    /// 1. Validates the target table and column.
    /// 2. Allocates a B-Tree root page (for future persistence in Phase 3+).
    /// 3. Persists a `vector_index` row to the schema catalog.
    /// 4. Registers a live `VectorIndex` (HNSW graph) in `schema.vector_indexes`.
    /// 5. Backfills all existing rows that have non-NULL BLOB values in the column.
    pub(crate) fn exec_create_vector_index(
        &mut self,
        stmt: &CreateVectorIndexStmt,
    ) -> Result<ExecResult> {
        use crate::vector::index::decode_vector;
        use crate::vector::{distance::DistanceMetric, VectorIndex};

        // 1. Validate table exists
        let tbl = self.schema.get_table(&stmt.table_name)?.clone();

        // 2. Validate column exists and find its index
        let col_lower = stmt.column.to_lowercase();
        let col_meta = tbl
            .columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&col_lower))
            .ok_or_else(|| {
                crate::error::KkdbError::ColumnNotFound(format!(
                    "{}.{}",
                    stmt.table_name, stmt.column
                ))
            })?;
        let col_idx = col_meta.col_index;

        // 3. Check IF NOT EXISTS
        let idx_lower = stmt.index_name.to_lowercase();
        if self.schema.vector_indexes.get(&idx_lower).is_some() {
            if stmt.if_not_exists {
                return Ok(ExecResult::Ok {
                    message: format!("VECTOR INDEX '{}' already exists", stmt.index_name),
                });
            }
            return Err(crate::error::KkdbError::RuntimeError(format!(
                "VECTOR INDEX '{}' already exists",
                stmt.index_name
            )));
        }

        // 4. Allocate a B-Tree root page for future persistence
        let vec_root = {
            let mut btree = BTree::new(&mut self.pager);
            btree.create_table()?
        };

        // 5. Persist vector_index metadata to schema catalog
        let distance_str = match stmt.distance {
            crate::sql::ast::VecDistanceType::Cosine => "COSINE",
            crate::sql::ast::VecDistanceType::L2 => "L2",
        };
        let original_sql = format!(
            "CREATE VECTOR INDEX {} ON {} ({}) DIM {} DISTANCE {}",
            stmt.index_name, stmt.table_name, stmt.column, stmt.dim, distance_str
        );
        let schema_root = self.pager.schema_root_page();
        {
            let schema_row: crate::types::Row = vec![
                Value::Text("vector_index".into()),
                Value::Text(stmt.index_name.clone().into()),
                Value::Text(stmt.table_name.clone().into()),
                Value::Integer(vec_root as i64),
                Value::Text(original_sql.into()),
            ];
            let mut btree = BTree::new(&mut self.pager);
            let max_id = btree.max_rowid(schema_root).unwrap_or(0);
            let new_root = btree.insert(schema_root, max_id + 1, &schema_row)?;
            if new_root != schema_root {
                self.pager.set_schema_root_page(new_root)?;
            }
        }

        // 6. Register in-memory VectorIndex (HNSW graph)
        let metric = match stmt.distance {
            crate::sql::ast::VecDistanceType::Cosine => DistanceMetric::Cosine,
            crate::sql::ast::VecDistanceType::L2 => DistanceMetric::L2,
        };
        let index_id = self.schema.vector_indexes.alloc_index_id();
        let vi = VectorIndex::new(
            stmt.index_name.clone(),
            stmt.table_name.clone(),
            stmt.column.clone(),
            col_idx,
            stmt.dim,
            metric,
            index_id,
        );
        self.schema.vector_indexes.register(vi);

        // 7. Backfill: scan existing rows and insert their vectors into HNSW
        let rows = {
            let pager = self.get_table_pager_mut(&stmt.table_name);
            let mut btree = BTree::new(pager);
            btree.scan_all(tbl.root_page)?
        };

        let mut backfill_count = 0u64;
        let mut error_count = 0u64;
        let dim = stmt.dim;

        // SAFETY: the VectorIndex was just registered above; get() always returns Some
        let vi_ref = self
            .schema
            .vector_indexes
            .get(&stmt.index_name)
            .unwrap()
            .clone();
        for (rowid, row) in &rows {
            if let Some(Value::Blob(blob)) = row.get(col_idx) {
                if let Some(vec) = decode_vector(blob) {
                    if vec.len() as u32 == dim {
                        if vi_ref.insert_vec(*rowid as u64, vec).is_ok() {
                            backfill_count += 1;
                        } else {
                            error_count += 1;
                        }
                    } else {
                        error_count += 1;
                    }
                }
            }
        }

        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!(
                "VECTOR INDEX '{}' created on {}.{} (dim={}, distance={}, backfilled={}, errors={})",
                stmt.index_name, stmt.table_name, stmt.column,
                dim, distance_str, backfill_count, error_count
            ),
        })
    }

    /// Execute `DROP VECTOR INDEX [IF EXISTS] name`.
    ///
    /// Removes the index from the in-memory registry and from the schema catalog.
    pub(crate) fn exec_drop_vector_index(
        &mut self,
        index_name: &str,
        if_exists: bool,
    ) -> Result<ExecResult> {
        let lower = index_name.to_lowercase();
        if self.schema.vector_indexes.get(&lower).is_none() {
            if if_exists {
                return Ok(ExecResult::Ok {
                    message: format!("VECTOR INDEX '{}' does not exist", index_name),
                });
            }
            return Err(crate::error::KkdbError::RuntimeError(format!(
                "VECTOR INDEX '{}' not found",
                index_name
            )));
        }

        // Remove from in-memory registry
        self.schema.vector_indexes.drop(&lower);

        // Remove from schema catalog (scan for the vector_index row)
        let schema_root = self.pager.schema_root_page();
        let schema_rows = {
            let mut btree = BTree::new(&mut self.pager);
            btree.scan_all(schema_root)?
        };
        for (rowid, row) in schema_rows {
            if row.len() >= 2 {
                if let (Value::Text(ref typ), Value::Text(ref name)) = (&row[0], &row[1]) {
                    if typ.as_ref() == "vector_index" && name.eq_ignore_ascii_case(&lower) {
                        let mut btree = BTree::new(&mut self.pager);
                        let (_, new_root) = btree.delete_by_rowid(schema_root, rowid)?;
                        if new_root != schema_root {
                            self.pager.set_schema_root_page(new_root)?;
                        }
                        break;
                    }
                }
            }
        }

        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!("VECTOR INDEX '{}' dropped", index_name),
        })
    }

    /// Update the root page number in the schema table for a table or index object.
    pub(crate) fn update_schema_object_root_page(
        &mut self,
        object_name: &str,
        new_root: u32,
    ) -> Result<()> {
        let schema_root = self.pager.schema_root_page();
        let mut btree = BTree::new(&mut self.pager);
        let schema_rows = btree.scan_all(schema_root)?;

        for (rowid, row) in schema_rows {
            if row.len() >= 5 {
                if let Value::Text(ref name) = row[1] {
                    if name.eq_ignore_ascii_case(object_name) {
                        let mut new_row = row.clone();
                        new_row[3] = Value::Integer(new_root as i64);
                        let mut btree = BTree::new(&mut self.pager);
                        let new_schema_root = btree.update_row(schema_root, rowid, &new_row)?;
                        if new_schema_root != schema_root {
                            btree.pager.set_schema_root_page(new_schema_root)?;
                        }
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Update the root page number for a table.
    pub(crate) fn update_schema_root_page(
        &mut self,
        table_name: &str,
        new_root: u32,
    ) -> Result<()> {
        self.update_schema_object_root_page(table_name, new_root)
    }

    // ---- EXPLAIN ----

    /// Execute an `EXPLAIN` statement and return a human-readable query plan.
    ///
    /// The current implementation produces a simple textual plan that lists the
    /// top-level operations (SCAN, FILTER, SORT, LIMIT).  It does **not** yet
    /// perform cost-based optimisation or index selection analysis.
    pub(crate) fn exec_explain(&mut self, stmt: &Statement) -> Result<ExecResult> {
        let plan = match stmt {
            Statement::Select(select) => {
                let mut plan = String::new();
                plan.push_str("QUERY PLAN\n");
                if let Some(ref from) = select.from {
                    self.explain_from_plan(from, &mut plan, 1);
                }
                if select.where_clause.is_some() {
                    plan.push_str("  FILTER (WHERE clause)\n");
                    if let Some(ref from) = select.from {
                        self.explain_index_decision(
                            from,
                            select.where_clause.as_ref().unwrap(),
                            &mut plan,
                        );
                    }
                }
                if !select.order_by.is_empty() {
                    plan.push_str("  SORT (ORDER BY)\n");
                }
                if select.limit.is_some() {
                    plan.push_str("  LIMIT\n");
                }
                plan
            }
            Statement::Insert(insert) => {
                format!("INSERT INTO {}\n", insert.table_name)
            }
            Statement::Update(update) => {
                let mut plan = format!("UPDATE {}\n", update.table_name);
                plan.push_str("  SCAN table\n");
                if update.where_clause.is_some() {
                    plan.push_str("  FILTER (WHERE clause)\n");
                }
                plan
            }
            Statement::Delete(delete) => {
                let mut plan = format!("DELETE FROM {}\n", delete.table_name);
                plan.push_str("  SCAN table\n");
                if delete.where_clause.is_some() {
                    plan.push_str("  FILTER (WHERE clause)\n");
                }
                plan
            }
            _ => "No plan available for this statement type\n".to_string(),
        };
        Ok(ExecResult::Explain { plan })
    }

    /// Execute `EXPLAIN ANALYZE`: run the query, then report the plan with
    /// CBO decisions and actual execution statistics (row counts, timing).
    pub(crate) fn exec_explain_analyze(&mut self, stmt: &Statement) -> Result<ExecResult> {
        let mut plan = String::new();
        plan.push_str("QUERY PLAN (ANALYZE)\n");

        match stmt {
            Statement::Select(select) => {
                // ── Phase 1: Collect CBO statistics pre-execution ─────────
                if let Some(ref from) = select.from {
                    self.explain_from_plan(from, &mut plan, 1);
                }

                // Report WHERE clause analysis
                if let Some(ref where_expr) = select.where_clause {
                    plan.push_str("  FILTER (WHERE clause)\n");
                    // Check for index-eligible predicates
                    if let Some(ref from) = select.from {
                        self.explain_index_decision(from, where_expr, &mut plan);
                    }
                }

                if !select.order_by.is_empty() {
                    plan.push_str("  SORT (ORDER BY)\n");
                }
                if select.limit.is_some() {
                    plan.push_str("  LIMIT\n");
                }

                // ── Phase 2: Execute and measure ─────────────────────────
                let start = std::time::Instant::now();
                let result = self.exec_select(select);
                let elapsed = start.elapsed();

                match result {
                    Ok(ExecResult::QueryResult { rows, columns }) => {
                        plan.push_str(&format!(
                            "  Actual rows: {}, columns: {}\n",
                            rows.len(),
                            columns.len()
                        ));
                        plan.push_str(&format!("  Execution time: {:.3} ms\n", elapsed.as_secs_f64() * 1000.0));
                    }
                    Ok(_) => {
                        plan.push_str(&format!("  Execution time: {:.3} ms\n", elapsed.as_secs_f64() * 1000.0));
                    }
                    Err(e) => {
                        plan.push_str(&format!("  ERROR: {}\n", e));
                    }
                }
            }
            _ => {
                // For non-SELECT, produce basic EXPLAIN then execute
                let start = std::time::Instant::now();
                let result = self.exec_explain(stmt);
                let elapsed = start.elapsed();
                match result {
                    Ok(ExecResult::Explain { plan: ref p }) => {
                        plan.push_str(p);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        plan.push_str(&format!("  ERROR: {}\n", e));
                    }
                }
                plan.push_str(&format!("  Execution time: {:.3} ms\n", elapsed.as_secs_f64() * 1000.0));
            }
        }

        Ok(ExecResult::Explain { plan })
    }

    /// Execute `EXPLAIN (FORMAT TREE)`: tree-style plan output using
    /// box-drawing characters (├──, └──, │) for visual hierarchy.
    pub(crate) fn exec_explain_format_tree(&mut self, stmt: &Statement) -> Result<ExecResult> {
        let mut lines: Vec<String> = Vec::new();
        lines.push("QUERY PLAN (TREE)".to_string());

        match stmt {
            Statement::Select(select) => {
                let mut children: Vec<TreeNode> = Vec::new();

                // FROM clause → scan / join tree
                if let Some(ref from) = select.from {
                    children.push(self.tree_from_plan(from));
                }

                // WHERE
                if let Some(ref where_expr) = select.where_clause {
                    let mut filter = TreeNode::new("FILTER (WHERE clause)");
                    if let Some(ref from) = select.from {
                        if let crate::sql::ast::FromClause::Table { name, .. } = from {
                            let table_lower = name.to_lowercase();
                            let indexes = self.schema.indexes_for_table(&table_lower);
                            if !indexes.is_empty() {
                                for idx in &indexes {
                                    let sel_info = self.estimate_selectivity_label(
                                        &table_lower,
                                        idx,
                                        where_expr,
                                    );
                                    filter.children.push(TreeNode::new(&sel_info));
                                }
                            } else {
                                filter.children.push(TreeNode::new("Seq Scan (no index)"));
                            }
                        }
                    }
                    children.push(filter);
                }

                // ORDER BY
                if !select.order_by.is_empty() {
                    children.push(TreeNode::new("SORT (ORDER BY)"));
                }

                // GROUP BY
                if !select.group_by.is_empty() {
                    children.push(TreeNode::new("AGGREGATE (GROUP BY)"));
                }

                // HAVING
                if select.having.is_some() {
                    children.push(TreeNode::new("HAVING FILTER"));
                }

                // LIMIT
                if select.limit.is_some() {
                    children.push(TreeNode::new("LIMIT"));
                }

                // Render tree
                let root = TreeNode { label: "SELECT".to_string(), children };
                self.render_tree_node(&root, &mut lines, "", true);
            }
            Statement::Insert(insert) => {
                let node = TreeNode::new(&format!("INSERT INTO {}", insert.table_name));
                self.render_tree_node(&node, &mut lines, "", true);
            }
            Statement::Update(update) => {
                let mut node = TreeNode::new(&format!("UPDATE {}", update.table_name));
                node.children.push(TreeNode::new("SCAN table"));
                if update.where_clause.is_some() {
                    node.children.push(TreeNode::new("FILTER (WHERE clause)"));
                }
                self.render_tree_node(&node, &mut lines, "", true);
            }
            Statement::Delete(delete) => {
                let mut node = TreeNode::new(&format!("DELETE FROM {}", delete.table_name));
                node.children.push(TreeNode::new("SCAN table"));
                if delete.where_clause.is_some() {
                    node.children.push(TreeNode::new("FILTER (WHERE clause)"));
                }
                self.render_tree_node(&node, &mut lines, "", true);
            }
            _ => {
                lines.push("└── No plan available for this statement type".to_string());
            }
        }

        let plan = lines.join("\n") + "\n";
        Ok(ExecResult::Explain { plan })
    }

    /// Execute `EXPLAIN (FORMAT JSON)`: JSON-formatted query plan output.
    pub(crate) fn exec_explain_format_json(&mut self, stmt: &Statement) -> Result<ExecResult> {
        let root = match stmt {
            Statement::Select(select) => {
                let mut children: Vec<TreeNode> = Vec::new();

                if let Some(ref from) = select.from {
                    children.push(self.tree_from_plan(from));
                }

                if select.where_clause.is_some() {
                    children.push(TreeNode::new("FILTER (WHERE clause)"));
                }

                if !select.order_by.is_empty() {
                    children.push(TreeNode::new("SORT (ORDER BY)"));
                }

                if !select.group_by.is_empty() {
                    children.push(TreeNode::new("AGGREGATE (GROUP BY)"));
                }

                if select.having.is_some() {
                    children.push(TreeNode::new("HAVING FILTER"));
                }

                if select.limit.is_some() {
                    children.push(TreeNode::new("LIMIT"));
                }

                TreeNode { label: "SELECT".to_string(), children }
            }
            Statement::Insert(insert) => {
                TreeNode::new(&format!("INSERT INTO {}", insert.table_name))
            }
            Statement::Update(update) => {
                let mut node = TreeNode::new(&format!("UPDATE {}", update.table_name));
                node.children.push(TreeNode::new("SCAN table"));
                if update.where_clause.is_some() {
                    node.children.push(TreeNode::new("FILTER (WHERE clause)"));
                }
                node
            }
            Statement::Delete(delete) => {
                let mut node = TreeNode::new(&format!("DELETE FROM {}", delete.table_name));
                node.children.push(TreeNode::new("SCAN table"));
                if delete.where_clause.is_some() {
                    node.children.push(TreeNode::new("FILTER (WHERE clause)"));
                }
                node
            }
            _ => TreeNode::new("UNKNOWN"),
        };

        let plan = self.tree_node_to_json(&root, 0);
        Ok(ExecResult::Explain { plan })
    }

    /// Serialize a `TreeNode` into indented JSON.
    fn tree_node_to_json(&self, node: &TreeNode, depth: usize) -> String {
        let indent = "  ".repeat(depth);
        let inner = "  ".repeat(depth + 1);
        if node.children.is_empty() {
            format!("{indent}{{\n{inner}\"operation\": \"{}\"\n{indent}}}\n", node.label)
        } else {
            let children_json: Vec<String> = node.children
                .iter()
                .map(|c| self.tree_node_to_json(c, depth + 2))
                .collect();
            let joined = children_json.join(&format!("{inner}  ,\n"));
            format!(
                "{indent}{{\n{inner}\"operation\": \"{}\",\n{inner}\"children\": [\n{joined}{inner}]\n{indent}}}\n",
                node.label
            )
        }
    }

    /// Build a `TreeNode` from a FROM clause (recursive for JOINs).
    fn tree_from_plan(&self, from: &crate::sql::ast::FromClause) -> TreeNode {
        use crate::sql::ast::FromClause;
        match from {
            FromClause::Table { name, alias } => {
                let table_name = name.to_lowercase();
                let card_str = if let Ok(table) = self.schema.get_table(&table_name) {
                    if let Some(ref stats) = table.columns.first().and_then(|c| c.stats.as_ref()) {
                        format!(" (estimated rows: {})", stats.total_count)
                    } else {
                        " (no stats)".to_string()
                    }
                } else {
                    String::new()
                };
                let alias_str = alias.as_ref().map(|a| format!(" AS {}", a)).unwrap_or_default();
                TreeNode::new(&format!("SCAN {name}{alias_str}{card_str}"))
            }
            FromClause::Join { left, join_type, right, on } => {
                let jt = match join_type {
                    crate::sql::ast::JoinType::Inner => "INNER JOIN",
                    crate::sql::ast::JoinType::Left => "LEFT JOIN",
                    crate::sql::ast::JoinType::Right => "RIGHT JOIN",
                    crate::sql::ast::JoinType::Full => "FULL OUTER JOIN",
                    crate::sql::ast::JoinType::Cross => "CROSS JOIN",
                    crate::sql::ast::JoinType::LeftSemi => "LEFT SEMI JOIN",
                    crate::sql::ast::JoinType::RightSemi => "RIGHT SEMI JOIN",
                    crate::sql::ast::JoinType::Natural => "NATURAL JOIN",
                };
                let left_card = self.estimate_from_row_count(left);
                let right_card = self.estimate_from_row_count(right);
                let is_equi = on.is_some();
                let (left_sorted, right_sorted) = if let Some(ref on_expr) = on {
                    self.check_join_key_sorted(left, right, on_expr)
                } else {
                    (false, false)
                };
                let algo = super::exec_select::choose_join_algorithm(
                    left_card, right_card, is_equi, left_sorted, right_sorted,
                );
                let mut node = TreeNode::new(&format!(
                    "{jt} ({algo}) [left≈{left_card}, right≈{right_card}]"
                ));
                node.children.push(self.tree_from_plan(left));
                node.children.push(self.tree_from_plan(right));
                node
            }
            FromClause::Subquery { alias, .. } => {
                TreeNode::new(&format!("SUBQUERY AS {alias}"))
            }
            _ => TreeNode::new("SCAN (complex source)"),
        }
    }

    /// Render a `TreeNode` into lines with box-drawing characters.
    fn render_tree_node(
        &self,
        node: &TreeNode,
        lines: &mut Vec<String>,
        prefix: &str,
        is_last: bool,
    ) {
        // Connector for this node
        let connector = if prefix.is_empty() {
            "└── ".to_string()
        } else if is_last {
            format!("{prefix}└── ")
        } else {
            format!("{prefix}├── ")
        };
        lines.push(format!("{connector}{}", node.label));

        // Child prefix extends the tree lines
        let child_prefix = if prefix.is_empty() {
            "    ".to_string()
        } else if is_last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}│   ")
        };

        for (i, child) in node.children.iter().enumerate() {
            let child_is_last = i == node.children.len() - 1;
            self.render_tree_node(child, lines, &child_prefix, child_is_last);
        }
    }

    /// Format a selectivity label for a WHERE-clause index decision.
    fn estimate_selectivity_label(
        &self,
        table_name: &str,
        idx: &crate::schema::IndexSchema,
        _where_expr: &crate::sql::ast::Expr,
    ) -> String {
        if idx.columns.is_empty() {
            return format!("INDEX {} (no columns)", idx.name);
        }
        let col_name = &idx.columns[0];
        if let Ok(table) = self.schema.get_table(table_name) {
            for col in &table.columns {
                if col.name == *col_name {
                    if let Some(ref stats) = col.stats {
                        let ndv = stats.ndv.max(1);
                        let sel = 1.0 / ndv as f64;
                        return format!(
                            "INDEX SCAN {} on .{} (selectivity: {:.2})",
                            idx.name, col_name, sel
                        );
                    }
                }
            }
        }
        format!("INDEX SCAN {} on .{} (no stats)", idx.name, col_name)
    }

    /// Generate plan description for a FROM clause (recursive for JOINs).
    fn explain_from_plan(&self, from: &crate::sql::ast::FromClause, plan: &mut String, depth: usize) {
        use crate::sql::ast::FromClause;
        let indent = "  ".repeat(depth);
        match from {
            FromClause::Table { name, alias } => {
                let table_name = name.to_lowercase();
                let card_str = if let Ok(table) = self.schema.get_table(&table_name) {
                    if let Some(ref stats) = table.columns.first().and_then(|c| c.stats.as_ref()) {
                        format!(" (estimated rows: {})", stats.total_count)
                    } else {
                        " (no stats — run ANALYZE TABLE)".to_string()
                    }
                } else {
                    String::new()
                };
                let alias_str = alias.as_ref().map(|a| format!(" AS {}", a)).unwrap_or_default();
                plan.push_str(&format!("{indent}SCAN {name}{alias_str}{card_str}\n"));
            }
            FromClause::Join { left, join_type, right, on } => {
                let jt = match join_type {
                    crate::sql::ast::JoinType::Inner => "INNER JOIN",
                    crate::sql::ast::JoinType::Left => "LEFT JOIN",
                    crate::sql::ast::JoinType::Right => "RIGHT JOIN",
                    crate::sql::ast::JoinType::Full => "FULL OUTER JOIN",
                    crate::sql::ast::JoinType::Cross => "CROSS JOIN",
                    crate::sql::ast::JoinType::LeftSemi => "LEFT SEMI JOIN",
                    crate::sql::ast::JoinType::RightSemi => "RIGHT SEMI JOIN",
                    crate::sql::ast::JoinType::Natural => "NATURAL JOIN",
                };
                // CBO: estimate cardinalities and choose join algorithm
                let left_card = self.estimate_from_row_count(left);
                let right_card = self.estimate_from_row_count(right);
                let is_equi = on.is_some();
                // Check if either side is sorted on join key (simplified check)
                let (left_sorted, right_sorted) = if let Some(ref on_expr) = on {
                    self.check_join_key_sorted(left, right, on_expr)
                } else {
                    (false, false)
                };
                let algo = super::exec_select::choose_join_algorithm(
                    left_card, right_card, is_equi, left_sorted, right_sorted,
                );
                plan.push_str(&format!(
                    "{indent}{jt} ({algo}) [left≈{left_card}, right≈{right_card}]\n"
                ));
                self.explain_from_plan(left, plan, depth + 1);
                self.explain_from_plan(right, plan, depth + 1);
            }
            FromClause::Subquery { alias, .. } => {
                plan.push_str(&format!("{indent}SUBQUERY AS {alias}\n"));
            }
            _ => {
                plan.push_str(&format!("{indent}SCAN (complex source)\n"));
            }
        }
    }

    /// Report CBO index vs seq-scan decision for a WHERE clause.
    fn explain_index_decision(
        &self,
        from: &crate::sql::ast::FromClause,
        _where_expr: &crate::sql::ast::Expr,
        plan: &mut String,
    ) {
        // Only for simple single-table scans
        if let crate::sql::ast::FromClause::Table { name, .. } = from {
            let table_name = name.to_lowercase();
            if let Ok(table) = self.schema.get_table(&table_name) {
                // Check each column for index + stats
                let indexes = self.schema.indexes_for_table(&table_name);
                if !indexes.is_empty() {
                    for idx in &indexes {
                        if idx.columns.is_empty() {
                            continue;
                        }
                        let col_name = &idx.columns[0];
                        let has_stats = table
                            .columns
                            .iter()
                            .find(|c| c.name.eq_ignore_ascii_case(col_name))
                            .and_then(|c| c.stats.as_ref())
                            .is_some();
                        let hist_info = if let Some(col) = table
                            .columns
                            .iter()
                            .find(|c| c.name.eq_ignore_ascii_case(col_name))
                        {
                            if let Some(ref stats) = col.stats {
                                if stats.histogram.is_some() {
                                    " [histogram available]"
                                } else {
                                    " [no histogram]"
                                }
                            } else {
                                " [no stats]"
                            }
                        } else {
                            ""
                        };
                        plan.push_str(&format!(
                            "    Index: {} on ({}) — stats: {}{}\n",
                            idx.name,
                            idx.columns.join(", "),
                            if has_stats { "yes" } else { "no" },
                            hist_info,
                        ));
                    }
                } else {
                    plan.push_str("    No indexes available — seq scan\n");
                }
            }
        }
    }

    // ---- CREATE VIEW ----

    /// Execute a `CREATE [OR REPLACE] VIEW` statement.
    ///
    /// Views are stored as [`crate::schema::TableSchema`] entries with
    /// `root_page = 0` and `view_select` populated.  When a view is referenced
    /// in a `FROM` clause, [`super::exec_select::VM::eval_from`] detects the
    /// `view_select` field and evaluates the stored query transparently.
    ///
    /// A view definition row is also written to the schema catalog B-tree so
    /// that the view survives a process restart.
    ///
    /// If `CREATE OR REPLACE VIEW` is used and the view already exists, the old
    /// catalog entry is dropped first (B-NEW-1 fix).
    pub(crate) fn exec_create_view(&mut self, create: &CreateViewStmt) -> Result<ExecResult> {
        use crate::schema::TableSchema;
        use crate::storage::btree::BTree;
        let exists = self.schema.get_table(&create.name).is_ok();
        if exists {
            if create.or_replace {
                // B-NEW-1 fix: drop from catalog before replacing
                self.schema
                    .drop_table(&mut self.pager, &create.name, true)?;
            } else if create.if_not_exists {
                return Ok(ExecResult::Ok {
                    message: format!("VIEW {} already exists", create.name),
                });
            } else {
                return Err(crate::error::KkdbError::RuntimeError(format!(
                    "view '{}' already exists",
                    create.name
                )));
            }
        }
        // Views are stored as TableSchema in-memory with root_page=0 and view_select set
        let view_schema = TableSchema {
            name: create.name.clone(),
            columns: Vec::new(),
            col_names: create.columns.clone(),
            root_page: 0,
            next_rowid: 0,
            view_select: Some(create.query.as_ref().clone()),
            is_fts: false,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            rls_enabled: false,
            policies: Vec::new(),
            clustered_index: false, // views have no storage
        };
        self.schema.add_view(view_schema);

        // B-NEW-1 fix: persist view definition to catalog BTree so it survives restarts
        let view_sql = format!("CREATE VIEW {} AS (view query)", create.name);
        let schema_root = self.pager.schema_root_page();
        let schema_row = vec![
            crate::types::Value::Text("view".into()),
            crate::types::Value::Text(create.name.clone().into()),
            crate::types::Value::Text(create.name.clone().into()),
            crate::types::Value::Integer(0),
            crate::types::Value::Text(view_sql.into()),
        ];
        let mut btree = BTree::new(&mut self.pager);
        let max_id = btree.max_rowid(schema_root).unwrap_or(0);
        let new_root = btree.insert(schema_root, max_id + 1, &schema_row)?;
        if new_root != schema_root {
            self.pager.set_schema_root_page(new_root)?;
        }

        self.auto_flush()?;
        Ok(ExecResult::Ok {
            message: format!("VIEW {} created", create.name),
        })
    }

    /// Return a concise display name for a `FROM` clause (used by `EXPLAIN`).
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn from_name(&self, from: &FromClause) -> String {
        match from {
            FromClause::Table { name, .. } => name.clone(),
            FromClause::Join { left, right, .. } => {
                format!("{} JOIN {}", self.from_name(left), self.from_name(right))
            }
            FromClause::Subquery { alias, .. } => format!("(subquery) AS {}", alias),
            FromClause::SetOp { alias, .. } => format!("(setop) AS {}", alias),
            FromClause::TableFunction { name, alias, .. } => {
                alias.as_deref().unwrap_or(name.as_str()).to_string()
            }
        }
    }

    // ---- VACUUM ----
    /// VACUUM: merge pending-free pages into the active freelist, then flush.
    /// Reclaims storage space by making deleted / overflow pages available for reuse.
    /// In multi-file mode, applies to every open pager.
    /// Enhanced: defragments leaf pages and collects fragmentation statistics.
    pub(crate) fn exec_vacuum(&mut self) -> Result<ExecResult> {
        let mut report = Vec::new();
        let mut total_defragged = 0u64;
        let mut total_leaves = 0u64;
        let mut total_frag_bytes = 0u64;
        let mut total_overflow = 0u64;

        // Defragment each table's B-Tree
        let table_names: Vec<String> = self.schema.list_tables();
        for name in &table_names {
            let root_page = {
                let ts = self.schema.get_table(name)?;
                if ts.view_select.is_some() {
                    continue; // Skip views
                }
                ts.root_page
            };

            let tbl_pager = self.get_table_pager_mut(name);
            let mut btree = BTree::new(tbl_pager);

            // Collect pre-defrag stats
            let (leaves, frag, overflow, free) = btree.fragmentation_stats(root_page)?;
            total_leaves += leaves;
            total_frag_bytes += frag;
            total_overflow += overflow;

            // Defragment leaf pages
            let defragged = btree.defragment_all(root_page)?;
            total_defragged += defragged;

            if defragged > 0 || frag > 0 || overflow > 0 {
                report.push(format!(
                    "  {} : {} leaves, {} frag bytes reclaimed, {} overflow pages, {} free bytes",
                    name, leaves, frag, overflow, free
                ));
            }
        }

        // Flush all pagers
        self.pager.flush()?;
        for pager in self.table_pagers.values_mut() {
            pager.flush()?;
        }

        let msg = if report.is_empty() {
            format!(
                "VACUUM completed: {} tables, {} leaves scanned, no fragmentation found",
                table_names.len(),
                total_leaves,
            )
        } else {
            format!(
                "VACUUM completed: {} pages defragmented, {} frag bytes reclaimed, {} overflow pages\n{}",
                total_defragged,
                total_frag_bytes,
                total_overflow,
                report.join("\n"),
            )
        };

        Ok(ExecResult::Ok { message: msg })
    }

    /// O1: ANALYZE TABLE — scan table data and compute per-column statistics.
    pub(crate) fn exec_analyze_table(&mut self, table_name: String) -> Result<ExecResult> {
        use crate::schema::{ColumnStats, HistogramBucket};
        use crate::types::Value;
        use std::collections::HashSet;

        let root_page = self.schema.get_table(&table_name)?.root_page;
        let col_count = self.schema.get_table(&table_name)?.columns.len();

        // Scan all rows
        let tbl_pager = self.get_table_pager_mut(&table_name);
        let mut btree = crate::storage::btree::BTree::new(tbl_pager);
        let all_rows = btree.scan_all(root_page)?;

        // Per-column accumulators
        let total_count = all_rows.len() as i64;
        let mut null_counts = vec![0i64; col_count];
        let mut mins: Vec<Option<Value>> = vec![None; col_count];
        let mut maxs: Vec<Option<Value>> = vec![None; col_count];
        let mut ndvs: Vec<HashSet<String>> = (0..col_count).map(|_| HashSet::new()).collect();
        // Collect non-null values per column for histogram construction
        let mut col_values: Vec<Vec<Value>> = (0..col_count).map(|_| Vec::new()).collect();

        for (_rowid, row) in &all_rows {
            for (ci, val) in row.iter().enumerate() {
                if ci >= col_count {
                    break;
                }
                match val {
                    Value::Null => {
                        null_counts[ci] += 1;
                    }
                    v => {
                        // Track NDV via string repr (fast enough for ANALYZE)
                        ndvs[ci].insert(format!("{:?}", v));
                        col_values[ci].push(v.clone());

                        // Min tracking
                        if mins[ci].is_none() {
                            mins[ci] = Some(v.clone());
                        } else if let Some(ref cur_min) = mins[ci].clone() {
                            if val_lt(v, cur_min) {
                                mins[ci] = Some(v.clone());
                            }
                        }

                        // Max tracking
                        if maxs[ci].is_none() {
                            maxs[ci] = Some(v.clone());
                        } else if let Some(ref cur_max) = maxs[ci].clone() {
                            if val_lt(cur_max, v) {
                                maxs[ci] = Some(v.clone());
                            }
                        }
                    }
                }
            }
        }

        // Write stats back to schema
        let tbl = self.schema.get_table_mut(&table_name)?;
        for ci in 0..col_count {
            if let Some(col) = tbl.columns.get_mut(ci) {
                // Build equi-depth histogram (up to 64 buckets)
                let histogram = Self::build_histogram(&mut col_values[ci], 64);
                col.stats = Some(ColumnStats {
                    total_count,
                    null_count: null_counts[ci],
                    ndv: ndvs[ci].len() as i64,
                    min: mins[ci].clone(),
                    max: maxs[ci].clone(),
                    histogram,
                });
            }
        }

        Ok(ExecResult::Ok {
            message: format!(
                "ANALYZE TABLE {} complete: {} rows, {} columns",
                table_name, total_count, col_count
            ),
        })
    }

    /// O2: Build equi-depth histogram from a list of non-null values.
    /// Returns `None` if fewer than 2 values, otherwise up to `max_buckets` buckets.
    fn build_histogram(
        values: &mut Vec<crate::types::Value>,
        max_buckets: usize,
    ) -> Option<Vec<crate::schema::HistogramBucket>> {
        use crate::schema::HistogramBucket;

        if values.len() < 2 {
            return None;
        }

        // Sort values (Integer < Float < Text < Blob)
        values.sort_by(|a, b| val_cmp(a, b));

        let n = values.len();
        let num_buckets = max_buckets.min(n);
        let bucket_size = n / num_buckets;
        let remainder = n % num_buckets;

        let mut buckets = Vec::with_capacity(num_buckets);
        let mut pos = 0;
        let mut cumulative = 0i64;

        for bi in 0..num_buckets {
            let this_size = bucket_size + if bi < remainder { 1 } else { 0 };
            let end = pos + this_size;
            cumulative += this_size as i64;

            // Count distinct values in this bucket
            let mut distinct = std::collections::HashSet::new();
            for v in &values[pos..end] {
                distinct.insert(format!("{:?}", v));
            }

            buckets.push(HistogramBucket {
                upper_bound: values[end - 1].clone(),
                cumulative_count: cumulative,
                ndv_in_bucket: distinct.len() as i64,
            });
            pos = end;
        }

        Some(buckets)
    }

    /// L3: CREATE TRIGGER — persist trigger definition and register in memory schema
    pub(crate) fn exec_create_trigger(&mut self, trig: &CreateTriggerStmt) -> Result<ExecResult> {
        use crate::schema::TriggerSchema;
        let trig_name_lower = trig.name.to_lowercase();
        // Check if trigger already exists
        let exists = self
            .schema
            .triggers
            .values()
            .any(|v| v.iter().any(|t| t.name.to_lowercase() == trig_name_lower));
        if exists {
            if trig.or_replace {
                // drop existing first
                self.schema
                    .drop_trigger_by_name(&mut self.pager, &trig.name, true)?;
            } else {
                return Err(crate::error::KkdbError::RuntimeError(format!(
                    "trigger '{}' already exists",
                    trig.name
                )));
            }
        }
        // Verify the table exists
        let _ = self.schema.get_table(&trig.table_name)?;
        let schema_trigger = TriggerSchema {
            name: trig.name.clone(),
            timing: trig.timing.clone(),
            event: trig.event.clone(),
            table_name: trig.table_name.clone(),
            body_sql: trig.body_sql.clone(),
            rowid: 0, // will be assigned in save_trigger
        };
        self.schema.save_trigger(&mut self.pager, schema_trigger)?;
        Ok(ExecResult::Ok {
            message: format!("TRIGGER {} created", trig.name),
        })
    }

    /// L3: DROP TRIGGER [IF EXISTS] name
    pub(crate) fn exec_drop_trigger(&mut self, name: &str, if_exists: bool) -> Result<ExecResult> {
        self.schema
            .drop_trigger_by_name(&mut self.pager, name, if_exists)?;
        Ok(ExecResult::Ok {
            message: format!("TRIGGER {} dropped", name),
        })
    }

    // ---- USER MANAGEMENT ----
    pub(crate) fn exec_create_user(&mut self, stmt: &CreateUserStmt) -> Result<ExecResult> {
        // S-NEW-1 fix: use parameterized insert to avoid SQL injection
        use crate::sql::ast::{ConflictPolicy, Expr, InsertSource, InsertStmt};
        let pw_hash = stmt.password.as_deref().unwrap_or("").to_string();
        let insert = InsertStmt {
            table_name: "kkdb_users".to_string(),
            columns: Some(vec!["username".to_string(), "password_hash".to_string()]),
            source: InsertSource::Values(vec![vec![
                Expr::StringLiteral(stmt.username.clone()),
                Expr::StringLiteral(pw_hash),
            ]]),
            conflict: ConflictPolicy::Error,
            returning: None,
        };
        self.exec_insert(&insert)?;
        Ok(ExecResult::Ok {
            message: format!("User '{}' created", stmt.username),
        })
    }

    pub(crate) fn exec_alter_user(&mut self, stmt: &AlterUserStmt) -> Result<ExecResult> {
        if let Some(ref pw) = stmt.password {
            // S-NEW-1 fix: use parameterized update to avoid SQL injection
            use crate::sql::ast::BinaryOperator;
            use crate::sql::ast::{Expr, UpdateStmt};
            let update = UpdateStmt {
                table_name: "kkdb_users".to_string(),
                assignments: vec![(
                    "password_hash".to_string(),
                    Expr::StringLiteral(pw.clone()),
                )],
                where_clause: Some(Expr::BinaryOp {
                    left: Box::new(Expr::ColumnRef {
                        table: None,
                        column: "username".to_string(),
                    }),
                    op: BinaryOperator::Equal,
                    right: Box::new(Expr::StringLiteral(stmt.username.clone())),
                }),
                returning: None,
            };
            self.exec_update(&update)?;
        }
        Ok(ExecResult::Ok {
            message: format!("User '{}' altered", stmt.username),
        })
    }

    pub(crate) fn exec_drop_user(&mut self, stmt: &DropUserStmt) -> Result<ExecResult> {
        // S-NEW-1 fix: use parameterized delete for each username
        use crate::sql::ast::{BinaryOperator, DeleteStmt, Expr};
        for username in &stmt.usernames {
            let where_clause = Expr::BinaryOp {
                left: Box::new(Expr::ColumnRef {
                    table: None,
                    column: "username".to_string(),
                }),
                op: BinaryOperator::Equal,
                right: Box::new(Expr::StringLiteral(username.clone())),
            };
            let del_user = DeleteStmt {
                table_name: "kkdb_users".to_string(),
                where_clause: Some(where_clause.clone()),
                returning: None,
            };
            let _ = self.exec_delete(&del_user);
            let del_privs = DeleteStmt {
                table_name: "kkdb_privileges".to_string(),
                where_clause: Some(Expr::BinaryOp {
                    left: Box::new(Expr::ColumnRef {
                        table: None,
                        column: "username".to_string(),
                    }),
                    op: BinaryOperator::Equal,
                    right: Box::new(Expr::StringLiteral(username.clone())),
                }),
                returning: None,
            };
            let _ = self.exec_delete(&del_privs);
        }
        Ok(ExecResult::Ok {
            message: "Dropped user(s)".to_string(),
        })
    }

    pub(crate) fn exec_grant(&mut self, stmt: &GrantStmt) -> Result<ExecResult> {
        let privs = match &stmt.privileges {
            PrivilegeList::All => vec!["ALL".to_string()],
            PrivilegeList::Specific(list) => list.clone(),
        };
        let obj_str = match &stmt.object {
            GrantObject::Table(t) => t.clone(),
            GrantObject::Database(d) => d.clone(),
            GrantObject::Global => "GLOBAL".to_string(),
        };

        for grantee in &stmt.grantees {
            for priv_type in &privs {
                let sql = format!(
                    "INSERT INTO kkdb_privileges (username, obj_name, priv_type) VALUES ('{}', '{}', '{}')",
                    grantee, obj_str, priv_type
                );
                self.execute_sql(&sql)?;
            }
        }
        Ok(ExecResult::Ok {
            message: "Privileges granted".into(),
        })
    }

    pub(crate) fn exec_revoke(&mut self, stmt: &RevokeStmt) -> Result<ExecResult> {
        let privs = match &stmt.privileges {
            PrivilegeList::All => vec!["ALL".to_string()],
            PrivilegeList::Specific(list) => list.clone(),
        };
        let obj_str = match &stmt.object {
            GrantObject::Table(t) => t.clone(),
            GrantObject::Database(d) => d.clone(),
            GrantObject::Global => "GLOBAL".to_string(),
        };

        for grantee in &stmt.grantees {
            for priv_type in &privs {
                let sql = format!(
                    "DELETE FROM kkdb_privileges WHERE username = '{}' AND obj_name = '{}' AND priv_type = '{}'",
                    grantee, obj_str, priv_type
                );
                self.execute_sql(&sql)?;
            }
        }
        Ok(ExecResult::Ok {
            message: "Privileges revoked".into(),
        })
    }

    // ---- RLS POLICIES ----
    pub(crate) fn exec_create_policy(&mut self, stmt: &CreatePolicyStmt) -> Result<ExecResult> {
        let tbl_key = stmt.table_name.to_ascii_lowercase();
        let tbl = self.schema.tables.get_mut(&tbl_key).ok_or_else(|| {
            crate::error::KkdbError::RuntimeError(format!("table '{}' not found", stmt.table_name))
        })?;

        // Remove existing policy with same name if any
        tbl.policies.retain(|p| p.name != stmt.name);
        tbl.policies.push(crate::schema::PolicySchema {
            name: stmt.name.clone(),
            role: stmt.role.clone(),
            using_expr: stmt.using_expr.clone(),
            check_expr: stmt.check_expr.clone(),
        });

        Ok(ExecResult::Ok {
            message: format!("POLICY '{}' on '{}' created", stmt.name, stmt.table_name),
        })
    }

    pub(crate) fn exec_drop_policy(&mut self, stmt: &DropPolicyStmt) -> Result<ExecResult> {
        let tbl_key = stmt.table_name.to_ascii_lowercase();
        let tbl = self.schema.tables.get_mut(&tbl_key).ok_or_else(|| {
            crate::error::KkdbError::RuntimeError(format!("table '{}' not found", stmt.table_name))
        })?;

        let before = tbl.policies.len();
        tbl.policies.retain(|p| p.name != stmt.name);
        if tbl.policies.len() == before && !stmt.if_exists {
            return Err(crate::error::KkdbError::RuntimeError(format!(
                "policy '{}' not found on '{}'",
                stmt.name, stmt.table_name
            )));
        }

        Ok(ExecResult::Ok {
            message: format!("POLICY '{}' on '{}' dropped", stmt.name, stmt.table_name),
        })
    }

    /// Write FTS postings directly to the given BTree root page (no execute_sql, no schema table).
    /// Row format: (token TEXT, doc_id INTEGER, tf INTEGER, field_len INTEGER, meta_key TEXT)
    ///  - posting: meta_key = Null
    ///  - doc-freq: meta_key = "DF"
    ///  - global stats: meta_key = "GLOBAL"
    pub(crate) fn write_fts_postings_raw(
        &mut self,
        fts_root: u32,
        postings: &std::collections::HashMap<String, std::collections::HashMap<u64, (u32, u32)>>,
        doc_freq: &std::collections::HashMap<String, u64>,
        total_docs: u64,
        total_field_len: u64,
    ) -> Result<()> {
        use crate::types::Value;
        // Get next rowid from in-memory sequence (atomic, race-free even across pagers).
        // Re-seed from btree if our cached value would collide with existing rows
        // (e.g., after CREATE FULLTEXT INDEX wrote rows before any DML).
        let actual_max = {
            let mut btree = BTree::new(&mut self.pager);
            btree.max_rowid(fts_root).unwrap_or(0)
        };
        let mut next_rowid = {
            let seq = self
                .fts_rowid_sequences
                .entry(fts_root)
                .or_insert(actual_max + 1);
            // If the cached value is stale (behind actual max), re-sync it.
            if *seq <= actual_max {
                *seq = actual_max + 1;
            }
            *seq
        };
        let mut current_root = fts_root;

        // Write posting entries
        for (token, row_map) in postings {
            for (&doc_id, &(tf, field_len)) in row_map {
                let row = vec![
                    Value::Text(token.as_str().into()),
                    Value::Integer(doc_id as i64),
                    Value::Integer(tf as i64),
                    Value::Integer(field_len as i64),
                    Value::Null,
                ];
                let mut btree = BTree::new(&mut self.pager);
                current_root = btree.insert(current_root, next_rowid, &row)?;
                next_rowid += 1;
            }
        }

        // Write DF entries
        for (token, &df) in doc_freq {
            let row = vec![
                Value::Text(token.as_str().into()),
                Value::Integer(df as i64),
                Value::Integer(0),
                Value::Integer(0),
                Value::Text("DF".into()),
            ];
            let mut btree = BTree::new(&mut self.pager);
            current_root = btree.insert(current_root, next_rowid, &row)?;
            next_rowid += 1;
        }

        // Write GLOBAL stats row (total_docs in col1, total_field_len in col2)
        let global_row = vec![
            Value::Text("_GLOBAL".into()),
            Value::Integer(total_docs as i64),
            Value::Integer(total_field_len as i64),
            Value::Integer(0),
            Value::Text("GLOBAL".into()),
        ];
        let mut btree = BTree::new(&mut self.pager);
        let final_root = btree.insert(current_root, next_rowid, &global_row)?;

        // If root changed (tree split), update schema
        if final_root != fts_root {
            // Update the root_page in IndexSchema for this FTS index
            for idx in self.schema.indexes.values_mut() {
                if idx.is_fts && idx.root_page == fts_root {
                    idx.root_page = final_root;
                    break;
                }
            }
            // Reset sequence so it will be reseeded on next open (avoids stale key)
            self.fts_rowid_sequences.remove(&fts_root);
            self.fts_rowid_sequences.insert(final_root, next_rowid + 1);
        } else {
            // Persist the incremented counter for this fts_root
            self.fts_rowid_sequences.insert(fts_root, next_rowid + 1);
        }
        Ok(())
    }

    /// Convenience wrapper that finds the fts_root from the schema for a given index_id.
    /// Here index_id IS the fts_root (stored in IndexSchema.root_page).
    pub(crate) fn write_fts_postings(
        &mut self,
        fts_root: u32,
        postings: &std::collections::HashMap<String, std::collections::HashMap<u64, (u32, u32)>>,
        doc_freq: &std::collections::HashMap<String, u64>,
        total_docs: u64,
        total_field_len: u64,
    ) -> Result<()> {
        self.write_fts_postings_raw(fts_root, postings, doc_freq, total_docs, total_field_len)
    }

    /// Drain the pending FTS inserts collected during the last statement execution.
    /// Called at the end of execute_sql to avoid reentrant execute_sql during DML.
    pub(crate) fn drain_pending_fts_inserts(&mut self) {
        if self.pending_fts_inserts.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_fts_inserts);

        for (stale_fts_root, doc_id, tfs, field_len) in pending {
            if stale_fts_root == 0 {
                continue;
            }

            // Re-read the CURRENT root_page from schema in case a previous write split
            // the B-Tree and updated the root (the queued value may now be stale).
            let fts_root = self
                .schema
                .indexes
                .values()
                .find(|idx| {
                    idx.is_fts
                        && (idx.root_page == stale_fts_root || {
                            // After a split the old root may no longer match; fall back to
                            // locating the FTS index whose columns cover the same pages.
                            // For now we trust that root_page==stale means no split yet;
                            // if not found, keep using stale_fts_root (best effort).
                            false
                        })
                })
                .map(|idx| idx.root_page)
                .unwrap_or(stale_fts_root);

            let (cur_docs, cur_field_len) = self.read_fts_global_stats(fts_root);
            let new_total_docs = cur_docs + 1;
            let new_total_field_len = cur_field_len + field_len as u64;

            let mut postings: std::collections::HashMap<
                String,
                std::collections::HashMap<u64, (u32, u32)>,
            > = std::collections::HashMap::new();
            let mut doc_freq: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();

            for (token, tf) in tfs {
                postings
                    .entry(token.clone())
                    .or_default()
                    .insert(doc_id as u64, (tf, field_len));
                let existing_df = self.get_fts_doc_freq(fts_root, &token);
                doc_freq.insert(token, existing_df + 1);
            }

            let _ = self.write_fts_postings_raw(
                fts_root,
                &postings,
                &doc_freq,
                new_total_docs,
                new_total_field_len,
            );
        }
    }

    /// Read global FTS stats (total_docs, total_field_len) directly from BTree.
    /// fts_root is the IndexSchema.root_page for the FTS index.
    /// Returns the stats from the LAST GLOBAL row found (most recent).
    pub(crate) fn read_fts_global_stats(&mut self, fts_root: u32) -> (u64, u64) {
        use crate::types::Value;
        if fts_root == 0 {
            return (0, 0);
        }
        let mut btree = BTree::new(&mut self.pager);
        let rows = btree.scan_rows(fts_root).unwrap_or_default();
        let mut result = (0u64, 0u64);
        for row in &rows {
            if row.get(4) == Some(&Value::Text("GLOBAL".into())) {
                let total_docs = if let Some(Value::Integer(v)) = row.get(1) {
                    *v as u64
                } else {
                    0
                };
                let total_field_len = if let Some(Value::Integer(v)) = row.get(2) {
                    *v as u64
                } else {
                    0
                };
                result = (total_docs, total_field_len); // take latest (last) GLOBAL row
            }
        }
        result
    }

    /// Scan all posting entries for a given token in the FTS index (direct BTree).
    pub(crate) fn scan_fts_postings(&mut self, fts_root: u32, token: &str) -> Vec<(u64, u32, u32)> {
        use crate::types::Value;
        if fts_root == 0 {
            return Vec::new();
        }
        let mut btree = BTree::new(&mut self.pager);
        let rows = btree.scan_rows(fts_root).unwrap_or_default();
        rows.into_iter()
            .filter_map(|row| {
                if row.get(4) != Some(&Value::Null) {
                    return None;
                }
                let row_token = if let Some(Value::Text(s)) = row.first() {
                    s.to_string()
                } else {
                    return None;
                };
                if row_token != token {
                    return None;
                }
                let doc_id = if let Some(Value::Integer(v)) = row.get(1) {
                    *v as u64
                } else {
                    return None;
                };
                let tf = if let Some(Value::Integer(v)) = row.get(2) {
                    *v as u32
                } else {
                    0
                };
                let fl = if let Some(Value::Integer(v)) = row.get(3) {
                    *v as u32
                } else {
                    0
                };
                Some((doc_id, tf, fl))
            })
            .collect()
    }

    /// Get doc_freq for a token directly from BTree.
    pub(crate) fn get_fts_doc_freq(&mut self, fts_root: u32, token: &str) -> u64 {
        use crate::types::Value;
        if fts_root == 0 {
            return 0;
        }
        let mut btree = BTree::new(&mut self.pager);
        let rows = btree.scan_rows(fts_root).unwrap_or_default();
        for row in &rows {
            if row.get(4) == Some(&Value::Text("DF".into())) {
                if let Some(Value::Text(t)) = row.first() {
                    if t.as_ref() == token {
                        return if let Some(Value::Integer(v)) = row.get(1) {
                            *v as u64
                        } else {
                            0
                        };
                    }
                }
            }
        }
        0
    }
}

fn val_lt(a: &crate::types::Value, b: &crate::types::Value) -> bool {
    use crate::types::Value;
    match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => x < y,
        (Value::Real(x), Value::Real(y)) => x < y,
        (Value::Integer(x), Value::Real(y)) => (*x as f64) < *y,
        (Value::Real(x), Value::Integer(y)) => *x < (*y as f64),
        (Value::Text(x), Value::Text(y)) => x < y,
        _ => false,
    }
}

/// O2: Total ordering for sorting values (used by histogram builder).
fn val_cmp(a: &crate::types::Value, b: &crate::types::Value) -> std::cmp::Ordering {
    use crate::types::Value;
    match (a, b) {
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
        (Value::Real(x), Value::Real(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
        (Value::Integer(x), Value::Real(y)) => (*x as f64)
            .partial_cmp(y)
            .unwrap_or(std::cmp::Ordering::Equal),
        (Value::Real(x), Value::Integer(y)) => x
            .partial_cmp(&(*y as f64))
            .unwrap_or(std::cmp::Ordering::Equal),
        (Value::Text(x), Value::Text(y)) => x.cmp(y),
        (Value::Blob(x), Value::Blob(y)) => x.cmp(y),
        // Type ordering: Integer/Real < Text < Blob
        (Value::Integer(_) | Value::Real(_), _) => std::cmp::Ordering::Less,
        (_, Value::Integer(_) | Value::Real(_)) => std::cmp::Ordering::Greater,
        (Value::Text(_), _) => std::cmp::Ordering::Less,
        (_, Value::Text(_)) => std::cmp::Ordering::Greater,
    }
}

impl VM {
    /// SHOW ENGINE STATUS — InnoDB-style status report.
    pub(crate) fn exec_show_engine_status(&self) -> Result<ExecResult> {
        let cfg = &self.pager.engine_config;
        let mut lines: Vec<String> = Vec::new();

        lines.push("===== InnoDB Engine Status =====".to_string());
        lines.push(String::new());

        // Buffer pool
        let bp = self.pager.buffer_pool_stats();
        lines.push(format!(
            "Buffer pool pages  : {} ({})",
            bp.max_pages,
            if bp.max_pages == 0 { "unlimited" } else { "limited" }
        ));
        lines.push(format!(
            "Buffer pool usage  : {} loaded, {} dirty, {} clean",
            bp.loaded_pages, bp.dirty_pages, bp.clean_pages
        ));
        lines.push(format!(
            "Buffer pool hit%   : {:.1}%",
            bp.hit_rate_approx * 100.0
        ));
        lines.push(format!(
            "LRU queue length   : {}",
            bp.lru_queue_len
        ));

        // WAL
        lines.push(format!("WAL enabled        : {}", self.pager.is_wal_enabled()));
        lines.push(format!("WAL auto-checkpoint: {} frames", cfg.wal_auto_checkpoint));
        if let Some(ref wal) = self.pager.wal {
            lines.push(format!(
                "WAL committed      : {} frames",
                wal.committed_frame_count()
            ));
            lines.push(format!(
                "WAL uncommitted    : {} frames",
                wal.uncommitted_frame_count()
            ));
            lines.push(format!("WAL sync mode      : {:?}", wal.sync_mode()));
            let ws = wal.wal_stats();
            lines.push(format!("WAL total commits  : {}", ws.total_commits));
            lines.push(format!("WAL total fsyncs   : {}", ws.total_fsyncs));
            lines.push(format!("WAL group syncs    : {}", ws.group_syncs));
            lines.push(format!("WAL frames written : {}", ws.total_frames_written));
            if ws.pending_sync_commits > 0 {
                lines.push(format!("WAL pending sync   : {} commits", ws.pending_sync_commits));
            }
        }
        lines.push(format!("Current LSN        : {}", self.pager.current_lsn()));

        // Compression
        lines.push(format!("LZ4 compression    : {}", cfg.use_lz4));

        // Flush method
        lines.push(format!("Flush method       : {:?}", cfg.flush_method));

        // Pages (from header)
        lines.push(format!("Total pages        : {}", self.pager.header.total_pages));

        // MVCC status
        lines.push(String::new());
        lines.push("--- MVCC ---".to_string());
        lines.push(format!("Current txn ID     : {}", self.current_txn_id));
        lines.push(format!("Next txn ID        : {}", self.txn_registry.next_id()));
        lines.push(format!("Active transactions: {}", self.txn_registry.active_count()));
        // Snapshot status
        if let Some(ref snap) = self.mvcc_snapshot {
            lines.push(format!("Snapshot reader    : txn {}", snap.reader_txn_id));
            lines.push(format!("Snapshot max commit: {}", snap.max_committed_txn_id));
            lines.push(format!("Snapshot active    : {:?}", snap.active_txn_ids));
        } else {
            lines.push("Snapshot           : none (autocommit)".to_string());
        }
        let undo_stats = self.mvcc_undo_log.stats();
        lines.push(format!("Undo log entries   : {}", undo_stats.total_entries));
        lines.push(format!("Undo log size      : {} bytes", undo_stats.size_bytes));
        lines.push(format!(
            "Undo breakdown     : {} inserts, {} updates, {} deletes, {} savepoints",
            undo_stats.inserts, undo_stats.updates, undo_stats.deletes, undo_stats.savepoints
        ));

        // Query Cache
        lines.push(String::new());
        lines.push("--- Query Cache ---".to_string());
        lines.push(format!("Cache enabled      : {}", self.query_cache.is_enabled()));
        lines.push(format!("Cache entries      : {} / {}", self.query_cache.len(), self.query_cache.max_entries()));
        lines.push(format!("Cache lookups      : {}", self.query_cache.stat_lookups));
        lines.push(format!("Cache hits         : {}", self.query_cache.stat_hits));
        lines.push(format!("Cache misses       : {}", self.query_cache.stat_misses));
        lines.push(format!("Cache hit rate     : {:.1}%", self.query_cache.hit_rate()));
        lines.push(format!("Cache invalidations: {}", self.query_cache.stat_invalidations));
        lines.push(format!("Cache evictions    : {}", self.query_cache.stat_evictions));

        // Clustered index info
        lines.push(String::new());
        lines.push("--- Clustered Index ---".to_string());
        let tables = self.schema.list_tables();
        let clustered_count = tables.iter().filter(|t| {
            self.schema.get_table(t).map(|s| s.clustered_index).unwrap_or(false)
        }).count();
        lines.push(format!("Tables (clustered) : {} / {}", clustered_count, tables.len()));
        let pk_clustered = tables.iter().filter(|t| {
            self.schema.get_table(t).map(|s| s.pk_is_integer_clustered()).unwrap_or(false)
        }).count();
        lines.push(format!("PK integer cluster : {} tables", pk_clustered));

        // Per-table pagers
        if !self.table_pagers.is_empty() {
            lines.push(String::new());
            lines.push(format!("--- Per-Table Pagers ({}) ---", self.table_pagers.len()));
            for (name, tp) in &self.table_pagers {
                let wal_str = if tp.is_wal_enabled() { "WAL" } else { "direct" };
                lines.push(format!(
                    "  {} : {} pages, mode={}, lsn={}",
                    name,
                    tp.header.total_pages,
                    wal_str,
                    tp.current_lsn()
                ));
            }
        }

        lines.push(String::new());
        lines.push("================================".to_string());

        Ok(ExecResult::Explain {
            plan: lines.join("\n"),
        })
    }
}