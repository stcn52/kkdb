/// Full-Text Search Inverted Index Storage Layer
///
/// This module defines the B-Tree key encoding schema for storing postings lists,
/// term frequency metadata, and global index statistics, enabling BM25 scoring.
///
/// # B-Tree Key Schema
///
/// For each full-text index (Index ID: `I`):
///
/// - **Postings List Entry**:
///   - Key:   `\x00FTS\x01{I}\x02{Token}\x03{RowID}`
///   - Value: `[tf: u32, field_len: u32]` (8 bytes)
///
/// - **Term Document Frequency** (used for IDF in BM25):
///   - Key:   `\x00FTS\x01{I}\x02{Token}\x03META`
///   - Value: `[doc_freq: u64]` (8 bytes)
///
/// - **Global Index Statistics** (avgdl = total_len / total_docs):
///   - Key:   `\x00FTS\x01{I}\x03GLOBAL`
///   - Value: `[total_docs: u64, total_field_len: u64]` (16 bytes)
use std::collections::HashMap;

// ─── Key Encoding Helpers ────────────────────────────────────────────────────

const FTS_PREFIX: &[u8] = b"\x00FTS\x01"; // Namespace prefix to avoid collision with user data
const SEP_INDEX: u8 = 0x02; // Separator between index id and token
const SEP_TOKEN: u8 = 0x03; // Separator between token and row id
const META_SUFFIX: &[u8] = b"META"; // Marks a term's document-frequency metadata entry
const GLOBAL_MARKER: &[u8] = b"GLOBAL"; // Marks the global stats key for the entire index

/// Returns the B-Tree key for a Postings List entry.
pub fn posting_key(index_id: u32, token: &str, row_id: u64) -> Vec<u8> {
    let mut key = FTS_PREFIX.to_vec();
    key.extend_from_slice(&index_id.to_be_bytes());
    key.push(SEP_INDEX);
    key.extend_from_slice(token.as_bytes());
    key.push(SEP_TOKEN);
    key.extend_from_slice(&row_id.to_be_bytes());
    key
}

/// Returns the B-Tree key for a Term's Document Frequency (DF) metadata.
pub fn term_meta_key(index_id: u32, token: &str) -> Vec<u8> {
    let mut key = FTS_PREFIX.to_vec();
    key.extend_from_slice(&index_id.to_be_bytes());
    key.push(SEP_INDEX);
    key.extend_from_slice(token.as_bytes());
    key.push(SEP_TOKEN);
    key.extend_from_slice(META_SUFFIX);
    key
}

/// Returns the B-Tree key for Global Index Statistics.
pub fn global_stats_key(index_id: u32) -> Vec<u8> {
    let mut key = FTS_PREFIX.to_vec();
    key.extend_from_slice(&index_id.to_be_bytes());
    key.push(SEP_TOKEN);
    key.extend_from_slice(GLOBAL_MARKER);
    key
}

/// Returns the B-Tree prefix for scanning all postings for a given token in an index.
/// Use this for prefix iteration to find all rows that contain a token.
pub fn token_scan_prefix(index_id: u32, token: &str) -> Vec<u8> {
    let mut key = FTS_PREFIX.to_vec();
    key.extend_from_slice(&index_id.to_be_bytes());
    key.push(SEP_INDEX);
    key.extend_from_slice(token.as_bytes());
    key.push(SEP_TOKEN);
    key
}

// ─── Value Encoding Helpers ──────────────────────────────────────────────────

/// Encodes a postings list entry value as 8 bytes.
/// `[tf (u32), field_len (u32)]`
pub fn encode_posting_value(tf: u32, field_len: u32) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[0..4].copy_from_slice(&tf.to_be_bytes());
    buf[4..8].copy_from_slice(&field_len.to_be_bytes());
    buf
}

/// Decodes a postings list entry value from 8 bytes.
pub fn decode_posting_value(buf: &[u8]) -> Option<(u32, u32)> {
    if buf.len() < 8 {
        return None;
    }
    let tf = u32::from_be_bytes(buf[0..4].try_into().ok()?);
    let field_len = u32::from_be_bytes(buf[4..8].try_into().ok()?);
    Some((tf, field_len))
}

/// Encodes a term document frequency value as 8 bytes.
pub fn encode_doc_freq(doc_freq: u64) -> [u8; 8] {
    doc_freq.to_be_bytes()
}

/// Decodes a term document frequency value from 8 bytes.
pub fn decode_doc_freq(buf: &[u8]) -> Option<u64> {
    if buf.len() < 8 {
        return None;
    }
    Some(u64::from_be_bytes(buf[0..8].try_into().ok()?))
}

/// Encodes global index statistics as 16 bytes.
/// `[total_docs (u64), total_field_len (u64)]`
pub fn encode_global_stats(total_docs: u64, total_field_len: u64) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&total_docs.to_be_bytes());
    buf[8..16].copy_from_slice(&total_field_len.to_be_bytes());
    buf
}

/// Decodes global index statistics from 16 bytes.
pub fn decode_global_stats(buf: &[u8]) -> Option<(u64, u64)> {
    if buf.len() < 16 {
        return None;
    }
    let total_docs = u64::from_be_bytes(buf[0..8].try_into().ok()?);
    let total_field_len = u64::from_be_bytes(buf[8..16].try_into().ok()?);
    Some((total_docs, total_field_len))
}

// ─── BM25 Scorer ─────────────────────────────────────────────────────────────

/// BM25 parameters (standard empirical defaults).
pub const BM25_K1: f64 = 1.2;
pub const BM25_B: f64 = 0.75;

/// Computes the BM25 score contribution of a single term for a single document.
///
/// # Arguments
/// - `tf`: term frequency in the document
/// - `field_len`: total token count in the document's indexed field
/// - `doc_freq`: number of documents containing this term in the index
/// - `total_docs`: total number of documents in the index
/// - `avgdl`: average document length across the entire index
pub fn bm25_score(tf: u32, field_len: u32, doc_freq: u64, total_docs: u64, avgdl: f64) -> f64 {
    if total_docs == 0 || doc_freq == 0 || avgdl <= 0.0 {
        return 0.0;
    }

    let n = total_docs as f64;
    let df = doc_freq as f64;
    let tf = tf as f64;
    let dl = field_len as f64;

    // IDF component: smoothed BM25 IDF
    let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();

    // TF normalization component
    let tf_norm = (tf * (BM25_K1 + 1.0)) / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * (dl / avgdl)));

    idf * tf_norm
}

/// Merges scores from multiple term scans into a single ordered result.
/// Returns a Vec of `(row_id, total_score)` sorted by score descending.
pub fn aggregate_scores(score_map: HashMap<u64, f64>) -> Vec<(u64, f64)> {
    let mut pairs: Vec<(u64, f64)> = score_map.into_iter().collect();
    pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_encoding_round_trip() {
        let posting = posting_key(42, "hello", 99);

        // Ensure prefix is correct
        assert!(posting.starts_with(FTS_PREFIX));

        // Term meta key for same token must share the same token prefix
        let meta = term_meta_key(42, "hello");
        let scan_pfx = token_scan_prefix(42, "hello");

        assert!(posting.starts_with(&scan_pfx));
        assert!(meta.starts_with(&scan_pfx));
        // meta != posting (different suffix: META vs row_id bytes)
        assert_ne!(posting, meta);
    }

    #[test]
    fn test_posting_value_round_trip() {
        let encoded = encode_posting_value(7, 42);
        let (tf, field_len) = decode_posting_value(&encoded).unwrap();
        assert_eq!(tf, 7);
        assert_eq!(field_len, 42);
    }

    #[test]
    fn test_global_stats_round_trip() {
        let encoded = encode_global_stats(1000, 50000);
        let (docs, total_len) = decode_global_stats(&encoded).unwrap();
        assert_eq!(docs, 1000);
        assert_eq!(total_len, 50000);
    }

    #[test]
    fn test_bm25_score_basic() {
        // A term that appears once in a doc of average length should get a positive score
        let score = bm25_score(1, 10, 5, 100, 10.0);
        assert!(score > 0.0, "BM25 score should be positive");

        // A term that appears more frequently gets a higher score (TF effect)
        let score_high_tf = bm25_score(5, 10, 5, 100, 10.0);
        assert!(
            score_high_tf > score,
            "Higher TF should give higher BM25 score"
        );

        // A term that appears in all documents gets near-zero or negative IDF
        // (doc_freq == total_docs)
        let score_common = bm25_score(1, 10, 100, 100, 10.0);
        // IDF = ln(0.5/100.5 + 1) which approaches 0
        assert!(
            score_common >= 0.0,
            "Very common term should have low but non-negative score"
        );
    }

    #[test]
    fn test_aggregate_scores_sorted() {
        let mut score_map = HashMap::new();
        score_map.insert(1u64, 0.5f64);
        score_map.insert(2u64, 2.3f64);
        score_map.insert(3u64, 1.1f64);

        let result = aggregate_scores(score_map);
        // Should be sorted descending
        assert_eq!(result[0].0, 2); // row 2 had highest score
        assert_eq!(result[1].0, 3);
        assert_eq!(result[2].0, 1);
    }

    // ── New coverage tests ──────────────────────────────────────────────

    #[test]
    fn test_decode_posting_value_short_buffer() {
        assert!(decode_posting_value(&[]).is_none());
        assert!(decode_posting_value(&[0u8; 7]).is_none());
        assert!(decode_posting_value(&[0u8; 8]).is_some()); // exact boundary
    }

    #[test]
    fn test_decode_doc_freq_short_buffer() {
        assert!(decode_doc_freq(&[]).is_none());
        assert!(decode_doc_freq(&[0u8; 7]).is_none());
        assert!(decode_doc_freq(&[0u8; 8]).is_some());
    }

    #[test]
    fn test_decode_global_stats_short_buffer() {
        assert!(decode_global_stats(&[]).is_none());
        assert!(decode_global_stats(&[0u8; 15]).is_none());
        assert!(decode_global_stats(&[0u8; 16]).is_some());
    }

    #[test]
    fn test_encode_doc_freq_round_trip() {
        for &v in &[0u64, 1, 42, u64::MAX] {
            let buf = encode_doc_freq(v);
            assert_eq!(decode_doc_freq(&buf), Some(v));
        }
    }

    #[test]
    fn test_bm25_edge_cases() {
        // total_docs = 0
        assert_eq!(bm25_score(1, 10, 5, 0, 10.0), 0.0);
        // doc_freq = 0
        assert_eq!(bm25_score(1, 10, 0, 100, 10.0), 0.0);
        // avgdl = 0
        assert_eq!(bm25_score(1, 10, 5, 100, 0.0), 0.0);
        // avgdl negative
        assert_eq!(bm25_score(1, 10, 5, 100, -1.0), 0.0);
        // tf = 0: score should be 0 because there's no term occurrence
        let score = bm25_score(0, 10, 5, 100, 10.0);
        assert!(score.abs() < 1e-10, "tf=0 should produce ~0 score");
        // Very high tf: should not overflow
        let score = bm25_score(100_000, 200_000, 5, 100, 10.0);
        assert!(score.is_finite(), "high tf should produce finite score");
    }

    #[test]
    fn test_aggregate_scores_empty() {
        let result = aggregate_scores(HashMap::new());
        assert!(result.is_empty());
    }

    #[test]
    fn test_token_scan_prefix_no_collision() {
        // Different index_id + same token should produce different prefixes
        let p1 = token_scan_prefix(1, "hello");
        let p2 = token_scan_prefix(2, "hello");
        assert_ne!(p1, p2);

        // Same index_id + different token should produce different prefixes
        let p3 = token_scan_prefix(1, "world");
        assert_ne!(p1, p3);
    }

    // ── Additional coverage tests (round 3) ─────────────────────────────

    #[test]
    fn test_global_stats_key_format() {
        let key = global_stats_key(42);
        // Must start with FTS_PREFIX
        assert!(key.starts_with(FTS_PREFIX));
        // After prefix: 4 bytes of index_id + SEP_TOKEN + GLOBAL_MARKER
        let after_prefix = &key[FTS_PREFIX.len()..];
        assert_eq!(&after_prefix[0..4], &42u32.to_be_bytes());
        assert_eq!(after_prefix[4], SEP_TOKEN);
        assert_eq!(&after_prefix[5..], GLOBAL_MARKER);
    }

    #[test]
    fn test_term_meta_key_full_format() {
        let key = term_meta_key(7, "rust");
        let after_prefix = &key[FTS_PREFIX.len()..];
        // index_id bytes
        assert_eq!(&after_prefix[0..4], &7u32.to_be_bytes());
        // SEP_INDEX
        assert_eq!(after_prefix[4], SEP_INDEX);
        // token bytes
        let token_start = 5;
        let token_end = token_start + "rust".len();
        assert_eq!(&after_prefix[token_start..token_end], b"rust");
        // SEP_TOKEN
        assert_eq!(after_prefix[token_end], SEP_TOKEN);
        // META_SUFFIX
        assert_eq!(&after_prefix[token_end + 1..], META_SUFFIX);
    }

    #[test]
    fn test_posting_key_full_format() {
        let key = posting_key(3, "db", 999);
        let after_prefix = &key[FTS_PREFIX.len()..];
        assert_eq!(&after_prefix[0..4], &3u32.to_be_bytes());
        assert_eq!(after_prefix[4], SEP_INDEX);
        assert_eq!(&after_prefix[5..7], b"db");
        assert_eq!(after_prefix[7], SEP_TOKEN);
        assert_eq!(&after_prefix[8..], &999u64.to_be_bytes());
    }

    #[test]
    fn test_unicode_token_key_encoding() {
        let key = posting_key(1, "中文", 100);
        assert!(key.starts_with(FTS_PREFIX));
        // Should contain UTF-8 bytes of "中文"
        let after_prefix = &key[FTS_PREFIX.len()..];
        let token_bytes = "中文".as_bytes();
        let token_start = 5; // 4 bytes index_id + 1 byte SEP_INDEX
        assert_eq!(&after_prefix[token_start..token_start + token_bytes.len()], token_bytes);
    }

    #[test]
    fn test_empty_token_key_encoding() {
        let key = posting_key(1, "", 10);
        // Empty token: after index_id + SEP_INDEX, immediately SEP_TOKEN + row_id
        let after_prefix = &key[FTS_PREFIX.len()..];
        assert_eq!(after_prefix[4], SEP_INDEX);
        assert_eq!(after_prefix[5], SEP_TOKEN); // no token bytes between separators
        assert_eq!(&after_prefix[6..], &10u64.to_be_bytes());
    }

    #[test]
    fn test_decode_posting_value_overlong_buffer() {
        // Extra trailing bytes should still decode correctly
        let mut buf = encode_posting_value(10, 20).to_vec();
        buf.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        let (tf, field_len) = decode_posting_value(&buf).unwrap();
        assert_eq!(tf, 10);
        assert_eq!(field_len, 20);
    }

    #[test]
    fn test_decode_doc_freq_overlong_buffer() {
        let mut buf = encode_doc_freq(12345).to_vec();
        buf.extend_from_slice(&[0xAA; 8]);
        assert_eq!(decode_doc_freq(&buf), Some(12345));
    }

    #[test]
    fn test_decode_global_stats_overlong_buffer() {
        let mut buf = encode_global_stats(100, 5000).to_vec();
        buf.extend_from_slice(&[0xBB; 4]);
        assert_eq!(decode_global_stats(&buf), Some((100, 5000)));
    }

    #[test]
    fn test_bm25_score_numerical_accuracy() {
        // Manual calculation: tf=2, field_len=10, doc_freq=10, total_docs=100, avgdl=10.0
        // IDF = ln((100 - 10 + 0.5) / (10 + 0.5) + 1.0) = ln(90.5/10.5 + 1.0) = ln(9.619..) ≈ 2.2636
        // TF_norm = (2 * (1.2+1)) / (2 + 1.2*(1 - 0.75 + 0.75*(10/10))) = (2*2.2)/(2+1.2) = 4.4/3.2 = 1.375
        // score = IDF * tf_norm ≈ 2.2636 * 1.375 ≈ 3.1125
        let score = bm25_score(2, 10, 10, 100, 10.0);
        // Recompute IDF more precisely:
        let idf = ((100.0 - 10.0 + 0.5) / (10.0 + 0.5) + 1.0_f64).ln();
        let tf_norm = (2.0 * (BM25_K1 + 1.0)) / (2.0 + BM25_K1 * (1.0 - BM25_B + BM25_B * (10.0 / 10.0)));
        let expected = idf * tf_norm;
        assert!((score - expected).abs() < 1e-10, "BM25 numerical mismatch: {} vs {}", score, expected);
    }

    #[test]
    fn test_bm25_short_doc_vs_long_doc() {
        // Same tf, same term stats, but different field_len
        // Shorter doc should get higher score (BM25 length normalization)
        let short = bm25_score(3, 5, 10, 100, 10.0);
        let long = bm25_score(3, 50, 10, 100, 10.0);
        assert!(short > long, "shorter doc should score higher: {} vs {}", short, long);
    }

    #[test]
    fn test_bm25_field_len_zero() {
        // field_len = 0: doc length is 0, should still be finite
        let score = bm25_score(1, 0, 5, 100, 10.0);
        assert!(score.is_finite());
        assert!(score > 0.0); // dl=0 makes length normalization favorable
    }

    #[test]
    fn test_bm25_field_len_max() {
        let score = bm25_score(1, u32::MAX, 5, 100, 10.0);
        assert!(score.is_finite());
        // Very long doc should have very low score
        assert!(score > 0.0);
    }

    #[test]
    fn test_aggregate_scores_same_scores() {
        let mut score_map = HashMap::new();
        score_map.insert(10u64, 1.5f64);
        score_map.insert(20u64, 1.5f64);
        score_map.insert(30u64, 1.5f64);
        let result = aggregate_scores(score_map);
        assert_eq!(result.len(), 3);
        // All scores equal, so all should be 1.5
        for (_, score) in &result {
            assert!((*score - 1.5).abs() < 1e-10);
        }
    }

    #[test]
    fn test_aggregate_scores_nan_handling() {
        let mut score_map = HashMap::new();
        score_map.insert(1u64, f64::NAN);
        score_map.insert(2u64, 1.0);
        score_map.insert(3u64, 2.0);
        // Should not panic — NaN comparisons fall back to Equal
        let result = aggregate_scores(score_map);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_key_lexicographic_order() {
        // posting_key for token "hello" with different row_ids should be in order
        let k1 = posting_key(1, "hello", 1);
        let k2 = posting_key(1, "hello", 2);
        let k3 = posting_key(1, "hello", 100);
        assert!(k1 < k2);
        assert!(k2 < k3);
    }

    #[test]
    fn test_posting_key_before_meta_key() {
        // For BTree scan correctness: posting keys for a token must come before meta key
        // posting_key ends with row_id bytes, term_meta_key ends with META_SUFFIX ("META")
        // Row IDs are 8-byte big-endian. For small IDs, they start with 0x00..., which is < 'M' (0x4D)
        let pk = posting_key(1, "hello", 1);
        let mk = term_meta_key(1, "hello");
        assert!(pk < mk, "posting key should be lexicographically before meta key for small row_ids");
    }

    #[test]
    fn test_posting_value_boundary_values() {
        // Test with extreme values
        let encoded = encode_posting_value(u32::MAX, u32::MAX);
        let (tf, fl) = decode_posting_value(&encoded).unwrap();
        assert_eq!(tf, u32::MAX);
        assert_eq!(fl, u32::MAX);

        let encoded = encode_posting_value(0, 0);
        let (tf, fl) = decode_posting_value(&encoded).unwrap();
        assert_eq!(tf, 0);
        assert_eq!(fl, 0);
    }

    #[test]
    fn test_global_stats_boundary_values() {
        let encoded = encode_global_stats(u64::MAX, u64::MAX);
        let (docs, total) = decode_global_stats(&encoded).unwrap();
        assert_eq!(docs, u64::MAX);
        assert_eq!(total, u64::MAX);
    }
}
