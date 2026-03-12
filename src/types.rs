use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

/// SQLite-compatible value types
#[derive(Debug, Clone, PartialEq)]
pub enum DataType {
    Null,
    Integer,
    Real,
    Text,
    Blob,
    /// TIMESTAMP / DATETIME / DATE / TIME — stored as i64 epoch milliseconds internally
    Timestamp,
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataType::Null => write!(f, "NULL"),
            DataType::Integer => write!(f, "INTEGER"),
            DataType::Real => write!(f, "REAL"),
            DataType::Text => write!(f, "TEXT"),
            DataType::Blob => write!(f, "BLOB"),
            DataType::Timestamp => write!(f, "TIMESTAMP"),
        }
    }
}

impl DataType {
    /// 从 SQL 类型名称字符串解析出 [`DataType`]。
    ///
    /// 支持常见别名（大小写不敏感）：
    /// - `INTEGER` / `INT` / `BIGINT` / `SMALLINT` / `TINYINT` → [`DataType::Integer`]
    /// - `REAL` / `FLOAT` / `DOUBLE` / `DECIMAL` / `NUMERIC` → [`DataType::Real`]
    /// - `BLOB` / `BINARY` / `VARBINARY` → [`DataType::Blob`]
    /// - `TIMESTAMP` / `DATETIME` / `DATE` / `TIME` → [`DataType::Timestamp`]
    /// - 其余一律映射为 [`DataType::Text`]（与 SQLite 语义一致）。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        if s.eq_ignore_ascii_case("INTEGER")
            || s.eq_ignore_ascii_case("INT")
            || s.eq_ignore_ascii_case("BIGINT")
            || s.eq_ignore_ascii_case("SMALLINT")
            || s.eq_ignore_ascii_case("TINYINT")
        {
            DataType::Integer
        } else if s.eq_ignore_ascii_case("REAL")
            || s.eq_ignore_ascii_case("FLOAT")
            || s.eq_ignore_ascii_case("DOUBLE")
            || s.eq_ignore_ascii_case("DOUBLE PRECISION")
            // M19 fix: DECIMAL and NUMERIC should map to Real, not TEXT.
            // SQLite uses "NUMERIC affinity" which rounds to integer; here we use Real.
            || s.eq_ignore_ascii_case("DECIMAL")
            || s.eq_ignore_ascii_case("NUMERIC")
            || s.to_ascii_uppercase().starts_with("DECIMAL(") // DECIMAL(p,s)
            || s.to_ascii_uppercase().starts_with("NUMERIC(")
        // NUMERIC(p,s)
        {
            DataType::Real
        } else if s.eq_ignore_ascii_case("BLOB")
            || s.eq_ignore_ascii_case("BINARY")
            || s.eq_ignore_ascii_case("VARBINARY")
        {
            DataType::Blob
        } else if s.eq_ignore_ascii_case("TIMESTAMP")
            || s.eq_ignore_ascii_case("DATETIME")
            || s.eq_ignore_ascii_case("DATE")
            || s.eq_ignore_ascii_case("TIME")
        {
            DataType::Timestamp
        } else {
            DataType::Text // default to TEXT like SQLite (covers TEXT, VARCHAR, CHAR, STRING, CLOB)
        }
    }
}

/// Runtime value - dynamically typed like SQLite
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(Arc<str>),
    Blob(Vec<u8>),
}

impl Value {
    /// Estimated serialized size in bytes
    #[inline]
    pub fn serialized_size(&self) -> usize {
        match self {
            Value::Null => 1,
            Value::Integer(_) => 10,           // max 9 bytes varint + 1 tag
            Value::Real(_) => 9,               // 8 bytes float + 1 tag
            Value::Text(v) => 1 + 9 + v.len(), // tag + varint len + text
            Value::Blob(v) => 1 + 9 + v.len(), // tag + varint len + text
        }
    }

    /// Serialize value directly into an existing buffer
    #[inline]
    pub fn serialize_into(&self, buf: &mut Vec<u8>) {
        match self {
            Value::Null => {
                buf.push(0x00);
            }
            Value::Integer(v) => {
                buf.push(0x01);
                crate::varint::write_varint_u64(crate::varint::zigzag_encode(*v), buf);
            }
            Value::Real(v) => {
                buf.push(0x02);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Text(v) => {
                buf.push(0x03);
                let bytes = v.as_bytes();
                crate::varint::write_varint_u64(bytes.len() as u64, buf);
                buf.extend_from_slice(bytes);
            }
            Value::Blob(v) => {
                buf.push(0x04);
                crate::varint::write_varint_u64(v.len() as u64, buf);
                buf.extend_from_slice(v);
            }
        }
    }

    /// Serialize value to bytes for storage
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.serialized_size());
        self.serialize_into(&mut buf);
        buf
    }

    /// Deserialize value from bytes, returns (value, bytes_consumed)
    #[inline]
    pub fn deserialize(data: &[u8]) -> crate::error::Result<(Self, usize)> {
        if data.is_empty() {
            return Err(crate::error::KkdbError::CorruptDatabase(
                "empty value data".into(),
            ));
        }
        match data[0] {
            0x00 => Ok((Value::Null, 1)),
            0x01 => {
                let (v_u64, consumed) = crate::varint::read_varint_u64(&data[1..])?;
                let v = crate::varint::zigzag_decode(v_u64);
                Ok((Value::Integer(v), 1 + consumed))
            }
            0x02 => {
                if data.len() < 9 {
                    return Err(crate::error::KkdbError::CorruptDatabase(
                        "truncated real".into(),
                    ));
                }
                // SAFETY: data.len() >= 9 is verified above, so data[1..9] is exactly 8 bytes
                let v = f64::from_le_bytes(data[1..9].try_into().unwrap());
                Ok((Value::Real(v), 9))
            }
            0x03 => {
                let (len_u64, consumed) = crate::varint::read_varint_u64(&data[1..])?;
                // Guard against malicious/corrupt lengths that would cause OOM or
                // truncation on 32-bit platforms.
                const MAX_VALUE_LEN: u64 = 256 * 1024 * 1024; // 256 MiB
                if len_u64 > MAX_VALUE_LEN {
                    return Err(crate::error::KkdbError::CorruptDatabase(format!(
                        "text value length {} exceeds maximum",
                        len_u64
                    )));
                }
                let len = len_u64 as usize;
                let start = 1 + consumed;
                let end = start + len;
                if data.len() < end {
                    return Err(crate::error::KkdbError::CorruptDatabase(
                        "truncated text data".into(),
                    ));
                }
                let s = std::str::from_utf8(&data[start..end]).map_err(|_| {
                    crate::error::KkdbError::CorruptDatabase("invalid utf-8 in text value".into())
                })?;
                Ok((Value::Text(Arc::from(s)), end))
            }
            0x04 => {
                let (len_u64, consumed) = crate::varint::read_varint_u64(&data[1..])?;
                const MAX_VALUE_LEN: u64 = 256 * 1024 * 1024; // 256 MiB
                if len_u64 > MAX_VALUE_LEN {
                    return Err(crate::error::KkdbError::CorruptDatabase(format!(
                        "blob value length {} exceeds maximum",
                        len_u64
                    )));
                }
                let len = len_u64 as usize;
                let start = 1 + consumed;
                let end = start + len;
                if data.len() < end {
                    return Err(crate::error::KkdbError::CorruptDatabase(
                        "truncated blob data".into(),
                    ));
                }
                let v = data[start..end].to_vec();
                Ok((Value::Blob(v), end))
            }
            tag => Err(crate::error::KkdbError::CorruptDatabase(format!(
                "unknown value tag: 0x{:02x}",
                tag
            ))),
        }
    }

    /// 判断该值在布尔上下文中是否为真。
    ///
    /// - `Null` → `false`
    /// - `Integer(0)` / `Real(0.0)` → `false`
    /// - 空字符串 / 空 Blob → `false`
    /// - 其余 → `true`
    #[inline]
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Integer(v) => *v != 0,
            Value::Real(v) => *v != 0.0,
            Value::Text(v) => !v.is_empty(),
            Value::Blob(v) => !v.is_empty(),
        }
    }

    /// 尝试将值转换为 `i64`。
    ///
    /// - `Integer` 直接返回。
    /// - `Real` 截断为整数。
    /// - `Text` 尝试解析；解析失败返回 `None`。
    /// - `Null` / `Blob` 返回 `None`。
    #[inline]
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Value::Integer(v) => Some(*v),
            Value::Real(v) => Some(*v as i64),
            Value::Text(v) => v.parse().ok(),
            _ => None,
        }
    }

    /// 尝试将值转换为 `f64`。
    ///
    /// - `Real` 直接返回。
    /// - `Integer` 无损转换（大整数可能丢失精度）。
    /// - `Text` 尝试解析；解析失败返回 `None`。
    /// - `Null` / `Blob` 返回 `None`。
    #[inline]
    pub fn to_f64(&self) -> Option<f64> {
        match self {
            Value::Integer(v) => Some(*v as f64),
            Value::Real(v) => Some(*v),
            Value::Text(v) => v.parse().ok(),
            _ => None,
        }
    }

    /// 返回该值对应的 [`DataType`] 标识。
    pub fn data_type(&self) -> DataType {
        match self {
            Value::Null => DataType::Null,
            Value::Integer(_) => DataType::Integer,
            Value::Real(_) => DataType::Real,
            Value::Text(_) => DataType::Text,
            Value::Blob(_) => DataType::Blob,
        }
    }
    /// Format this value as an ISO 8601 UTC timestamp string when the column type is Timestamp.
    /// Input: epoch-milliseconds stored as Value::Integer.
    pub fn format_as_timestamp(&self) -> String {
        match self {
            Value::Integer(epoch_ms) => {
                let secs = epoch_ms / 1000;
                let ms = (epoch_ms % 1000).unsigned_abs() as u32;
                // Manual ISO 8601 UTC formatting without an external crate.
                // Days since Unix epoch → date components.
                let secs_unsigned = secs.max(0) as u64;
                let (y, mo, d, h, mi, s) = epoch_secs_to_datetime(secs_unsigned);
                format!(
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                    y, mo, d, h, mi, s, ms
                )
            }
            Value::Text(t) => t.to_string(), // already formatted
            _ => self.to_string(),
        }
    }

    /// Display this value formatted for a given column type.
    /// Use this in the MySQL protocol / shell output layer.
    pub fn format_for_column(&self, dtype: &DataType) -> String {
        if matches!(dtype, DataType::Timestamp) {
            self.format_as_timestamp()
        } else {
            self.to_string()
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Integer(v) => write!(f, "{}", v),
            Value::Real(v) => write!(f, "{}", v),
            Value::Text(v) => write!(f, "{}", v),
            Value::Blob(v) => write!(f, "x'{}'", hex_encode(v)),
        }
    }
}

impl PartialEq for Value {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Integer(a), Value::Integer(b)) => a == b,
            (Value::Real(a), Value::Real(b)) => a == b,
            (Value::Integer(a), Value::Real(b)) => (*a as f64) == *b,
            (Value::Real(a), Value::Integer(b)) => *a == (*b as f64),
            (Value::Text(a), Value::Text(b)) => a == b,
            (Value::Blob(a), Value::Blob(b)) => a == b,
            _ => false,
        }
    }
}

impl PartialOrd for Value {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Value::Null, Value::Null) => Some(Ordering::Equal),
            (Value::Null, _) => Some(Ordering::Less),
            (_, Value::Null) => Some(Ordering::Greater),
            (Value::Integer(a), Value::Integer(b)) => a.partial_cmp(b),
            (Value::Real(a), Value::Real(b)) => a.partial_cmp(b),
            (Value::Integer(a), Value::Real(b)) => (*a as f64).partial_cmp(b),
            (Value::Real(a), Value::Integer(b)) => a.partial_cmp(&(*b as f64)),
            (Value::Text(a), Value::Text(b)) => a.partial_cmp(b),
            (Value::Blob(a), Value::Blob(b)) => a.partial_cmp(b),
            // SQLite ordering: NULL < INTEGER/REAL < TEXT < BLOB
            (Value::Integer(_) | Value::Real(_), Value::Text(_) | Value::Blob(_)) => {
                Some(Ordering::Less)
            }
            (Value::Text(_) | Value::Blob(_), Value::Integer(_) | Value::Real(_)) => {
                Some(Ordering::Greater)
            }
            (Value::Text(_), Value::Blob(_)) => Some(Ordering::Less),
            (Value::Blob(_), Value::Text(_)) => Some(Ordering::Greater),
        }
    }
}

/// 数据行 — 由一组 [`Value`] 组成的有序集合。
///
/// 列顺序与 [`TableSchema::columns`](crate::schema::TableSchema) 定义一致。
pub type Row = Vec<Value>;

/// Serialize a row to bytes
#[inline]
pub fn serialize_row(row: &Row) -> Vec<u8> {
    // Pre-calculate total size to avoid reallocations
    let total_size: usize = 9 + row.iter().map(|v| v.serialized_size()).sum::<usize>();
    let mut buf = Vec::with_capacity(total_size);
    // column count
    crate::varint::write_varint_u64(row.len() as u64, &mut buf);
    for val in row {
        val.serialize_into(&mut buf);
    }
    buf
}

/// Serialize a row into an existing buffer (reusable, avoids per-call allocation)
#[inline]
pub fn serialize_row_into(row: &Row, buf: &mut Vec<u8>) {
    buf.clear();
    let total_size: usize = 9 + row.iter().map(|v| v.serialized_size()).sum::<usize>();
    buf.reserve(total_size);
    crate::varint::write_varint_u64(row.len() as u64, buf);
    for val in row {
        val.serialize_into(buf);
    }
}

/// Deserialize a row from bytes
#[inline]
pub fn deserialize_row(data: &[u8]) -> crate::error::Result<Row> {
    if data.is_empty() {
        return Err(crate::error::KkdbError::CorruptDatabase(
            "row data too short".into(),
        ));
    }
    let (col_count_u64, mut offset) = crate::varint::read_varint_u64(data)?;
    // Prevent malicious data from triggering OOM via absurdly large col_count.
    const MAX_COLUMNS: u64 = 4096;
    if col_count_u64 > MAX_COLUMNS {
        return Err(crate::error::KkdbError::CorruptDatabase(format!(
            "row column count {} exceeds maximum {}",
            col_count_u64, MAX_COLUMNS
        )));
    }
    let col_count = col_count_u64 as usize;

    let mut row = Vec::with_capacity(col_count);
    for _ in 0..col_count {
        let (val, consumed) = Value::deserialize(&data[offset..])?;
        row.push(val);
        offset += consumed;
    }
    Ok(row)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

// ── F1: Index key prefix compression ─────────────────────────────────────────

/// Serialize a single-column index row with prefix-delta encoding of Text values.
///
/// Format: same as `serialize_row` but Text values are delta-encoded relative to
/// `prev_text_key` (the serialized text bytes of the previous row's first key column).
/// Non-text values use the standard `Value::serialize_into` format, unchanged.
///
/// Returns `(compressed_bytes, new_prev_key)` where `new_prev_key` is the raw
/// text bytes of the current key column (for the next call).
pub fn serialize_index_row_compressed(row: &Row, prev_key: &[u8]) -> (Vec<u8>, Vec<u8>) {
    use crate::storage::prefix_compress::prefix_encode;
    // Pre-estimate capacity: varint col_count + sum of per-value upper bounds.
    // For the first Text column we add the prefix-encoded size estimate (worst case = 3 + raw len).
    // Fix #7: avoids repeated reallocations on medium-length index keys.
    let est: usize = 9 + row.iter().map(|v| v.serialized_size()).sum::<usize>();
    let mut buf = Vec::with_capacity(est);
    crate::varint::write_varint_u64(row.len() as u64, &mut buf);
    let mut new_prev = prev_key.to_vec();
    let mut is_first = true;
    for val in row {
        if is_first {
            if let Value::Text(t) = val {
                let raw = t.as_bytes();
                // Write tag 0x03 = Text, but use prefix-encoded length+suffix
                buf.push(0x03);
                let encoded = prefix_encode(prev_key, raw);
                crate::varint::write_varint_u64(encoded.len() as u64, &mut buf);
                buf.extend_from_slice(&encoded);
                new_prev = raw.to_vec();
                is_first = false;
                continue;
            }
        }
        val.serialize_into(&mut buf);
        is_first = false;
    }
    (buf, new_prev)
}

/// Deserialize a single-column index row that was encoded with `serialize_index_row_compressed`.
///
/// `prev_key` must be the fully-decoded raw text bytes of the previous row's key column.
/// Returns `(row, new_prev_key)`.
pub fn deserialize_index_row_with_prefix(
    data: &[u8],
    prev_key: &[u8],
) -> crate::error::Result<(Row, Vec<u8>)> {
    use crate::storage::prefix_compress::prefix_decode;
    if data.is_empty() {
        return Err(crate::error::KkdbError::CorruptDatabase(
            "row data too short".into(),
        ));
    }
    let (col_count_u64, mut offset) = crate::varint::read_varint_u64(data)?;
    let col_count = col_count_u64 as usize;
    const MAX_COLUMNS: usize = 4096;
    if col_count > MAX_COLUMNS {
        return Err(crate::error::KkdbError::CorruptDatabase(format!(
            "index row col_count {} exceeds MAX_COLUMNS {}",
            col_count, MAX_COLUMNS
        )));
    }
    let mut row = Vec::with_capacity(col_count);
    let mut new_prev = prev_key.to_vec();
    let mut is_first = true;
    for _ in 0..col_count {
        if is_first && offset < data.len() && data[offset] == 0x03 {
            // Prefix-encoded Text value
            offset += 1;
            let (enc_len, consumed) = crate::varint::read_varint_u64(&data[offset..])?;
            offset += consumed;
            let enc_end = offset + enc_len as usize;
            // Strict bounds check: a truncated encoded payload is unambiguously corrupt.
            // The old code used enc_end.min(data.len()) which silently produced wrong keys.
            if enc_end > data.len() {
                return Err(crate::error::KkdbError::CorruptDatabase(format!(
                    "prefix-compressed index payload truncated: enc_end={} > data_len={}",
                    enc_end,
                    data.len()
                )));
            }
            let encoded = &data[offset..enc_end];
            let decoded_bytes = prefix_decode(prev_key, encoded);
            let s = std::str::from_utf8(&decoded_bytes)
                .map_err(|_| {
                    crate::error::KkdbError::CorruptDatabase("invalid utf-8 in index key".into())
                })?
                .to_owned();
            new_prev = decoded_bytes;
            row.push(Value::Text(std::sync::Arc::from(s.as_str())));
            offset = enc_end;
            is_first = false;
            continue;
        }
        let (val, consumed) = Value::deserialize(&data[offset..])?;
        row.push(val);
        offset += consumed;
        is_first = false;
    }
    Ok((row, new_prev))
}

/// Stateful decoder for scanning a B-Tree index leaf page with prefix-compressed rows.
/// Create one per page scan; reset between pages.
pub struct PrefixPageDecoder {
    pub prev_key: Vec<u8>,
}

impl Default for PrefixPageDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl PrefixPageDecoder {
    /// 创建一个新的解码器，初始前缀为空。
    pub fn new() -> Self {
        PrefixPageDecoder {
            prev_key: Vec::new(),
        }
    }

    /// Decode the next prefix-compressed index row payload.
    pub fn decode(&mut self, data: &[u8]) -> crate::error::Result<Row> {
        let (row, new_prev) = deserialize_index_row_with_prefix(data, &self.prev_key)?;
        self.prev_key = new_prev;
        Ok(row)
    }

    /// Reset state between pages (prev_key resets to empty at page boundary).
    pub fn reset(&mut self) {
        self.prev_key.clear();
    }
}

/// Stateful encoder for writing prefix-compressed index rows page-by-page.
pub struct PrefixPageEncoder {
    pub prev_key: Vec<u8>,
}

impl Default for PrefixPageEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl PrefixPageEncoder {
    /// 创建一个新的编码器，初始前缀为空。
    pub fn new() -> Self {
        PrefixPageEncoder {
            prev_key: Vec::new(),
        }
    }

    /// Encode the next index row, returns compressed bytes.
    pub fn encode(&mut self, row: &Row) -> Vec<u8> {
        let (bytes, new_prev) = serialize_index_row_compressed(row, &self.prev_key);
        self.prev_key = new_prev;
        bytes
    }

    /// Reset at page boundary.
    pub fn reset(&mut self) {
        self.prev_key.clear();
    }
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;

/// Convert Unix epoch-seconds to (year, month, day, hour, minute, second) in UTC.
/// Pure Rust, no external crate required.
pub(crate) fn epoch_secs_to_datetime(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = secs % 86400;
    let h = (s / 3600) as u32;
    let mi = ((s % 3600) / 60) as u32;
    let sec = (s % 60) as u32;
    let days = secs / 86400;
    // Days since 1970-01-01 -> Gregorian date (Proleptic Gregorian calendar).
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe + era * 400) as u32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, sec)
}
