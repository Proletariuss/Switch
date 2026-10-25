use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fs,
    os::unix::{
        ffi::OsStringExt,
        fs::{PermissionsExt, symlink},
    },
    sync::atomic::{AtomicUsize, Ordering},
};

struct Synthetic {
    root: PathBuf,
    clock: Cell<u64>,
    clock_calls: Cell<usize>,
    identity_calls: Cell<usize>,
    env_calls: Cell<usize>,
    overrides: RefCell<BTreeMap<String, Option<OsString>>>,
    drift: Cell<Option<(i32, usize)>>,
    exited: Cell<bool>,
}
impl Proof for Synthetic {
    fn pid(&self) -> i32 {
        43
    }
    fn now(&self) -> Result<u64> {
        self.clock_calls.set(self.clock_calls.get() + 1);
        Ok(self.clock.get())
    }
    fn environment(&self, name: &str) -> Option<OsString> {
        self.env_calls.set(self.env_calls.get() + 1);
        if let Some(value) = self.overrides.borrow().get(name) {
            return value.clone();
        }
        Some(match name {
            TOKEN_ENV => "synthetic-token!1".into(),
            "TMPDIR" => self.root.join("tmp").into_os_string(),
            "BLE_SOCKET" => self.root.join("data/estk-ble.sock").into_os_string(),
            "HOME" | "CFFIXED_USER_HOME" | "SWITCH_DATA_DIR" => {
                self.root.join("data").into_os_string()
            }
            _ => panic!("unexpected environment key"),
        })
    }
    fn identity(&self, pid: i32) -> Result<ProcessSnapshot> {
        self.identity_calls.set(self.identity_calls.get() + 1);
        if self.exited.get() {
            return invalid();
        }
        let (parent, image) = match pid {
            43 => (42, "backend"),
            42 => (41, "app"),
            41 => (40, "guardian"),
            _ => return invalid(),
        };
        let mut p = ProcessSnapshot {
            pid,
            parent,
            start: (100 + pid as u64, 123),
            executable: self.root.join(image),
            exec_identity: [1; 8],
        };
        if let Some((target, field)) = self.drift.get() {
            if target == pid {
                match field {
                    0 => p.pid += 1,
                    1 => p.parent += 1,
                    2 => p.start.0 += 1,
                    3 => p.start.1 += 1,
                    4 => p.executable = self.root.join("other"),
                    _ => p.exec_identity[0] += 1,
                }
            }
        }
        Ok(p)
    }
}
struct Fixture {
    root: PathBuf,
    parent: Value,
    child: Value,
    proof: Synthetic,
}
fn write(path: &std::path::Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root =
            PathBuf::from(std::env::var_os("G8_FIXTURE_ROOT").expect("isolated fixture root"))
                .join(format!(
                    "b1-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["data", "evidence", "tmp"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut p = serde_json::json!({"schema_version":1,"role":"app","purpose":PURPOSE,
            "run_id":"a".repeat(32),"nonce":"b".repeat(64),"guardian_pid":41,"uid":getuid().as_raw(),
            "created_unix_ms":1000,"expires_unix_ms":100000});
        for (key, name) in [
            ("capsule_root", ""),
            ("data_root", "data"),
            ("evidence_root", "evidence"),
            ("tmp_root", "tmp"),
        ] {
            let path = if name.is_empty() {
                root.clone()
            } else {
                root.join(name)
            };
            let meta = fs::metadata(&path).unwrap();
            p[key] = path.to_str().unwrap().into();
            p[format!("{key}_dev")] = meta.dev().into();
            p[format!("{key}_ino")] = meta.ino().into();
        }
        for name in ["app", "backend", "guardian"] {
            write(&root.join(name), name.as_bytes());
            p[format!("{name}_path")] = root.join(name).to_str().unwrap().into();
            p[format!("{name}_sha256")] = format!("{:x}", Sha256::digest(name.as_bytes())).into();
        }
        let mut f = Self {
            root: root.clone(),
            parent: p.clone(),
            child: p,
            proof: Synthetic {
                root,
                clock: Cell::new(3000),
                clock_calls: Cell::new(0),
                identity_calls: Cell::new(0),
                env_calls: Cell::new(0),
                overrides: RefCell::new(BTreeMap::new()),
                drift: Cell::new(None),
                exited: Cell::new(false),
            },
        };
        write(
            &f.root.join("app.json"),
            &serde_json::to_vec(&f.parent).unwrap(),
        );
        f.child["role"] = "daemon".into();
        f.child["app_pid"] = 42.into();
        f.child["created_unix_ms"] = 2000.into();
        f.child["expires_unix_ms"] = 30000.into();
        f.child["child_nonce"] = "c".repeat(64).into();
        f.child["parent_manifest_path"] = f.root.join("app.json").to_str().unwrap().into();
        f.child["parent_manifest_inode"] =
            fs::metadata(f.root.join("app.json")).unwrap().ino().into();
        f.child["parent_manifest_sha256"] = format!(
            "{:x}",
            Sha256::digest(fs::read(f.root.join("app.json")).unwrap())
        )
        .into();
        f.write_child();
        f
    }
    fn path(&self) -> PathBuf {
        self.root
            .join(format!("data/daemon-{}.json", "c".repeat(64)))
    }
    fn nonce(&self) -> PathBuf {
        self.root
            .join(format!("data/.consumed-child-{}", "c".repeat(64)))
    }
    fn args(&self) -> Vec<OsString> {
        vec![
            "-listen".into(),
            format!("unix:{}/data/switch.sock", self.root.display()).into(),
            "-backend=at".into(),
            format!("--switch-acceptance-capsule={}", self.path().display()).into(),
        ]
    }
    fn write_child(&self) {
        write(&self.path(), &serde_json::to_vec(&self.child).unwrap());
    }
    fn admit(&self) -> Result<AcceptanceAdmission> {
        match classify_and_admit(&self.args(), &self.proof, |_| Ok(()))? {
            Entry::Acceptance(a) => Ok(a),
            Entry::Production => panic!("candidate fell back"),
        }
    }
}
struct ForbiddenProof;
impl Proof for ForbiddenProof {
    fn pid(&self) -> i32 {
        panic!("pid accessed")
    }
    fn identity(&self, _: i32) -> Result<ProcessSnapshot> {
        panic!("identity accessed")
    }
    fn environment(&self, _: &str) -> Option<OsString> {
        panic!("environment accessed")
    }
    fn now(&self) -> Result<u64> {
        panic!("clock accessed")
    }
}
fn exact() -> Vec<OsString> {
    [
        "-listen",
        "unix:/private/run/data/switch.sock",
        "-backend=at",
        "--switch-acceptance-capsule=/private/run/data/daemon.json",
    ]
    .map(OsString::from)
    .into()
}
fn os(bytes: &[u8]) -> OsString {
    OsString::from_vec(bytes.to_vec())
}

#[test]
fn e1_no_prefix_and_env_only_do_not_touch_any_proof_or_loader() {
    for args in [
        vec![],
        vec![os(b"--switch-acceptanc")],
        vec![os(b"-switch-acceptanc")],
        vec![os(b"--"), os(b"position")],
        vec![os(b"-listen"), os(b"unix:/a"), os(b"-backend=at")],
        vec![os(b"PROLETARIUS_APP_TOKEN=synthetic-token!1")],
        vec![os(b"\xff")],
    ] {
        assert!(matches!(
            classify_and_admit(&args, &ForbiddenProof, |_| panic!("loader touched")),
            Ok(Entry::Production)
        ));
    }
}
#[test]
fn e1_both_reserved_prefixes_and_exact_raw_shape_matrix_reject_before_load() {
    let mut cases = Vec::new();
    for prefix in [b"--switch-acceptance".as_slice(), b"-switch-acceptance"] {
        for suffix in [
            b"".as_slice(),
            b"-capsul",
            b"-capsule",
            b"-capsule=",
            b"-capsule=/a",
            b"-extended",
            b"\xff",
        ] {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(suffix);
            cases.push(vec![os(&bytes)]);
            cases.push(vec![os(b"--"), os(&bytes)]);
            cases.push(vec![os(b"positional"), os(&bytes)]);
        }
    }
    for index in 0..4 {
        let mut invalid_utf8 = exact();
        let mut bytes = invalid_utf8[index].as_bytes().to_vec();
        bytes.push(0xff);
        invalid_utf8[index] = OsString::from_vec(bytes);
        cases.push(invalid_utf8);
        let mut a = exact();
        a.swap(index, 3);
        if index != 3 {
            cases.push(a);
        }
    }
    for extra in [
        "--",
        "positional",
        "-port=1",
        "--switch-acceptance-capsule=/a",
    ] {
        let mut a = exact();
        a.push(extra.into());
        cases.push(a);
        let mut a = exact();
        a.insert(0, extra.into());
        cases.push(a);
    }
    for replacement in [
        os(b"--switch-acceptance-capsule=/bad\xff"),
        os(b"-switch-acceptance-capsule=/a"),
        os(b"--switch-acceptance-capsule=relative"),
        os(b"--switch-acceptance-capsule=/a/../b"),
        os(b"--switch-acceptance-capsule=/a\0b"),
        os(b"--switch-acceptance-capsule=/a/"),
    ] {
        let mut a = exact();
        a[3] = replacement;
        cases.push(a);
    }
    for replacement in [
        "unix:/data/other.sock",
        "unix:/switch.sock",
        "unix:relative/switch.sock",
        "unix:/a//switch.sock",
        "unix:/a/../b/switch.sock",
        "unix:/a/switch.sock/",
        "unix:/a/switch.sock?x",
    ] {
        let mut a = exact();
        a[1] = replacement.into();
        cases.push(a);
    }
    for (index, value) in [
        (0, "--listen"),
        (0, "-listen=unix:/a/switch.sock"),
        (2, "-backend=qmi"),
        (2, "--backend=at"),
    ] {
        let mut a = exact();
        a[index] = value.into();
        cases.push(a);
    }
    for a in cases {
        assert!(
            classify_and_admit(&a, &ForbiddenProof, |_| panic!("loader touched")).is_err(),
            "{:?}",
            a
        );
    }
    assert!(matches!(
        classify(&exact()),
        Ok(Classification::Candidate(_))
    ));
}
#[test]
fn e2_single_load_single_claim_retained_authority_and_replay() {
    let f = Fixture::new();
    let mut stages = Vec::new();
    let mut admission = match classify_and_admit(&f.args(), &f.proof, |s| {
        stages.push(s);
        Ok(())
    })
    .unwrap()
    {
        Entry::Acceptance(a) => a,
        _ => panic!("production fallback"),
    };
    assert_eq!(
        stages,
        [
            Stage::Load,
            Stage::BeforeClaim,
            Stage::ClaimCreated,
            Stage::ClaimWritten,
            Stage::ClaimSynced,
            Stage::RootSynced,
            Stage::Admitted
        ]
    );
    assert!(!admission.binding.held.is_empty());
    assert_eq!(fs::read(f.nonce()).unwrap(), b"consumed\n");
    assert_eq!(admission.binding.capsule, f.path().to_str().unwrap());
    assert_eq!(
        admission.binding.socket,
        format!("{}/data/switch.sock", f.root.display())
    );
    let clock_reads = f.proof.clock_calls.get();
    f.proof.clock.set(1_000_000);
    let view = admission.runtime_proof(&f.proof).unwrap();
    assert_eq!(view.run_id(), "a".repeat(32));
    assert_eq!(view.daemon_pid(), 43);
    assert_eq!(view.app_pid(), 42);
    assert_eq!(
        f.proof.clock_calls.get(),
        clock_reads,
        "runtime consulted admission TTL"
    );
    f.proof.clock.set(3000);
    let mut replay = Vec::new();
    assert!(
        classify_and_admit(&f.args(), &f.proof, |s| {
            replay.push(s);
            Ok(())
        })
        .is_err()
    );
    assert_eq!(replay, [Stage::Load, Stage::BeforeClaim]);
}
#[test]
fn e2_post_claim_every_stage_failure_never_retries_or_falls_back() {
    for failed in [
        Stage::ClaimCreated,
        Stage::ClaimWritten,
        Stage::ClaimSynced,
        Stage::RootSynced,
        Stage::Admitted,
    ] {
        let f = Fixture::new();
        let mut stages = Vec::new();
        assert!(
            classify_and_admit(&f.args(), &f.proof, |s| {
                stages.push(s);
                if s == failed { invalid() } else { Ok(()) }
            })
            .is_err()
        );
        assert_eq!(stages.iter().filter(|s| **s == Stage::Load).count(), 1);
        assert_eq!(
            stages.iter().filter(|s| **s == Stage::BeforeClaim).count(),
            1
        );
        assert!(f.nonce().exists());
        assert!(f.admit().is_err());
    }
}
#[test]
fn e2_post_claim_drift_or_expiry_consumes_nonce_and_rejects_permanently() {
    for change in 0..3 {
        let f = Fixture::new();
        let mut stages = Vec::new();
        assert!(
            classify_and_admit(&f.args(), &f.proof, |s| {
                stages.push(s);
                if s == Stage::RootSynced {
                    match change {
                        0 => f.proof.clock.set(30000),
                        1 => f.proof.drift.set(Some((42, 2))),
                        _ => {
                            write(&f.root.join("app"), b"changed");
                        }
                    }
                }
                Ok(())
            })
            .is_err()
        );
        assert!(f.nonce().exists());
        assert_eq!(
            stages.iter().filter(|s| **s == Stage::BeforeClaim).count(),
            1
        );
        assert!(!stages.contains(&Stage::Admitted));
    }
}
#[test]
fn e2_admission_ttl_invalid_root_shape_and_socket_bytes_reject_before_claim() {
    for now in [0, 1999, 30000, 30001, 1_000_000] {
        let f = Fixture::new();
        f.proof.clock.set(now);
        assert!(f.admit().is_err());
        assert!(!f.nonce().exists());
    }
    for bad in [
        "unix:/private/other/data/switch.sock",
        "unix:/private/other/switch.sock",
    ] {
        let f = Fixture::new();
        let mut a = f.args();
        a[1] = bad.into();
        let mut stages = Vec::new();
        assert!(
            classify_and_admit(&a, &f.proof, |s| {
                stages.push(s);
                Ok(())
            })
            .is_err()
        );
        assert_eq!(stages, [Stage::Load]);
        assert!(!f.nonce().exists());
    }
    for key in ["child_nonce", "data_root", "parent_manifest_path"] {
        let mut f = Fixture::new();
        f.child[key] = "/wrong".into();
        f.write_child();
        assert!(f.admit().is_err());
        assert!(!f.nonce().exists());
    }
}
#[test]
fn e2_environment_native_bytes_exact_missing_suffix_and_no_ble_open() {
    for key in [
        "HOME",
        "CFFIXED_USER_HOME",
        "SWITCH_DATA_DIR",
        "TMPDIR",
        "BLE_SOCKET",
    ] {
        for bad in [
            None,
            Some(os(b"/wrong")),
            Some(os(b"/wrong\xff")),
            Some(os(b"/wrong/")),
        ] {
            let f = Fixture::new();
            f.proof.overrides.borrow_mut().insert(key.into(), bad);
            assert!(f.admit().is_err(), "{key}");
            assert!(!f.nonce().exists());
        }
    }
    let f = Fixture::new();
    symlink("/deliberately/missing", f.root.join("data/estk-ble.sock")).unwrap();
    assert!(
        f.admit().is_ok(),
        "BLE path must only be compared, never opened"
    );
}
#[test]
fn e2_token_printable_byte_boundaries_and_runtime_token_drift() {
    for bytes in [
        vec![],
        vec![b'a'; 15],
        vec![b'a'; 4097],
        vec![b' '; 16],
        vec![0x7f; 16],
        vec![0xff; 16],
        vec![0; 16],
        vec![b'\n'; 16],
    ] {
        let f = Fixture::new();
        f.proof
            .overrides
            .borrow_mut()
            .insert(TOKEN_ENV.into(), Some(OsString::from_vec(bytes)));
        assert!(f.admit().is_err());
        assert!(!f.nonce().exists());
    }
    for byte in [b'!', b'~'] {
        for len in [16, 4096] {
            let f = Fixture::new();
            f.proof
                .overrides
                .borrow_mut()
                .insert(TOKEN_ENV.into(), Some(OsString::from_vec(vec![byte; len])));
            let mut a = f.admit().unwrap();
            assert!(a.runtime_proof(&f.proof).is_ok());
            f.proof
                .overrides
                .borrow_mut()
                .insert(TOKEN_ENV.into(), Some("different-token!1".into()));
            assert!(a.runtime_proof(&f.proof).is_err());
            assert!(a.terminal);
        }
    }
    let f = Fixture::new();
    f.proof
        .overrides
        .borrow_mut()
        .insert(TOKEN_ENV.into(), None);
    assert!(f.admit().is_err());
}
#[test]
fn e2_directory_legal_nonce_socket_mtime_changes_keep_held_binding() {
    let f = Fixture::new();
    let mut a = f.admit().unwrap();
    let held = a.binding.held.len();
    write(
        &f.root.join("data/legal-entry"),
        b"legal namespace mutation",
    );
    write(&f.root.join("tmp/legal-entry"), b"legal namespace mutation");
    assert!(a.runtime_proof(&f.proof).is_ok());
    assert_eq!(a.binding.held.len(), held);
    fs::set_permissions(f.root.join("data"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(a.runtime_proof(&f.proof).is_err());
    fs::set_permissions(f.root.join("data"), fs::Permissions::from_mode(0o700)).unwrap();
    let calls = f.proof.identity_calls.get();
    assert!(a.runtime_proof(&f.proof).is_err());
    assert_eq!(
        calls,
        f.proof.identity_calls.get(),
        "terminal drift reauthorized"
    );
}
#[test]
fn e2_regular_held_named_images_manifests_nonce_and_parent_drift_terminal() {
    for target in [
        "app", "backend", "guardian", "app.json", "child", "nonce", "data", "evidence", "tmp",
    ] {
        let f = Fixture::new();
        let mut a = f.admit().unwrap();
        let path = match target {
            "child" => f.path(),
            "nonce" => f.nonce(),
            _ => f.root.join(target),
        };
        let saved = f.root.join("preserved-old");
        fs::rename(&path, &saved).unwrap();
        if ["data", "evidence", "tmp"].contains(&target) {
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        } else {
            write(&path, &fs::read(&saved).unwrap());
        }
        assert!(a.runtime_proof(&f.proof).is_err(), "{target}");
        assert!(a.terminal);
    }
    let f = Fixture::new();
    let mut a = f.admit().unwrap();
    write(&f.root.join("backend"), b"same-inode-different-bytes");
    assert!(a.runtime_proof(&f.proof).is_err());
    let f = Fixture::new();
    let mut a = f.admit().unwrap();
    fs::rename(&f.root, f.root.with_extension("retained-old-root")).unwrap();
    fs::create_dir(&f.root).unwrap();
    fs::set_permissions(&f.root, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(a.runtime_proof(&f.proof).is_err());
    assert!(a.binding.held.iter().all(|h| h.file.metadata().is_ok()));
}
#[test]
fn e2_process_pid_reuse_parent_path_start_exec_and_exit_are_sticky_terminal() {
    for pid in [43, 42, 41] {
        for field in 0..6 {
            let f = Fixture::new();
            let mut a = f.admit().unwrap();
            f.proof.drift.set(Some((pid, field)));
            assert!(
                a.runtime_proof(&f.proof).is_err(),
                "pid {pid}, field {field}"
            );
            f.proof.drift.set(None);
            assert!(a.runtime_proof(&f.proof).is_err());
        }
    }
    let f = Fixture::new();
    let mut a = f.admit().unwrap();
    f.proof.exited.set(true);
    assert!(a.runtime_proof(&f.proof).is_err());
}
