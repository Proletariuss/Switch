use super::*;
use crate::tests::TestDirectory;
use serde_json::json;
use std::{
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

const AUTHORITY: &str = "0123456789abcdef0123456789abcdef";
const GENERATION: &str = "abcdef0123456789abcdef0123456789";
fn identity() -> Identity {
    Identity::parse(AUTHORITY, GENERATION).unwrap()
}
fn valid(bytes: &[u8]) -> Result<(), &'static str> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| "json")?;
    if v.as_array().is_none_or(|a| a.len() != 1) {
        return Err("shape");
    }
    Ok(())
}
fn fixture() -> (TestDirectory, Kernel, ArchiveStore) {
    let dir = TestDirectory::new();
    let old = ArchiveStore::with_limits(dir.archive(ACTIVE), MAX_ACTIVE, 1)
        .unwrap()
        .track_durable_presence();
    old.replace(vec![json!({"schema_version":1,"records":{}})])
        .unwrap();
    let kernel = Kernel::existing(&dir.0).unwrap();
    (dir, kernel, old)
}
fn target(legacy: &[u8], floor: u64, body: &str) -> Vec<u8> {
    serde_json::to_vec(&json!([{"schema_version":2,"family":"device_sms","authority_id":AUTHORITY,"epoch":"1",
        "issued_high_water":floor.to_string(),"legacy":{"path":"device-send-ledger.legacy-v1.json","sha256":digest(legacy)},"body":body}])).unwrap()
}
fn old_permission(old: &ArchiveStore) -> bool {
    old.mutate(|_| Ok(())).is_ok()
}
fn prefix_fenced(point: Point) -> bool {
    matches!(
        point,
        Point::MarkerRenamed
            | Point::MarkerDirectorySynced
            | Point::AnchorTruncated
            | Point::AnchorPartial
            | Point::AnchorWritten
            | Point::AnchorSynced
    )
}

fn evidence(case: &str, point: Point, kernel: &Kernel) {
    let Some(dir) = std::env::var_os("SWITCH_SEAL_EVIDENCE_DIR") else {
        return;
    };
    let dir = Path::new(&dir);
    fs::create_dir_all(dir).unwrap();
    let members = [
        kernel.archive.path.clone(),
        kernel.archive.presence_path().unwrap(),
        kernel.archive.lock_path().unwrap(),
    ];
    let snapshots:Vec<_>=members.iter().map(|path| {
        let bytes=fs::read(path).ok();
        json!({"member":path.file_name().unwrap().to_string_lossy(),"sha256":bytes.as_ref().map(|b|digest(b)),
            "bytes_hex":bytes.map(|b|b.iter().map(|byte|format!("{byte:02x}")).collect::<String>())})
    }).collect();
    fs::write(dir.join(format!("{case}-{point:?}.json")),serde_json::to_vec_pretty(&json!({"case":case,"point":format!("{point:?}"),"members":snapshots,"new_dispatch_calls":0})).unwrap()).unwrap();
}

#[test]
fn install_is_idempotent_preserves_active_and_current_v1_rejects_original_witnesses() {
    let (dir, kernel, old) = fixture();
    let active = fs::read(old.path()).unwrap();
    let inode = fs::metadata(old.path()).unwrap().ino();
    let view = kernel.install(&identity(), valid).unwrap();
    assert_eq!(view.revision(), 0);
    assert_eq!(view.issued_floor(), 0);
    assert!(!old_permission(&old));
    assert_eq!(kernel.inspect(&identity(), valid).unwrap(), view);
    assert_eq!(kernel.install(&identity(), valid).unwrap(), view);
    assert_eq!(fs::read(old.path()).unwrap(), active);
    assert_eq!(fs::metadata(old.path()).unwrap().ino(), inode);
    let marker = fs::read(kernel.archive.presence_path().unwrap()).unwrap();
    assert_eq!(
        marker,
        fs::read(kernel.archive.lock_path().unwrap()).unwrap()
    );
    assert!(Seal::decode(&marker).is_ok());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 3);
}

#[test]
fn red_legacy_sidecars_do_not_fence_current_v1_but_versioned_original_witnesses_do() {
    let (dir, kernel, old) = fixture();
    fs::write(
        dir.archive("device-send-ledger.v2-migration.json"),
        b"[{\"family\":\"device_sms\"}]",
    )
    .unwrap();
    assert!(
        old_permission(&old),
        "RED: an ignored sibling alone leaves current v1 writable"
    );
    kernel.install(&identity(), valid).unwrap();
    assert!(
        !old_permission(&old),
        "GREEN: original versioned witnesses stop current v1 before callback"
    );
}

#[test]
fn every_install_fault_prefix_returns_no_receipt_and_never_changes_active() {
    for point in Point::ALL {
        let (_dir, kernel, old) = fixture();
        let active = fs::read(old.path()).unwrap();
        let fault = kernel
            .clone()
            .with_hook_for_test(Arc::new(move |at| at == point));
        assert!(matches!(fault.install(&identity(),valid),Err(Error::Injected(p)) if p==point));
        assert_eq!(fs::read(old.path()).unwrap(), active);
        evidence("install-error", point, &kernel);
        assert_eq!(
            !old_permission(&old),
            prefix_fenced(point),
            "point={point:?}"
        );
        let inspected = kernel.inspect(&identity(), valid);
        // Written-but-unsynced complete anchor is visible as coherent here;
        // this is process-visible state, not simulated power-loss durability.
        assert_eq!(
            inspected.is_ok(),
            matches!(point, Point::AnchorWritten | Point::AnchorSynced),
            "{point:?}"
        );
    }
}

#[test]
fn target_staging_is_not_an_active_commit_or_issuer_and_floor_never_decreases() {
    let (_dir, kernel, old) = fixture();
    let legacy = fs::read(old.path()).unwrap();
    let installed = kernel.install(&identity(), valid).unwrap();
    let initial = target(&legacy, 0, "initial");
    let first = kernel.stage_v2_target(&installed, &initial, valid).unwrap();
    assert_eq!(first.revision(), 1);
    assert_eq!(first.issued_floor(), 0);
    assert_eq!(fs::read(old.path()).unwrap(), legacy);
    assert!(
        kernel.inspect(&identity(), valid).is_err(),
        "target remains uncommitted and fenced"
    );
    assert!(kernel.stage_v2_target(&first, &initial, valid).is_err());
    // Fixture-only simulation of a future executor; Kernel has no active writer.
    fs::write(old.path(), &initial).unwrap();
    assert_eq!(kernel.inspect(&identity(), valid).unwrap(), first);
    let next = target(&legacy, 7, "reserved");
    let second = kernel.stage_v2_target(&first, &next, valid).unwrap();
    assert_eq!(second.issued_floor(), 7);
    assert_eq!(fs::read(old.path()).unwrap(), initial);
    assert!(kernel.inspect(&identity(), valid).is_err());
    fs::write(old.path(), &next).unwrap();
    assert_eq!(kernel.inspect(&identity(), valid).unwrap(), second);
    assert!(matches!(
        kernel.stage_v2_target(&second, &initial, valid),
        Err(Error::Guard(GuardError::Rollback))
    ));
    assert!(matches!(
        kernel.stage_v2_target(&first, &next, valid),
        Err(Error::Guard(GuardError::Conflict))
    ));
    fs::write(old.path(), &initial).unwrap();
    assert!(
        kernel.inspect(&identity(), valid).is_err(),
        "active-only rollback denied"
    );
}

#[test]
fn every_target_update_fault_is_fenced_and_reopen_never_accepts_old_active_as_new_floor() {
    for point in Point::ALL {
        let (_dir, kernel, old) = fixture();
        let legacy = fs::read(old.path()).unwrap();
        let initial_view = kernel.install(&identity(), valid).unwrap();
        let v2 = target(&legacy, 0, "initial");
        let first = kernel.stage_v2_target(&initial_view, &v2, valid).unwrap();
        fs::write(old.path(), &v2).unwrap();
        let proposed = target(&legacy, 9, "reserved");
        let faulty = kernel
            .clone()
            .with_hook_for_test(Arc::new(move |at| at == point));
        assert!(
            matches!(faulty.stage_v2_target(&first,&proposed,valid),Err(Error::Injected(p)) if p==point)
        );
        assert_eq!(fs::read(old.path()).unwrap(), v2);
        evidence("target-error", point, &kernel);
        assert!(!old_permission(&old));
        let state = kernel.inspect(&identity(), valid);
        assert_eq!(state.is_ok(), !prefix_fenced(point), "{point:?}");
        if let Ok(view) = state {
            assert_eq!(view, first);
        }
    }
}

#[test]
fn missing_partial_cross_identity_replaced_and_rolled_back_witnesses_never_recreate() {
    for member in [
        ACTIVE,
        "device-send-ledger.json.lock",
        ".device-send-ledger.json.presence-v1",
    ] {
        for kind in ["missing", "empty", "truncated", "symlink", "directory"] {
            let (dir, kernel, old) = fixture();
            kernel.install(&identity(), valid).unwrap();
            let path = dir.archive(member);
            let original = fs::read(&path).unwrap();
            match kind {
                "missing" => fs::remove_file(&path).unwrap(),
                "empty" => fs::write(&path, b"").unwrap(),
                "truncated" => fs::write(&path, &original[..original.len() / 2]).unwrap(),
                "symlink" => {
                    fs::remove_file(&path).unwrap();
                    std::os::unix::fs::symlink("missing", &path).unwrap();
                }
                "directory" => {
                    fs::remove_file(&path).unwrap();
                    fs::create_dir(&path).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                kernel.inspect(&identity(), valid).is_err(),
                "{member} {kind}"
            );
            assert!(
                kernel.install(&identity(), valid).is_err(),
                "no authority recreation {member} {kind}"
            );
            if kind == "missing" {
                assert!(!path.exists(), "kernel did not recreate missing witness");
            }
            if member != ACTIVE {
                // Current v1 may create an empty lock while trying to open a
                // deleted anchor; the surviving new marker must still fence it.
                assert!(!old_permission(&old));
            }
        }
    }
    let (_dir, kernel, old) = fixture();
    let legacy = fs::read(old.path()).unwrap();
    let installed = kernel.install(&identity(), valid).unwrap();
    let previous = installed.0.encode();
    let v2 = target(&legacy, 1, "next");
    kernel.stage_v2_target(&installed, &v2, valid).unwrap();
    fs::write(old.path(), &v2).unwrap();
    for member in [
        kernel.archive.presence_path().unwrap(),
        kernel.archive.lock_path().unwrap(),
    ] {
        let new = fs::read(&member).unwrap();
        fs::write(&member, &previous).unwrap();
        assert!(
            kernel.inspect(&identity(), valid).is_err(),
            "single-witness rollback"
        );
        fs::write(&member, new).unwrap();
    }
    let other = Identity::parse("11111111111111111111111111111111", GENERATION).unwrap();
    assert!(kernel.inspect(&other, valid).is_err());
    let other = Identity::parse(AUTHORITY, "11111111111111111111111111111111").unwrap();
    assert!(kernel.inspect(&other, valid).is_err());
}

#[test]
fn strict_raw_input_and_schema_refusal_happen_under_lock_before_any_seal_write() {
    let (_dir, kernel, old) = fixture();
    let marker = fs::read(kernel.archive.presence_path().unwrap()).unwrap();
    fs::write(
        old.path(),
        br#"[{"schema_version":1,"records":{},"rec\u006frds":{}}]"#,
    )
    .unwrap();
    assert!(matches!(
        kernel.install(&identity(), |_| -> Result<(), ()> {
            panic!("duplicate reached decoder")
        }),
        Err(Error::Storage(_))
    ));
    assert_eq!(
        fs::read(kernel.archive.presence_path().unwrap()).unwrap(),
        marker
    );
    fs::write(old.path(), b"[{\"schema_version\":1,\"records\":{}}]").unwrap();
    assert!(matches!(
        kernel.install(&identity(), |_| Err::<(), _>("refused")),
        Err(Error::Schema("refused"))
    ));
    assert_eq!(
        fs::read(kernel.archive.presence_path().unwrap()).unwrap(),
        marker
    );
}

#[test]
fn callback_same_byte_entry_replacement_and_post_marker_swaps_are_rejected() {
    for member in [
        ACTIVE,
        "device-send-ledger.json.lock",
        ".device-send-ledger.json.presence-v1",
    ] {
        let (dir, kernel, _old) = fixture();
        let path = dir.archive(member);
        assert!(
            kernel
                .install(&identity(), |_| {
                    let tmp = dir.archive("swapped");
                    fs::write(&tmp, fs::read(&path).unwrap()).unwrap();
                    fs::rename(tmp, &path).unwrap();
                    Ok::<_, ()>(())
                })
                .is_err(),
            "{member}"
        );
    }
    let (dir, kernel, _) = fixture();
    let marker = kernel.archive.presence_path().unwrap();
    let moved = dir.archive("swapped");
    let hooked = kernel.with_hook_for_test(Arc::new(move |p| {
        if p == Point::MarkerDirectorySynced {
            fs::write(&moved, fs::read(&marker).unwrap()).unwrap();
            fs::rename(&moved, &marker).unwrap();
        }
        false
    }));
    assert!(matches!(
        hooked.install(&identity(), valid),
        Err(Error::Guard(GuardError::Changed))
    ));
}

#[test]
fn canonical_witness_and_target_identity_counters_are_bounded_and_exact() {
    let (_dir, kernel, old) = fixture();
    let legacy = fs::read(old.path()).unwrap();
    let installed = kernel.install(&identity(), valid).unwrap();
    let encoded = installed.0.encode();
    assert!(encoded.len() as u64 <= MAX_WITNESS);
    for invalid in [
        b"".to_vec(),
        [encoded.clone(), b"\n".to_vec()].concat(),
        String::from_utf8(encoded.clone())
            .unwrap()
            .replacen("device_sms", "other", 1)
            .into_bytes(),
        encoded[..encoded.len() - 1].to_vec(),
    ] {
        assert!(Seal::decode(&invalid).is_err());
    }
    for (field, value) in [
        ("authority_id", json!(GENERATION)),
        ("epoch", json!("2")),
        ("family", json!("other")),
        ("issued_high_water", json!("01")),
        ("issued_high_water", json!("18446744073709551616")),
    ] {
        let mut v: serde_json::Value = serde_json::from_slice(&target(&legacy, 1, "body")).unwrap();
        v[0][field] = value;
        assert!(
            kernel
                .stage_v2_target(&installed, &serde_json::to_vec(&v).unwrap(), valid)
                .is_err(),
            "{field}"
        );
    }
    let mut exhausted = installed.0.clone();
    exhausted.kind = Kind::V2Target;
    exhausted.revision = u64::MAX;
    let v2 = target(&legacy, 1, "full");
    exhausted.floor = 1;
    exhausted.active_digest = digest(&v2);
    fs::write(old.path(), &v2).unwrap();
    fs::write(kernel.archive.presence_path().unwrap(), exhausted.encode()).unwrap();
    fs::write(kernel.archive.lock_path().unwrap(), exhausted.encode()).unwrap();
    assert!(matches!(
        kernel.stage_v2_target(&View(exhausted), &v2, valid),
        Err(Error::Guard(GuardError::Exhausted))
    ));
}

#[test]
#[ignore = "subprocess helper invoked by crash-prefix test"]
fn seal_crash_worker() {
    let root = std::env::var_os("SWITCH_SEAL_CRASH_ROOT").expect("child root");
    let index: usize = std::env::var("SWITCH_SEAL_CRASH_POINT")
        .unwrap()
        .parse()
        .unwrap();
    let hit = Path::new(&root).join("hit");
    let kernel = Kernel::existing(Path::new(&root))
        .unwrap()
        .with_hook_for_test(Arc::new(move |p| {
            if p == Point::ALL[index] {
                fs::write(&hit, b"hit").unwrap();
                loop {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            false
        }));
    if std::env::var("SWITCH_SEAL_CRASH_MODE").as_deref() == Ok("target") {
        let expected = kernel.inspect(&identity(), valid).unwrap();
        let proposed = fs::read(Path::new(&root).join("proposed.json")).unwrap();
        kernel.stage_v2_target(&expected, &proposed, valid).unwrap();
    } else {
        kernel.install(&identity(), valid).unwrap();
    }
    panic!("child did not stop at fault");
}

#[test]
fn actual_process_kill_at_every_install_and_target_prefix_preserves_active_and_fences_old_writer() {
    for mode in ["install", "target"] {
        for (index, point) in Point::ALL.into_iter().enumerate() {
            let (dir, kernel, old) = fixture();
            let legacy = fs::read(old.path()).unwrap();
            if mode == "target" {
                let installed = kernel.install(&identity(), valid).unwrap();
                let initial = target(&legacy, 0, "initial");
                kernel.stage_v2_target(&installed, &initial, valid).unwrap();
                fs::write(old.path(), initial).unwrap();
                fs::write(dir.archive("proposed.json"), target(&legacy, 5, "next")).unwrap();
            }
            let active = fs::read(old.path()).unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "device_sms_seal::tests::seal_crash_worker",
                    "--nocapture",
                ])
                .env("SWITCH_SEAL_CRASH_ROOT", &dir.0)
                .env("SWITCH_SEAL_CRASH_POINT", index.to_string())
                .env("SWITCH_SEAL_CRASH_MODE", mode)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while !dir.archive("hit").exists() {
                assert!(Instant::now() < deadline, "child {mode} point {point:?}");
                assert!(child.try_wait().unwrap().is_none());
                std::thread::sleep(Duration::from_millis(5));
            }
            child.kill().unwrap();
            assert!(!child.wait().unwrap().success());
            assert_eq!(fs::read(old.path()).unwrap(), active);
            evidence(&format!("{mode}-kill"), point, &kernel);
            assert_eq!(
                !old_permission(&old),
                mode == "target" || prefix_fenced(point),
                "killed {mode} {point:?}"
            );
            let coherent = if mode == "install" {
                matches!(point, Point::AnchorWritten | Point::AnchorSynced)
            } else {
                !prefix_fenced(point)
            };
            assert_eq!(
                kernel.inspect(&identity(), valid).is_ok(),
                coherent,
                "{mode} {point:?}"
            );
        }
    }
}

#[test]
fn coherent_external_rollback_of_all_witnesses_and_active_is_not_locally_detectable() {
    let (_dir, kernel, old) = fixture();
    let legacy = fs::read(old.path()).unwrap();
    let installed = kernel.install(&identity(), valid).unwrap();
    let initial = target(&legacy, 0, "initial");
    let first = kernel.stage_v2_target(&installed, &initial, valid).unwrap();
    fs::write(old.path(), &initial).unwrap();
    let first_seal = first.0.encode();
    let next = target(&legacy, 9, "later");
    kernel.stage_v2_target(&first, &next, valid).unwrap();
    fs::write(old.path(), &next).unwrap();
    // Explicit threat-model limit, not a claim of anti-rollback capability.
    fs::write(old.path(), &initial).unwrap();
    fs::write(kernel.archive.presence_path().unwrap(), &first_seal).unwrap();
    fs::write(kernel.archive.lock_path().unwrap(), &first_seal).unwrap();
    assert_eq!(kernel.inspect(&identity(), valid).unwrap(), first);
}

#[test]
fn sealed_family_strict_validation_precedes_callback_and_never_enrolls_or_writes() {
    for invalid in [
        b"[]".as_slice(),
        b"[{\"x\":1,\"x\":2}]",
        b"[{\"x\":{\"z\":1,\"z\":2}}]",
        b"[",
        b"null",
        b"",
    ] {
        let (dir, kernel, old) = fixture();
        kernel.install(&identity(), valid).unwrap();
        let path = dir.archive("device-send-ledger.legacy-v1.json");
        fs::write(&path, invalid).unwrap();
        let active = fs::read(old.path()).unwrap();
        let called = std::sync::atomic::AtomicBool::new(false);
        let result = kernel.inspect_family(&identity(), |_, _| {
            called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, ()>(())
        });
        assert!(result.is_err());
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(fs::read(&path).unwrap(), invalid);
        assert_eq!(fs::read(old.path()).unwrap(), active);
    }
}
#[test]
fn sealed_family_rechecks_all_sibling_bytes_and_inodes_after_callback() {
    for name in [
        "device-send-ledger.legacy-v1.json",
        "device-send-ledger.v2-prepared.json",
        "device-send-ledger.v2-migration.json",
    ] {
        for change in ["same-byte-rename", "different-bytes", "delete", "appear"] {
            let (dir, kernel, _) = fixture();
            kernel.install(&identity(), valid).unwrap();
            let path = dir.archive(name);
            if change != "appear" {
                fs::write(&path, b"[{\"data\":1}]").unwrap();
            }
            let result = kernel.inspect_family(&identity(), |_, _| {
                match change {
                    "same-byte-rename" => {
                        let temp = dir.archive("test-replacement");
                        fs::write(&temp, fs::read(&path).unwrap()).unwrap();
                        fs::rename(temp, &path).unwrap();
                    }
                    "delete" => fs::remove_file(&path).unwrap(),
                    _ => fs::write(&path, b"[{\"data\":2}]").unwrap(),
                }
                Ok::<_, ()>(())
            });
            assert!(
                matches!(result, Err(Error::Guard(GuardError::Changed))),
                "{name} {change}"
            );
        }
    }
}
#[test]
fn sealed_family_callback_cannot_replace_active_marker_or_original_lock_with_same_bytes() {
    for which in ["active", "marker", "lock"] {
        let (dir, kernel, _) = fixture();
        kernel.install(&identity(), valid).unwrap();
        let path = match which {
            "active" => kernel.archive.path.clone(),
            "marker" => kernel.archive.presence_path().unwrap(),
            _ => kernel.archive.lock_path().unwrap(),
        };
        let result = kernel.inspect_family(&identity(), |_, _| {
            let temp = dir.archive("test-replacement");
            fs::write(&temp, fs::read(&path).unwrap()).unwrap();
            fs::rename(temp, &path).unwrap();
            Ok::<_, ()>(())
        });
        assert!(result.is_err(), "{which}");
    }
}


fn migration(kernel: &Kernel) -> Result<Identity, Error<&'static str>> {
    migration_with_candidate(kernel, |_| Ok(identity()))
}
fn migration_with_candidate(
    kernel: &Kernel, candidate: impl FnOnce(&[u8]) -> Result<Identity, &'static str>,
) -> Result<Identity, Error<&'static str>> {
    kernel.open_or_migrate(candidate, |active, _, _| valid(active), |active, family, _| {
        let row: serde_json::Value = serde_json::from_slice(active).map_err(|_| "json")?;
        if row[0]["schema_version"] == 2 { return Ok(MigrationStep::Complete); }
        if family[2].is_none() {
            return Ok(MigrationStep::WriteSibling { index: 2,
                bytes: serde_json::to_vec(&json!([{"legacy":digest(active)}])).unwrap() });
        }
        if family[0].is_none() {
            return Ok(MigrationStep::WriteSibling { index: 0, bytes: active.to_vec() });
        }
        if family[1].is_none() {
            return Ok(MigrationStep::WriteSibling { index: 1, bytes: target(active, 0, "initial") });
        }
        Ok(MigrationStep::ReplaceActive(family[1].clone().unwrap()))
    })
}

#[test]
fn original_lock_migration_recovers_every_durable_protocol_boundary() {
    let points = Point::ALL.into_iter().chain([
        Point::WalCommitted, Point::ActiveCommitted, Point::WalRemoved,
        Point::WalDirectorySynced, Point::JournalCommitted, Point::FrozenCommitted,
        Point::PreparedCommitted,
    ]);
    for point in points {
        let (dir, kernel, old) = fixture();
        let original = fs::read(old.path()).unwrap();
        let fault = kernel.clone().with_hook_for_test(Arc::new(move |at| at == point));
        assert!(migration(&fault).is_err(), "{point:?}");
        let recovered = migration_with_candidate(&kernel, |_| panic!("recovery created another authority")).unwrap();
        assert_eq!(recovered, identity());
        let current = fs::read(old.path()).unwrap();
        assert_eq!(current, target(&original, 0, "initial"));
        assert_eq!(fs::read(dir.archive("device-send-ledger.legacy-v1.json")).unwrap(), original);
        assert!(!old_permission(&old));
        assert_eq!(kernel.inspect(&identity(), valid).unwrap().issued_floor(), 0);
        assert!(!dir.archive("device-send-ledger.v2-transaction.json").exists());
    }
}

#[test]
fn sealed_commit_ack_loss_never_rolls_back_an_issued_high_water() {
    for point in Point::ALL.into_iter().chain([
        Point::WalCommitted, Point::ActiveCommitted, Point::WalRemoved, Point::WalDirectorySynced,
    ]) {
        let (_dir, kernel, old) = fixture();
        let legacy = fs::read(old.path()).unwrap();
        migration(&kernel).unwrap();
        let before = kernel.inspect(&identity(), valid).unwrap();
        let fault = kernel.clone().with_hook_for_test(Arc::new(move |at| at == point));
        let next = target(&legacy, 9, "issued-nine");
        let outcome = fault.transact_family(
            &identity(), &before, |active, _, _| valid(active),
            |_, _| Ok((Some(next.clone()), ())),
        );
        assert!(outcome.is_err(), "{point:?}");
        migration(&kernel).unwrap();
        let sealed = kernel.inspect(&identity(), valid).unwrap();
        assert_eq!(sealed.issued_floor(), 9, "{point:?}");
        assert_eq!(fs::read(old.path()).unwrap(), next);
        assert!(!old_permission(&old));
        let rollback = target(&legacy, 8, "rollback");
        assert!(matches!(kernel.transact_family(
            &identity(), &sealed, |active, _, _| valid(active),
            |_, _| Ok((Some(rollback), ())),
        ), Err(Error::Guard(GuardError::Rollback))));
    }
}

#[test]
fn a_foreign_or_partial_transaction_wal_is_never_treated_as_a_fresh_authority() {
    for bytes in [b"".as_slice(), b"[]", b"[{\"schema_version\":1}]", b"["] {
        let (dir, kernel, old) = fixture();
        migration(&kernel).unwrap();
        let before = fs::read(old.path()).unwrap();
        fs::write(dir.archive("device-send-ledger.v2-transaction.json"), bytes).unwrap();
        assert!(migration(&kernel).is_err());
        assert_eq!(fs::read(old.path()).unwrap(), before);
        assert!(!old_permission(&old));
    }
}
