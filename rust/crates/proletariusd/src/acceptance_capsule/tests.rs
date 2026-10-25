use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    root: PathBuf,
    parent: Value,
    child: Value,
    proof: Synthetic,
}
struct Synthetic {
    root: PathBuf,
    app_pid: i32,
    guardian_pid: i32,
    wrong: Option<String>,
}
impl ProcessProof for Synthetic {
    fn parent(&self) -> i32 {
        self.app_pid
    }
    fn executable(&self) -> Result<PathBuf> {
        Ok(self.root.join(if self.wrong.as_deref() == Some("EXE") {
            "other"
        } else {
            "backend"
        }))
    }
    fn identity(&self, pid: i32) -> Result<(i32, PathBuf)> {
        if pid == 42 {
            Ok((
                self.guardian_pid,
                self.root
                    .join(if self.wrong.as_deref() == Some("APP_IMAGE") {
                        "other"
                    } else {
                        "app"
                    }),
            ))
        } else if pid == 41 {
            Ok((
                40,
                self.root
                    .join(if self.wrong.as_deref() == Some("GUARDIAN_IMAGE") {
                        "other"
                    } else {
                        "guardian"
                    }),
            ))
        } else {
            invalid()
        }
    }
    fn environment(&self, name: &str) -> Option<String> {
        if self.wrong.as_deref() == Some(name) {
            return Some("/wrong".into());
        }
        Some(
            self.root
                .join(if name == "TMPDIR" { "tmp" } else { "data" })
                .to_str()
                .unwrap()
                .into(),
        )
    }
}
impl Fixture {
    fn new() -> Self {
        let base =
            PathBuf::from(std::env::var_os("G8_FIXTURE_ROOT").expect("isolated fixture root"));
        let ancestor = base.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&ancestor).unwrap();
        let root = ancestor.join("capsule");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["data", "evidence", "tmp"] {
            fs::create_dir(root.join(name)).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut p = serde_json::json!({"schema_version":1,"role":"app","purpose":PURPOSE,"run_id":"a".repeat(32),"nonce":"b".repeat(64),
            "guardian_pid":41,"uid":getuid().as_raw(),"created_unix_ms":1000,"expires_unix_ms":100000});
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
            p[key] = Value::from(path.to_str().unwrap());
            p[format!("{key}_dev")] = Value::from(meta.dev());
            p[format!("{key}_ino")] = Value::from(meta.ino());
        }
        for name in ["app", "backend", "guardian"] {
            fs::write(root.join(name), name).unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
            p[format!("{name}_path")] = Value::from(root.join(name).to_str().unwrap());
            p[format!("{name}_sha256")] =
                Value::from(format!("{:x}", Sha256::digest(name.as_bytes())));
        }
        let mut f = Self {
            root: root.clone(),
            parent: p.clone(),
            child: p,
            proof: Synthetic {
                root,
                app_pid: 42,
                guardian_pid: 41,
                wrong: None,
            },
        };
        f.write_parent();
        f.child["role"] = "daemon".into();
        f.child["app_pid"] = 42.into();
        f.child["created_unix_ms"] = 2000.into();
        f.child["expires_unix_ms"] = 30000.into();
        f.child["child_nonce"] = "c".repeat(64).into();
        f.child["parent_manifest_path"] = f.parent_path().to_str().unwrap().into();
        f.bind_parent();
        f.write_child();
        f
    }
    fn parent_path(&self) -> PathBuf {
        self.root.join("app.json")
    }
    fn path(&self) -> PathBuf {
        self.root
            .join(format!("data/daemon-{}.json", "c".repeat(64)))
    }
    fn write_parent(&self) {
        write(
            &self.parent_path(),
            &serde_json::to_vec(&self.parent).unwrap(),
        );
    }
    fn bind_parent(&mut self) {
        let p = self.parent_path();
        self.child["parent_manifest_inode"] = fs::metadata(&p).unwrap().ino().into();
        self.child["parent_manifest_sha256"] =
            format!("{:x}", Sha256::digest(fs::read(p).unwrap())).into();
    }
    fn write_child(&self) {
        write(&self.path(), &serde_json::to_vec(&self.child).unwrap());
    }
    fn validate(&self) -> Result<AcceptanceContext> {
        validate(self.path().to_str().unwrap(), &self.proof, 3000, || Ok(()))
    }
}
fn write(path: &std::path::Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
#[test]
fn valid_direct_parent_context_and_atomic_child_replay() {
    let f = Fixture::new();
    let c = f.validate().unwrap();
    assert_eq!(c.data_root, f.root.join("data").to_str().unwrap());
    assert!(!c.held.is_empty());
    assert!(f.validate().is_err());
    assert!(fs::read_dir(f.root.join("data")).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".consumed-child-")
    }));
}
#[test]
fn exact_schema_canonical_size_and_integer_failures() {
    for (key, value) in [
        ("unknown", Value::Null),
        ("schema_version", true.into()),
        ("uid", Value::from(1.0)),
        ("role", "app".into()),
        ("purpose", "wrong".into()),
        ("run_id", "A".repeat(32).into()),
        ("nonce", "a".into()),
        ("child_nonce", "wrong".into()),
    ] {
        let mut f = Fixture::new();
        f.child[key] = value;
        f.write_child();
        assert!(f.validate().is_err(), "{key}");
    }
    for bytes in [
        vec![b' '; 16385],
        b"{}".to_vec(),
        b"{\"role\":\"daemon\",\"role\":\"app\"}".to_vec(),
    ] {
        let f = Fixture::new();
        write(&f.path(), &bytes);
        assert!(f.validate().is_err());
    }
    let f = Fixture::new();
    let mut bytes = fs::read(f.path()).unwrap();
    bytes.push(b'\n');
    write(&f.path(), &bytes);
    assert!(f.validate().is_err());
    let mut f = Fixture::new();
    f.child.as_object_mut().unwrap().remove("child_nonce");
    f.write_child();
    assert!(f.validate().is_err());
}
#[test]
fn file_directory_owner_mode_links_symlinks_and_layout() {
    for mode in [0o644, 0o666, 0o1600] {
        let f = Fixture::new();
        fs::set_permissions(f.path(), fs::Permissions::from_mode(mode)).unwrap();
        assert!(f.validate().is_err());
    }
    let f = Fixture::new();
    fs::hard_link(f.path(), f.root.join("hard")).unwrap();
    assert!(f.validate().is_err());
    let f = Fixture::new();
    fs::rename(f.path(), f.root.join("actual")).unwrap();
    symlink(f.root.join("actual"), f.path()).unwrap();
    assert!(f.validate().is_err());
    let f = Fixture::new();
    fs::set_permissions(f.root.join("data"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(f.validate().is_err());
    for key in ["uid", "data_root_ino", "capsule_root_dev"] {
        let mut f = Fixture::new();
        f.child[key] = 999999u64.into();
        f.write_child();
        assert!(f.validate().is_err());
    }
    for path in [
        "relative", "/a//b", "/a/../b", "/a/./b", "/a/", "/", "/a\0b",
    ] {
        assert!(canonical(path).is_err());
    }
    let f = Fixture::new();
    fs::rename(f.root.join("data"), f.root.join("data-old")).unwrap();
    symlink(f.root.join("data-old"), f.root.join("data")).unwrap();
    assert!(f.validate().is_err());
}
#[test]
fn parent_hash_inode_schema_and_inherited_field_mismatch() {
    for key in [
        "parent_manifest_sha256",
        "parent_manifest_inode",
        "parent_manifest_path",
    ] {
        let mut f = Fixture::new();
        f.child[key] = if key.ends_with("inode") {
            1u64.into()
        } else {
            "/wrong".into()
        };
        f.write_child();
        assert!(f.validate().is_err());
    }
    for key in ["role", "run_id", "schema_version", "unknown"] {
        let mut f = Fixture::new();
        f.parent[key] = if key == "schema_version" {
            2u64.into()
        } else {
            "mismatch".into()
        };
        f.write_parent();
        f.bind_parent();
        f.write_child();
        assert!(f.validate().is_err());
    }
    let f = Fixture::new();
    let b = fs::read(f.parent_path()).unwrap();
    fs::remove_file(f.parent_path()).unwrap();
    write(&f.parent_path(), &b);
    assert!(f.validate().is_err());
}
#[test]
fn authority_hash_expiry_and_environment() {
    for key in [
        "app_pid",
        "guardian_pid",
        "created_unix_ms",
        "expires_unix_ms",
    ] {
        let mut f = Fixture::new();
        f.child[key] = 0u64.into();
        f.write_child();
        assert!(f.validate().is_err());
    }
    for name in ["app", "backend", "guardian"] {
        let f = Fixture::new();
        fs::write(f.root.join(name), "changed").unwrap();
        assert!(f.validate().is_err());
    }
    for key in [
        "HOME",
        "CFFIXED_USER_HOME",
        "TMPDIR",
        "EXE",
        "APP_IMAGE",
        "GUARDIAN_IMAGE",
    ] {
        let mut f = Fixture::new();
        f.proof.wrong = Some(key.into());
        assert!(f.validate().is_err());
    }
    let mut f = Fixture::new();
    f.proof.app_pid = 43;
    assert!(f.validate().is_err());
    let mut f = Fixture::new();
    f.proof.guardian_pid = 40;
    assert!(f.validate().is_err());
    let mut f = Fixture::new();
    f.child["expires_unix_ms"] = 3000.into();
    f.write_child();
    assert!(f.validate().is_err());
    let mut f = Fixture::new();
    f.child["expires_unix_ms"] = 40000.into();
    f.write_child();
    assert!(f.validate().is_err());
}
#[test]
fn full_absolute_rewalk_rejects_post_hash_file_root_and_ancestor_replacement() {
    for target in ["app", "app.json", "data", ""] {
        let f = Fixture::new();
        let path = f.path();
        let replace = if target.is_empty() {
            f.root.clone()
        } else {
            f.root.join(target)
        };
        let result = validate(path.to_str().unwrap(), &f.proof, 3000, || {
            let backup = replace.with_extension("moved");
            fs::rename(&replace, &backup).unwrap();
            if backup.is_dir() {
                fs::create_dir(&replace).unwrap();
                fs::set_permissions(&replace, fs::Permissions::from_mode(0o700)).unwrap();
            } else {
                write(&replace, &fs::read(backup).unwrap());
            }
            Ok(())
        });
        assert!(result.is_err(), "{target}");
    }
    // Specifically replace an ancestor above the declared capsule root.
    let f = Fixture::new();
    let parent = f.root.parent().unwrap().to_path_buf();
    let moved = parent.with_extension(format!("old-{}", NEXT.fetch_add(1, Ordering::SeqCst)));
    let result = validate(f.path().to_str().unwrap(), &f.proof, 3000, || {
        fs::rename(&parent, &moved).unwrap();
        fs::create_dir(&parent).unwrap();
        Ok(())
    });
    fs::remove_dir(&parent).unwrap();
    fs::rename(&moved, &parent).unwrap();
    assert!(result.is_err());
}
#[test]
fn strict_acceptance_arguments_and_unchanged_production_flags() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    for args in [
        vec!["--switch-acceptance-capsule"],
        vec!["--switch-acceptance-capsule=relative"],
        vec!["--switch-acceptance-unknown"],
        vec!["-switch-acceptance-capsule=/a"],
        vec![
            "--switch-acceptance-capsule=/a",
            "--switch-acceptance-capsule=/b",
        ],
        vec!["positional", "--switch-acceptance-capsule="],
    ] {
        assert!(crate::daemon_args::parse(args.into_iter().map(OsString::from)).is_err());
    }
    assert!(
        crate::daemon_args::parse([OsString::from_vec(
            b"--switch-acceptance-capsule=/bad\xff".to_vec()
        )])
        .is_err()
    );
    assert_eq!(
        crate::daemon_args::parse([OsString::from("--switch-acceptance-capsule=/a")])
            .unwrap()
            .acceptance_capsule
            .as_deref(),
        Some("/a")
    );
    assert!(pre_entry(&[OsString::from("--internal-real-helper")]).is_ok());
}

#[test]
fn actual_metadata_owner_predicate_and_readonly_private_file() {
    let f = Fixture::new();
    let mut held = walk(f.path().to_str().unwrap(), false).unwrap();
    let file = held.pop().unwrap();
    let mut id = file.identity;
    id.uid = getuid().as_raw().wrapping_add(1);
    assert!(id.regular(true, getuid().as_raw()).is_err());
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o400)).unwrap();
    assert!(f.validate().is_ok());
}

#[test]
fn renamed_parent_same_hash_inode_is_rejected_before_nonce_claim() {
    let mut f = Fixture::new();
    let old_identity = fs::metadata(f.parent_path()).unwrap();
    let old_bytes = fs::read(f.parent_path()).unwrap();
    let alternate = f.root.join("alternate.json");
    fs::rename(f.parent_path(), &alternate).unwrap();
    assert_eq!(fs::metadata(&alternate).unwrap().ino(), old_identity.ino());
    assert_eq!(fs::read(&alternate).unwrap(), old_bytes);
    f.child["parent_manifest_path"] = alternate.to_str().unwrap().into();
    f.write_child();
    assert!(f.validate().is_err());
    assert!(
        !f.root
            .join(format!("data/.consumed-child-{}", "c".repeat(64)))
            .exists()
    );
}
