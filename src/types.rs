use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;

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
    Text(Rc<str>),
    Blob(Vec<u8>),
}

impl Value {
    /// Estimated serialized size in bytes
    #[inline]
    pub fn serialized_size(&self) -> usize {
        match self {
            Value::Null => 1,
            Value::Integer(_) => 9,
            Value::Real(_) => 9,
            Value::Text(v) => 5 + v.len(),
            Value::Blob(v) => 5 + v.len(),
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
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Real(v) => {
                buf.push(0x02);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Text(v) => {
                buf.push(0x03);
                let bytes = v.as_bytes();
                buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(bytes);
            }
            Value::Blob(v) => {
                buf.push(0x04);
                buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
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
                if data.len() < 9 {
                    return Err(crate::error::KkdbError::CorruptDatabase(
                        "truncated integer".into(),
                    ));
                }
                let v = i64::from_le_bytes(data[1..9].try_into().unwrap());
                Ok((Value::Integer(v), 9))
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
                if data.len() < 5 {
                    return Err(crate::error::KkdbError::CorruptDatabase(
                        "truncated text length".into(),
                    ));
                }
                let len = u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize;
                if data.len() < 5 + len {
                    return Err(crate::error::KkdbError::CorruptDatabase(
                        "truncated text data".into(),
                    ));
                }
                let s = std::str::from_utf8(&data[5..5 + len]).map_err(|_| {
                    crate::error::KkdbError::CorruptDatabase("invalid utf-8 in text value".into())
                })?;
                Ok((Value::Text(Rc::from(s)), 5 + len))
            }
            0x04 => {
                if data.len() < 5 {
                    return Err(crate::error::KkdbError::CorruptDatabase(
                        "truncated blob length".into(),
                    ));
                }
                let len = u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize;
                if data.len() < 5 + len {
                    return Err(crate::error::KkdbError::CorruptDatabase(
                        "truncated blob data".into(),
                    ));
                }
                let v = data[5..5 + len].to_vec();
                Ok((Value::Blob(v), 5 + len))
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
    let total_size: usize = 2 + row.iter().map(|v| v.serialized_size()).sum::<usize>();
    let mut buf = Vec::with_capacity(total_size);
    // column count
    buf.extend_from_slice(&(row.len() as u16).to_le_bytes());
    for val in row {
        val.serialize_into(&mut buf);
    }
    buf
}

/// Serialize a row into an existing buffer (reusable, avoids per-call allocation)
#[inline]
pub fn serialize_row_into(row: &Row, buf: &mut Vec<u8>) {
    buf.clear();
    let total_size: usize = 2 + row.iter().map(|v| v.serialized_size()).sum::<usize>();
    buf.reserve(total_size);
    buf.extend_from_slice(&(row.len() as u16).to_le_bytes());
    for val in row {
        val.serialize_into(buf);
    }
}

/// Deserialize a row from bytes
#[inline]
pub fn deserialize_row(data: &[u8]) -> crate::error::Result<Row> {
    if data.len() < 2 {
        return Err(crate::error::KkdbError::CorruptDatabase(
            "row data too short".into(),
        ));
    }
    let col_count = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
    let mut row = Vec::with_capacity(col_count);
    let mut offset = 2;
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

#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
