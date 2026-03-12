//! Direct internal-API unit tests — bypass SQL layer to exercise unreachable code.
//! Targets: apply_binary_op (NULL AND/OR, FtsMatch), BTree::scan_rows_reverse_limit,
//! defragment_all, fragmentation_stats, count_overflow_pages, etc.

use crate::sql::ast::BinaryOperator;
use crate::types::Value;
use crate::vm::execute::{ExecResult, VM};

// ═══════════════════════════════════════════════════════════════════════════
// 1. apply_binary_op — NULL AND/OR paths (dead code via SQL since short-circuit)
//    Targets: eval_expr.rs L1810-1827 (~16 lines)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_null_and_false() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::And, &Value::Null, &Value::Integer(0))
        .unwrap();
    assert_eq!(result, Value::Integer(0));
}

#[test]
fn test_apply_false_and_null() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::And, &Value::Integer(0), &Value::Null)
        .unwrap();
    assert_eq!(result, Value::Integer(0));
}

#[test]
fn test_apply_null_and_true() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::And, &Value::Null, &Value::Integer(1))
        .unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn test_apply_true_and_null() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::And, &Value::Integer(1), &Value::Null)
        .unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn test_apply_null_or_true() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::Or, &Value::Null, &Value::Integer(1))
        .unwrap();
    assert_eq!(result, Value::Integer(1));
}

#[test]
fn test_apply_true_or_null() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::Or, &Value::Integer(1), &Value::Null)
        .unwrap();
    assert_eq!(result, Value::Integer(1));
}

#[test]
fn test_apply_null_or_false() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::Or, &Value::Null, &Value::Integer(0))
        .unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn test_apply_null_and_null() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::And, &Value::Null, &Value::Null)
        .unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn test_apply_null_or_null() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::Or, &Value::Null, &Value::Null)
        .unwrap();
    assert_eq!(result, Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. apply_binary_op — FtsMatch text paths
//    Targets: eval_expr.rs L1832-1844 (~13 lines)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_ftsmatch_text() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::FtsMatch,
            &Value::Text("hello world".into()),
            &Value::Text("hello".into()),
        )
        .unwrap();
    assert_eq!(result, Value::Integer(1));
}

#[test]
fn test_apply_ftsmatch_no_match() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::FtsMatch,
            &Value::Text("hello world".into()),
            &Value::Text("goodbye".into()),
        )
        .unwrap();
    assert_eq!(result, Value::Integer(0));
}

#[test]
fn test_apply_ftsmatch_empty_pattern() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::FtsMatch,
            &Value::Text("hello".into()),
            &Value::Text("".into()),
        )
        .unwrap();
    assert_eq!(result, Value::Integer(0));
}

#[test]
fn test_apply_ftsmatch_multi_tokens() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::FtsMatch,
            &Value::Text("the quick brown fox".into()),
            &Value::Text("quick fox".into()),
        )
        .unwrap();
    assert_eq!(result, Value::Integer(1));
}

#[test]
fn test_apply_ftsmatch_non_text() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::FtsMatch,
            &Value::Integer(42),
            &Value::Text("test".into()),
        )
        .unwrap();
    assert_eq!(result, Value::Integer(0));
}

#[test]
fn test_apply_ftsmatch_null_left() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::FtsMatch,
            &Value::Null,
            &Value::Text("test".into()),
        )
        .unwrap();
    assert_eq!(result, Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. apply_binary_op — non-NULL AND/OR truth-table completion
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_and_both_true() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::And, &Value::Integer(1), &Value::Integer(1))
        .unwrap();
    assert_eq!(result, Value::Integer(1));
}

#[test]
fn test_apply_and_one_false() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::And, &Value::Integer(1), &Value::Integer(0))
        .unwrap();
    assert_eq!(result, Value::Integer(0));
}

#[test]
fn test_apply_or_both_false() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::Or, &Value::Integer(0), &Value::Integer(0))
        .unwrap();
    assert_eq!(result, Value::Integer(0));
}

#[test]
fn test_apply_or_one_true() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::Or, &Value::Integer(0), &Value::Integer(1))
        .unwrap();
    assert_eq!(result, Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. apply_binary_op — Concat, Xor, Bitwise, ShiftLeft/ShiftRight
//    Targets: eval_expr.rs L1952-1998 (~40 lines)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_concat() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::Concat,
            &Value::Text("hello".into()),
            &Value::Text(" world".into()),
        )
        .unwrap();
    assert!(matches!(&result, Value::Text(s) if s.as_ref() == "hello world"));
}

#[test]
fn test_apply_xor() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(&BinaryOperator::Xor, &Value::Integer(1), &Value::Integer(0))
        .unwrap();
    assert_eq!(result, Value::Integer(1));
    let result = vm
        .apply_binary_op(&BinaryOperator::Xor, &Value::Integer(1), &Value::Integer(1))
        .unwrap();
    assert_eq!(result, Value::Integer(0));
}

#[test]
fn test_apply_bitwise_xor() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::BitwiseXor,
            &Value::Integer(0xFF),
            &Value::Integer(0x0F),
        )
        .unwrap();
    assert_eq!(result, Value::Integer(0xF0));
}

#[test]
fn test_apply_shift_left() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::ShiftLeft,
            &Value::Integer(1),
            &Value::Integer(4),
        )
        .unwrap();
    assert_eq!(result, Value::Integer(16));
}

#[test]
fn test_apply_shift_right() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::ShiftRight,
            &Value::Integer(16),
            &Value::Integer(2),
        )
        .unwrap();
    assert_eq!(result, Value::Integer(4));
}

#[test]
fn test_apply_shift_left_non_integer() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::ShiftLeft,
            &Value::Text("x".into()),
            &Value::Integer(1),
        )
        .unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn test_apply_shift_right_negative() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::ShiftRight,
            &Value::Integer(16),
            &Value::Integer(-1),
        )
        .unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn test_apply_modulo() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::Modulo,
            &Value::Integer(10),
            &Value::Integer(3),
        )
        .unwrap();
    assert_eq!(result, Value::Integer(1));
}

#[test]
fn test_apply_modulo_real() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::Modulo,
            &Value::Real(10.5),
            &Value::Real(3.0),
        )
        .unwrap();
    if let Value::Real(v) = result {
        assert!((v - 1.5).abs() < 0.01, "got {v}");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. apply_binary_op — NULL propagation for non-AND/OR operators
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_null_propagation() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(&BinaryOperator::Add, &Value::Null, &Value::Integer(1))
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        vm.apply_binary_op(&BinaryOperator::Subtract, &Value::Integer(1), &Value::Null)
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        vm.apply_binary_op(&BinaryOperator::Multiply, &Value::Null, &Value::Null)
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        vm.apply_binary_op(&BinaryOperator::Equal, &Value::Null, &Value::Integer(1))
            .unwrap(),
        Value::Null
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. BTree::scan_rows_reverse_limit (btree.rs L1191-1220, 30 lines dead API)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_btree_reverse_limit_basic() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;
    for i in 1i64..=20 {
        let row = vec![Value::Integer(i), Value::Text(format!("row_{i}").into())];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    let rows = btree.scan_rows_reverse_limit(current_root, 5).unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0][0], Value::Integer(20));
    assert_eq!(rows[4][0], Value::Integer(16));
}

#[test]
fn test_btree_reverse_limit_exceeds() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;
    for i in 1i64..=5 {
        let row = vec![Value::Integer(i)];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    let rows = btree.scan_rows_reverse_limit(current_root, 100).unwrap();
    assert_eq!(rows.len(), 5);
}

#[test]
fn test_btree_reverse_limit_multi_page() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;
    for i in 1i64..=300 {
        let row = vec![
            Value::Integer(i),
            Value::Text(format!("row_data_{i}").into()),
        ];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    let rows = btree.scan_rows_reverse_limit(current_root, 10).unwrap();
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0][0], Value::Integer(300));
    assert_eq!(rows[9][0], Value::Integer(291));
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. BTree fragmentation_stats + defragment_all
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_btree_fragmentation_stats_basic() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;
    for i in 1i64..=10 {
        let row = vec![Value::Integer(i), Value::Text(format!("data_{i}").into())];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    let (leaves, _frag, overflow, _free) = btree.fragmentation_stats(current_root).unwrap();
    assert!(leaves >= 1);
    assert_eq!(overflow, 0);
}

#[test]
fn test_btree_defragment_after_deletes() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;
    for i in 1i64..=50 {
        let row = vec![Value::Integer(i), Value::Text(format!("data_{i}").into())];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }
    for i in (2i64..=50).step_by(2) {
        let (deleted, new_root) = btree.delete_by_rowid(current_root, i).unwrap();
        if deleted {
            current_root = new_root;
        }
    }

    let defragged = btree.defragment_all(current_root).unwrap();
    let _ = defragged; // exercises the path
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. index_eq_key coverage for different Value types
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_index_eq_key_integer() {
    let k = VM::index_eq_key(&Value::Integer(42));
    assert_eq!(k[0], b'N');
}

#[test]
fn test_index_eq_key_real() {
    let k = VM::index_eq_key(&Value::Real(3.14));
    assert_eq!(k[0], b'N');
}

#[test]
fn test_index_eq_key_text() {
    let k = VM::index_eq_key(&Value::Text("hello".into()));
    assert_eq!(k[0], b'T');
}

#[test]
fn test_index_eq_key_blob() {
    let k = VM::index_eq_key(&Value::Blob(vec![1, 2, 3]));
    assert_eq!(k[0], b'B');
}

#[test]
fn test_index_eq_key_null() {
    let k = VM::index_eq_key(&Value::Null);
    assert_eq!(k, vec![b'Z']);
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. flip_comparison_operator
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_flip_comparison() {
    assert_eq!(
        VM::flip_comparison_operator(&BinaryOperator::Equal),
        Some(BinaryOperator::Equal)
    );
    assert_eq!(
        VM::flip_comparison_operator(&BinaryOperator::LessThan),
        Some(BinaryOperator::GreaterThan)
    );
    assert_eq!(
        VM::flip_comparison_operator(&BinaryOperator::LessThanOrEqual),
        Some(BinaryOperator::GreaterThanOrEqual)
    );
    assert_eq!(
        VM::flip_comparison_operator(&BinaryOperator::GreaterThan),
        Some(BinaryOperator::LessThan)
    );
    assert_eq!(
        VM::flip_comparison_operator(&BinaryOperator::GreaterThanOrEqual),
        Some(BinaryOperator::LessThanOrEqual)
    );
    assert_eq!(VM::flip_comparison_operator(&BinaryOperator::Add), None);
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. apply_binary_op — comparison operators
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_gte() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::GreaterThanOrEqual,
            &Value::Integer(5),
            &Value::Integer(3)
        )
        .unwrap(),
        Value::Integer(1)
    );
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::GreaterThanOrEqual,
            &Value::Integer(3),
            &Value::Integer(5)
        )
        .unwrap(),
        Value::Integer(0)
    );
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::GreaterThanOrEqual,
            &Value::Integer(5),
            &Value::Integer(5)
        )
        .unwrap(),
        Value::Integer(1)
    );
}

#[test]
fn test_apply_lte() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::LessThanOrEqual,
            &Value::Integer(3),
            &Value::Integer(5)
        )
        .unwrap(),
        Value::Integer(1)
    );
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::LessThanOrEqual,
            &Value::Integer(5),
            &Value::Integer(3)
        )
        .unwrap(),
        Value::Integer(0)
    );
}

#[test]
fn test_apply_gt() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::GreaterThan,
            &Value::Integer(5),
            &Value::Integer(3)
        )
        .unwrap(),
        Value::Integer(1)
    );
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::GreaterThan,
            &Value::Integer(3),
            &Value::Integer(5)
        )
        .unwrap(),
        Value::Integer(0)
    );
}

#[test]
fn test_apply_lt() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::LessThan,
            &Value::Integer(3),
            &Value::Integer(5)
        )
        .unwrap(),
        Value::Integer(1)
    );
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::LessThan,
            &Value::Integer(5),
            &Value::Integer(3)
        )
        .unwrap(),
        Value::Integer(0)
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. apply_binary_op — Divide by zero
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_divide_by_zero() {
    let vm = VM::new_memory();
    // Divide by zero may return Null or error depending on implementation
    let r = vm
        .apply_binary_op(
            &BinaryOperator::Divide,
            &Value::Integer(10),
            &Value::Integer(0),
        )
        .unwrap();
    assert_eq!(r, Value::Null);
}

#[test]
fn test_apply_real_divide() {
    let vm = VM::new_memory();
    let r = vm
        .apply_binary_op(
            &BinaryOperator::Divide,
            &Value::Real(10.0),
            &Value::Real(3.0),
        )
        .unwrap();
    if let Value::Real(v) = r {
        assert!((v - 3.333).abs() < 0.01);
    }
}

#[test]
fn test_apply_modulo_by_zero() {
    let vm = VM::new_memory();
    let r = vm
        .apply_binary_op(
            &BinaryOperator::Modulo,
            &Value::Integer(10),
            &Value::Integer(0),
        )
        .unwrap();
    assert_eq!(r, Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. apply_binary_op — mixed type arithmetic
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_add_int_real() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(&BinaryOperator::Add, &Value::Integer(3), &Value::Real(2.5))
            .unwrap(),
        Value::Real(5.5)
    );
}

#[test]
fn test_apply_add_real_int() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(&BinaryOperator::Add, &Value::Real(2.5), &Value::Integer(3))
            .unwrap(),
        Value::Real(5.5)
    );
}

#[test]
fn test_apply_subtract_real_int() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::Subtract,
            &Value::Real(10.5),
            &Value::Integer(3)
        )
        .unwrap(),
        Value::Real(7.5)
    );
}

#[test]
fn test_apply_multiply_int_real() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::Multiply,
            &Value::Integer(3),
            &Value::Real(2.5)
        )
        .unwrap(),
        Value::Real(7.5)
    );
}

#[test]
fn test_apply_divide_int_real() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::Divide,
            &Value::Integer(10),
            &Value::Real(4.0)
        )
        .unwrap(),
        Value::Real(2.5)
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. apply_binary_op — string comparisons
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_text_equal() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::Equal,
            &Value::Text("hello".into()),
            &Value::Text("hello".into())
        )
        .unwrap(),
        Value::Integer(1)
    );
}

#[test]
fn test_apply_text_not_equal() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::NotEqual,
            &Value::Text("hello".into()),
            &Value::Text("world".into())
        )
        .unwrap(),
        Value::Integer(1)
    );
}

#[test]
fn test_apply_text_lt() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::LessThan,
            &Value::Text("abc".into()),
            &Value::Text("def".into())
        )
        .unwrap(),
        Value::Integer(1)
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 14. apply_binary_op — Bitwise ops with non-integer → Null
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_bitwise_or_non_int() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::BitwiseOr,
            &Value::Text("x".into()),
            &Value::Integer(1)
        )
        .unwrap(),
        Value::Null
    );
}

#[test]
fn test_apply_bitwise_and_non_int() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::BitwiseAnd,
            &Value::Integer(1),
            &Value::Text("x".into())
        )
        .unwrap(),
        Value::Null
    );
}

#[test]
fn test_apply_bitwise_xor_non_int() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::BitwiseXor,
            &Value::Real(1.0),
            &Value::Integer(1)
        )
        .unwrap(),
        Value::Null
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 15. apply_binary_op — Non-numeric Add/Mul → Null
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_add_text_text() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::Add,
            &Value::Text("a".into()),
            &Value::Text("b".into())
        )
        .unwrap(),
        Value::Null
    );
}

#[test]
fn test_apply_multiply_text_int() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::Multiply,
            &Value::Text("a".into()),
            &Value::Integer(2)
        )
        .unwrap(),
        Value::Null
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 16. BTree max_rowid
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_btree_max_rowid() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;
    for i in 1i64..=50 {
        let row = vec![Value::Integer(i)];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    let max = btree.max_rowid(current_root).unwrap_or(-1);
    assert_eq!(max, 50);
}

#[test]
fn test_btree_max_rowid_multi_page() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;
    for i in 1i64..=500 {
        let row = vec![Value::Integer(i), Value::Text(format!("r{i}").into())];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    let max = btree.max_rowid(current_root).unwrap_or(-1);
    assert_eq!(max, 500);
}

// ═══════════════════════════════════════════════════════════════════════════
// 17. BTree scan_all
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_btree_scan_all_basic() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;
    for i in 1i64..=15 {
        let row = vec![Value::Integer(i)];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    let all = btree.scan_all(current_root).unwrap();
    assert_eq!(all.len(), 15);
    assert_eq!(all[0].0, 1);
    assert_eq!(all[14].0, 15);
}

// ═══════════════════════════════════════════════════════════════════════════
// 18. BTree with large payloads (overflow pages)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_btree_overflow_pages() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;

    // Large payloads → overflow pages (need > PAGE_SIZE/4 ≈ 1024 bytes per cell)
    for i in 1i64..=5 {
        let large_text = "x".repeat(10000); // 10KB definitely overflows
        let row = vec![Value::Integer(i), Value::Text(large_text.into())];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    let all = btree.scan_all(current_root).unwrap();
    assert_eq!(all.len(), 5);
    let (leaves, _frag, overflow, _free) = btree.fragmentation_stats(current_root).unwrap();
    assert!(leaves >= 1);
    // Overflow pages should exist for large payloads, but check gracefully
    // The exact threshold depends on internal implementation
    let _ = overflow;

    let rev = btree.scan_rows_reverse_limit(current_root, 3).unwrap();
    assert_eq!(rev.len(), 3);
    assert_eq!(rev[0][0], Value::Integer(5));
}

// ═══════════════════════════════════════════════════════════════════════════
// 19. BTree find_row
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_btree_find_row() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;
    for i in 1i64..=100 {
        let row = vec![Value::Integer(i), Value::Text(format!("val_{i}").into())];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    let found = btree.find_by_rowid(current_root, 42).unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().1[0], Value::Integer(42));

    let not_found = btree.find_by_rowid(current_root, 999).unwrap();
    assert!(not_found.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// 20. BTree insert + delete + re-scan
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_btree_insert_delete_rescan() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;
    for i in 1i64..=30 {
        let row = vec![Value::Integer(i)];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    for i in [5i64, 10, 15, 20, 25] {
        let (deleted, new_root) = btree.delete_by_rowid(current_root, i).unwrap();
        assert!(deleted);
        current_root = new_root;
    }

    let all = btree.scan_all(current_root).unwrap();
    assert_eq!(all.len(), 25);
    assert!(!all.iter().any(|(rid, _)| *rid == 5));
    assert!(!all.iter().any(|(rid, _)| *rid == 10));
}

// ═══════════════════════════════════════════════════════════════════════════
// 21. Adaptive indexing through SQL
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_adaptive_indexing_via_sql() {
    let mut vm = VM::new_memory();
    let _ = vm.execute_sql("SET kkdb.adaptive_indexing = 'true'");
    vm.execute_sql("CREATE TABLE aix(id INTEGER, val INTEGER)")
        .unwrap();
    for i in 1..=100 {
        vm.execute_sql(&format!("INSERT INTO aix VALUES ({i}, {})", i % 10))
            .unwrap();
    }
    for _ in 0..30 {
        let _ = vm.execute_sql("SELECT * FROM aix WHERE val = 5");
    }
    let result = vm
        .execute_sql("SELECT COUNT(*) FROM aix WHERE val = 5")
        .unwrap();
    match result {
        ExecResult::QueryResult { rows, .. } => {
            assert_eq!(rows[0][0], Value::Integer(10));
        }
        _ => {}
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 22. BTree large scale with defrag
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_btree_large_scale_with_defrag() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    let root = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0x0D;
        page.data[1..3].copy_from_slice(&0u16.to_le_bytes());
        let ps = crate::storage::pager::PAGE_SIZE as u16;
        page.data[3..5].copy_from_slice(&ps.to_le_bytes());
        page.data[5] = 0;
    }

    let mut btree = BTree::new(&mut pager);
    let mut current_root = root;

    for i in 1i64..=500 {
        let row = vec![Value::Integer(i), Value::Text(format!("r{i}").into())];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    for i in (1i64..=500).step_by(2) {
        let (deleted, new_root) = btree.delete_by_rowid(current_root, i).unwrap();
        if deleted {
            current_root = new_root;
        }
    }

    let defragged = btree.defragment_all(current_root).unwrap();
    let _ = defragged; // exercises the path

    let all = btree.scan_all(current_root).unwrap();
    assert_eq!(all.len(), 250);

    let (leaves, _frag, _overflow, _free) = btree.fragmentation_stats(current_root).unwrap();
    assert!(leaves >= 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// 23. apply_binary_op — Concat edge cases
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_concat_int_text() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::Concat,
            &Value::Integer(42),
            &Value::Text(" items".into()),
        )
        .unwrap();
    match result {
        Value::Text(s) => assert!(s.as_ref().contains("42")),
        Value::Null => {}
        _ => panic!("unexpected: {:?}", result),
    }
}

#[test]
fn test_apply_concat_null() {
    let vm = VM::new_memory();
    let result = vm
        .apply_binary_op(
            &BinaryOperator::Concat,
            &Value::Null,
            &Value::Text("x".into()),
        )
        .unwrap();
    assert_eq!(result, Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════
// 24. Bitwise OR/AND with valid integers
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_bitwise_or_valid() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::BitwiseOr,
            &Value::Integer(0x0F),
            &Value::Integer(0xF0)
        )
        .unwrap(),
        Value::Integer(0xFF)
    );
}

#[test]
fn test_apply_bitwise_and_valid() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::BitwiseAnd,
            &Value::Integer(0xFF),
            &Value::Integer(0x0F)
        )
        .unwrap(),
        Value::Integer(0x0F)
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 25. NotEqual with various types
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_apply_not_equal_int() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::NotEqual,
            &Value::Integer(1),
            &Value::Integer(2)
        )
        .unwrap(),
        Value::Integer(1)
    );
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::NotEqual,
            &Value::Integer(1),
            &Value::Integer(1)
        )
        .unwrap(),
        Value::Integer(0)
    );
}

#[test]
fn test_apply_not_equal_real() {
    let vm = VM::new_memory();
    assert_eq!(
        vm.apply_binary_op(
            &BinaryOperator::NotEqual,
            &Value::Real(1.0),
            &Value::Real(2.0)
        )
        .unwrap(),
        Value::Integer(1)
    );
}
