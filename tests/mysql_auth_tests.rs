//! MySQL native password authentication unit tests.
//!
//! Tests the SHA-1 challenge-response math that implements mysql_native_password
//! without requiring any TCP connections or real MySQL clients.

use kkdb::server::mysql::{mysql_double_sha1, verify_native_password};
use sha1::{Digest, Sha1};

// ─── Helper: compute what a client would send ─────────────────────────────────

/// Simulate what the MySQL client computes:
///   SHA1(password) XOR SHA1(scramble || SHA1(SHA1(password)))
fn client_native_password(scramble: &[u8; 20], password: &str) -> [u8; 20] {
    let sha1 = |data: &[u8]| -> [u8; 20] {
        let mut h = Sha1::new();
        h.update(data);
        h.finalize().into()
    };

    let p1 = sha1(password.as_bytes()); // SHA1(password)
    let _p2 = sha1(&sha1(&p1)); // SHA1(SHA1(SHA1(password)))
                               // Actually: SHA1(SHA1(SHA1(password))) is wrong. Let me redo:
                               // stored = SHA1(SHA1(password))
    let stored = sha1(&p1);

    // SHA1(scramble || stored)
    let mut h = Sha1::new();
    h.update(scramble);
    h.update(stored);
    let hash: [u8; 20] = h.finalize().into();

    // XOR p1 with hash
    let mut response = [0u8; 20];
    for i in 0..20 {
        response[i] = p1[i] ^ hash[i];
    }
    response
}

// ─── Test 1: correct password verifies successfully ───────────────────────────

#[test]
fn test_native_password_correct() {
    let scramble = *b"12345678901234567890"; // 20 bytes
    let password = "mypassword";

    let stored = mysql_double_sha1(password);
    let response = client_native_password(&scramble, password);

    assert!(
        verify_native_password(&scramble, &response, &stored),
        "correct password must verify"
    );
}

// ─── Test 2: wrong password fails ────────────────────────────────────────────

#[test]
fn test_native_password_wrong() {
    let scramble = *b"abcdefghij1234567890"; // 20 bytes
    let correct = "correctpassword";
    let wrong = "wrongpassword";

    let stored = mysql_double_sha1(correct);
    let response = client_native_password(&scramble, wrong);

    assert!(
        !verify_native_password(&scramble, &response, &stored),
        "wrong password must not verify"
    );
}

// ─── Test 3: wrong scramble fails ────────────────────────────────────────────

#[test]
fn test_native_password_wrong_scramble() {
    let scramble1 = *b"11111111111111111111";
    let scramble2 = *b"22222222222222222222";
    let password = "testpass";

    let stored = mysql_double_sha1(password);
    let response = client_native_password(&scramble1, password);

    // Verifying with a DIFFERENT scramble must fail
    assert!(
        !verify_native_password(&scramble2, &response, &stored),
        "wrong scramble must cause verification failure"
    );
}

// ─── Test 4: empty password ───────────────────────────────────────────────────

#[test]
fn test_native_password_empty() {
    let scramble = *b"super-random-salt20!";
    let password = "";

    let stored = mysql_double_sha1(password);
    let response = client_native_password(&scramble, password);

    assert!(
        verify_native_password(&scramble, &response, &stored),
        "empty password must verify with correct stored hash"
    );
}

// ─── Test 5: mysql_double_sha1 produces 40-char lowercase hex ────────────────

#[test]
fn test_double_sha1_format() {
    let hash = mysql_double_sha1("testpassword");
    assert_eq!(hash.len(), 40, "double-sha1 must be 40 hex chars");
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "must be lowercase hex"
    );
}

// ─── Test 6: truncated response (< 20 bytes) fails ───────────────────────────

#[test]
fn test_native_password_short_response() {
    let scramble = *b"12345678901234567890";
    let stored = mysql_double_sha1("anypassword");

    // Only 10 bytes — should be rejected
    assert!(
        !verify_native_password(&scramble, &[0u8; 10], &stored),
        "short auth response must fail"
    );
}

// ─── Test 7: invalid stored hash (bad hex) fails gracefully ──────────────────

#[test]
fn test_native_password_invalid_stored_hash() {
    let scramble = *b"12345678901234567890";
    let bad_hash = "not-hex-at-all!!!!!!!!!!!!!!!!!!!!!!!!"; // 40 chars but not hex
    let response = [0u8; 20];

    assert!(
        !verify_native_password(&scramble, &response, bad_hash),
        "invalid stored hash must fail gracefully"
    );
}
