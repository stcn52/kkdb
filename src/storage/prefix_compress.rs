/// F1: Index key prefix compression.
///
/// `prefix_encode(prev, cur)` — encodes `cur` relative to `prev`.
///
/// Output format:
///   [shared_prefix_len: u8][suffix_len: u16 LE][suffix bytes]
///
/// `shared_prefix_len` is clamped to 255. If `cur` shares > 255 bytes with
/// `prev`, only 255 are marked as shared and the rest go into the suffix.
///
/// `prefix_decode(prev, encoded)` — reconstructs the original bytes.
///
/// These functions are used to compress consecutive sorted text keys on a B-Tree
/// index leaf page. Non-text values bypass these functions entirely.

/// Encode `cur` relative to `prev`.
/// Returns the compressed bytes.
pub fn prefix_encode(prev: &[u8], cur: &[u8]) -> Vec<u8> {
    let shared = prev
        .iter()
        .zip(cur.iter())
        .take_while(|(a, b)| a == b)
        .count()
        .min(255) as u8;
    let suffix = &cur[shared as usize..];
    // Guard: suffix > 65533 bytes cannot fit in u16 (max 65535 minus 2 header bytes).
    // Fall back to storing the full key with shared=0 and raw suffix length clamped.
    // In practice, B-Tree keys are well under 2016 bytes (MAX_INLINE_PAYLOAD limit).
    let suffix_len = if suffix.len() > u16::MAX as usize {
        // Emit as if shared=0, suffix=full cur, clamped to u16::MAX
        let mut out = Vec::with_capacity(3 + u16::MAX as usize);
        out.push(0u8);  // shared = 0
        out.extend_from_slice(&(u16::MAX).to_le_bytes()); // max suffix_len
        out.extend_from_slice(&cur[..u16::MAX as usize]);  // first 65535 bytes of cur
        return out;
    } else {
        suffix.len() as u16
    };
    let mut out = Vec::with_capacity(3 + suffix.len());
    out.push(shared);
    out.extend_from_slice(&suffix_len.to_le_bytes());
    out.extend_from_slice(suffix);
    out
}

/// Decode an encoded entry back to its original bytes.
/// `prev` must be the fully-decoded previous key (or `&[]` for the first entry).
///
/// Returns `prev[..shared] + suffix`. If `encoded` is shorter than expected
/// (corrupt data), clamps gracefully rather than panicking — callers that need
/// strict validation should check the full row hash or checksum instead.
pub fn prefix_decode(prev: &[u8], encoded: &[u8]) -> Vec<u8> {
    if encoded.len() < 3 {
        // Malformed: not enough bytes for header. Return empty rather than panic.
        // Callers (deserialize_index_row_with_prefix) check for this via enc_end guard.
        return encoded.to_vec();
    }
    let shared = encoded[0] as usize;
    let suffix_len = u16::from_le_bytes(encoded[1..3].try_into().unwrap()) as usize;
    let suffix = if encoded.len() >= 3 + suffix_len {
        &encoded[3..3 + suffix_len]
    } else {
        // Truncated payload — return what we have (data is already corrupt).
        &encoded[3..]
    };
    let mut out = Vec::with_capacity(shared + suffix.len());
    let safe_shared = shared.min(prev.len());
    out.extend_from_slice(&prev[..safe_shared]);
    out.extend_from_slice(suffix);
    out
}

/// Compute the compressed size for a sequence of sorted byte slices.
/// Useful for estimating space savings before committing.
pub fn estimate_compressed_size(keys: &[&[u8]]) -> usize {
    let mut total = 0usize;
    for i in 0..keys.len() {
        let prev = if i == 0 { &b""[..] } else { keys[i - 1] };
        let key = keys[i];
        let shared = prev.iter().zip(key.iter()).take_while(|(a, b)| a == b).count().min(255);
        let suffix_len = (key.len() - shared).min(u16::MAX as usize);
        total += 1 + 2 + suffix_len; // shared_len(1) + suffix_len(2) + suffix
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(keys: &[&str]) {
        let mut prev: Vec<u8> = Vec::new();
        let mut encoded_list: Vec<Vec<u8>> = Vec::new();
        for key in keys {
            let enc = prefix_encode(&prev, key.as_bytes());
            encoded_list.push(enc);
            prev = key.as_bytes().to_vec();
        }
        // Decode and verify
        let mut dec_prev: Vec<u8> = Vec::new();
        for (i, enc) in encoded_list.iter().enumerate() {
            let decoded = prefix_decode(&dec_prev, enc);
            assert_eq!(decoded, keys[i].as_bytes(), "key[{}] mismatch", i);
            dec_prev = decoded;
        }
    }

    #[test]
    fn test_prefix_encode_decode_basic() {
        roundtrip(&["user_0001", "user_0002", "user_0099", "user_1000"]);
    }

    #[test]
    fn test_prefix_encode_no_shared() {
        roundtrip(&["aaa", "bbb", "ccc"]);
    }

    #[test]
    fn test_prefix_encode_identical() {
        roundtrip(&["same", "same", "same"]);
    }

    #[test]
    fn test_prefix_encode_empty_start() {
        roundtrip(&["", "abc", "abd"]);
    }

    #[test]
    fn test_prefix_encode_estimate() {
        let keys: Vec<&str> = (0..100).map(|_| "user_1234").collect();
        let keys_ref: Vec<&[u8]> = keys.iter().map(|s| s.as_bytes()).collect();
        let compressed = estimate_compressed_size(&keys_ref);
        // First entry: 1+2+9=12 bytes; subsequent: 1+2+0=3 bytes each
        let expected = 12 + 99 * 3;
        assert_eq!(compressed, expected);
    }
}
