use super::*;
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/tmp/s10-family-inspection");
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!(
            "{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let fixture = Self(path);
        fixture.put(ACTIVE, &legacy());
        fixture
    }
    fn put(&self, name: &str, bytes: &[u8]) {
        fs::write(self.0.join(name), bytes).unwrap();
    }
    fn remove(&self, name: &str) {
        fs::remove_file(self.0.join(name)).unwrap();
    }
    fn proposal(&self) -> MigrationProposal {
        inspect(&self.0)
            .unwrap()
            .propose_migration("0123456789abcdef0123456789abcdef")
            .unwrap()
    }
    fn snapshot(&self) -> Vec<(String, Vec<u8>, u64)> {
        std::iter::once(ACTIVE)
            .chain(SIBLINGS)
            .filter_map(|name| {
                fs::read(self.0.join(name)).ok().map(|bytes| {
                    (
                        name.to_owned(),
                        bytes,
                        fs::metadata(self.0.join(name)).unwrap().ino(),
                    )
                })
            })
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn legacy() -> Vec<u8> {
    let records: serde_json::Map<_, _> = [
        ("old-prepared", "prepared", false),
        ("old-submitting", "submitting", false),
        ("old-unknown", "unknown", false),
        ("old-accepted-unarchived", "accepted", false),
        ("old-accepted", "accepted", true),
    ]
    .iter()
    .map(|(id, state, archived)| {
        (
            id.to_string(),
            json!({
                "client_request_id":id,"phone":"+12025550100","message":"old body",
                "device":"reader","subscription_id":"sub-A","binding":"original-route",
                "state":state,"attempt":if *state=="prepared" {""} else {"original-attempt"},
                "segments":if *state=="accepted" {1} else {0},"archive_confirmed":archived,
                "created_at_ms":1,"updated_at_ms":2
            }),
        )
    })
    .collect();
    // Whitespace is deliberately retained in the frozen byte witness.
    serde_json::to_vec_pretty(&json!([{"schema_version":1,"records":records}])).unwrap()
}
fn step(f: &Fixture) -> RecoveryStep {
    inspect(&f.0).unwrap().next_step()
}
fn stage(f: &Fixture, proposal: &MigrationProposal) {
    f.put(JOURNAL, &proposal.journal_bytes);
    f.put(FROZEN, &proposal.frozen_bytes);
    f.put(PREPARED, &proposal.prepared_bytes);
}

#[test]
fn ordered_crash_family_matrix_preserves_all_legacy_phases_and_exact_raw_bytes() {
    let f = Fixture::new();
    let before = f.snapshot();
    assert_eq!(step(&f), RecoveryStep::ProposeMigration);
    let p = f.proposal();
    assert_eq!(p.frozen_bytes, legacy());
    let v2 = decode_snapshot_bytes(&p.prepared_bytes).unwrap();
    assert_eq!(v2.issued_high_water.0, 0);
    assert!(v2.live_slots.is_empty() && v2.terminal_cache.is_empty());
    let index =
        CompleteLegacyIndex::from_snapshot(&p.frozen_bytes, &digest(&p.frozen_bytes).0).unwrap();
    assert!(v2.legacy_overlay == index.fences.values().cloned().collect::<Vec<_>>());
    assert_eq!(
        f.snapshot(),
        before,
        "inspection and proposal never write archive data"
    );
    f.put(JOURNAL, &p.journal_bytes);
    assert_eq!(step(&f), RecoveryStep::FreezeLegacy);
    f.put(FROZEN, &p.frozen_bytes);
    assert_eq!(step(&f), RecoveryStep::PrepareV2);
    f.put(PREPARED, &p.prepared_bytes);
    assert_eq!(step(&f), RecoveryStep::ReplaceActive);
    f.put(ACTIVE, &p.prepared_bytes);
    assert_eq!(step(&f), RecoveryStep::V2Present);
    f.remove(PREPARED);
    assert!(inspect(&f.0).is_err());
    f.put(PREPARED, &p.prepared_bytes);
    let before = f.snapshot();
    assert!(
        inspect(&f.0)
            .unwrap()
            .propose_migration("0123456789abcdef0123456789abcdef")
            .is_err()
    );
    assert_eq!(f.snapshot(), before);
}

#[test]
fn every_partial_or_orphan_family_is_classified_or_rejected_without_repair() {
    // All 8 sidecar-presence combinations for both active versions.
    for active_v2 in [false, true] {
        for mask in 0..8 {
            let f = Fixture::new();
            let p = f.proposal();
            for (bit, name, bytes) in [
                (1, FROZEN, &p.frozen_bytes),
                (2, PREPARED, &p.prepared_bytes),
                (4, JOURNAL, &p.journal_bytes),
            ] {
                if mask & bit != 0 {
                    f.put(name, bytes);
                }
            }
            if active_v2 {
                f.put(ACTIVE, &p.prepared_bytes);
            }
            let before = f.snapshot();
            let got = inspect(&f.0).map(|state| state.next_step());
            let expected = match (active_v2, mask) {
                (false, 0) => Some(RecoveryStep::ProposeMigration),
                (false, 4) => Some(RecoveryStep::FreezeLegacy),
                (false, 5) => Some(RecoveryStep::PrepareV2),
                (false, 7) => Some(RecoveryStep::ReplaceActive),
                (true, 7) => Some(RecoveryStep::V2Present),
                _ => None,
            };
            assert_eq!(got.ok(), expected, "active_v2={active_v2} mask={mask}");
            assert_eq!(f.snapshot(), before, "ambiguous family never repaired");
        }
    }
}

#[test]
fn locked_raw_family_rejects_duplicate_keys_at_every_member_and_escaped_depth() {
    for name in std::iter::once(ACTIVE).chain(SIBLINGS) {
        for escaped in [false, true] {
            let f = Fixture::new();
            let p = f.proposal();
            stage(&f, &p);
            let raw = fs::read(f.0.join(name)).unwrap();
            let string = String::from_utf8(raw).unwrap();
            // Same effective field value: Value-first loading would erase the
            // duplicate evidence without causing a digest or schema difference
            // for active v1. Every member must fail BEFORE schema inspection.
            let key = if escaped {
                "schema_versi\\u006fn"
            } else {
                "schema_version"
            };
            let malformed = string.replacen('{', &format!("{{\"{key}\":1,"), 1);
            f.put(name, malformed.as_bytes());
            let before = f.snapshot();
            assert!(
                matches!(inspect(&f.0), Err(FamilyInspectionError::Storage(_))),
                "member={name}"
            );
            assert_eq!(f.snapshot(), before);
        }
    }
    let f = Fixture::new();
    let text = String::from_utf8(legacy()).unwrap();
    f.put(
        ACTIVE,
        text.replacen("\"records\": {", "\"records\": {\"old-prepared\":{},", 1)
            .as_bytes(),
    );
    assert!(matches!(
        inspect(&f.0),
        Err(FamilyInspectionError::Storage(_))
    ));
}

#[test]
fn original_writer_lock_is_held_before_any_raw_family_read() {
    let f = Fixture::new();
    // Establish the original named lock, then hold it as a separate opener.
    assert_eq!(step(&f), RecoveryStep::ProposeMigration);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.0.join(format!("{ACTIVE}.lock")))
        .unwrap();
    file.lock().unwrap();
    let root = f.0.clone();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(inspect(&root).map(|v| v.next_step())).unwrap();
    });
    assert!(matches!(
        rx.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    // A writer changes raw bytes while owning the ORIGINAL lock. The waiting
    // inspector must read these corrupt bytes only after acquiring that lock.
    let malformed = String::from_utf8(legacy())
        .unwrap()
        .replacen('{', "{\"schema_version\":1,", 1);
    f.put(ACTIVE, malformed.as_bytes());
    drop(file);
    assert!(rx.recv_timeout(Duration::from_secs(10)).unwrap().is_err());
    worker.join().unwrap();
}

#[test]
fn concurrent_proposals_are_read_only_and_no_stale_proposal_can_commit() {
    let f = Fixture::new();
    let root = Arc::new(f.0.clone());
    let before = f.snapshot();
    let barrier = Arc::new(Barrier::new(12));
    let workers: Vec<_> = (0..12)
        .map(|_| {
            let root = root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                inspect(&root)
                    .unwrap()
                    .propose_migration("0123456789abcdef0123456789abcdef")
                    .unwrap()
                    .prepared_bytes
            })
        })
        .collect();
    let expected = f.proposal().prepared_bytes;
    for worker in workers {
        assert_eq!(worker.join().unwrap(), expected);
    }
    assert_eq!(f.snapshot(), before);
}

#[test]
fn family_refuses_missing_or_bad_witnesses_changed_v1_and_rewritten_frozen_bytes() {
    for name in [ACTIVE, FROZEN, JOURNAL, PREPARED] {
        for corrupt in [b"".as_slice(), b"null", b"[]", b"[{}]", b"not json"] {
            let f = Fixture::new();
            let p = f.proposal();
            stage(&f, &p);
            f.put(name, corrupt);
            let before = f.snapshot();
            assert!(inspect(&f.0).is_err(), "{name} {corrupt:?}");
            assert_eq!(f.snapshot(), before);
        }
    }
    let f = Fixture::new();
    let p = f.proposal();
    stage(&f, &p);
    let value: Value = serde_json::from_slice(&p.frozen_bytes).unwrap();
    f.put(FROZEN, &serde_json::to_vec(&value).unwrap());
    assert!(inspect(&f.0).is_err(), "rewritten raw witness refused");
    f.put(FROZEN, &p.frozen_bytes);
    f.put(ACTIVE, &serde_json::to_vec(&value).unwrap());
    assert!(inspect(&f.0).is_err(), "changed original v1 refused");
    f.remove(ACTIVE);
    assert!(inspect(&f.0).is_err(), "never initialize absent authority");
}

#[test]
fn symlinks_directories_and_unsafe_sibling_names_fail_closed() {
    for name in std::iter::once(ACTIVE).chain(SIBLINGS) {
        let f = Fixture::new();
        let p = f.proposal();
        stage(&f, &p);
        f.remove(name);
        std::os::unix::fs::symlink("missing-target", f.0.join(name)).unwrap();
        assert!(inspect(&f.0).is_err(), "symlink {name}");
        f.remove(name);
        fs::create_dir(f.0.join(name)).unwrap();
        assert!(inspect(&f.0).is_err(), "directory {name}");
    }
    let f = Fixture::new();
    let store = ArchiveStore::with_limits(f.0.join(ACTIVE), SNAPSHOT_BYTES as u64, 1).unwrap();
    for names in [
        vec!["../outside.json"],
        vec!["/absolute.json"],
        vec![ACTIVE],
        vec!["same", "same"],
        vec!["device-send-ledger.json.lock"],
        vec!["a", "b", "c", "d"],
    ] {
        assert!(
            store
                .inspect_raw_family(&names, |_, _| Ok::<_, ()>(()))
                .is_err()
        );
    }
    assert!(f.0.join(ACTIVE).exists());
}

#[test]
fn v2_reserve_retirement_and_old_decoder_refusal_keep_complete_family() {
    let f = Fixture::new();
    let p = f.proposal();
    stage(&f, &p);
    f.put(ACTIVE, &p.prepared_bytes);
    let legacy =
        CompleteLegacyIndex::from_snapshot(&p.frozen_bytes, &digest(&p.frozen_bytes).0).unwrap();
    let state = decode_snapshot_bytes(&p.prepared_bytes).unwrap();
    let id = next_identity(&state).unwrap();
    let request=decode_reserve_bytes(&serde_json::to_vec(&json!({
        "identity_version":2,"family":"device_sms","authority_id":state.authority_id.0,
        "epoch":"1","sequence":"1","client_request_id":id.wire(),
        "intent_nonce":"abcdef0123456789abcdef0123456789","purpose":"device_sms_send",
        "device":"reader","payload":{"phone":"+12025550100","message":"body","expected_subscription_id":"sub-A"}
    })).unwrap()).unwrap();
    let reserved = plan_reserve(&state, &request, &legacy).unwrap();
    f.put(ACTIVE, &reserved.encoded().unwrap());
    assert_eq!(step(&f), RecoveryStep::V2Present);
    let retired = plan_retire_unclaimed(&reserved.next, &request, &legacy).unwrap();
    f.put(ACTIVE, &retired.encoded().unwrap());
    assert_eq!(step(&f), RecoveryStep::V2Present);
    assert_eq!(retired.next.issued_high_water.0, 1);
    assert_eq!(legacy.fences.len(), 5);
    assert!(legacy.fences.contains_key("old-accepted-unarchived"));
    // The old production schema1 decoder refuses a migrated family: it cannot
    // make a v2 slot claimable by treating it as a legacy request.
    let old = crate::device_send_store::Store::open(&f.0).unwrap();
    for action in ["prepare", "claim", "finish"] {
        assert!(
            old.apply(
                action,
                json!({"client_request_id":id.wire(),"binding":"old-route","attempt":"old-attempt"})
            )
            .is_err()
        );
    }
    assert_eq!(step(&f), RecoveryStep::V2Present);
}

#[test]
fn candidate_authority_never_reinterprets_legacy_v2_looking_identity() {
    let f = Fixture::new();
    let old_id = "m2:ds:0123456789abcdef0123456789abcdef:0000000000000001:ffffffffffffffff";
    let mut value: Value = serde_json::from_slice(&legacy()).unwrap();
    let mut record = value[0]["records"]["old-unknown"].take();
    value[0]["records"]
        .as_object_mut()
        .unwrap()
        .remove("old-unknown");
    record["client_request_id"] = json!(old_id);
    value[0]["records"][old_id] = record;
    f.put(ACTIVE, &serde_json::to_vec(&value).unwrap());
    let inspected = inspect(&f.0).unwrap();
    assert!(matches!(
        inspected.propose_migration("0123456789abcdef0123456789abcdef"),
        Err(CodecError::LegacyCollision)
    ));
    assert!(
        inspected
            .propose_migration("11111111111111111111111111111111")
            .is_ok()
    );
    for authority in ["", "UPPERCASE", "0123456789ABCDEF0123456789ABCDEF"] {
        assert!(inspected.propose_migration(authority).is_err());
    }
}

#[test]
fn journal_digest_authority_and_full_legacy_overlay_are_cross_checked() {
    let f = Fixture::new();
    let p = f.proposal();
    stage(&f, &p);
    f.put(ACTIVE, &p.prepared_bytes);
    let original = decode_snapshot_bytes(&p.prepared_bytes).unwrap();
    let mut current = original.clone();
    current
        .legacy_overlay
        .retain(|fence| fence.client_request_id != "old-accepted-unarchived");
    assert!(
        encode_checked(&current).is_ok(),
        "codec alone accepts subset; family may not"
    );
    f.put(ACTIVE, &encode_checked(&current).unwrap());
    assert!(
        inspect(&f.0).is_err(),
        "lost accepted-unarchived fence refused"
    );
    f.put(ACTIVE, &p.prepared_bytes);
    let mut journal: Value = serde_json::from_slice(&p.journal_bytes).unwrap();
    for field in ["authority_id", "legacy_sha256", "initial_v2_sha256"] {
        let saved = journal[0][field].clone();
        journal[0][field] = json!("1".repeat(if field == "authority_id" { 32 } else { 64 }));
        f.put(JOURNAL, &serde_json::to_vec(&journal).unwrap());
        assert!(inspect(&f.0).is_err(), "tampered {field}");
        journal[0][field] = saved;
    }
}

#[test]
fn full_ten_thousand_record_family_is_frozen_without_paging_or_losing_last_fence() {
    let f = Fixture::new();
    let base: Value = serde_json::from_slice(&legacy()).unwrap();
    let template = &base[0]["records"]["old-unknown"];
    let mut records = serde_json::Map::new();
    for n in 0..crate::device_send_store::MAX_RECORDS {
        let id = format!("legacy-{n:05}");
        let mut record = template.clone();
        record["client_request_id"] = json!(id);
        records.insert(id, record);
    }
    let bytes = serde_json::to_vec(&json!([{"schema_version":1,"records":records}])).unwrap();
    f.put(ACTIVE, &bytes);
    let p = f.proposal();
    assert_eq!(p.frozen_bytes, bytes);
    let snapshot = decode_snapshot_bytes(&p.prepared_bytes).unwrap();
    assert_eq!(snapshot.legacy_overlay.len(), 10_000);
    assert_eq!(
        snapshot.legacy_overlay.last().unwrap().client_request_id,
        "legacy-09999"
    );
    stage(&f, &p);
    f.put(ACTIVE, &p.prepared_bytes);
    assert_eq!(step(&f), RecoveryStep::V2Present);
}

#[test]
fn legacy_overlay_allows_only_recovery_and_archive_ack_monotonic_progress() {
    let f = Fixture::new();
    let p = f.proposal();
    stage(&f, &p);
    let mut current = decode_snapshot_bytes(&p.prepared_bytes).unwrap();
    for item in &mut current.legacy_overlay {
        if item.original_state == LegacyPhase::Submitting {
            item.original_state = LegacyPhase::Unknown;
        }
        if item.original_state == LegacyPhase::Accepted {
            item.archive_confirmed = true;
        }
    }
    let progressed = encode_checked(&current).unwrap();
    f.put(ACTIVE, &progressed);
    assert_eq!(step(&f), RecoveryStep::V2Present);
    assert!(validate_transition(&p.prepared_bytes, &progressed).is_ok());
    assert!(validate_transition(&progressed, &p.prepared_bytes).is_err());
    let accepted = current
        .legacy_overlay
        .iter_mut()
        .find(|fence| fence.original_state == LegacyPhase::Accepted)
        .unwrap();
    accepted.original_attempt = "different-attempt".into();
    f.put(ACTIVE, &encode_checked(&current).unwrap());
    assert!(inspect(&f.0).is_err());
}
