/// FTS tokenizer for KKDB BM25 index.
///
/// Supports two modes:
/// - **Latin / ASCII / mixed**: splits on non-alphanumeric characters, lowercases.
/// - **CJK (Chinese / Japanese / Korean)**: routes through `jieba-rs` `cut_for_search`
///   which produces granular word-level and bi-gram tokens ideal for search recall.
use std::sync::OnceLock;

/// Lazy global Jieba instance (loads the built-in dictionary on first use).
static JIEBA: OnceLock<jieba_rs::Jieba> = OnceLock::new();

#[inline]
fn jieba() -> &'static jieba_rs::Jieba {
    JIEBA.get_or_init(jieba_rs::Jieba::new)
}

/// Returns true if the string contains at least one CJK Unified Ideograph.
#[inline]
fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32,
            0x4E00..=0x9FFF  // CJK Unified Ideographs
          | 0x3400..=0x4DBF  // CJK Extension A
          | 0x20000..=0x2A6DF // CJK Extension B
          | 0x2A700..=0x2CEAF // CJK Extensions C-F
          | 0xF900..=0xFAFF  // CJK Compatibility Ideographs
        )
    })
}

/// Tokenize text for FTS **document indexing** (write path).
///
/// Tokens are NOT deduplicated so that term frequencies (tf) are preserved.
/// Both jieba and ASCII paths lowercase everything.
pub fn simple_tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();

    if contains_cjk(&lower) {
        // Jieba cut_for_search: word-level + shorter sub-words.
        jieba()
            .cut_for_search(&lower, false)
            .into_iter()
            .filter_map(|s| {
                let t = s.trim().to_string();
                if t.is_empty() || t.chars().all(|c| !c.is_alphanumeric()) {
                    None
                } else {
                    Some(t)
                }
            })
            .collect()
    } else {
        // Latin / ASCII: split on non-alphanumeric characters.
        lower
            .split(|c: char| !c.is_alphanumeric())
            .filter_map(|s| {
                let t = s.trim().to_string();
                if t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            })
            .collect()
    }
}

/// Tokenize a **query string** for BM25 matching.
///
/// Identical to `simple_tokenize` but deduplicates tokens (important for
/// jieba which may emit the same sub-word multiple times, and for correctness
/// of BM25 where each unique query term should be scored exactly once).
pub fn query_tokenize(text: &str) -> Vec<String> {
    let mut tokens = simple_tokenize(text);
    let mut seen = std::collections::HashSet::with_capacity(tokens.len());
    tokens.retain(|t| seen.insert(t.clone()));
    tokens
}

/// Tokenize text and return a (token → per-row term frequency) map + total token count.
pub fn simple_tokenize_to_tf(text: &str) -> (std::collections::HashMap<String, u32>, u32) {
    let mut tf_map = std::collections::HashMap::new();
    let mut total = 0u32;
    for token in simple_tokenize(text) {
        *tf_map.entry(token).or_insert(0) += 1;
        total += 1;
    }
    (tf_map, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_tokenize_english() {
        let tokens = simple_tokenize("Hello, world! Great database engine.");
        assert_eq!(
            tokens,
            vec!["hello", "world", "great", "database", "engine"]
        );
    }

    #[test]
    fn test_simple_tokenize_preserves_tf() {
        // Deduplication must NOT happen in document indexing path
        let tokens = simple_tokenize("the cat sat on the mat");
        assert_eq!(
            tokens.iter().filter(|t| t.as_str() == "the").count(),
            2,
            "Document tokenizer must preserve duplicate 'the' for tf counting"
        );
    }

    #[test]
    fn test_query_tokenize_deduplicates() {
        // Query path must deduplicate
        let tokens = query_tokenize("the cat the");
        assert_eq!(
            tokens.iter().filter(|t| t.as_str() == "the").count(),
            1,
            "Query tokenizer must deduplicate 'the'"
        );
    }

    #[test]
    fn test_simple_tokenize_chinese_jieba() {
        // jieba should segment "数据库引擎" into component words
        let tokens = simple_tokenize("数据库引擎");
        assert!(
            tokens.contains(&"数据库".to_string()) || tokens.contains(&"数据".to_string()),
            "Expected '数据库' or '数据' in {:?}",
            tokens
        );
        assert!(
            tokens.contains(&"引擎".to_string()),
            "Expected '引擎' in {:?}",
            tokens
        );
    }

    #[test]
    fn test_simple_tokenize_chinese_with_punct() {
        let tokens = simple_tokenize("数据库，引擎！BM25");
        assert!(
            tokens.iter().any(|t| t.contains("数据")),
            "Expected a '数据' fragment in {:?}",
            tokens
        );
        assert!(
            tokens.contains(&"bm25".to_string()),
            "Expected 'bm25' in {:?}",
            tokens
        );
    }

    #[test]
    fn test_simple_tokenize_tf() {
        let (tf, total) = simple_tokenize_to_tf("the cat sat on the mat");
        assert_eq!(total, 6);
        assert_eq!(tf["the"], 2);
        assert_eq!(tf["cat"], 1);
    }

    #[test]
    fn test_simple_tokenize_mixed() {
        // Mixed Chinese + English
        let tokens = simple_tokenize("Rust 数据库 engine");
        assert!(tokens.contains(&"rust".to_string()), "Got: {:?}", tokens);
        assert!(tokens.contains(&"engine".to_string()), "Got: {:?}", tokens);
    }

    // ── New coverage tests ──────────────────────────────────────────────

    #[test]
    fn test_simple_tokenize_empty() {
        assert!(simple_tokenize("").is_empty());
    }

    #[test]
    fn test_simple_tokenize_only_punctuation() {
        assert!(simple_tokenize("!!! @@@ ### ??? ...").is_empty());
    }

    #[test]
    fn test_query_tokenize_empty() {
        assert!(query_tokenize("").is_empty());
    }

    #[test]
    fn test_simple_tokenize_emoji() {
        // Emojis should not cause panic; they are filtered as non-alphanumeric
        let tokens = simple_tokenize("hello 🎉 world 🌍");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
    }

    #[test]
    fn test_simple_tokenize_sql_injection_chars() {
        let tokens = simple_tokenize("'; DROP TABLE users; --");
        // Should not panic, just tokenize the alphanumeric parts
        assert!(tokens.contains(&"drop".to_string()));
        assert!(tokens.contains(&"table".to_string()));
        assert!(tokens.contains(&"users".to_string()));
    }

    #[test]
    fn test_simple_tokenize_to_tf_empty() {
        let (tf, total) = simple_tokenize_to_tf("");
        assert!(tf.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn test_contains_cjk_basic() {
        assert!(contains_cjk("你好"));
        assert!(contains_cjk("hello 你好 world"));
        assert!(!contains_cjk("hello world"));
        assert!(!contains_cjk(""));
    }

    #[test]
    fn test_query_tokenize_mixed_cjk_dedup() {
        // CJK + English, same token repeated
        let tokens = query_tokenize("数据库 数据库 engine engine");
        // Each unique token should appear at most once
        let counts: std::collections::HashMap<&str, usize> = tokens
            .iter()
            .fold(std::collections::HashMap::new(), |mut m, t| {
                *m.entry(t.as_str()).or_insert(0) += 1;
                m
            });
        for count in counts.values() {
            assert_eq!(*count, 1, "query_tokenize should deduplicate");
        }
    }
}
