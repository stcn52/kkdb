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
const SEP_INDEX: u8 = 0x02;               // Separator between index id and token
const SEP_TOKEN: u8 = 0x03;               // Separator between token and row id
const META_SUFFIX: &[u8] = b"META";       // Marks a term's document-frequency metadata entry
const GLOBAL_MARKER: &[u8] = b"GLOBAL";   // Marks the global stats key for the entire index

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
    if buf.len() < 8 { return None; }
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
    if buf.len() < 8 { return None; }
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
    if buf.len() < 16 { return None; }
    let total_docs = u64::from_be_bytes(buf[0..8].try_into().ok()?);
    let total_field_len = u64::from_be_bytes(buf[8..16].try_into().ok()?);
    Some((total_docs, total_field_len))
}

// ─── BM25 Scorer ─────────────────────────────────────────────────────────────

/// BM25 parameters (standard empirical defaults).
pub const BM25_K1: f64 = 1.2;
pub const BM25_B: f64  = 0.75;

/// Computes the BM25 score contribution of a single term for a single document.
///
/// # Arguments
/// - `tf`: term frequency in the document
/// - `field_len`: total token count in the document's indexed field
/// - `doc_freq`: number of documents containing this term in the index
/// - `total_docs`: total number of documents in the index
/// - `avgdl`: average document length across the entire index
pub fn bm25_score(tf: u32, field_len: u32, doc_freq: u64, total_docs: u64, avgdl: f64) -> f64 {
    if total_docs == 0 || doc_freq == 0 { return 0.0; }

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
        assert!(score_high_tf > score, "Higher TF should give higher BM25 score");
        
        // A term that appears in all documents gets near-zero or negative IDF
        // (doc_freq == total_docs)
        let score_common = bm25_score(1, 10, 100, 100, 10.0);
        // IDF = ln(0.5/100.5 + 1) which approaches 0
        assert!(score_common >= 0.0, "Very common term should have low but non-negative score");
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
}
