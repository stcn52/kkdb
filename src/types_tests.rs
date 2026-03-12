use super::*;

// ---- DataType ----

#[test]
fn test_datatype_display() {
    assert_eq!(format!("{}", DataType::Null), "NULL");
    assert_eq!(format!("{}", DataType::Integer), "INTEGER");
    assert_eq!(format!("{}", DataType::Real), "REAL");
    assert_eq!(format!("{}", DataType::Text), "TEXT");
    assert_eq!(format!("{}", DataType::Blob), "BLOB");
}

#[test]
fn test_datatype_from_str() {
    assert_eq!(DataType::from_str("INTEGER"), DataType::Integer);
    assert_eq!(DataType::from_str("INT"), DataType::Integer);
    assert_eq!(DataType::from_str("BIGINT"), DataType::Integer);
    assert_eq!(DataType::from_str("SMALLINT"), DataType::Integer);
    assert_eq!(DataType::from_str("TINYINT"), DataType::Integer);
    assert_eq!(DataType::from_str("REAL"), DataType::Real);
    assert_eq!(DataType::from_str("FLOAT"), DataType::Real);
    assert_eq!(DataType::from_str("DOUBLE"), DataType::Real);
    assert_eq!(DataType::from_str("TEXT"), DataType::Text);
    assert_eq!(DataType::from_str("VARCHAR"), DataType::Text);
    assert_eq!(DataType::from_str("CHAR"), DataType::Text);
    assert_eq!(DataType::from_str("STRING"), DataType::Text);
    assert_eq!(DataType::from_str("CLOB"), DataType::Text);
    assert_eq!(DataType::from_str("BLOB"), DataType::Blob);
    assert_eq!(DataType::from_str("BINARY"), DataType::Blob);
    assert_eq!(DataType::from_str("VARBINARY"), DataType::Blob);
    assert_eq!(DataType::from_str("unknown"), DataType::Text); // default
    assert_eq!(DataType::from_str("integer"), DataType::Integer); // case insensitive
}

#[test]
fn test_datatype_eq() {
    assert_eq!(DataType::Integer, DataType::Integer);
    assert_ne!(DataType::Integer, DataType::Text);
}

// ---- Value serialization/deserialization ----

#[test]
fn test_serialize_null() {
    let v = Value::Null;
    let bytes = v.serialize();
    assert_eq!(bytes, vec![0x00]);
    let (deserialized, consumed) = Value::deserialize(&bytes).unwrap();
    assert_eq!(consumed, 1);
    assert_eq!(deserialized, Value::Null);
}

#[test]
fn test_serialize_integer() {
    let v = Value::Integer(42);
    let bytes = v.serialize();
    assert_eq!(bytes[0], 0x01);
    assert_eq!(bytes.len(), 2); // 1 byte tag + 1 byte varint for zigzag(42)
    let (deserialized, consumed) = Value::deserialize(&bytes).unwrap();
    assert_eq!(consumed, 2);
    assert_eq!(deserialized, Value::Integer(42));
}

#[test]
fn test_serialize_negative_integer() {
    let v = Value::Integer(-1000);
    let bytes = v.serialize();
    let (deserialized, _) = Value::deserialize(&bytes).unwrap();
    assert_eq!(deserialized, Value::Integer(-1000));
}

#[test]
#[allow(clippy::approx_constant)]
fn test_serialize_real() {
    let v = Value::Real(3.14);
    let bytes = v.serialize();
    assert_eq!(bytes[0], 0x02);
    assert_eq!(bytes.len(), 9);
    let (deserialized, consumed) = Value::deserialize(&bytes).unwrap();
    assert_eq!(consumed, 9);
    assert_eq!(deserialized, Value::Real(3.14));
}

#[test]
fn test_serialize_text() {
    let v = Value::Text("hello".into());
    let bytes = v.serialize();
    assert_eq!(bytes[0], 0x03);
    let (deserialized, consumed) = Value::deserialize(&bytes).unwrap();
    assert_eq!(consumed, 1 + 1 + 5); // 1 tag + 1 varint len + 5 data
    assert_eq!(deserialized, Value::Text("hello".into()));
}

#[test]
fn test_serialize_empty_text() {
    let v = Value::Text("".into());
    let bytes = v.serialize();
    let (deserialized, consumed) = Value::deserialize(&bytes).unwrap();
    assert_eq!(consumed, 1 + 1); // 1 tag + 1 varint len
    assert_eq!(deserialized, Value::Text("".into()));
}

#[test]
fn test_serialize_blob() {
    let v = Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]);
    let bytes = v.serialize();
    assert_eq!(bytes[0], 0x04);
    let (deserialized, consumed) = Value::deserialize(&bytes).unwrap();
    assert_eq!(consumed, 1 + 1 + 4); // 1 tag + 1 varint len + 4 bytes
    assert_eq!(deserialized, Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]));
}

#[test]
fn test_serialize_empty_blob() {
    let v = Value::Blob(vec![]);
    let bytes = v.serialize();
    let (deserialized, _) = Value::deserialize(&bytes).unwrap();
    assert_eq!(deserialized, Value::Blob(vec![]));
}

#[test]
fn test_deserialize_empty_data() {
    let result = Value::deserialize(&[]);
    assert!(result.is_err());
}

#[test]
fn test_deserialize_unknown_tag() {
    let result = Value::deserialize(&[0xFF]);
    assert!(result.is_err());
}

#[test]
fn test_deserialize_truncated_integer() {
    let result = Value::deserialize(&[0x01, 0x80]); // 0x80 expects more bytes
    assert!(result.is_err());
}

#[test]
fn test_deserialize_truncated_real() {
    let result = Value::deserialize(&[0x02, 0x00, 0x00]); // too short
    assert!(result.is_err());
}

#[test]
fn test_deserialize_truncated_text_length() {
    let result = Value::deserialize(&[0x03, 0x80]); // too short for varint length
    assert!(result.is_err());
}

#[test]
fn test_deserialize_truncated_text_data() {
    // length says 10 bytes but only 2 available
    let result = Value::deserialize(&[0x03, 10, 0, 0, 0, b'h', b'i']);
    assert!(result.is_err());
}

#[test]
fn test_deserialize_truncated_blob_length() {
    let result = Value::deserialize(&[0x04, 0x80]); // too short for varint length
    assert!(result.is_err());
}

#[test]
fn test_deserialize_truncated_blob_data() {
    let result = Value::deserialize(&[0x04, 10, 0, 0, 0, 0xFF]);
    assert!(result.is_err());
}

// ---- Value methods ----

#[test]
fn test_is_truthy() {
    assert!(!Value::Null.is_truthy());
    assert!(!Value::Integer(0).is_truthy());
    assert!(Value::Integer(1).is_truthy());
    assert!(Value::Integer(-1).is_truthy());
    assert!(!Value::Real(0.0).is_truthy());
    assert!(Value::Real(1.5).is_truthy());
    assert!(!Value::Text("".into()).is_truthy());
    assert!(Value::Text("abc".into()).is_truthy());
    assert!(!Value::Blob(vec![]).is_truthy());
    assert!(Value::Blob(vec![1]).is_truthy());
}

#[test]
fn test_to_i64() {
    assert_eq!(Value::Integer(42).to_i64(), Some(42));
    assert_eq!(Value::Real(3.7).to_i64(), Some(3));
    assert_eq!(Value::Text("123".into()).to_i64(), Some(123));
    assert_eq!(Value::Text("abc".into()).to_i64(), None);
    assert_eq!(Value::Null.to_i64(), None);
    assert_eq!(Value::Blob(vec![]).to_i64(), None);
}

#[test]
#[allow(clippy::approx_constant)]
fn test_to_f64() {
    assert_eq!(Value::Integer(42).to_f64(), Some(42.0));
    assert_eq!(Value::Real(3.14).to_f64(), Some(3.14));
    assert_eq!(Value::Text("1.5".into()).to_f64(), Some(1.5));
    assert_eq!(Value::Text("abc".into()).to_f64(), None);
    assert_eq!(Value::Null.to_f64(), None);
    assert_eq!(Value::Blob(vec![]).to_f64(), None);
}

#[test]
fn test_data_type() {
    assert_eq!(Value::Null.data_type(), DataType::Null);
    assert_eq!(Value::Integer(0).data_type(), DataType::Integer);
    assert_eq!(Value::Real(0.0).data_type(), DataType::Real);
    assert_eq!(Value::Text("".into()).data_type(), DataType::Text);
    assert_eq!(Value::Blob(vec![]).data_type(), DataType::Blob);
}

// ---- Display ----

#[test]
#[allow(clippy::approx_constant)]
fn test_value_display() {
    assert_eq!(format!("{}", Value::Null), "NULL");
    assert_eq!(format!("{}", Value::Integer(42)), "42");
    assert_eq!(format!("{}", Value::Real(3.14)), "3.14");
    assert_eq!(format!("{}", Value::Text("hi".into())), "hi");
    assert_eq!(format!("{}", Value::Blob(vec![0xAB, 0xCD])), "x'abcd'");
}

// ---- PartialEq ----

#[test]
fn test_value_eq_same_types() {
    assert_eq!(Value::Null, Value::Null);
    assert_eq!(Value::Integer(1), Value::Integer(1));
    assert_ne!(Value::Integer(1), Value::Integer(2));
    assert_eq!(Value::Real(1.0), Value::Real(1.0));
    assert_ne!(Value::Real(1.0), Value::Real(2.0));
    assert_eq!(Value::Text("a".into()), Value::Text("a".into()));
    assert_ne!(Value::Text("a".into()), Value::Text("b".into()));
    assert_eq!(Value::Blob(vec![1]), Value::Blob(vec![1]));
    assert_ne!(Value::Blob(vec![1]), Value::Blob(vec![2]));
}

#[test]
fn test_value_eq_cross_types() {
    assert_eq!(Value::Integer(1), Value::Real(1.0));
    assert_eq!(Value::Real(1.0), Value::Integer(1));
    assert_ne!(Value::Integer(1), Value::Text("1".into()));
    assert_ne!(Value::Null, Value::Integer(0));
    assert_ne!(Value::Text("a".into()), Value::Blob(vec![b'a']));
}

// ---- PartialOrd ----

#[test]
fn test_value_ord_null() {
    assert_eq!(Value::Null.partial_cmp(&Value::Null), Some(Ordering::Equal));
    assert_eq!(
        Value::Null.partial_cmp(&Value::Integer(0)),
        Some(Ordering::Less)
    );
    assert_eq!(
        Value::Integer(0).partial_cmp(&Value::Null),
        Some(Ordering::Greater)
    );
}

#[test]
fn test_value_ord_integers() {
    assert_eq!(
        Value::Integer(1).partial_cmp(&Value::Integer(2)),
        Some(Ordering::Less)
    );
    assert_eq!(
        Value::Integer(2).partial_cmp(&Value::Integer(1)),
        Some(Ordering::Greater)
    );
    assert_eq!(
        Value::Integer(1).partial_cmp(&Value::Integer(1)),
        Some(Ordering::Equal)
    );
}

#[test]
fn test_value_ord_reals() {
    assert_eq!(
        Value::Real(1.0).partial_cmp(&Value::Real(2.0)),
        Some(Ordering::Less)
    );
    assert_eq!(
        Value::Real(2.0).partial_cmp(&Value::Real(1.0)),
        Some(Ordering::Greater)
    );
}

#[test]
fn test_value_ord_cross_numeric() {
    assert_eq!(
        Value::Integer(1).partial_cmp(&Value::Real(2.0)),
        Some(Ordering::Less)
    );
    assert_eq!(
        Value::Real(1.0).partial_cmp(&Value::Integer(2)),
        Some(Ordering::Less)
    );
}

#[test]
fn test_value_ord_text() {
    assert_eq!(
        Value::Text("a".into()).partial_cmp(&Value::Text("b".into())),
        Some(Ordering::Less)
    );
    assert_eq!(
        Value::Text("b".into()).partial_cmp(&Value::Text("a".into())),
        Some(Ordering::Greater)
    );
}

#[test]
fn test_value_ord_blob() {
    assert_eq!(
        Value::Blob(vec![1]).partial_cmp(&Value::Blob(vec![2])),
        Some(Ordering::Less)
    );
}

#[test]
fn test_value_ord_sqlite_type_ordering() {
    // NULL < INTEGER/REAL < TEXT < BLOB
    assert_eq!(
        Value::Integer(0).partial_cmp(&Value::Text("a".into())),
        Some(Ordering::Less)
    );
    assert_eq!(
        Value::Real(0.0).partial_cmp(&Value::Text("a".into())),
        Some(Ordering::Less)
    );
    assert_eq!(
        Value::Text("a".into()).partial_cmp(&Value::Integer(0)),
        Some(Ordering::Greater)
    );
    assert_eq!(
        Value::Text("a".into()).partial_cmp(&Value::Blob(vec![])),
        Some(Ordering::Less)
    );
    assert_eq!(
        Value::Blob(vec![]).partial_cmp(&Value::Text("a".into())),
        Some(Ordering::Greater)
    );
    assert_eq!(
        Value::Integer(0).partial_cmp(&Value::Blob(vec![])),
        Some(Ordering::Less)
    );
    assert_eq!(
        Value::Blob(vec![]).partial_cmp(&Value::Real(0.0)),
        Some(Ordering::Greater)
    );
}

// ---- Row serialization ----

#[test]
#[allow(clippy::approx_constant)]
fn test_serialize_deserialize_row() {
    let row = vec![
        Value::Integer(1),
        Value::Text("hello".into()),
        Value::Real(3.14),
        Value::Null,
        Value::Blob(vec![0xFF]),
    ];
    let bytes = serialize_row(&row);
    let deserialized = deserialize_row(&bytes).unwrap();
    assert_eq!(deserialized.len(), 5);
    assert_eq!(format!("{}", deserialized[0]), "1");
    assert_eq!(format!("{}", deserialized[1]), "hello");
    assert_eq!(format!("{}", deserialized[3]), "NULL");
}

#[test]
fn test_serialize_empty_row() {
    let row: Vec<Value> = vec![];
    let bytes = serialize_row(&row);
    let deserialized = deserialize_row(&bytes).unwrap();
    assert_eq!(deserialized.len(), 0);
}

#[test]
fn test_deserialize_row_too_short() {
    let result = deserialize_row(&[]);
    assert!(result.is_err());
    let result = deserialize_row(&[0x80]); // Incomplete varint col count
    assert!(result.is_err());
}

// ---- hex_encode ----

#[test]
fn test_hex_encode() {
    assert_eq!(hex_encode(&[0xAB, 0xCD, 0xEF]), "abcdef");
    assert_eq!(hex_encode(&[]), "");
    assert_eq!(hex_encode(&[0x00, 0xFF]), "00ff");
}

// ── New coverage tests ──────────────────────────────────────────────────

#[test]
fn test_value_real_nan_serialize_roundtrip() {
    let val = Value::Real(f64::NAN);
    let bytes = val.serialize();
    let (decoded, consumed) = Value::deserialize(&bytes).unwrap();
    assert_eq!(consumed, 9);
    match decoded {
        Value::Real(v) => assert!(v.is_nan(), "NaN should survive roundtrip"),
        _ => panic!("expected Real"),
    }
}

#[test]
fn test_value_real_infinity_serialize_roundtrip() {
    for &v in &[f64::INFINITY, f64::NEG_INFINITY] {
        let val = Value::Real(v);
        let bytes = val.serialize();
        let (decoded, _) = Value::deserialize(&bytes).unwrap();
        assert_eq!(decoded, val);
    }
}

#[test]
fn test_value_integer_extreme_serialize_roundtrip() {
    for &v in &[i64::MAX, i64::MIN, 0i64, 1, -1] {
        let val = Value::Integer(v);
        let bytes = val.serialize();
        let (decoded, _) = Value::deserialize(&bytes).unwrap();
        assert_eq!(decoded, val);
    }
}

#[test]
fn test_value_deserialize_invalid_utf8_text() {
    // 0x03 tag (Text) + varint length 2 + invalid UTF-8 bytes
    let data = [0x03, 0x02, 0xFF, 0xFE];
    let result = Value::deserialize(&data);
    assert!(result.is_err(), "invalid UTF-8 should be rejected");
}

#[test]
fn test_value_deserialize_truncated_text() {
    // 0x03 tag + varint length 10 + only 3 bytes of data
    let data = [0x03, 0x0A, b'h', b'e', b'l'];
    let result = Value::deserialize(&data);
    assert!(result.is_err(), "truncated text data should be rejected");
}

#[test]
fn test_value_deserialize_truncated_blob() {
    // 0x04 tag + varint length 10 + only 2 bytes
    let data = [0x04, 0x0A, 0x00, 0x01];
    let result = Value::deserialize(&data);
    assert!(result.is_err(), "truncated blob data should be rejected");
}

#[test]
fn test_value_deserialize_unknown_tag() {
    let data = [0xFF];
    let result = Value::deserialize(&data);
    assert!(result.is_err());
}

#[test]
fn test_value_deserialize_empty() {
    let result = Value::deserialize(&[]);
    assert!(result.is_err());
}

#[test]
fn test_value_deserialize_huge_len_rejected() {
    // 0x03 (Text) + varint representing a huge length (~4 GiB)
    // LEB128 encoding of 0xFFFFFFFF (4294967295)
    let data = [0x03, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F];
    let result = Value::deserialize(&data);
    // Should be rejected because length > MAX_VALUE_LEN (256 MiB) or truncated
    assert!(result.is_err(), "huge text length should be rejected");
}

#[test]
fn test_deserialize_row_huge_col_count_rejected() {
    // Varint encoding of 10000 columns
    let mut data = Vec::new();
    crate::varint::write_varint_u64(10000, &mut data);
    let result = deserialize_row(&data);
    assert!(
        result.is_err(),
        "col_count > MAX_COLUMNS should be rejected"
    );
}

#[test]
fn test_serialize_into_consistency() {
    // serialize and serialize_into should produce identical output
    let val = Value::Text(std::sync::Arc::from("hello world"));
    let bytes = val.serialize();
    let mut buf = Vec::new();
    val.serialize_into(&mut buf);
    assert_eq!(bytes, buf);
}

#[test]
#[allow(clippy::approx_constant)]
fn test_serialize_row_into_reuse() {
    let row1 = vec![Value::Integer(1), Value::Text(std::sync::Arc::from("abc"))];
    let row2 = vec![Value::Null, Value::Real(3.14)];
    let mut buf = Vec::new();

    serialize_row_into(&row1, &mut buf);
    let bytes1 = buf.clone();

    serialize_row_into(&row2, &mut buf);
    let bytes2 = buf.clone();

    // Each call should produce correct, independent results
    let d1 = deserialize_row(&bytes1).unwrap();
    let d2 = deserialize_row(&bytes2).unwrap();
    assert_eq!(d1, row1);
    assert_eq!(d2, row2);
}

#[test]
fn test_large_blob_roundtrip() {
    let big_blob = vec![0xABu8; 100_000];
    let val = Value::Blob(big_blob.clone());
    let bytes = val.serialize();
    let (decoded, _) = Value::deserialize(&bytes).unwrap();
    match decoded {
        Value::Blob(b) => assert_eq!(b, big_blob),
        _ => panic!("expected Blob"),
    }
}
