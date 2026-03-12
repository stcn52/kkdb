use super::*;

#[test]
fn test_param_insert_or_replace_with_special_chars() {
    let mut vm = VM::new_memory();
    vm.execute_sql(
        "CREATE TABLE skills (name TEXT PRIMARY KEY, desc TEXT, tags TEXT, prompts TEXT)",
    )
    .unwrap();

    let result = vm.execute_params(
        "INSERT OR REPLACE INTO skills (name, desc, tags, prompts) VALUES (?, ?, ?, ?)",
        &[
            Value::Text("langchain".into()),
            Value::Text("LangChain AI framework".into()),
            Value::Text("llm agent python".into()),
            Value::Text("# LangChain\n\nBuild LLM apps.\n\n| A | B |\n|---|---|\n| 1 | 2 |".into()),
        ],
    );
    assert!(
        result.is_ok(),
        "execute_params INSERT OR REPLACE failed: {:?}",
        result
    );

    let rows = match vm.execute_sql("SELECT name, prompts FROM skills").unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("langchain".into()));
}

#[test]
fn test_param_insert_or_replace_dedup() {
    let mut vm = VM::new_memory();
    vm.execute_sql(
        "CREATE TABLE skills (name TEXT PRIMARY KEY, desc TEXT, tags TEXT, prompts TEXT)",
    )
    .unwrap();

    let sql = "INSERT OR REPLACE INTO skills (name, desc, tags, prompts) VALUES (?, ?, ?, ?)";

    vm.execute_params(
        sql,
        &[
            Value::Text("langchain".into()),
            Value::Text("v1 description".into()),
            Value::Text("python".into()),
            Value::Text("prompt v1".into()),
        ],
    )
    .unwrap();

    vm.execute_params(
        sql,
        &[
            Value::Text("langchain".into()),
            Value::Text("v2 description".into()),
            Value::Text("python llm".into()),
            Value::Text("# Heading\n\nprompt v2\n\n| Col |\n|-----|\n| val |".into()),
        ],
    )
    .unwrap();

    let rows = match vm.execute_sql("SELECT desc, prompts FROM skills").unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("{:?}", other),
    };
    assert_eq!(rows.len(), 1, "should have exactly one row after replace");
    assert_eq!(rows[0][0], Value::Text("v2 description".into()));
    assert!(rows[0][1].to_string().contains("Heading"));
}

#[test]
fn test_param_insert_or_replace_with_fts() {
    let mut vm = VM::new_memory();
    vm.execute_sql(
        "CREATE TABLE skills (name TEXT PRIMARY KEY, desc TEXT, tags TEXT, prompts TEXT)",
    )
    .unwrap();
    vm.execute_sql("CREATE FULLTEXT INDEX idx_skills_fts ON skills (desc, tags, prompts)")
        .unwrap();

    // Goal: INSERT OR REPLACE with ? placeholders should succeed even when a
    // FULLTEXT INDEX exists (exercises the FTS pending-insert path with placeholder values).
    let result = vm.execute_params(
        "INSERT OR REPLACE INTO skills (name, desc, tags, prompts) VALUES (?, ?, ?, ?)",
        &[
            Value::Text("langchain".into()),
            Value::Text("LangChain AI framework".into()),
            Value::Text("llm agent python".into()),
            Value::Text("# LangChain\n\nBuild LLM apps.\n\n| A | B |\n|---|---|\n| 1 | 2 |".into()),
        ],
    );
    assert!(
        result.is_ok(),
        "INSERT OR REPLACE + FTS failed: {:?}",
        result
    );

    // Verify row is actually in the table
    let rows = match vm.execute_sql("SELECT name FROM skills").unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("{:?}", other),
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("langchain".into()));
}
