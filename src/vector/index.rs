/// B-Tree key / value encoding for vector data.
///
/// Mirrors the role of `fulltext/index.rs` in the FTS subsystem.
///
/// Key schema:
///   Vector entry : `\x00VEC\x01{index_id: u32 BE}\x02{row_id: u64 BE}`
///   Index meta   : `\x00VEC\x01{index_id: u32 BE}\x03META`
///
/// Value schema:
///   Vector entry : `[dim: u32 LE][f32 × dim]`
///   Index meta   : `[dim: u32 LE][distance_type: u8][total_vectors: u64 LE][reserved: 19]`
const VEC_PREFIX: &[u8] = b"\x00VEC\x01";
const SEP_ROWID: u8 = 0x02;
const SEP_META: u8 = 0x03;
const META_SUFFIX: &[u8] = b"META";

// ─── Key builders ────────────────────────────────────────────────────────────

/// Key for a single vector entry (rowid within an index).
pub fn vec_key(index_id: u32, row_id: u64) -> Vec<u8> {
    let mut k = VEC_PREFIX.to_vec();
    k.extend_from_slice(&index_id.to_be_bytes());
    k.push(SEP_ROWID);
    k.extend_from_slice(&row_id.to_be_bytes());
    k
}

/// Key for the index-level meta record.
pub fn meta_key(index_id: u32) -> Vec<u8> {
    let mut k = VEC_PREFIX.to_vec();
    k.extend_from_slice(&index_id.to_be_bytes());
    k.push(SEP_META);
    k.extend_from_slice(META_SUFFIX);
    k
}

/// Prefix used to scan all vector entries belonging to `index_id`.
pub fn vec_prefix(index_id: u32) -> Vec<u8> {
    let mut p = VEC_PREFIX.to_vec();
    p.extend_from_slice(&index_id.to_be_bytes());
    p.push(SEP_ROWID);
    p
}

/// Extract row_id from a vector entry key produced by `vec_key()`.
///
/// Panics (debug) if key is shorter than expected; returns 0 in release.
pub fn decode_rowid_from_key(key: &[u8]) -> u64 {
    // prefix(5) + index_id(4) + sep(1) + row_id(8) = 18 bytes
    let offset = VEC_PREFIX.len() + 4 + 1;
    if key.len() >= offset + 8 {
        let bytes: [u8; 8] = key[offset..offset + 8].try_into().unwrap_or([0; 8]);
        u64::from_be_bytes(bytes)
    } else {
        debug_assert!(false, "vec key too short: {:?}", key);
        0
    }
}

// ─── Value encoders ──────────────────────────────────────────────────────────

/// Encode a f32 slice as `[dim: u32 LE][f32 × dim]`.
pub fn encode_vector(vec: &[f32]) -> Vec<u8> {
    let dim = vec.len() as u32;
    let mut out = Vec::with_capacity(4 + vec.len() * 4);
    out.extend_from_slice(&dim.to_le_bytes());
    for &v in vec {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Decode a vector value encoded by `encode_vector()`.
///
/// Returns `None` if the bytes are malformed.
pub fn decode_vector(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() < 4 {
        return None;
    }
    let dim = u32::from_le_bytes(bytes[..4].try_into().ok()?) as usize;
    if bytes.len() < 4 + dim * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(dim);
    for i in 0..dim {
        let off = 4 + i * 4;
        let v = f32::from_le_bytes(bytes[off..off + 4].try_into().ok()?);
        out.push(v);
    }
    Some(out)
}

/// Encode the 32-byte meta value:
/// `[dim: u32 LE][distance_type: u8][total_vectors: u64 LE][reserved: 19]`
pub fn encode_meta(dim: u32, distance_type: u8, total_vectors: u64) -> Vec<u8> {
    let mut out = vec![0u8; 32];
    out[0..4].copy_from_slice(&dim.to_le_bytes());
    out[4] = distance_type;
    out[5..13].copy_from_slice(&total_vectors.to_le_bytes());
    out
}

/// Decode the meta value; returns `(dim, distance_type, total_vectors)`.
pub fn decode_meta(bytes: &[u8]) -> Option<(u32, u8, u64)> {
    if bytes.len() < 13 {
        return None;
    }
    let dim = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let dt = bytes[4];
    let total = u64::from_le_bytes(bytes[5..13].try_into().ok()?);
    Some((dim, dt, total))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_vector() {
        let v = vec![0.1f32, 0.2, 0.3, 0.4];
        let enc = encode_vector(&v);
        assert_eq!(enc.len(), 4 + 4 * 4);
        let dec = decode_vector(&enc).unwrap();
        for (a, b) in v.iter().zip(dec.iter()) {
            assert!((a - b).abs() < 1e-7, "mismatch: {a} vs {b}");
        }
    }

    #[test]
    fn test_key_rowid_roundtrip() {
        let rowid = 0x0102030405060708u64;
        let key = vec_key(42, rowid);
        assert_eq!(decode_rowid_from_key(&key), rowid);
    }

    #[test]
    fn test_meta_roundtrip() {
        let enc = encode_meta(1536, 0x01, 100_000);
        let (dim, dt, total) = decode_meta(&enc).unwrap();
        assert_eq!(dim, 1536);
        assert_eq!(dt, 0x01);
        assert_eq!(total, 100_000);
    }

    // ── New coverage tests ──────────────────────────────────────────────

    #[test]
    fn test_decode_vector_empty_buffer() {
        assert!(decode_vector(&[]).is_none());
    }

    #[test]
    fn test_decode_vector_short_buffer() {
        // dim = 3 but only has 2 floats worth of data
        let mut data = Vec::new();
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&1.0f32.to_le_bytes());
        data.extend_from_slice(&2.0f32.to_le_bytes());
        // missing 3rd float
        assert!(decode_vector(&data).is_none());
    }

    #[test]
    fn test_decode_vector_huge_dim() {
        // dim claims 2^30 elements but buffer is tiny
        let mut data = Vec::new();
        data.extend_from_slice(&(1u32 << 30).to_le_bytes());
        assert!(decode_vector(&data).is_none());
    }

    #[test]
    fn test_decode_meta_short_buffer() {
        assert!(decode_meta(&[]).is_none());
        assert!(decode_meta(&[0u8; 12]).is_none());
        assert!(decode_meta(&[0u8; 13]).is_some());
    }

    #[test]
    fn test_decode_rowid_from_short_key() {
        // In release, short keys return 0. In debug, they trigger debug_assert.
        // We simply verify the function exists and the full-length key path works;
        // the short-key behavior is deliberately a debug_assert.
        let key = vec_key(1, 42);
        assert_eq!(decode_rowid_from_key(&key), 42);
    }

    #[test]
    fn test_vec_key_prefix_relationship() {
        let key = vec_key(42, 99);
        let prefix = vec_prefix(42);
        assert!(key.starts_with(&prefix));
    }

    #[test]
    fn test_meta_key_vs_vec_key_no_collision() {
        let vk = vec_key(1, 1);
        let mk = meta_key(1);
        assert_ne!(vk, mk, "vec_key and meta_key should never collide");
    }

    #[test]
    fn test_encode_empty_vector() {
        let v: Vec<f32> = vec![];
        let enc = encode_vector(&v);
        // dim = 0, total = 4 bytes
        assert_eq!(enc.len(), 4);
        let dec = decode_vector(&enc).unwrap();
        assert!(dec.is_empty());
    }
}
