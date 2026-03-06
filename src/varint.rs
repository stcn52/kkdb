use crate::error::{KkdbError, Result};

/// Encodes a u64 into LEB128 and appends it to the buffer.
#[inline]
pub fn write_varint_u64(mut val: u64, buf: &mut Vec<u8>) {
    loop {
        let mut byte = (val & 0x7F) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80; // More bytes to follow
        }
        buf.push(byte);
        if val == 0 {
            break;
        }
    }
}

/// Decodes a LEB128 u64 from the slice.
/// Returns the decoded value and the number of bytes consumed.
#[inline]
pub fn read_varint_u64(data: &[u8]) -> Result<(u64, usize)> {
    let mut val: u64 = 0;
    let mut shift = 0;
    let mut consumed = 0;

    for &byte in data {
        consumed += 1;
        val |= ((byte & 0x7F) as u64) << shift;
        if (byte & 0x80) == 0 {
            return Ok((val, consumed));
        }
        shift += 7;
        if shift >= 64 {
            return Err(KkdbError::CorruptDatabase(
                "varint overflowed 64 bits".into(),
            ));
        }
    }
    
    Err(KkdbError::CorruptDatabase(
        "truncated varint".into(),
    ))
}

/// ZigZag encodes a signed integer into an unsigned integer.
#[inline]
pub fn zigzag_encode(val: i64) -> u64 {
    ((val << 1) ^ (val >> 63)) as u64
}

/// ZigZag decodes an unsigned integer into a signed integer.
#[inline]
pub fn zigzag_decode(val: u64) -> i64 {
    (val >> 1) as i64 ^ -((val & 1) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_u64() {
        let values = [
            0,
            1,
            127,
            128,
            255,
            10000,
            u64::MAX,
            u64::MAX / 2
        ];

        for &val in &values {
            let mut buf = Vec::new();
            write_varint_u64(val, &mut buf);
            let (decoded, consumed) = read_varint_u64(&buf).unwrap();
            assert_eq!(val, decoded);
            assert_eq!(buf.len(), consumed);
        }
    }

    #[test]
    fn test_zigzag() {
        let values = [
            0,
            -1,
            1,
            -2,
            2,
            i64::MAX,
            i64::MIN
        ];

        for &val in &values {
            let encoded = zigzag_encode(val);
            let decoded = zigzag_decode(encoded);
            assert_eq!(val, decoded);
        }
    }
}
