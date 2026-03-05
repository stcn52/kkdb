# KKDB API 文档

本文档聚焦 `kkdb` crate 的公开 API 与推荐调用方式。

## 1. 模块总览

`src/lib.rs` 暴露如下模块：

- `kkdb::error`
- `kkdb::schema`
- `kkdb::sql`
- `kkdb::storage`
- `kkdb::types`
- `kkdb::vm`

推荐应用层优先使用：

- `kkdb::vm::execute::VM`
- `kkdb::vm::execute::ExecResult`
- `kkdb::error::{KkdbError, Result}`

## 2. 快速开始（推荐入口）

```rust
use kkdb::vm::execute::{ExecResult, VM};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = VM::new_memory();

    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")?;
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")?;

    let result = vm.execute_sql("SELECT id, name FROM t1")?;
    match result {
        ExecResult::QueryResult { columns, rows } => {
            println!("{:?}", columns);
            println!("{:?}", rows);
        }
        _ => {}
    }
    Ok(())
}
```

## 3. `vm::execute`

路径：`kkdb::vm::execute`

### 3.1 `struct VM`

核心数据库执行器。

公开方法：

- `VM::new_memory() -> VM`
- `VM::open(path: &str) -> Result<VM>`
- `VM::execute_sql(&mut self, sql: &str) -> Result<ExecResult>`

说明：

- `execute_sql` 一次执行一条语句（可带结尾 `;`）。
- 文件库模式会在打开时加载 schema。
- 事务语句（`BEGIN/COMMIT/ROLLBACK`）也通过 `execute_sql` 执行。

### 3.2 `enum ExecResult`

SQL 执行结果：

- `Ok { message: String }`：DDL / 事务等成功消息
- `RowsAffected { count: usize, message: String }`：INSERT/UPDATE/DELETE
- `QueryResult { columns: Vec<String>, rows: Vec<Vec<Value>> }`：SELECT 查询结果
- `Explain { plan: String }`：EXPLAIN 文本计划

## 4. `error`

路径：`kkdb::error`

- `type Result<T> = std::result::Result<T, KkdbError>`
- `enum KkdbError`：统一错误类型

常见错误：

- `SyntaxError` / `ParseError`
- `TableNotFound` / `ColumnNotFound`
- `ConstraintViolation`
- `RuntimeError`
- `Io` / `CorruptDatabase`

## 5. `types`

路径：`kkdb::types`

主要类型：

- `enum DataType`
- `enum Value`
- `type Row = Vec<Value>`

常用 API：

- `Value::to_i64()`
- `Value::to_f64()`
- `Value::is_truthy()`
- `serialize_row(row: &Row)`
- `deserialize_row(data: &[u8])`

`Value` 变体：

- `Null`
- `Integer(i64)`
- `Real(f64)`
- `Text(Rc<str>)`
- `Blob(Vec<u8>)`

## 6. `sql`

路径：`kkdb::sql`

子模块：

- `ast`：SQL AST 结构定义（`Statement`、`Expr` 等）
- `tokenizer`：词法分析器（`Tokenizer`、`Token`）
- `parser`：语法分析器（`Parser`、`parse_sql`）

常用函数：

- `parse_sql(sql: &str) -> Result<Statement>`

适用场景：

- 仅需解析 SQL（不执行）时可直接使用 parser/tokenizer。

## 7. `schema`

路径：`kkdb::schema`

主要类型：

- `Schema`
- `TableSchema`
- `ColumnInfo`
- `IndexSchema`

主要公开方法（偏底层）：

- `Schema::new()`
- `Schema::load_from_pager(...)`
- `Schema::create_table(...)`
- `Schema::drop_table(...)`
- `Schema::create_index(...)`
- `Schema::get_table(...)`
- `Schema::get_table_mut(...)`
- `Schema::find_column(...)`
- `Schema::alter_add_column(...)`
- `Schema::alter_drop_column(...)`
- `Schema::alter_rename_table(...)`
- `Schema::alter_rename_column(...)`

说明：

- 该层更接近引擎内部实现，业务应用建议优先通过 `VM` 间接使用。

## 8. `storage`

路径：`kkdb::storage`

子模块与主要类型：

- `pager::Pager`：页缓存、文件读写、事务快照
- `btree::BTree`：表/索引 B-Tree 操作
- `cursor::Cursor`：顺序遍历游标

### 8.1 `Pager` 常用方法

- `Pager::open(path)`
- `Pager::open_memory()`
- `get_page/get_page_mut`
- `allocate_page`
- `flush`
- `begin_transaction/commit_transaction/rollback_transaction`

### 8.2 `BTree` 常用方法

- `create_table`
- `insert / insert_with_buf`
- `scan_all / scan_rows / scan_rows_limit`
- `find_by_rowid`
- `update_row / update_row_with_buf`
- `delete_by_rowid`
- `max_rowid / count_rows`

### 8.3 `Cursor` 常用方法

- `Cursor::table_start`
- `current`
- `advance`

## 9. API 设计建议

建议分层使用：

1. 应用层：只依赖 `VM + ExecResult + KkdbError`
2. 工具层：需要 SQL 分析时使用 `sql::parser`
3. 内核扩展层：谨慎使用 `schema/storage`（与内部实现耦合更高）

## 10. 版本与兼容性

当前版本：`0.1.0`

由于项目仍处于早期阶段，低层模块（尤其 `schema/storage`）可能随优化迭代调整。若追求调用稳定性，请优先使用 `VM` 作为边界 API。
