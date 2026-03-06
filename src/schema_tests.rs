use super::*;
use crate::storage::pager::Pager;

fn setup() -> (Pager, Schema) {
    let pager = Pager::open_memory();
    let schema = Schema::new();
    (pager, schema)
}

/// Convenience for tests: call create_table with the same pager as both catalog and table.
/// SAFETY: catalog_pager and table_pager point to the same Pager object.
/// In single-file/memory mode this is valid because there is only one B-Tree file.
fn create_table_sp(
    schema: &mut Schema,
    pager: &mut Pager,
    name: &str,
    cols: &[ColumnDef],
    if_not_exists: bool,
    sql: &str,
) -> crate::error::Result<()> {
    // SAFETY: single aliased pager is the same object; OK for memory-mode tests.
    let p2: &mut Pager = unsafe { &mut *(pager as *mut Pager) };
    schema.create_table(pager, p2, name, cols, if_not_exists, sql)
}

fn sample_columns() -> Vec<ColumnDef> {
    vec![
        ColumnDef {
            name: "id".into(),
            data_type: DataType::Integer,
            primary_key: true,
            autoincrement: true,
            not_null: true,
            unique: false,
            default: None,
            references: None,
        },
        ColumnDef {
            name: "name".into(),
            data_type: DataType::Text,
            primary_key: false,
            autoincrement: false,
            not_null: false,
            unique: false,
            default: None,
            references: None,
        },
    ]
}

#[test]
fn test_schema_new() {
    let schema = Schema::new();
    assert!(schema.tables.is_empty());
}

#[test]
fn test_create_table_and_get() {
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "users",
            &cols,
            false,
            "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();

    let table = schema.get_table("users").unwrap();
    assert_eq!(table.name, "users");
    assert_eq!(table.columns.len(), 2);
    assert_eq!(table.columns[0].name, "id");
    assert!(table.columns[0].primary_key);
    assert!(table.columns[0].autoincrement);
    assert_eq!(table.next_rowid, 1);
}

#[test]
fn test_create_table_already_exists() {
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();
    let result = create_table_sp(
        &mut schema,
        &mut pager,
        "t1",
        &cols,
        false,
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
    );
    assert!(result.is_err());
}

#[test]
fn test_create_table_if_not_exists() {
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();
    let result = create_table_sp(
        &mut schema,
        &mut pager,
        "t1",
        &cols,
        true,
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
    );
    assert!(result.is_ok());
}

#[test]
fn test_drop_table() {
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();
    schema.drop_table(&mut pager, "t1", false).unwrap();
    assert!(schema.get_table("t1").is_err());
}

#[test]
fn test_drop_table_not_found() {
    let (mut pager, mut schema) = setup();
    let result = schema.drop_table(&mut pager, "nonexistent", false);
    assert!(result.is_err());
}

#[test]
fn test_drop_table_if_exists() {
    let (mut pager, mut schema) = setup();
    let result = schema.drop_table(&mut pager, "nonexistent", true);
    assert!(result.is_ok());
}

#[test]
fn test_get_table_not_found() {
    let schema = Schema::new();
    assert!(schema.get_table("nope").is_err());
}

#[test]
fn test_get_table_mut() {
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();
    let table = schema.get_table_mut("t1").unwrap();
    table.next_rowid = 100;
    assert_eq!(schema.get_table("t1").unwrap().next_rowid, 100);
}

#[test]
fn test_get_table_mut_not_found() {
    let mut schema = Schema::new();
    assert!(schema.get_table_mut("nope").is_err());
}

#[test]
fn test_find_column() {
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();
    assert_eq!(schema.find_column("t1", "id").unwrap(), 0);
    assert_eq!(schema.find_column("t1", "name").unwrap(), 1);
    assert_eq!(schema.find_column("t1", "NAME").unwrap(), 1); // case insensitive
}

#[test]
fn test_find_column_not_found() {
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();
    assert!(schema.find_column("t1", "nonexistent").is_err());
}

#[test]
fn test_find_column_table_not_found() {
    let schema = Schema::new();
    assert!(schema.find_column("nope", "col").is_err());
}

#[test]
fn test_load_from_pager() {
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();

    // Create a fresh schema and load from pager
    let mut schema2 = Schema::new();
    schema2.load_from_pager(&mut pager).unwrap();
    assert!(schema2.get_table("t1").is_ok());
    assert_eq!(schema2.get_table("t1").unwrap().columns.len(), 2);
}

#[test]
fn test_load_multiple_tables_from_pager() {
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();
    create_table_sp(&mut schema, &mut pager,
            "t2",
            &cols,
            false,
            "CREATE TABLE t2 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();

    let mut schema2 = Schema::new();
    schema2.load_from_pager(&mut pager).unwrap();
    assert!(schema2.get_table("t1").is_ok());
    assert!(schema2.get_table("t2").is_ok());
}

#[test]
fn test_create_table_column_info() {
    let (mut pager, mut schema) = setup();
    let cols = vec![
        ColumnDef {
            name: "id".into(),
            data_type: DataType::Integer,
            primary_key: true,
            autoincrement: false,
            not_null: true,
            unique: true,
            default: None,
            references: None,
        },
        ColumnDef {
            name: "email".into(),
            data_type: DataType::Text,
            primary_key: false,
            autoincrement: false,
            not_null: true,
            unique: true,
            default: None,
            references: None,
        },
    ];
    create_table_sp(&mut schema, &mut pager, "users", &cols, false, "CREATE TABLE users (id INTEGER PRIMARY KEY NOT NULL UNIQUE, email TEXT NOT NULL UNIQUE)").unwrap();

    let table = schema.get_table("users").unwrap();
    assert_eq!(table.columns[0].col_index, 0);
    assert_eq!(table.columns[1].col_index, 1);
    assert!(table.columns[0].not_null);
    assert!(table.columns[1].not_null);
    assert_eq!(table.columns[1].data_type, DataType::Text);
}

#[test]
fn test_drop_and_recreate_table() {
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();
    schema.drop_table(&mut pager, "t1", false).unwrap();
    // Should be able to create again
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();
    assert!(schema.get_table("t1").is_ok());
}

#[test]
fn test_load_from_pager_with_data() {
    // Create a table with data, reload schema, verify next_rowid is correct
    let (mut pager, mut schema) = setup();
    let cols = sample_columns();
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
        )
        .unwrap();

    // Insert some data to advance the rowid
    let table = schema.get_table("t1").unwrap();
    let root = table.root_page;
    {
        let mut btree = crate::storage::btree::BTree::new(&mut pager);
        btree
            .insert(root, 1, &vec![Value::Integer(1), Value::Text("A".into())])
            .unwrap();
        btree
            .insert(root, 2, &vec![Value::Integer(2), Value::Text("B".into())])
            .unwrap();
    }

    // Load fresh schema - next_rowid should be 3
    let mut schema2 = Schema::new();
    schema2.load_from_pager(&mut pager).unwrap();
    let table2 = schema2.get_table("t1").unwrap();
    assert_eq!(table2.next_rowid, 3);
}

#[test]
fn test_column_info_unique_flag() {
    let (mut pager, mut schema) = setup();
    let cols = vec![
        ColumnDef {
            name: "id".into(),
            data_type: DataType::Integer,
            primary_key: true,
            autoincrement: false,
            not_null: false,
            unique: false,
            default: None,
            references: None,
        },
        ColumnDef {
            name: "email".into(),
            data_type: DataType::Text,
            primary_key: false,
            autoincrement: false,
            not_null: false,
            unique: true,
            default: None,
            references: None,
        },
    ];
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY, email TEXT UNIQUE)",
        )
        .unwrap();

    let table = schema.get_table("t1").unwrap();
    assert!(!table.columns[0].unique);
    assert!(table.columns[1].unique);
    assert!(!table.columns[1].primary_key);
    assert!(!table.columns[1].autoincrement);
}

#[test]
fn test_load_from_pager_with_short_row() {
    // Insert a row with < 5 columns into schema table, should be skipped
    let mut pager = Pager::open_memory();
    {
        let schema_root = pager.schema_root_page();
        let mut btree = crate::storage::btree::BTree::new(&mut pager);
        // Insert short row into page 1 (schema table)
        btree
            .insert(
                schema_root,
                100,
                &vec![Value::Text("table".into()), Value::Text("bad".into())],
            )
            .unwrap();
    }
    let mut schema = Schema::new();
    schema.load_from_pager(&mut pager).unwrap();
    // Short row should be skipped
    assert!(schema.get_table("bad").is_err());
}

#[test]
fn test_load_from_pager_with_non_text_type() {
    // Insert a row where obj_type (row[0]) is not Text
    let mut pager = Pager::open_memory();
    {
        let schema_root = pager.schema_root_page();
        let mut btree = crate::storage::btree::BTree::new(&mut pager);
        btree
            .insert(
                schema_root,
                100,
                &vec![
                    Value::Integer(999), // not Text -> skip
                    Value::Text("bad".into()),
                    Value::Text("bad".into()),
                    Value::Integer(2),
                    Value::Text("CREATE TABLE bad (id INT)".into()),
                ],
            )
            .unwrap();
    }
    let mut schema = Schema::new();
    schema.load_from_pager(&mut pager).unwrap();
    assert!(schema.get_table("bad").is_err());
}

#[test]
fn test_load_from_pager_with_non_text_name() {
    // Insert a row where name (row[1]) is not Text
    let mut pager = Pager::open_memory();
    {
        let schema_root = pager.schema_root_page();
        let mut btree = crate::storage::btree::BTree::new(&mut pager);
        btree
            .insert(
                schema_root,
                100,
                &vec![
                    Value::Text("table".into()),
                    Value::Integer(42), // not Text -> skip
                    Value::Text("x".into()),
                    Value::Integer(2),
                    Value::Text("CREATE TABLE x (id INT)".into()),
                ],
            )
            .unwrap();
    }
    let mut schema = Schema::new();
    schema.load_from_pager(&mut pager).unwrap();
    assert!(schema.tables.is_empty());
}

#[test]
fn test_load_from_pager_with_non_integer_rootpage() {
    // Insert a row where rootpage (row[3]) is not Integer
    let mut pager = Pager::open_memory();
    {
        let schema_root = pager.schema_root_page();
        let mut btree = crate::storage::btree::BTree::new(&mut pager);
        btree
            .insert(
                schema_root,
                100,
                &vec![
                    Value::Text("table".into()),
                    Value::Text("bad".into()),
                    Value::Text("bad".into()),
                    Value::Text("not_int".into()), // not Integer -> skip
                    Value::Text("CREATE TABLE bad (id INT)".into()),
                ],
            )
            .unwrap();
    }
    let mut schema = Schema::new();
    schema.load_from_pager(&mut pager).unwrap();
    assert!(schema.get_table("bad").is_err());
}

#[test]
fn test_load_from_pager_with_non_text_sql() {
    // Insert a row where sql (row[4]) is not Text
    let mut pager = Pager::open_memory();
    {
        let schema_root = pager.schema_root_page();
        let mut btree = crate::storage::btree::BTree::new(&mut pager);
        btree
            .insert(
                schema_root,
                100,
                &vec![
                    Value::Text("table".into()),
                    Value::Text("bad".into()),
                    Value::Text("bad".into()),
                    Value::Integer(2),
                    Value::Integer(0), // not Text -> skip
                ],
            )
            .unwrap();
    }
    let mut schema = Schema::new();
    schema.load_from_pager(&mut pager).unwrap();
    assert!(schema.get_table("bad").is_err());
}

#[test]
fn test_load_from_pager_unparseable_sql() {
    // Insert a valid schema row but with SQL that can't be parsed as CREATE TABLE
    let mut pager = Pager::open_memory();
    {
        let schema_root = pager.schema_root_page();
        let mut btree = crate::storage::btree::BTree::new(&mut pager);
        btree
            .insert(
                schema_root,
                100,
                &vec![
                    Value::Text("table".into()),
                    Value::Text("broken".into()),
                    Value::Text("broken".into()),
                    Value::Integer(2),
                    Value::Text("NOT VALID SQL AT ALL ~~~".into()),
                ],
            )
            .unwrap();
    }
    let mut schema = Schema::new();
    schema.load_from_pager(&mut pager).unwrap();
    // Should still be registered but with empty columns (fallback path)
    let table = schema.get_table("broken").unwrap();
    assert!(table.columns.is_empty());
    assert_eq!(table.next_rowid, 1);
}

#[test]
fn test_load_from_pager_index_object_type() {
    // Insert an "index" type row - should be ignored (only "table" is processed)
    let mut pager = Pager::open_memory();
    {
        let schema_root = pager.schema_root_page();
        let mut btree = crate::storage::btree::BTree::new(&mut pager);
        btree
            .insert(
                schema_root,
                100,
                &vec![
                    Value::Text("index".into()), // type = "index", not "table"
                    Value::Text("idx1".into()),
                    Value::Text("t1".into()),
                    Value::Integer(3),
                    Value::Text("CREATE INDEX idx1 ON t1 (col)".into()),
                ],
            )
            .unwrap();
    }
    let mut schema = Schema::new();
    schema.load_from_pager(&mut pager).unwrap();
    assert!(schema.tables.is_empty());
}

#[test]
fn test_load_from_pager_preserves_column_flags() {
    let (mut pager, mut schema) = setup();
    let cols = vec![
        ColumnDef {
            name: "id".into(),
            data_type: DataType::Integer,
            primary_key: true,
            autoincrement: true,
            not_null: true,
            unique: false,
            default: None,
            references: None,
        },
        ColumnDef {
            name: "val".into(),
            data_type: DataType::Real,
            primary_key: false,
            autoincrement: false,
            not_null: true,
            unique: false,
            default: None,
            references: None,
        },
    ];
    create_table_sp(&mut schema, &mut pager,
            "t1",
            &cols,
            false,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, val REAL NOT NULL)",
        )
        .unwrap();

    // Reload from pager and verify column properties preserved
    let mut schema2 = Schema::new();
    schema2.load_from_pager(&mut pager).unwrap();
    let table = schema2.get_table("t1").unwrap();
    assert_eq!(table.columns.len(), 2);
    assert!(table.columns[0].primary_key);
    assert!(table.columns[0].autoincrement);
    assert!(table.columns[0].not_null);
    assert_eq!(table.columns[1].data_type, DataType::Real);
    assert!(table.columns[1].not_null);
}

