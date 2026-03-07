# KKDB 内置函数参考手册

本文档列出 KKDB SQL 引擎支持的所有内置标量函数、聚合函数及相关语法。  
所有内容均依据源码 `src/vm/eval_expr.rs`、`src/sql/ast.rs`、`src/vm/exec_select.rs` 逐行审计得出。

---

## 目录

1. [聚合函数](#1-聚合函数)
2. [窗口函数](#2-窗口函数)
3. [数学函数](#3-数学函数)
4. [字符串函数](#4-字符串函数)
5. [类型转换与控制函数](#5-类型转换与控制函数)
6. [日期与时间](#6-日期与时间)
7. [JSON 函数](#7-json-函数)
8. [系统与会话函数](#8-系统与会话函数)
9. [全文检索函数](#9-全文检索函数)
10. [操作符扩展](#10-操作符扩展)
11. [FROM 子句表值函数](#11-from-子句表值函数)
12. [JOIN 类型完整列表](#12-join-类型完整列表)

---

## 1. 聚合函数

| 函数 | 说明 |
|------|------|
| `COUNT(expr)` / `COUNT(*)` | 行计数，支持 `COUNT(DISTINCT col)` |
| `SUM(expr)` | 求和（NULL 行忽略）|
| `AVG(expr)` | 平均值（NULL 行忽略）|
| `MIN(expr)` | 最小值 |
| `MAX(expr)` | 最大值 |
| `GROUP_CONCAT(expr [, sep])` | 字符串拼接，默认分隔符 `,` |

> 所有聚合函数均可用作**窗口函数**，详见 §2。

---

## 2. 窗口函数

语法：`func() OVER ([PARTITION BY ...] [ORDER BY ...] [frame])`

| 函数 | 说明 |
|------|------|
| `ROW_NUMBER()` | 组内连续行号（无并列）|
| `RANK()` | 组内排名（并列跳号）|
| `DENSE_RANK()` | 组内排名（并列不跳号）|
| `PERCENT_RANK()` | 百分比排名 `(rank-1)/(rows-1)` |
| `CUME_DIST()` | 累积分布 `rank/rows` |
| `NTILE(n)` | 将行分为 n 份，返回桶编号 |
| `LAG(expr [, offset [, default]])` | 向前（上方）偏移访问 |
| `LEAD(expr [, offset [, default]])` | 向后（下方）偏移访问 |
| `FIRST_VALUE(expr)` | 窗口帧内第一行的值 |
| `LAST_VALUE(expr)` | 窗口帧内最后一行的值 |
| `NTH_VALUE(expr, n)` | 窗口帧内第 n 行的值 |
| `SUM/AVG/MIN/MAX/COUNT OVER (...)` | 聚合函数作窗口函数 |

**帧单位**：`ROWS` / `RANGE` / `GROUPS`  
**帧边界**：`UNBOUNDED PRECEDING` / `N PRECEDING` / `CURRENT ROW` / `N FOLLOWING` / `UNBOUNDED FOLLOWING`

**命名窗口（WINDOW 子句）**：
```sql
SELECT salary,
       AVG(salary) OVER w1 AS dept_avg
FROM employees
WINDOW w1 AS (PARTITION BY dept ORDER BY hire_date ROWS UNBOUNDED PRECEDING);
```

---

## 3. 数学函数

| 函数 | 说明 |
|------|------|
| `ABS(x)` | 绝对值（整数溢出绕回）|
| `ROUND(x [, d])` | 四舍五入到 d 位（默认 0）|
| `CEIL(x)` / `CEILING(x)` | 向上取整 |
| `FLOOR(x)` | 向下取整 |
| `SIGN(x)` | 符号：-1 / 0 / 1 |
| `POWER(base, exp)` / `POW(base, exp)` | 幂运算；整数溢出自动晋级为 REAL |
| `CBRT(x)` | 立方根 → REAL |
| `FACTORIAL(n)` | 阶乘，n ∈ [0, 20]（21! 溢出 i64 报错）|
| `SQRT(x)` | 平方根 → REAL |
| `MOD(x,y)` | 取模（等价 `%`）|

---

## 4. 字符串函数

| 函数 | 说明 |
|------|------|
| `UPPER(s)` | 转大写 |
| `LOWER(s)` | 转小写 |
| `LENGTH(s)` / `CHAR_LENGTH(s)` | Unicode 字符数（BLOB 返回字节数）|
| `SUBSTR(s, start [, len])` / `SUBSTRING(s, start [, len])` | 子串，start 从 1 开始，Unicode 安全 |
| `INSTR(haystack, needle)` | 首次出现位置（1-indexed，未找到返回 0）|
| `REPLACE(s, from, to)` | 全量替换 |
| `TRIM(s [, chars])` | 去两端空白或指定字符集 |
| `LTRIM(s [, chars])` | 去左侧 |
| `RTRIM(s [, chars])` | 去右侧 |
| `OVERLAY(s, placing, from [, for])` | SQL OVERLAY：按位置替换子串 |
| `STARTS_WITH(s, prefix)` | 是否以 prefix 开头（1/0）|
| `CONCAT(s1, s2, ...)` | 拼接（等同 `\|\|`）|
| `LPAD(s, len [, pad])` | 左填充 |
| `RPAD(s, len [, pad])` | 右填充 |
| `REVERSE(s)` | 逆序（Unicode 安全）|
| `SPLIT_PART(s, delim, n)` | 按分隔符拆分取第 n 段 |
| `REGEXP_LIKE(s, pattern)` | 正则匹配（基础 ERE 子集：`.` `.*` `^` `$` 锚）|
| `REGEXP_REPLACE(s, pattern, repl)` | 正则替换 |
| `UNICODE(s)` | 返回首字符的 Unicode 码点（INTEGER）|
| `CHAR(n, ...)` | Unicode 码点 → 字符串 |
| `HEX(x)` | BLOB/INTEGER → 大写十六进制字符串 |
| `UNHEX(s)` | 十六进制字符串 → BLOB |

**LIKE / ILIKE / SIMILAR TO：**
```sql
s LIKE '%foo%'              -- 大小写敏感
s ILIKE '%foo%'             -- 大小写不敏感
s LIKE '%\_%' ESCAPE '\'   -- 自定义转义字符
```
`%` 匹配任意字符序列，`_` 匹配任意单个 Unicode 字符，Unicode 字符级精确匹配。

---

## 5. 类型转换与控制函数

| 函数 / 语法 | 说明 |
|-------------|------|
| `CAST(x AS type)` | 类型转换。支持：`INTEGER` `REAL` `NUMERIC` `TEXT` `BLOB` `DATE` `TIME` `TIMESTAMP` `JSON` |
| `TRY_CAST(x AS type)` | 转换失败返回 NULL（而非报错）|
| `TYPEOF(x)` | 返回类型字符串：`null` / `integer` / `real` / `text` / `blob` |
| `COALESCE(x1, x2, ...)` | 返回首个非 NULL 值 |
| `IFNULL(x, default)` | x 为 NULL 时返回 default |
| `NULLIF(a, b)` | a = b 时返回 NULL，否则返回 a |
| `IIF(cond, t, f)` | 条件表达式（同 `CASE WHEN cond THEN t ELSE f END`）|
| `CASE WHEN ... THEN ... ELSE ... END` | 多分支条件（简单 CASE 和搜索 CASE 均支持）|

**NULL 安全比较：**
| 语法 | 等价于 |
|------|--------|
| `a IS DISTINCT FROM b` | `NOT (a = b OR (a IS NULL AND b IS NULL))` |
| `a IS NOT DISTINCT FROM b` | `a = b OR (a IS NULL AND b IS NULL)` |

**INTERVAL 语法：**
```sql
INTERVAL '1' DAY
INTERVAL '3' MONTH
-- 存储为 Text "1 DAY"，可与日期字段拼接使用
```

---

## 6. 日期与时间

| 函数 | 说明 |
|------|------|
| `NOW()` / `UNIXEPOCH()` | Unix 时间戳（秒，INTEGER）|
| `DATE(ts)` | 时间戳 → `YYYY-MM-DD` 字符串 |
| `STRFTIME(fmt, ts)` | strftime 格式化 |
| `DATE_EXTRACT(field, ts)` / `EXTRACT(field FROM ts)` | 提取日期字段（YEAR/MONTH/DAY/HOUR/MINUTE/SECOND）；INTEGER 输入视为 Unix epoch（秒）|

---

## 7. JSON 函数

KKDB 内置零依赖 JSON 处理（纯 Rust 实现）。

| 函数 | 说明 |
|------|------|
| `JSON_EXTRACT(json, path)` / `JSON_EXTRACT_TEXT(json, path)` | 提取路径值（支持 `$.key` 和裸 `key`），自动类型转换 |
| `JSON_OBJECT(k1, v1, ...)` | 构建 JSON 对象 |
| `JSON_ARRAY(v1, v2, ...)` | 构建 JSON 数组 |
| `JSON_TYPE(json)` | 返回 JSON 值类型：`OBJECT` / `ARRAY` / `BOOLEAN` / `INTEGER` / `DOUBLE` / `STRING` |
| `JSON_VALID(json)` | 是否合法 JSON（1/0）|
| `JSON_LENGTH(json [, path])` | 对象键数或数组元素数 |
| `JSON_KEYS(json)` | 返回对象键的 JSON 数组 |
| `JSON_CONTAINS(json, val [, path])` | 是否包含值（标量查找 + 数组成员）|
| `JSON_MEMBER_OF(val, json_array)` | val 是否是 json_array 的成员 |
| `JSON_REMOVE(json, path, ...)` | 删除路径 |
| `JSON_SET(json, path, val, ...)` | 设置/插入/替换路径值 |
| `JSON_INSERT(json, path, val, ...)` | 仅插入（不覆盖已有键）|
| `JSON_REPLACE(json, path, val, ...)` | 仅替换（已有键才生效）|
| `JSON_QUOTE(val)` | 将值序列化为 JSON 字符串（加双引号转义）|
| `JSON_UNQUOTE(json_str)` | 去除 JSON 字符串的双引号 |

**示例：**
```sql
SELECT JSON_EXTRACT('{"user":{"name":"Alice"}}', '$.user.name');  -- 'Alice'
SELECT JSON_ARRAY(1, 'hello', NULL);                               -- '[1, "hello", null]'
SELECT JSON_MEMBER_OF(2, '[1,2,3]');                               -- 1
SELECT JSON_UNQUOTE('"hello"');                                    -- hello
```

---

## 8. 系统与会话函数

| 函数 | 说明 |
|------|------|
| `LAST_INSERT_ROWID()` | 当前会话最后一次 INSERT 的 rowid |
| `RANDOM()` / `RAND()` | 随机 64 位整数（XorShift64 + 时间 nanos，避免同毫秒碰撞）|
| `CHANGES()` | 最后一条 DML 影响的行数 |
| `ZEROBLOB(n)` | 长度 n 的全零 BLOB |

**RLS / 认证函数（Supabase 风格）：**

| 函数 | 说明 |
|------|------|
| `auth.uid()` / `auth_uid()` | 返回当前 JWT 的 `sub`（用户 ID）；未登录返回 NULL |
| `current_setting('key')` | 读取会话变量（`SET kkdb.key = 'val'` 设置）|
| `current_user()` | 当前用户名（`request.jwt.sub` 或 `kkdb.current_user`）|
| `CURRENT_USER` | 同 `current_user()`，SQL 标准写法 |

**设置会话变量：**
```sql
SET kkdb.current_user = 'alice';
-- 查询时 current_user() = 'alice'，RLS 策略据此过滤
```

---

## 9. 全文检索函数

> 详细文档见 [ADVANCED_SQL.md §12](ADVANCED_SQL.md#12-全文检索bm25)

| 函数 | 说明 |
|------|------|
| `FTS_MATCH(table, index, query)` | BM25 相关度评分（同时支持 WHERE 过滤和 ORDER BY 排序）|
| `MATCH (col1, col2) AGAINST ('query')` | MySQL 风格全文搜索（向量匹配分数查询）|

**分词器：**
- `jieba-rs 0.7` — 结巴中文分词（`Cargo.toml` 已集成）
- Hugging Face `tokenizers 0.22.2` — BPE / WordPiece，兼容 HuggingFace tokenizer JSON 配置
- 默认 `Whitespace + Lowercase` — Unicode 感知，按非字母数字字符分词

---

## 10. 操作符扩展

### 算术运算符

| 运算符 | 说明 |
|--------|------|
| `+` `-` `*` `/` `%` | 标准算术 |
| `\|\|` | 字符串拼接 |

### 比较运算符

| 运算符 | 说明 |
|--------|------|
| `=` `!=` `<` `<=` `>` `>=` | 标准比较（NULL 运算结果为 NULL）|
| `IS NULL` / `IS NOT NULL` | NULL 检测 |
| `IS DISTINCT FROM` | NULL 安全不等 |
| `IS NOT DISTINCT FROM` | NULL 安全等于 |
| `BETWEEN a AND b` | 闭区间包含检测（NULL 安全）|
| `IN (...)` / `NOT IN (...)` | 枚举匹配 |
| `LIKE` / `ILIKE` / `NOT LIKE` | 通配符匹配 |
| `SIMILAR TO` / `~` | 正则匹配（基础 ERE 子集）|

### 逻辑运算符

| 运算符 | 说明 |
|--------|------|
| `AND` | 短路求值（NULL 语义正确）|
| `OR` | 短路求值 |
| `NOT` | 逻辑非（NULL → NULL）|
| `XOR` | 逻辑异或 |

### 位运算符（INTEGER）

| 运算符 | 说明 |
|--------|------|
| `&` | 按位与 |
| `\|` | 按位或 |
| `^` | 按位异或 |
| `<<` | 左移 |
| `>>` | 右移 |
| `BITWISE_NOT(x)` | 按位取反（`~` 运算符）|

### 子查询操作符

```sql
x > ANY (SELECT ...)   -- 存在一行使条件成立
x > ALL (SELECT ...)   -- 对所有行均成立
EXISTS (SELECT ...)    -- 子查询结果非空为 TRUE
```

**NULLS FIRST / LAST（ORDER BY）：**
```sql
ORDER BY score DESC NULLS LAST
ORDER BY name ASC NULLS FIRST
```

---

## 11. FROM 子句表值函数

| 语法 | 说明 |
|------|------|
| `UNNEST(array_expr) AS alias(col)` | 展开 JSON 数组或逗号分隔字符串为行 |
| `generate_series(start, stop [, step])` | 生成整数序列 |

**示例：**
```sql
-- 展开 JSON 数组
SELECT val FROM UNNEST('[10, 20, 30]') AS t(val);

-- 展开逗号分隔字符串
SELECT val FROM UNNEST('a,b,c') AS t(val);

-- 整数序列
SELECT n FROM generate_series(1, 10) AS t(n) WHERE n % 2 = 0;
```

---

## 12. JOIN 类型完整列表

| JOIN 类型 | 语法 | 说明 |
|-----------|------|------|
| INNER JOIN | `JOIN t ON ...` | 默认，只保留匹配行 |
| LEFT [OUTER] JOIN | `LEFT JOIN t ON ...` | 保留左表所有行 |
| RIGHT [OUTER] JOIN | `RIGHT JOIN t ON ...` | 保留右表所有行 |
| FULL [OUTER] JOIN | `FULL JOIN t ON ...` | 两侧均保留 |
| CROSS JOIN | `CROSS JOIN t` | 笛卡尔积 |
| NATURAL JOIN | `NATURAL JOIN t` | 按同名列自动等值连接 |
| LEFT SEMI JOIN | 内部 AST | 对应 `EXISTS` 的优化 |
| RIGHT SEMI JOIN | 内部 AST | 对应 `IN (subquery)` 的优化 |

**等值连接优化（Hash Join）：**
KKDB 自动检测 `ON a.id = b.id` 形式的等值连接，切换为哈希连接（O(N+M)），其他条件退化为嵌套循环连接（O(N×M)）。

---

## 实现位置速查

| 类别 | 源文件 |
|------|--------|
| 标量函数 | `src/vm/eval_expr.rs` (`VM::eval_expr`) |
| 聚合、GROUP BY | `src/vm/exec_select.rs` (`VM::apply_group_by`) |
| 窗口函数 | `src/vm/exec_select.rs` (`VM::project_columns`) |
| JSON 辅助 | `src/vm/eval_expr.rs`（文件底部 `json_*` 函数）|
| LIKE 匹配 | `src/vm/eval_expr.rs` (`like_match()`，Unicode 字符级）|
| BM25 评分 | `src/fulltext/index.rs` (`bm25_score()`)，分词 `src/fulltext/mod.rs` |
| 锁管理 | `src/vm/lock_manager.rs` (`LockTable`，等待图 DFS 死锁检测）|
| 备份/CSV | `src/vm/data_transfer.rs` (`backup` / `restore` / `export_csv` / `import_csv`) |

---

## 参考资料

- [ADVANCED_SQL.md](ADVANCED_SQL.md) — 高阶 SQL 完整示例（触发器、RLS、窗口函数等）
- [API.md](API.md) — HTTP REST API 参考
- [PROJECT.md](PROJECT.md) — 整体架构与模块说明
