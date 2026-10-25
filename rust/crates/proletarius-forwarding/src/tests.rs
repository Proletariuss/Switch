use super::*;
use zeroize::Zeroize;

const GOLDEN_JSON: &[u8] = include_bytes!("../tests/fixtures/go-state.json");
const GOLDEN_HEX: &str = include_str!("../tests/fixtures/go-state.swf1.hex");

fn fixture_key() -> StateKey {
    StateKey::from_slice(&(0_u8..32).collect::<Vec<_>>()).unwrap()
}

fn golden_wire() -> Vec<u8> {
    GOLDEN_HEX
        .trim()
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn document(bytes: &[u8]) -> JsonDocument {
    JsonDocument::from_bytes(Zeroizing::new(bytes.to_vec()), StateLimits::default()).unwrap()
}

#[test]
fn go_production_golden_decrypts_and_reencrypts_exactly() {
    let wire = golden_wire();
    let key = fixture_key();
    let decoded = open(&key, &wire, StateLimits::default()).unwrap();
    assert_eq!(decoded.as_bytes(), GOLDEN_JSON);
    // Reusing this nonce is restricted to synthetic key/data in this unit test.
    let encoded = seal_with_entropy(&key, &decoded, StateLimits::default(), |nonce| {
        nonce.copy_from_slice(&wire[4..16]);
        Ok::<_, ()>(())
    })
    .unwrap();
    assert_eq!(encoded, wire);
    assert!(
        GOLDEN_JSON
            .windows(b"telegram_submission_unconfirmed".len())
            .any(|part| part == b"telegram_submission_unconfirmed")
    );
}

#[test]
fn random_nonce_round_trip_changes_ciphertext() {
    let key = fixture_key();
    let doc = document(GOLDEN_JSON);
    let first = seal(&key, &doc, StateLimits::default()).unwrap();
    let second = seal(&key, &doc, StateLimits::default()).unwrap();
    assert_ne!(&first[4..16], &second[4..16]);
    for wire in [first, second] {
        assert_eq!(wire.len(), GOLDEN_JSON.len() + OVERHEAD);
        assert_eq!(
            open(&key, &wire, StateLimits::default())
                .unwrap()
                .as_bytes(),
            GOLDEN_JSON
        );
    }
}

#[test]
fn every_truncation_is_rejected_without_an_empty_queue() {
    let key = fixture_key();
    let wire = seal(
        &key,
        &document(br#"{"seen":{},"pending":{}}"#),
        StateLimits::default(),
    )
    .unwrap();
    for length in 0..wire.len() {
        assert!(
            open(&key, &wire[..length], StateLimits::default()).is_err(),
            "length {length}"
        );
    }
}

#[test]
fn every_wire_byte_is_authenticated_or_format_checked() {
    let key = fixture_key();
    let wire = seal(
        &key,
        &document(br#"{"seeded":true}"#),
        StateLimits::default(),
    )
    .unwrap();
    for offset in 0..wire.len() {
        let mut changed = wire.clone();
        changed[offset] ^= 1;
        let error = open(&key, &changed, StateLimits::default()).unwrap_err();
        assert_eq!(
            error,
            if offset < 4 {
                StateError::UnsupportedFormat
            } else {
                StateError::AuthenticationFailed
            }
        );
    }
    let mut appended = wire;
    appended.push(0);
    assert_eq!(
        open(&key, &appended, StateLimits::default()).unwrap_err(),
        StateError::AuthenticationFailed
    );
}

#[test]
fn wrong_key_and_missing_aad_are_rejected() {
    let key = fixture_key();
    let other = StateKey::from_slice(&[0xee; 32]).unwrap();
    assert_eq!(
        open(&other, &golden_wire(), StateLimits::default()).unwrap_err(),
        StateError::AuthenticationFailed
    );
    let cipher = Aes256Gcm::new_from_slice(key.0.as_ref()).unwrap();
    let mut wrong_aad = b"{}".to_vec();
    cipher
        .encrypt_in_place(&[7; 12].into(), b"", &mut wrong_aad)
        .unwrap();
    let mut wire = b"SWF1".to_vec();
    wire.extend_from_slice(&[7; 12]);
    wire.extend_from_slice(&wrong_aad);
    assert_eq!(
        open(&key, &wire, StateLimits::default()).unwrap_err(),
        StateError::AuthenticationFailed
    );
}

#[test]
fn invalid_key_and_size_limits_fail_before_encryption_or_allocation() {
    for size in [0, 1, 16, 24, 31, 33, 64] {
        assert_eq!(
            StateKey::from_slice(&vec![0; size]).unwrap_err(),
            StateError::InvalidKeyLength
        );
    }
    for size in [0, MAX_PLAINTEXT_BYTES + 1, usize::MAX] {
        assert_eq!(
            StateLimits::new(size).unwrap_err(),
            StateError::InvalidLimit
        );
    }
    let key = fixture_key();
    let doc = document(br#"{"pending":{}}"#);
    let exact = StateLimits::new(doc.as_bytes().len()).unwrap();
    let smaller = StateLimits::new(doc.as_bytes().len() - 1).unwrap();
    let wire = seal(&key, &doc, exact).unwrap();
    assert_eq!(open(&key, &wire, exact).unwrap().as_bytes(), doc.as_bytes());
    assert_eq!(
        open(&key, &wire, smaller).unwrap_err(),
        StateError::SizeLimit
    );
    assert_eq!(
        JsonDocument::from_bytes(Zeroizing::new(doc.as_bytes().to_vec()), smaller).unwrap_err(),
        StateError::SizeLimit
    );
    let result = seal_with_entropy(&key, &doc, smaller, |_| -> Result<(), ()> {
        panic!("limit must be checked before entropy")
    });
    assert_eq!(result.unwrap_err(), StateError::SizeLimit);
}

#[test]
fn malformed_json_and_nonobjects_are_rejected_even_after_authentication() {
    let key = fixture_key();
    for plain in [
        b"".as_slice(),
        b"null",
        b"[]",
        b"0",
        b"true",
        b"\"body\"",
        b"{",
        b"{}{}",
        b"{} trailing",
        b"{\"x\":01}",
        b"{\"x\":NaN}",
        b"{\"x\":\"\xff\"}",
    ] {
        assert_eq!(
            JsonDocument::from_bytes(Zeroizing::new(plain.to_vec()), StateLimits::default())
                .unwrap_err(),
            StateError::InvalidJsonObject
        );
        // Synthetically authenticated bad plaintext proves JSON is checked only
        // after the complete AEAD check and cannot silently become an empty state.
        let cipher = Aes256Gcm::new_from_slice(key.0.as_ref()).unwrap();
        let mut ciphertext = plain.to_vec();
        cipher
            .encrypt_in_place(&[9; 12].into(), MAGIC, &mut ciphertext)
            .unwrap();
        let mut wire = b"SWF1".to_vec();
        wire.extend_from_slice(&[9; 12]);
        wire.extend_from_slice(&ciphertext);
        assert_eq!(
            open(&key, &wire, StateLimits::default()).unwrap_err(),
            StateError::InvalidJsonObject
        );
    }
}

#[test]
fn opaque_json_keeps_unknown_fields_escapes_numbers_and_whitespace() {
    let plain = " \r\n{\"future\":{\"unicode\":\"汉字😀\",\"huge\":18446744073709551615999,\"escape\":\"\\u003c\\u2028\",\"decimal\":1.000e+2},\"pending\":null,\"seen\":{},\"future\":false}\t";
    let key = fixture_key();
    let doc = document(plain.as_bytes());
    let wire = seal(&key, &doc, StateLimits::default()).unwrap();
    assert_eq!(
        open(&key, &wire, StateLimits::default())
            .unwrap()
            .as_bytes(),
        plain.as_bytes()
    );
    // Even duplicate fields and null maps are preserved, not interpreted as
    // authorized state. A future typed migration must separately validate them.
}

#[test]
fn entropy_failure_returns_no_envelope() {
    let error = seal_with_entropy(
        &fixture_key(),
        &document(b"{}"),
        StateLimits::default(),
        |_| Err(()),
    )
    .unwrap_err();
    assert_eq!(error, StateError::EntropyUnavailable);
}

#[test]
fn deeply_nested_json_has_an_explicit_bound_without_counting_string_brackets() {
    let nested = |arrays| {
        format!(
            "{{\"future\":{}0{}}}",
            "[".repeat(arrays),
            "]".repeat(arrays)
        )
    };
    let exact = nested(MAX_JSON_DEPTH - 1);
    let doc = document(exact.as_bytes());
    let key = fixture_key();
    let wire = seal(&key, &doc, StateLimits::default()).unwrap();
    assert_eq!(
        open(&key, &wire, StateLimits::default())
            .unwrap()
            .as_bytes(),
        exact.as_bytes()
    );
    let too_deep = nested(MAX_JSON_DEPTH);
    assert_eq!(
        JsonDocument::from_bytes(
            Zeroizing::new(too_deep.into_bytes()),
            StateLimits::default()
        )
        .unwrap_err(),
        StateError::NestingLimit
    );
    let text = format!(
        "{{\"future\":\"{}\\\"{}\\\\\"}}",
        "[".repeat(1000),
        "}".repeat(1000)
    );
    assert_eq!(document(text.as_bytes()).as_bytes(), text.as_bytes());
}

#[test]
fn key_and_plaintext_use_zeroizing_storage_and_redacted_debug() {
    fn zeroizes_on_drop<T: zeroize::ZeroizeOnDrop>(_: &T) {}
    let mut key = fixture_key();
    zeroizes_on_drop(&key.0);
    let cipher = Aes256Gcm::new_from_slice(key.0.as_ref()).unwrap();
    zeroizes_on_drop(&cipher);
    let mut doc = document(br#"{"secret":"fixture-content"}"#);
    zeroizes_on_drop(&doc.0);
    assert_eq!(format!("{key:?}"), "StateKey([REDACTED])");
    assert_eq!(format!("{doc:?}"), "JsonDocument([REDACTED])");
    key.0.zeroize();
    doc.0.zeroize();
    assert_eq!(key.0.as_ref(), &[0; 32]);
    assert!(doc.as_bytes().is_empty());
}

#[test]
fn telegram_destination_binding_matches_go_identity_and_hides_credentials() {
    let key = fixture_key();
    let binding = DestinationBinding::telegram(&key, " bot-token ", " 12345 ");
    assert_eq!(
        binding.to_hex(),
        "5465b5f8fd805ed1235ced96636f70c1d2aa9198ab8c01a673f2bb34d51ce7e8"
    );
    assert_eq!(
        DestinationBinding::telegram(&key, "bot-token", "12345").to_hex(),
        binding.to_hex()
    );
    assert_ne!(
        DestinationBinding::telegram(&key, "other-token", "12345").to_hex(),
        binding.to_hex()
    );
    let debug = format!("{binding:?}");
    assert_eq!(debug, "DestinationBinding([REDACTED])");
    assert!(!debug.contains("bot-token"));
}
