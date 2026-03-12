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
        // On the 10th byte (shift == 63), only the lowest bit is valid.
        if shift >= 63 {
            if byte > 0x01 {
                return Err(KkdbError::CorruptDatabase(
                    "varint overflowed 64 bits".into(),
                ));
            }
            val |= (byte as u64) << shift;
            return Ok((val, consumed));
        }
        val |= ((byte & 0x7F) as u64) << shift;
        if (byte & 0x80) == 0 {
            return Ok((val, consumed));
        }
        shift += 7;
    }

    Err(KkdbError::CorruptDatabase("truncated varint".into()))
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
        let values = [0, 1, 127, 128, 255, 10000, u64::MAX, u64::MAX / 2];

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
        let values = [0, -1, 1, -2, 2, i64::MAX, i64::MIN];

        for &val in &values {
            let encoded = zigzag_encode(val);
            let decoded = zigzag_decode(encoded);
            assert_eq!(val, decoded);
        }
    }

    // ── New coverage tests ──────────────────────────────────────────────

    #[test]
    fn test_varint_empty_slice() {
        let err = read_varint_u64(&[]);
        assert!(err.is_err(), "empty slice should return error");
    }

    #[test]
    fn test_varint_overflow_11_continuation_bytes() {
        // 11 continuation bytes (all 0x80) should fail - too many for u64
        let data = vec![0x80u8; 11];
        let err = read_varint_u64(&data);
        assert!(err.is_err(), "11 continuation bytes should overflow");
    }

    #[test]
    fn test_varint_10th_byte_overflow() {
        // Construct a 10-byte varint where the 10th byte has value > 1
        // (which would mean a value > u64::MAX)
        let mut data = vec![0x80u8; 9];
        data.push(0x02); // 10th byte = 2, should be rejected (max allowed = 1)
        let err = read_varint_u64(&data);
        assert!(
            err.is_err(),
            "10th byte = 0x02 should be rejected as overflow"
        );
    }

    #[test]
    fn test_varint_10th_byte_valid() {
        // u64::MAX: all 64 bits set
        // LEB128 of u64::MAX = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]
        let mut buf = Vec::new();
        write_varint_u64(u64::MAX, &mut buf);
        assert_eq!(buf.len(), 10);
        assert_eq!(buf[9], 0x01); // 10th byte should be 0x01
        let (decoded, consumed) = read_varint_u64(&buf).unwrap();
        assert_eq!(decoded, u64::MAX);
        assert_eq!(consumed, 10);
    }

    #[test]
    fn test_varint_boundary_127_128() {
        // 127 = single byte
        let mut buf = Vec::new();
        write_varint_u64(127, &mut buf);
        assert_eq!(buf.len(), 1);

        // 128 = two bytes
        buf.clear();
        write_varint_u64(128, &mut buf);
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn test_varint_single_truncated_byte() {
        // Single continuation byte with no terminator
        let data = vec![0x80u8];
        let err = read_varint_u64(&data);
        assert!(
            err.is_err(),
            "single continuation byte should be truncated varint"
        );
    }
}
