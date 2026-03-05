# SQL Parser Refactor Analysis (sqlparser-rs)

Date: 2026-03-05
Status: Core migration complete; hardening in progress

## Goal

Replace KKDB custom SQL parser entrypoint with `sqlparser-rs` while preserving current internal AST and runtime behavior.

## Current State

1. `sqlparser = "0.61"` is added in `Cargo.toml`.
2. `parse_sql` entrypoint in `src/sql/parser.rs` routes to `sqlparser-rs` adapter.
3. Legacy handwritten parser logic is no longer used in runtime parse path.
4. Parser test suite (`src/sql/parser_tests.rs`) remains the compatibility baseline.

## Migration Scope

Target entrypoint:

- `crate::sql::parser::parse_sql(&str) -> Result<Statement>`

Keep internal AST unchanged:

- `src/sql/ast.rs`

Add adapter layer:

- `src/sql/sqlparser_adapter/`
- Parse with `SQLiteDialect`.
- Convert `sqlparser::ast` -> KKDB AST.

## Compatibility Requirements

Must keep behavior expected by existing tests:

1. Single-statement parsing contract.
2. `SELECT` without `FROM`.
3. Projection variants: `*`, `t.*`, expression + alias (explicit/implicit).
4. Joins: comma join, INNER/LEFT/RIGHT/CROSS, `ON`/`USING` (bounded).
5. Subquery forms:
   - scalar subquery `(SELECT ...)`
   - `IN (SELECT ...)`
   - `EXISTS (SELECT ...)`
6. Expression operators and precedence:
   - arithmetic, compare, logical, concat (`||`), modulo (`%`)
7. `COUNT(*)` internal mapping consistency.
8. DDL/DML/Txn statements:
   - `CREATE TABLE`, `DROP TABLE`, `ALTER TABLE`, `CREATE INDEX`
   - `INSERT`, `UPDATE`, `DELETE`
   - `BEGIN`, `COMMIT`, `ROLLBACK`
   - `EXPLAIN <statement>`

## Known Conversion Gaps To Implement

1. `sqlparser::Statement` to KKDB `Statement` conversion.
2. `Query` and `Select` conversion:
   - `group_by`, `order_by`, `limit`, `offset`
3. `TableWithJoins` / `TableFactor` conversion to KKDB `FromClause`.
4. `Expr` conversion coverage.
5. `DataType` and `ColumnOption` mapping for `CREATE TABLE`.
6. `ALTER TABLE` operation mapping.

## Read Log (Checkpoint For Restart)

The following files were inspected to lock down `sqlparser-rs 0.61` AST shapes:

1. `C:/Users/Admin/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sqlparser-0.61.0/src/ast/mod.rs`
2. `C:/Users/Admin/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sqlparser-0.61.0/src/ast/query.rs`
3. `C:/Users/Admin/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sqlparser-0.61.0/src/ast/dml.rs`
4. `C:/Users/Admin/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sqlparser-0.61.0/src/ast/ddl.rs`
5. `C:/Users/Admin/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sqlparser-0.61.0/src/ast/operator.rs`
6. `C:/Users/Admin/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sqlparser-0.61.0/src/ast/value.rs`

Confirmed key structures:

1. Statement variants:
   - `Statement::Query(Box<Query>)`
   - `Statement::Insert(Insert)`
   - `Statement::Update(Update)`
   - `Statement::Delete(Delete)`
   - `Statement::CreateTable(CreateTable)`
   - `Statement::CreateIndex(CreateIndex)`
   - `Statement::AlterTable(AlterTable)`
   - `Statement::Drop { object_type, if_exists, names, ... }`
   - `Statement::StartTransaction { .. }`
   - `Statement::Commit { .. }`
   - `Statement::Rollback { .. }`
   - `Statement::Explain { statement, .. }`
2. Query/Select:
   - `Query { body, order_by, limit_clause, .. }`
   - `SetExpr::{Select, Query, Values, ...}`
   - `Select { projection, from, selection, group_by, having, distinct, .. }`
3. FROM/JOIN:
   - `TableWithJoins { relation, joins }`
   - `TableFactor::{Table, Derived, TableFunction, NestedJoin, ...}`
   - `JoinOperator::{Join, Inner, Left, LeftOuter, Right, RightOuter, CrossJoin, ...}`
   - `JoinConstraint::{On, Using, Natural, None}`
4. Expressions:
   - `Expr::{Identifier, CompoundIdentifier, Value, UnaryOp, BinaryOp, IsNull, IsNotNull, InList, InSubquery, Between, Like, ILike, Exists, Subquery, Nested, Function, ...}`
   - `Value` literals include `Number`, `SingleQuotedString`, `HexStringLiteral`, `Boolean`, `Null`.
5. Functions:
   - `Function { name, args: FunctionArguments, .. }`
   - `FunctionArguments::{None, Subquery, List(FunctionArgumentList)}`
   - `FunctionArgExpr::{Expr, QualifiedWildcard, Wildcard}`
6. DDL details:
   - `CreateTable { name, columns, constraints, if_not_exists, ... }`
   - `ColumnDef { name, data_type, options }`
   - `ColumnOption::{NotNull, Unique{..}, Default(_), DialectSpecific(_), ...}`
   - `CreateIndex { name, table_name, columns, unique, if_not_exists, ... }`
   - `AlterTable { name, operations, ... }`
   - `AlterTableOperation::{AddColumn, DropColumn, RenameTable, RenameColumn, ...}`
7. Transaction/Explain fields:
   - `StartTransaction { begin, transaction, modes, ... }`
   - `Commit { chain, end, modifier }`
   - `Rollback { chain, savepoint }`
   - `Explain { describe_alias, analyze, verbose, statement, ... }`

Adapter design decisions already fixed:

1. Keep KKDB internal AST unchanged (`src/sql/ast.rs`).
2. Parse dialect: `SQLiteDialect`.
3. Single statement only at entrypoint.
4. Preserve `COUNT(*)` behavior by mapping wildcard arg to `IntegerLiteral(1)`.

## Risks

1. Dialect parse differences (legacy permissive syntax vs `sqlparser-rs` rules).
2. Alias edge-cases (`SELECT a b`) and table wildcard handling.
3. Function argument shape differences (`FunctionArg` model).
4. Test regressions in parser suite and downstream schema/vm flows.

## Execution Plan

1. Implement `sqlparser_adapter.rs` for all required statement and expression mappings.
2. Wire `parse_sql` entrypoint to adapter.
3. Run parser tests and fix conversion gaps iteratively.
4. Run full `scripts/check.ps1`.
5. After all green, decide whether to remove legacy parser internals or keep as temporary fallback.

## Acceptance Criteria

1. Parser tests pass (or only explicitly approved deltas remain).
2. No compile errors in schema/vm/storage layers due to AST mismatch.
3. Existing SQL execution path remains stable under integration checks.

## Implementation Progress (2026-03-05)

Completed:

1. `parse_sql` entrypoint now routes to `sqlparser-rs` adapter.
2. Adapter refactored into multiple files:
   - `src/sql/sqlparser_adapter/mod.rs`
   - `src/sql/sqlparser_adapter/common.rs`
   - `src/sql/sqlparser_adapter/statement.rs`
   - `src/sql/sqlparser_adapter/query.rs`
   - `src/sql/sqlparser_adapter/expr.rs`
3. Statement conversion implemented for:
   - `SELECT/INSERT/UPDATE/DELETE`
   - `CREATE TABLE/DROP TABLE/CREATE INDEX/ALTER TABLE`
   - `BEGIN/COMMIT/ROLLBACK`
   - `EXPLAIN <statement>`
4. Expression conversion implemented for core SQL subset, including:
   - arithmetic/comparison/logical operators
   - `IS NULL`, `IN`, `BETWEEN`, `LIKE`
   - subquery forms (`Subquery`, `IN (SELECT)`, `EXISTS`)
   - function calls with `DISTINCT`
   - compatibility remap for `COUNT(*)`
   - compatibility remap for parser-native nodes:
     - `SUBSTR/SUBSTRING`
     - `TRIM`
5. FROM/JOIN conversion implemented for table, subquery, nested join, and comma-cross-join expansion.

Validation snapshot:

1. Parser suite:
   - `cargo test sql::parser::tests:: -- --nocapture`
   - Result: `168 passed, 0 failed`
2. Full test suite:
   - `cargo test -j 1`
   - Result: `642 passed, 0 failed`
3. Project gate:
   - `./scripts/check.ps1` (`fmt + clippy + test`)
   - Result: PASS

## Hardening Progress (2026-03-05, follow-up)

Completed:

1. Removed test-only AST re-export coupling from `src/sql/parser.rs`.
2. Updated `src/sql/parser_tests.rs` to explicit imports (`ast`, `parse_sql`, `DataType`).
3. Added boundary regression tests for adapter-level behavior:
   - multi-statement rejection
   - `WITH` unsupported mapping
   - `JOIN USING` rewrite mapping (`USING` -> `ON` equality predicates)
   - `JOIN USING` on join trees is supported with directional/merged key rewrite:
     - `INNER`: `COALESCE(left_key, right_key)`
     - `LEFT`: left key
     - `RIGHT`: right key
   - `NATURAL JOIN` unsupported mapping
   - `ROLLBACK TO SAVEPOINT` unsupported mapping
4. Added VM regression test for runtime behavior of `JOIN ... USING (...)`.
5. Added parse/runtime coverage for `JOIN USING` on LEFT/RIGHT join-tree sides.
6. Added parse/runtime coverage for `JOIN USING` on CROSS-side join trees (`COALESCE` rewrite).

Validation snapshot:

1. Parser suite:
   - `cargo test sql::parser::tests:: -j 1`
   - Result: PASS
2. Full test suite:
   - `cargo test -j 1`
   - Result: `654 passed, 0 failed`
3. Project gate:
   - `./scripts/check.ps1` (`fmt + clippy + test`)
   - Result: PASS

## Remaining Work Queue

1. Expand supported SQL surface intentionally (if desired), e.g. selected currently-unsupported clauses.
2. Decide whether to keep `src/sql/tokenizer.rs` long-term (still used by tokenizer tests but no longer on main parse path).
3. Continue validating `JOIN USING` semantics on deeper mixed join trees (INNER/LEFT/RIGHT/CROSS nesting).
4. Keep adapter-specific behavior documented whenever unsupported clauses are promoted to supported ones.
