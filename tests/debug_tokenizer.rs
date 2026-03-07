use kkdb::fulltext::FullTextTokenizer;

#[test]
fn test_integration_tokenize_default() {
    let tokenizer = FullTextTokenizer::create_default();
    let text = "Hello,   world! Beautiful database Engine 🚀";
    let (tf, total) = tokenizer.tokenize_to_tf(text);

    println!("TF MAP ({total} tokens): {tf:#?}");

    assert_eq!(total, 5);
    assert!(tf.contains_key("database"));
    assert!(tf.contains_key("engine"));
}
