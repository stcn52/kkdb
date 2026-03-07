# KKDB 内置函数参考手册

本文档列出 KKDB SQL 引擎支持的所有内置标量函数、聚合函数及操作符扩展。

---

## 目录

1. [聚合函数](#1-聚合函数)
2. [数学函数](#2-数学函数)
3. [字符串函数](#3-字符串函数)
4. [类型与控制函数](#4-类型与控制函数)
5. [日期与时间函数](#5-日期与时间函数)
6. [JSON 函数](#6-json-函数)
7. [全文检索函数](#7-全文检索函数)
8. [系统函数](#8-系统函数)
9. [操作符扩展](#9-操作符扩展)

---

## 1. 聚合函数

| 函数 | 说明 |
|------|------|
| `COUNT(expr)` / `COUNT(*)` | 行计数，支持 `DISTINCT` |
| `SUM(expr)` | 求和（NULL 行忽略）|
| `AVG(expr)` | 平均值 |
| `MIN(expr)` | 最小值 |
| `MAX(expr)` | 最大值 |
| `GROUP_CONCAT(expr [, sep])` | 拼接字符串，默认分隔符 `,` |

所有聚合函数均可作为**窗口函数**，与 `OVER (PARTITION BY ... ORDER BY ...)` 组合使用。

---

## 2. 数学函数

| 函数 | 返回类型 | 说明 |
|------|----------|------|
| `ABS(x)` | 同输入 | 绝对值，整数/实数均支持（整数溢出绕回）|
| `ROUND(x [, d])` | REAL | 四舍五入到 `d` 位小数（默认 0）|
| `CEIL(x)` / `CEILING(x)` | REAL | 向上取整 |
| `FLOOR(x)` | REAL | 向下取整 |
| `SIGN(x)` | 同输入 | 符号函数：-1 / 0 / 1 |
| `POWER(base, exp)` / `POW(base, exp)` | INTEGER or REAL | 幂运算；整数正指数溢出时自动晋级为 REAL |
| `CBRT(x)` | REAL | 立方根 |
| `FACTORIAL(n)` | INTEGER | 阶乘，`n` 须为 0-20 之间的非负整数（21! 溢出 i64）|
| `SQRT(x)` | REAL | 平方根 |
| `MOD(x, y)` | 同输入 | 取模（`%` 运算符等价）|

---

## 3. 字符串函数

| 函数 | 说明 |
|------|------|
| `UPPER(s)` | 转大写 |
| `LOWER(s)` | 转小写 |
| `LENGTH(s)` | Unicode 字符数（**非字节数**）；BLOB 返回字节数 |
| `SUBSTR(s, start [, len])` / `SUBSTRING(s, start [, len])` | 子串（start 从 1 开始，Unicode 安全）|
| `INSTR(haystack, needle)` | 返回 needle 在 haystack 中第一次出现的位置（1-indexed，未找到返回 0）|
| `REPLACE(s, from, to)` | 全量字符串替换 |
| `TRIM(s [, chars])` | 去两端空白或指定字符集 |
| `LTRIM(s [, chars])` | 去左侧空白或指定字符集 |
| `RTRIM(s [, chars])` | 去右侧空白或指定字符集 |
| `OVERLAY(s, placing, from [, for])` | SQL 标准 OVERLAY：在指定位置用 placing 替换 for 个字符 |
| `CONCAT(s1, s2, ...)` | 字符串拼接（等同 `\|\|` 运算符）|
| `REPEAT(s, n)` | 将字符串 `s` 重复 `n` 次 |
| `LPAD(s, len [, pad])` | 左填充到指定长度 |
| `RPAD(s, len [, pad])` | 右填充到指定长度 |
| `REVERSE(s)` | 字符串逆序（Unicode 安全）|
| `CHAR_LENGTH(s)` | 同 `LENGTH(s)` |
| `SPLIT_PART(s, delim, n)` | 按分隔符拆分，返回第 n 部分 |
| `REGEXP_REPLACE(s, pattern, repl)` | 正则替换（基础 POSIX ERE 子集）|

### LIKE / ILIKE / SIMILAR TO

```sql
-- LIKE（大小写敏感）
SELECT * FROM t WHERE name LIKE 'J%';

-- ILIKE（大小写不敏感）
SELECT * FROM t WHERE name ILIKE 'j%';

-- ESCAPE 指定转义字符
SELECT * FROM t WHERE path LIKE '%\_%' ESCAPE '\';
```

---

## 4. 类型与控制函数

| 函数 | 说明 |
|------|------|
| `TYPEOF(x)` | 返回值类型字符串：`'null'` / `'integer'` / `'real'` / `'text'` / `'blob'` |
| `CAST(x AS type)` | 显式类型转换（supported types: INTEGER, REAL, TEXT, BLOB）|
| `COALESCE(x1, x2, ...)` | 返回第一个非 NULL 参数 |
| `IFNULL(x, default)` | `x` 为 NULL 时返回 `default`（`COALESCE` 双参数简写）|
| `NULLIF(a, b)` | `a = b` 时返回 NULL，否则返回 `a` |
| `IIF(cond, true_val, false_val)` | 条件表达式（等同 `CASE WHEN cond THEN ... ELSE ... END`）|
| `CASE WHEN ... THEN ... ELSE ... END` | 多分支条件 |

### NULL 安全比较操作符

| 语法 | 含义 |
|------|------|
| `a IS DISTINCT FROM b` | 包含 NULL 感知的不等于 |
| `a IS NOT DISTINCT FROM b` | 包含 NULL 感知的等于 |

---

## 5. 日期与时间函数

| 函数 | 说明 |
|------|------|
| `NOW()` | 当前时间戳（Unix epoch 秒数）|
| `DATE(ts)` | 将时间戳格式化为 `YYYY-MM-DD` 字符串 |
| `STRFTIME(fmt, ts)` | 按 strftime 格式格式化时间戳 |
| `UNIXEPOCH()` | 同 `NOW()`，返回 Unix 时间戳 |

---

## 6. JSON 函数

KKDB 内置轻量级 JSON 处理函数（无外部依赖，纯 Rust 实现）。

| 函数 | 说明 |
|------|------|
| `JSON_EXTRACT(json, path)` / `JSON_EXTRACT_TEXT(json, path)` | 提取 JSON 路径值（支持 `$.key` 和 `key` 两种格式）|
| `JSON_OBJECT(k1, v1, ...)` | 构建 JSON 对象 |
| `JSON_ARRAY(v1, v2, ...)` | 构建 JSON 数组 |
| `JSON_TYPE(json)` | 返回 JSON 值类型：`OBJECT` / `ARRAY` / `BOOLEAN` / `INTEGER` / `DOUBLE` / `STRING` |
| `JSON_VALID(json)` | JSON 格式是否合法（1/0）|
| `JSON_LENGTH(json [, path])` | 对象键数或数组元素数 |
| `JSON_KEYS(json)` | 返回对象的键列表（JSON 数组格式）|
| `JSON_CONTAINS(json, val [, path])` | 是否包含值（支持标量和数组元素查找）|
| `JSON_REMOVE(json, path, ...)` | 删除路径对应的键 |
| `JSON_SET(json, path, val, ...)` / `JSON_INSERT` / `JSON_REPLACE` | 设置/插入/替换路径对应的值 |
| `JSON_MEMBER_OF(val, json_array)` | val 是否是 json_array 的成员 |

**示例：**

```sql
-- 提取嵌套字段
SELECT JSON_EXTRACT('{"user":{"name":"Alice","age":30}}', '$.user.name');
-- 返回 'Alice'

-- 构建 JSON 对象
SELECT JSON_OBJECT('id', 1, 'name', 'Bob');
-- 返回 '{"id": 1, "name": "Bob"}'

-- 类型检查
SELECT JSON_TYPE('[1,2,3]');
-- 返回 'ARRAY'

-- 数组成员检测
SELECT JSON_MEMBER_OF(2, '[1,2,3]');
-- 返回 1
```

---

## 7. 全文检索函数

> 详细文档见 [ADVANCED_SQL.md §12](ADVANCED_SQL.md#12-全文检索bm25)

| 函数 | 说明 |
|------|------|
| `FTS_MATCH(table, index, query)` | BM25 相关度评分（`WHERE` 子句过滤配合 `ORDER BY` 排序）|

**中文分词**：KKDB 集成 `jieba-rs 0.7` 中文分词，以及 Unicode 感知的 `Whitespace + Lowercase` 基础分词器，均由 `tokenizers 0.22.2`（Hugging Face）驱动，可通过 JSON 配置加载 BPE / WordPiece 分词器。

---

## 8. 系统函数

| 函数 | 说明 |
|------|------|
| `LAST_INSERT_ROWID()` | 当前会话最后一次 INSERT 的 rowid |
| `RANDOM()` | 返回随机 64 位整数 |
| `HEX(blob)` | BLOB → 十六进制字符串 |
| `UNHEX(s)` | 十六进制字符串 → BLOB |
| `ZEROBLOB(n)` | 返回长度为 n 的全零 BLOB |
| `CHANGES()` | 最后一条 DML 影响的行数 |
| `CURRENT_USER` | 当前 RLS 用户名（通过 `SET kkdb.current_user = 'alice'` 设置）|

---

## 9. 操作符扩展

### 位运算（INTEGER 类型）

| 运算符 | 说明 |
|--------|------|
| `a & b` | 按位与 |
| `a \| b` | 按位或 |
| `a ^ b` _(XOR)_ | 按位异或（`BinaryOp::BitwiseXor`）|
| `a << n` | 左移 |
| `a >> n` | 右移 |
| `~a` / `BITWISE_NOT(a)` | 按位取反 |

### 字符串操作符

| 运算符 | 说明 |
|--------|------|
| `s1 \|\| s2` | 字符串拼接 |
| `s LIKE pattern` | 通配符匹配（`%` 任意序列，`_` 单字符）|
| `s ILIKE pattern` | 大小写不敏感 LIKE |
| `s SIMILAR TO pattern` | POSIX 正则（基础子集）|
| `s ~ pattern` | 正则匹配（ERE 基础，`.` `.*` `^` `$`）|

### 范围操作符

```sql
-- BETWEEN...AND（闭区间，NULL 安全）
WHERE price BETWEEN 10 AND 100

-- IN / NOT IN
WHERE status IN ('pending', 'processing')

-- ANY / ALL（配合子查询）
WHERE salary > ALL (SELECT avg_salary FROM departments)
```

### UNNEST

```sql
-- 展开 JSON 数组为行
SELECT * FROM UNNEST('[1,2,3]') AS t(val);

-- 展开逗号分隔字符串
SELECT * FROM UNNEST('a,b,c') AS t(val);
```

---

## 函数实现位置

| 类别 | 实现文件 |
|------|---------|
| 标量函数 | `src/vm/eval_expr.rs` (`VM::eval_expr`) |
| 聚合函数 | `src/vm/exec_select.rs` (`VM::apply_group_by`) |
| 窗口函数 | `src/vm/exec_select.rs` (`VM::project_columns`) |
| JSON 辅助函数 | `src/vm/eval_expr.rs`（模块底部：`json_extract_primitive`, `json_keys`, ...）|
| LIKE 匹配 | `src/vm/eval_expr.rs` (`like_match`)，Unicode 字符级精确匹配 |
| FTS 评分 | `src/fulltext/index.rs` (`bm25_score`) |

---

## 参考资料

- [ADVANCED_SQL.md](ADVANCED_SQL.md) — 高阶 SQL 完整示例
- [PROJECT.md](PROJECT.md) — 整体架构与模块说明
- [API.md](API.md) — HTTP REST API 参考
