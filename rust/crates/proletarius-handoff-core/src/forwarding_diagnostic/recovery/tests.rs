use super::*;
use proletarius_forwarding::{JsonDocument, seal, seal_empty_queue};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};
struct Fixture {
    root: PathBuf,
    directory: File,
}
impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/recovery-rust")
            .join(crate::uuid().unwrap());
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = File::open(&root).unwrap();
        Self { root, directory }
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.root.join(name))
            .unwrap();
        f.write_all(bytes).unwrap();
        f.sync_all().unwrap();
        self.directory.sync_all().unwrap();
    }
    fn seed(&self) {
        self.write(
            "state.enc",
            &seal_empty_queue(&StateKey::from_slice(&[7; 32]).unwrap()).unwrap(),
        );
    }
    fn begin(&self) -> Result<Recovery, i32> {
        Recovery::begin(self.directory.as_raw_fd(), &[7; 32])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn explicit_missing_witness_completion_preserves_original_seed_and_confirms_same_inode() {
    let f = Fixture::new();
    f.seed();
    let before = fs::read(f.root.join("state.enc")).unwrap();
    let inode = fs::metadata(f.root.join("state.enc")).unwrap().ino();
    let mut proof = f.begin().unwrap();
    assert_eq!(proof.confirm(), Err(REFUSED));
    proof.prepare().unwrap();
    proof.confirm().unwrap();
    assert_eq!(fs::read(f.root.join("state.enc")).unwrap(), before);
    assert_eq!(fs::metadata(f.root.join("state.enc")).unwrap().ino(), inode);
    assert_eq!(
        fs::read(f.root.join("state.enc.keyid")).unwrap(),
        proof.expected
    );
    let witness_inode = fs::metadata(f.root.join("state.enc.keyid")).unwrap().ino();
    drop(proof);
    let mut reopened = f.begin().unwrap();
    reopened.prepare().unwrap();
    reopened.confirm().unwrap();
    assert_eq!(
        fs::metadata(f.root.join("state.enc.keyid")).unwrap().ino(),
        witness_inode
    );
}
#[test]
fn wrong_key_nonempty_noncanonical_torn_missing_or_bad_witness_never_gain_recovery() {
    for kind in 0..7 {
        let f = Fixture::new();
        match kind {
            0 => (),
            1 => f.write("state.enc", b"SWF1"),
            2 | 3 => {
                let text = if kind == 2 {
                    br#"{"seen":{},"pending":{"x":1}}"#.as_slice()
                } else {
                    br#"{ "seen":{},"pending":{}}"#.as_slice()
                };
                let doc = JsonDocument::from_bytes(
                    zeroize::Zeroizing::new(text.to_vec()),
                    StateLimits::default(),
                )
                .unwrap();
                f.write(
                    "state.enc",
                    &seal(
                        &StateKey::from_slice(&[7; 32]).unwrap(),
                        &doc,
                        StateLimits::default(),
                    )
                    .unwrap(),
                );
            }
            _ => {
                f.seed();
                if kind == 4 {
                    f.write("state.enc.keyid", &[1; 40]);
                }
                if kind == 5 {
                    f.write("state.enc.keyid", b"SWFKID1\0");
                }
            }
        }
        let before = fs::read(f.root.join("state.enc")).ok();
        let result = if kind == 6 {
            Recovery::begin(f.directory.as_raw_fd(), &[8; 32])
        } else {
            f.begin()
        };
        assert!(result.is_err(), "kind {kind}");
        assert_eq!(fs::read(f.root.join("state.enc")).ok(), before);
    }
}
#[test]
fn same_bytes_new_inode_and_restored_names_poison_existing_recovery_handle() {
    let f = Fixture::new();
    f.seed();
    let mut proof = f.begin().unwrap();
    proof.prepare().unwrap();
    let original = f.root.join("state.enc");
    let saved = f.root.join("saved");
    fs::rename(&original, &saved).unwrap();
    f.write("state.enc", &fs::read(&saved).unwrap());
    assert_eq!(proof.confirm(), Err(UNSAFE));
    fs::remove_file(&original).unwrap();
    fs::rename(&saved, &original).unwrap();
    assert_eq!(proof.confirm(), Err(REFUSED));
}
#[test]
fn witness_appearing_after_begin_is_never_replaced_or_adopted_by_that_handle() {
    let f = Fixture::new();
    f.seed();
    let mut proof = f.begin().unwrap();
    f.write("state.enc.keyid", &proof.expected);
    let before = fs::read(f.root.join("state.enc.keyid")).unwrap();
    assert_eq!(proof.prepare(), Err(UNSAFE));
    assert_eq!(proof.confirm(), Err(REFUSED));
    assert_eq!(fs::read(f.root.join("state.enc.keyid")).unwrap(), before);
}

#[test]
fn kernel_o_excl_race_keeps_existing_witness_and_poisoned_handle() {
    let f = Fixture::new();
    f.seed();
    let mut proof = f.begin().unwrap();
    let path = f.root.join("state.enc.keyid");
    let expected = proof.expected;
    proof.before_create = Some(Box::new(move || {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(&expected).unwrap();
        file.sync_all().unwrap();
    }));
    assert_eq!(proof.prepare(), Err(EXISTS));
    let inode = fs::metadata(f.root.join("state.enc.keyid")).unwrap().ino();
    assert_eq!(proof.prepare(), Err(REFUSED));
    assert_eq!(proof.confirm(), Err(REFUSED));
    assert_eq!(
        fs::metadata(f.root.join("state.enc.keyid")).unwrap().ino(),
        inode
    );
    assert_eq!(fs::read(f.root.join("state.enc.keyid")).unwrap(), expected);
}
#[test]
fn ffi_panic_poisons_the_same_live_recovery_handle() {
    let f = Fixture::new();
    f.seed();
    let mut proof = f.begin().unwrap();
    proof.before_create = Some(Box::new(|| {
        panic!("synthetic owner panic before witness create")
    }));
    let handle = Box::into_raw(Box::new(proof)).cast();
    unsafe {
        assert_eq!(switch_forwarding_recovery_prepare_witness(handle), PANIC);
        assert_eq!(switch_forwarding_recovery_prepare_witness(handle), REFUSED);
        assert_eq!(switch_forwarding_recovery_confirm(handle), REFUSED);
        switch_forwarding_recovery_destroy(handle);
    }
    assert!(!f.root.join("state.enc.keyid").exists());
}
