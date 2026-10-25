use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::{AtomicU64, Ordering};
struct Fixture(PathBuf);
impl Fixture {
    fn new(seed: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let p = PathBuf::from(std::env::var_os("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "tmp/b3a-root-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        let f = Self(p);
        if seed {
            f.seed();
        }
        f
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.0.join(name), bytes).unwrap();
        fs::set_permissions(self.0.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn seed(&self) {
        let source = SourceIdentity {
            schema_version: 1,
            root_id: "a".repeat(32),
            source_id: "b".repeat(32),
            creation_nonce: "c".repeat(32),
            store_relative_path: "multistandby/store-v1.json".into(),
            contract: "nwait-source-identity-v1".into(),
        };
        let s = serde_json::to_vec(&source).unwrap();
        let root = RootIdentity {
            schema_version: 1,
            root_id: source.root_id.clone(),
            creation_nonce: source.creation_nonce.clone(),
            source_identity_sha256: hash(&s),
        };
        let r = serde_json::to_vec(&root).unwrap();
        let w = FreshWitness {
            schema_version: 1,
            contract: "switch-fresh-root-intent-v1".into(),
            phase: "fresh_intent".into(),
            root_id: source.root_id,
            source_id: source.source_id,
            creation_nonce: source.creation_nonce,
            root_identity_sha256: hash(&r),
            source_identity_sha256: hash(&s),
            inspected_archive_paths: ARCHIVES.iter().map(|s| s.to_string()).collect(),
        };
        self.write(SOURCE_FILE, &s);
        self.write(ROOT_FILE, &r);
        self.write(WITNESS_FILE, &serde_json::to_vec(&w).unwrap());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn b3a_preflight_is_read_only_and_never_production_authority() {
    let f = Fixture::new(true);
    let before = [ROOT_FILE, SOURCE_FILE, WITNESS_FILE].map(|p| fs::read(f.0.join(p)).unwrap());
    let report = inspect(&f.0).unwrap();
    assert!(report.fresh_intent_consistent);
    assert!(
        !report.production_authorized
            && !report.owner_drained
            && !report.creation_order_durability_proven
            && !report.hostile_namespace_safety_proven
    );
    assert_eq!(
        before,
        [ROOT_FILE, SOURCE_FILE, WITNESS_FILE].map(|p| fs::read(f.0.join(p)).unwrap())
    );
    assert!(matches!(
        inspect_expected(&f.0, &"d".repeat(32), &"b".repeat(32)),
        Err(PreflightError::InvalidBinding)
    ));
}
#[test]
fn b3a_missing_witness_never_initializes_empty_root() {
    let f = Fixture::new(false);
    assert!(matches!(
        inspect(&f.0),
        Err(PreflightError::MissingEvidence)
    ));
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 0);
}
#[test]
fn b3a_existing_archive_or_unknown_content_never_counts_as_fresh() {
    for path in [
        "sms-forwarding-admission.json",
        "multistandby/store-v1.json",
        "unrecognized-history",
    ] {
        let f = Fixture::new(true);
        let full = f.0.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, b"[]").unwrap();
        let r = inspect(&f.0).unwrap();
        assert!(!r.fresh_intent_consistent && !r.production_authorized);
        assert_eq!(fs::read(full).unwrap(), b"[]");
    }
}
#[test]
fn b3a_hash_binding_duplicates_and_future_marker_refuse() {
    for case in ["hash", "binding", "duplicate", "future"] {
        let f = Fixture::new(true);
        let mut raw = fs::read_to_string(f.0.join(WITNESS_FILE)).unwrap();
        raw = match case {
            "hash" => raw.replace(
                &hash(&fs::read(f.0.join(ROOT_FILE)).unwrap()),
                &"0".repeat(64),
            ),
            "binding" => raw.replace(&"a".repeat(32), &"d".repeat(32)),
            "duplicate" => raw.replacen(
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_version\":1",
                1,
            ),
            _ => raw.replacen("\"schema_version\":1", "\"schema_version\":2", 1),
        };
        f.write(WITNESS_FILE, raw.as_bytes());
        assert!(inspect(&f.0).is_err());
    }
}
#[test]
fn b3a_file_root_symlink_and_broad_permission_refuse() {
    let f = Fixture::new(true);
    fs::set_permissions(f.0.join(ROOT_FILE), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        inspect(&f.0),
        Err(PreflightError::UnsafePermissions)
    ));
    fs::set_permissions(f.0.join(ROOT_FILE), fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(f.0.join(SOURCE_FILE), f.0.join("saved-source")).unwrap();
    symlink(f.0.join("saved-source"), f.0.join(SOURCE_FILE)).unwrap();
    assert!(matches!(inspect(&f.0), Err(PreflightError::UnsafePath)));
}
#[test]
fn b3a_replacement_during_read_is_rejected_even_with_identical_bytes() {
    let f = Fixture::new(true);
    let original = fs::read(f.0.join(ROOT_FILE)).unwrap();
    let result = inspect_with_hook(&f.0, || {
        fs::rename(f.0.join(ROOT_FILE), f.0.join("previous-root")).unwrap();
        f.write(ROOT_FILE, &original);
    });
    assert!(matches!(result, Err(PreflightError::ChangedDuringRead)));
}
#[test]
fn b3a_same_inode_witness_mutation_during_read_is_rejected() {
    let f = Fixture::new(true);
    assert!(matches!(
        inspect_with_hook(&f.0, || {
            f.write(WITNESS_FILE, b"{}");
        }),
        Err(PreflightError::ChangedDuringRead)
    ));
}

#[test]
fn b3a_archive_creation_inside_existing_directory_during_read_refuses_fresh_report() {
    let f = Fixture::new(true);
    fs::create_dir(f.0.join("sms-forwarding")).unwrap();
    assert!(matches!(
        inspect_with_hook(&f.0, || {
            f.write("sms-forwarding/state.enc", b"synthetic-existing-archive");
        }),
        Err(PreflightError::ChangedDuringRead)
    ));
    assert_eq!(
        fs::read(f.0.join("sms-forwarding/state.enc")).unwrap(),
        b"synthetic-existing-archive"
    );
}
