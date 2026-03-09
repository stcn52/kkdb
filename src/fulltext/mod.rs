use std::collections::HashMap;
use tokenizers::Tokenizer;

pub mod index;
pub mod tokenizer;

/// A wrapper around Hugging Face's Tokenizer for extracting Term Frequencies (TF).
pub struct FullTextTokenizer {
    inner: Tokenizer,
}

impl FullTextTokenizer {
    /// Creates a tokenizer from a JSON string configuration mapping.
    /// This supports BPE, WordPiece, or Unigram configurations.
    pub fn from_json(json: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        use std::str::FromStr;
        let inner = Tokenizer::from_str(json)?;
        Ok(Self { inner })
    }

    /// Creates a fallback out-of-the-box Tokenizer.
    /// It lowercases the text and splits it by whitespace and basic punctuation.
    pub fn create_default() -> Self {
        use tokenizers::models::wordlevel::WordLevel;
        use tokenizers::normalizers::Lowercase;
        use tokenizers::pre_tokenizers::whitespace::Whitespace;

        // An empty vocabulary. In a real Full-Text engine, out-of-vocabulary (OOV)
        // isn't actually an error unless we enforce strict ID mapping.
        // For BM25, we just want the string tokens themselves!
        // The tokenizers crate uses ahash::AHashMap for performance.
        let empty_vocab = std::collections::HashMap::new()
            .into_iter()
            .collect::<ahash::AHashMap<String, u32>>();

        let model = WordLevel::builder()
            .vocab(empty_vocab)
            .unk_token(String::from("[UNK]"))
            .build()
            .unwrap();

        let mut tokenizer = Tokenizer::new(model);
        tokenizer.with_normalizer(Some(Lowercase));
        // Replace punctuation with spaces, then split on whitespace
        tokenizer.with_pre_tokenizer(Some(Whitespace));

        Self { inner: tokenizer }
    }

    /// Tokenizes the input text and returns a map of (Token String -> Frequency Count)
    /// and the total number of tokens parsed.
    pub fn tokenize_to_tf(&self, text: &str) -> (HashMap<String, u32>, u32) {
        let mut tf_map = HashMap::new();
        let mut total_tokens = 0;

        if text.trim().is_empty() {
            return (tf_map, total_tokens);
        }

        // 1. Try to fully encode using the inner tokenizer (for BPE/WordPiece json loaded)
        // If it parses and gives us tokens (meaning it has a real vocabulary), we use it.
        // NOTE: For the programmatic fallback with an empty vocab, `encode` will just map
        // everything to `[UNK]` and might smash words together.
        if self.inner.get_vocab_size(false) > 0 {
            if let Ok(encoding) = self.inner.encode(text, false) {
                let offsets = encoding.get_offsets();
                for &(start, end) in offsets {
                    if start == end {
                        continue;
                    }
                    let token_str = text[start..end].to_lowercase();
                    if token_str.trim().is_empty() {
                        continue;
                    }
                    *tf_map.entry(token_str).or_insert(0) += 1;
                    total_tokens += 1;
                }
                return (tf_map, total_tokens);
            }
        }

        // 2. Fallback for the default programmatic tokenizer (empty vocabulary):
        //    Split on any character that is NOT a Unicode letter or digit.
        //    This correctly handles ASCII punctuation, Chinese/Japanese/Korean punctuation
        //    (e.g. ，。！？), full-width symbols, emoji, and other Unicode delimiters.
        let split_iter = text.split(|c: char| !c.is_alphanumeric());
        for split_str in split_iter {
            let token_str = split_str.trim().to_lowercase();
            if token_str.is_empty() {
                continue;
            }
            *tf_map.entry(token_str).or_insert(0) += 1;
            total_tokens += 1;
        }

        (tf_map, total_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_fallback_default() {
        let tokenizer = FullTextTokenizer::create_default();

        // Unicode-aware split: splits on any non-alphanumeric character.
        // "Hello,   world! Beautiful database Engine 🚀"
        //   -> "hello", "world", "beautiful", "database", "engine"
        // Emoji 🚀 is not alphanumeric -> split boundary, gives empty string -> filtered out.
        let text = "Hello,   world! Beautiful database Engine 🚀";
        let (tf, total) = tokenizer.tokenize_to_tf(text);

        assert_eq!(total, 5); // 5 word tokens, emoji is a delimiter and gets filtered out
        assert_eq!(tf.get("hello"), Some(&1));
        assert_eq!(tf.get("world"), Some(&1));
        assert_eq!(tf.get("beautiful"), Some(&1));
        assert_eq!(tf.get("database"), Some(&1));
        assert_eq!(tf.get("engine"), Some(&1));
        assert_eq!(tf.get("🚀"), None); // Emoji is non-alphanumeric -> split boundary, not a token
        assert_eq!(tf.get("missing"), None);
    }

    #[test]
    fn test_tokenize_fallback_unicode_punctuation() {
        let tokenizer = FullTextTokenizer::create_default();

        // Chinese punctuation like ，。！？ are non-alphanumeric and correctly used as delimiters
        let text = "数据库，引擎！分布式系统";
        let (tf, _total) = tokenizer.tokenize_to_tf(text);

        // Each Chinese word segment is extracted as a token
        assert!(tf.contains_key("数据库"));
        assert!(tf.contains_key("引擎"));
        assert!(tf.contains_key("分布式系统"));
        // Punctuation chars are NOT tokens
        assert_eq!(tf.get("，"), None);
        assert_eq!(tf.get("！"), None);
    }

    // A minimal dummy BPE tokenizer JSON for testing purposes without needing external files.
    const DUMMY_TOKENIZER_JSON: &str = r#"{
      "version": "1.0",
      "truncation": null,
      "padding": null,
      "added_tokens": [],
      "normalizer": {
        "type": "Lowercase"
      },
      "pre_tokenizer": {
        "type": "Whitespace"
      },
      "post_processor": null,
      "decoder": null,
      "model": {
        "type": "WordLevel",
        "vocab": {
          "hello": 0,
          "world": 1,
          "database": 2,
          "engine": 3
        },
        "unk_token": "[UNK]"
      }
    }"#;

    #[test]
    fn test_tokenize_to_tf_basic() {
        let tokenizer =
            FullTextTokenizer::from_json(DUMMY_TOKENIZER_JSON).expect("Failed to parse dummy JSON");

        let text = "Hello world database Engine engine";
        let (tf, total) = tokenizer.tokenize_to_tf(text);

        // Whitespace pre-tokenizer splits by space, Lowercase normalizer makes it all lowercase.
        // Therefore: "hello", "world", "database", "engine", "engine"
        assert_eq!(total, 5);
        assert_eq!(tf.get("hello"), Some(&1));
        assert_eq!(tf.get("world"), Some(&1));
        assert_eq!(tf.get("database"), Some(&1));
        assert_eq!(tf.get("engine"), Some(&2)); // Appeared twice
        assert_eq!(tf.get("missing"), None);
    }
}
