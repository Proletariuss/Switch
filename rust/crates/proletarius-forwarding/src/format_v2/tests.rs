use super::*;
use crate::{
    DeliveryOutcome, DestinationBinding, EnqueueOptions, ForwardingChannel, ForwardingMessage,
    QueueStore,
};
use std::{fs, os::unix::fs::PermissionsExt};
fn key() -> StateKey {
    StateKey::from_slice(&[9; 32]).unwrap()
}
fn identity() -> FormatIdentity {
    FormatIdentity {
        root_id: "a".repeat(32),
        source_id: "b".repeat(32),
        queue_id: "c".repeat(32),
    }
}
fn legacy(bytes: &[u8]) -> Vec<u8> {
    crate::seal(
        &key(),
        &JsonDocument::from_bytes(Zeroizing::new(bytes.to_vec()), StateLimits::default()).unwrap(),
        StateLimits::default(),
    )
    .unwrap()
}
fn prepared(wire: &[u8]) -> Vec<u8> {
    prepare_from_swf1(
        &key(),
        wire,
        identity(),
        "d".repeat(64),
        StateLimits::default(),
    )
    .unwrap()
}
fn encrypted_payload(payload: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new_from_slice(key().as_bytes()).unwrap();
    let n: Nonce<Aes256Gcm> = [3; 12].into();
    let mut p = payload.to_vec();
    cipher.encrypt_in_place(&n, MAGIC, &mut p).unwrap();
    let mut out = b"SWF2".to_vec();
    out.extend_from_slice(&[3; 12]);
    out.extend(p);
    out
}
fn header_body(header: &[u8], body: &[u8]) -> Vec<u8> {
    let mut p = (header.len() as u32).to_be_bytes().to_vec();
    p.extend_from_slice(header);
    p.extend_from_slice(body);
    encrypted_payload(&p)
}
const RAW:&[u8]=b"{ \"seen\" : {}, \"pending\":{}, \"opaque\":{\"big\":9007199254740993.0,\"escape\":\"\\u4e2d\"} }\n";
#[test]
fn b3a_dual_read_and_prepared_v2_preserve_exact_legacy_plaintext() {
    let old = legacy(RAW);
    let one = read_compatible(&key(), &old, StateLimits::default()).unwrap();
    assert_eq!(one.format, DecodedFormat::Swf1);
    assert_eq!(one.document.as_bytes(), RAW);
    let wire = prepared(&old);
    let two = read_compatible(&key(), &wire, StateLimits::default()).unwrap();
    assert_eq!(two.format, DecodedFormat::Swf2Prepared);
    assert_eq!(two.document.as_bytes(), RAW);
    assert_eq!(
        two.header.unwrap().legacy_wire_sha256,
        format!("{:x}", Sha256::digest(&old))
    );
}
#[test]
fn b3a_old_public_reader_rejects_swf2_before_queue_projection() {
    let wire = prepared(&legacy(RAW));
    assert!(matches!(
        crate::open(&key(), &wire, StateLimits::default()),
        Err(StateError::UnsupportedFormat)
    ));
}
#[test]
fn b3a_magic_relabel_cannot_cross_aad_or_normalize_v2_into_v1() {
    let mut wire = prepared(&legacy(RAW));
    wire[..4].copy_from_slice(b"SWF1");
    assert!(matches!(
        crate::open(&key(), &wire, StateLimits::default()),
        Err(StateError::AuthenticationFailed)
    ));
    let mut old = legacy(RAW);
    old[..4].copy_from_slice(b"SWF2");
    assert!(matches!(
        read_compatible(&key(), &old, StateLimits::default()),
        Err(FormatError::State(StateError::AuthenticationFailed))
    ));
}
#[test]
fn b3a_rng_auth_truncation_and_limits_never_return_document() {
    let old = legacy(RAW);
    assert!(matches!(
        prepare_with_entropy(
            &key(),
            &old,
            identity(),
            "d".repeat(64),
            StateLimits::default(),
            |_| Err(())
        ),
        Err(FormatError::State(StateError::EntropyUnavailable))
    ));
    let mut wire = prepared(&old);
    let last = wire.len() - 1;
    wire[last] ^= 1;
    assert!(matches!(
        read_compatible(&key(), &wire, StateLimits::default()),
        Err(FormatError::State(StateError::AuthenticationFailed))
    ));
    assert!(matches!(
        read_compatible(&key(), b"SWF2", StateLimits::default()),
        Err(FormatError::State(StateError::Truncated))
    ));
    assert!(read_compatible(&key(), &prepared(&old), StateLimits::new(2).unwrap()).is_err());
}
#[test]
fn b3a_strict_header_length_duplicate_future_unknown_and_phase_refuse() {
    let good = read_compatible(&key(), &prepared(&legacy(RAW)), StateLimits::default())
        .unwrap()
        .header
        .unwrap();
    let header = serde_json::to_string(&good).unwrap();
    for altered in [
        header.replacen("\"schema_version\":2", "\"schema_version\":3", 1),
        header.replacen(
            "\"schema_version\":2",
            "\"schema_version\":2,\"schema_version\":2",
            1,
        ),
        header.replacen("\"prepared\"", "\"bound\"", 1),
        header.replacen("{", "{\"unknown\":1,", 1),
    ] {
        assert!(
            read_compatible(
                &key(),
                &header_body(altered.as_bytes(), RAW),
                StateLimits::default()
            )
            .is_err()
        );
    }
    for prefix in [0u32, u32::MAX, 4097] {
        let p = prefix.to_be_bytes();
        assert!(matches!(
            read_compatible(&key(), &encrypted_payload(&p), StateLimits::default()),
            Err(FormatError::InvalidHeader)
        ));
    }
}
#[test]
fn b3a_digest_and_nested_escaped_duplicate_evidence_refuse() {
    let good = read_compatible(&key(), &prepared(&legacy(RAW)), StateLimits::default())
        .unwrap()
        .header
        .unwrap();
    let header = serde_json::to_vec(&good).unwrap();
    assert!(matches!(
        read_compatible(
            &key(),
            &header_body(&header, b"{\"seen\":{},\"pending\":{}}"),
            StateLimits::default()
        ),
        Err(FormatError::DigestMismatch)
    ));
    let duplicate = br#"{"seen":{},"pending":{},"unknown":{"x":1,"\u0078":2}}"#;
    assert!(matches!(
        prepare_from_swf1(
            &key(),
            &legacy(duplicate),
            identity(),
            "d".repeat(64),
            StateLimits::default()
        ),
        Err(FormatError::DuplicateKey)
    ));
}
#[test]
fn b3a_actual_unknown_attempt_and_frozen_chunks_survive_format_exactly() {
    let root = std::path::PathBuf::from(std::env::var_os("SWITCH_BUILD_DIR").unwrap())
        .join(format!("tmp/b3a-state-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("queue.enc");
    let state_key = key();
    let target = DestinationBinding::telegram(&state_key, "synthetic", "synthetic");
    let options = EnqueueOptions::new(true, &target, false, &target);
    let mut store = QueueStore::open(&path, state_key, StateLimits::default()).unwrap();
    let message = ForwardingMessage::new(
        "synthetic",
        "inbound",
        "sender",
        "body",
        "2026-01-01T00:00:00Z",
        "N待",
    )
    .unwrap();
    store
        .enqueue_and_persist(&[message], "2026-01-01T00:00:00Z", &options)
        .unwrap();
    let task = store.inspect().unwrap().tasks[0].id.clone();
    store
        .freeze_telegram_progress(&task, &target, &[Zeroizing::new("chunk".into())])
        .unwrap();
    let token = store
        .begin_destination_attempt(
            &task,
            ForwardingChannel::Telegram,
            true,
            &target,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
    store
        .finish_destination_attempt(
            &task,
            ForwardingChannel::Telegram,
            &target,
            "2026-01-01T00:00:00Z",
            &DeliveryOutcome::Failed {
                error_code: "synthetic_unknown".into(),
                permanent: false,
                result_unknown: true,
                retry_at: None,
            },
            &token,
        )
        .unwrap();
    let original = fs::read(&path).unwrap();
    let doc = crate::open(&key(), &original, StateLimits::default()).unwrap();
    let result = read_compatible(&key(), &prepared(&original), StateLimits::default()).unwrap();
    assert_eq!(result.document.as_bytes(), doc.as_bytes());
    assert_eq!(fs::read(&path).unwrap(), original);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
