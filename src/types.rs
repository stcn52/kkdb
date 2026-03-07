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
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataType::Null => write!(f, "NULL"),
            DataType::Integer => write!(f, "INTEGER"),
            DataType::Real => write!(f, "REAL"),
            DataType::Text => write!(f, "TEXT"),
            DataType::Blob => write!(f, "BLOB"),
        }
    }
}

impl DataType {
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
        {
            DataType::Real
        } else if s.eq_ignore_ascii_case("BLOB")
            || s.eq_ignore_ascii_case("BINARY")
            || s.eq_ignore_ascii_case("VARBINARY")
        {
            DataType::Blob
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
            Value::Integer(_) => 10, // max 9 bytes varint + 1 tag
            Value::Real(_) => 9,     // 8 bytes float + 1 tag
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
                let v = f64::from_le_bytes(data[1..9].try_into().unwrap());
                Ok((Value::Real(v), 9))
            }
            0x03 => {
                let (len_u64, consumed) = crate::varint::read_varint_u64(&data[1..])?;
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

    #[inline]
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Value::Integer(v) => Some(*v),
            Value::Real(v) => Some(*v as i64),
            Value::Text(v) => v.parse().ok(),
            _ => None,
        }
    }

    #[inline]
    pub fn to_f64(&self) -> Option<f64> {
        match self {
            Value::Integer(v) => Some(*v as f64),
            Value::Real(v) => Some(*v),
            Value::Text(v) => v.parse().ok(),
            _ => None,
        }
    }

    pub fn data_type(&self) -> DataType {
        match self {
            Value::Null => DataType::Null,
            Value::Integer(_) => DataType::Integer,
            Value::Real(_) => DataType::Real,
            Value::Text(_) => DataType::Text,
            Value::Blob(_) => DataType::Blob,
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

/// Row is a collection of values
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
pub fn deserialize_index_row_with_prefix(data: &[u8], prev_key: &[u8]) -> crate::error::Result<(Row, Vec<u8>)> {
    use crate::storage::prefix_compress::prefix_decode;
    if data.is_empty() {
        return Err(crate::error::KkdbError::CorruptDatabase("row data too short".into()));
    }
    let (col_count_u64, mut offset) = crate::varint::read_varint_u64(data)?;
    let col_count = col_count_u64 as usize;
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
                    enc_end, data.len()
                )));
            }
            let encoded = &data[offset..enc_end];
            let decoded_bytes = prefix_decode(prev_key, encoded);
            let s = std::str::from_utf8(&decoded_bytes).map_err(|_| {
                crate::error::KkdbError::CorruptDatabase("invalid utf-8 in index key".into())
            })?.to_owned();
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

impl PrefixPageDecoder {
    pub fn new() -> Self {
        PrefixPageDecoder { prev_key: Vec::new() }
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

impl PrefixPageEncoder {
    pub fn new() -> Self {
        PrefixPageEncoder { prev_key: Vec::new() }
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
