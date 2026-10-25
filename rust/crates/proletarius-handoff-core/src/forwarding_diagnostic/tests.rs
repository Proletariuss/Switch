use super::*;
use proletarius_forwarding::{JsonDocument, seal, seal_empty_queue};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::PathBuf,
};
use zeroize::Zeroizing;
struct Fixture {
    root: PathBuf,
    directory: File,
}
impl Fixture {
    fn new() -> Self {
        let parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/rust-diagnostic-fixtures");
        fs::create_dir_all(&parent).unwrap();
        let root = parent.join(format!("{}-{}", std::process::id(), crate::uuid().unwrap()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&root)
            .unwrap();
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
    }
    fn seed(&self, key: &[u8]) {
        self.write(
            "state.enc",
            &seal_empty_queue(&StateKey::from_slice(key).unwrap()).unwrap(),
        );
    }
    fn witness(&self, key: &[u8]) {
        let mut hash = Sha256::new();
        hash.update(b"Switch.SMSForwarding.QueueKey.v1\0");
        hash.update(key);
        let mut bytes = b"SWFKID1\0".to_vec();
        bytes.extend_from_slice(&hash.finalize());
        self.write("state.enc.keyid", &bytes);
    }
    fn inspect(&self, keys: &[u8]) -> Result<(i32, Vec<u8>), i32> {
        diagnose(self.directory.as_raw_fd(), keys)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
#[test]
fn diagnoses_absent_torn_unsupported_and_missing_witness_without_creating() {
    let key = [7; 32];
    let f = Fixture::new();
    assert_eq!(f.inspect(&key), Ok((0, vec![0])));
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 0);
    for wire in [b"".as_slice(), b"S", b"SWF1", b"SWF1-torn"] {
        f.write("state.enc", wire);
        assert_eq!(f.inspect(&key), Ok((1, vec![0])));
        assert_eq!(fs::read(f.root.join("state.enc")).unwrap(), wire);
        fs::remove_file(f.root.join("state.enc")).unwrap();
    }
    f.write("state.enc", b"BAD!");
    assert_eq!(f.inspect(&key), Ok((2, vec![0])));
}
#[test]
fn authenticates_exact_ciphertext_each_key_and_checks_witness_independently() {
    let f = Fixture::new();
    let good = [7; 32];
    let old = [3; 32];
    f.seed(&good);
    f.witness(&good);
    let wire = fs::read(f.root.join("state.enc")).unwrap();
    let keyid = fs::read(f.root.join("state.enc.keyid")).unwrap();
    let keys = [old, good, good].concat();
    assert_eq!(f.inspect(&keys), Ok((67, vec![0x20, 0x11, 0x11])));
    assert_eq!(fs::read(f.root.join("state.enc")).unwrap(), wire);
    assert_eq!(fs::read(f.root.join("state.enc.keyid")).unwrap(), keyid);
    fs::remove_file(f.root.join("state.enc.keyid")).unwrap();
    assert_eq!(f.inspect(&good), Ok((3, vec![1])));
    f.write("state.enc.keyid", b"SWFKID1\0torn");
    assert_eq!(f.inspect(&good), Ok((51, vec![0x31])));
}
#[test]
fn other_authenticated_json_and_corrupt_ciphertext_never_become_exact_seed() {
    let f = Fixture::new();
    let raw = [7; 32];
    let key = StateKey::from_slice(&raw).unwrap();
    let doc = JsonDocument::from_bytes(
        Zeroizing::new(br#"{"seen":{"durable":"work"},"pending":{}}"#.to_vec()),
        StateLimits::default(),
    )
    .unwrap();
    let mut wire = seal(&key, &doc, StateLimits::default()).unwrap();
    f.write("state.enc", &wire);
    assert_eq!(f.inspect(&raw), Ok((3, vec![2])));
    fs::remove_file(f.root.join("state.enc")).unwrap();
    wire[20] ^= 1;
    f.write("state.enc", &wire);
    assert_eq!(f.inspect(&raw), Ok((3, vec![0])));
}
#[test]
fn detects_replaced_removed_hardlinked_and_in_place_changed_evidence() {
    for mutation in ["replace", "remove", "hardlink", "in-place"] {
        let f = Fixture::new();
        f.seed(&[7; 32]);
        let dir = unsafe { BorrowedFd::borrow_raw(f.directory.as_raw_fd()) };
        let e = capture(dir, "state.enc", 1024).unwrap();
        match mutation {
            "replace" => {
                fs::rename(f.root.join("state.enc"), f.root.join("original")).unwrap();
                f.seed(&[7; 32]);
            }
            "remove" => {
                fs::remove_file(f.root.join("state.enc")).unwrap();
            }
            "hardlink" => {
                fs::hard_link(f.root.join("state.enc"), f.root.join("alias")).unwrap();
            }
            _ => {
                let mut file = OpenOptions::new()
                    .write(true)
                    .open(f.root.join("state.enc"))
                    .unwrap();
                file.write_all(b"SWF1BAD!").unwrap();
            }
        }
        assert_eq!(confirm(dir, "state.enc", e.as_ref(), 1024), Err(UNSAFE));
    }
    let f = Fixture::new();
    let dir = unsafe { BorrowedFd::borrow_raw(f.directory.as_raw_fd()) };
    f.seed(&[7; 32]);
    assert_eq!(confirm(dir, "state.enc", None, 1024), Err(UNSAFE));
}
#[test]
fn rejects_symlink_fifo_permissions_links_and_oversize_without_writing_results() {
    for bad in ["symlink", "fifo", "mode", "hardlink", "witness-large"] {
        let f = Fixture::new();
        match bad {
            "symlink" => std::os::unix::fs::symlink("target", f.root.join("state.enc")).unwrap(),
            "fifo" => {
                nix::unistd::mkfifo(&f.root.join("state.enc"), Mode::from_bits_truncate(0o600))
                    .unwrap()
            }
            "mode" => {
                f.seed(&[7; 32]);
                fs::set_permissions(f.root.join("state.enc"), fs::Permissions::from_mode(0o640))
                    .unwrap();
            }
            "hardlink" => {
                f.seed(&[7; 32]);
                fs::hard_link(f.root.join("state.enc"), f.root.join("alias")).unwrap();
            }
            _ => {
                f.seed(&[7; 32]);
                f.write("state.enc.keyid", &[0; 41]);
            }
        }
        let raw = [7; 32];
        let mut output = [0xaa];
        assert_eq!(
            unsafe {
                switch_forwarding_diagnose_queue(
                    f.directory.as_raw_fd(),
                    raw.as_ptr(),
                    1,
                    output.as_mut_ptr(),
                    &mut std::ptr::null_mut(),
                )
            },
            UNSAFE
        );
        assert_eq!(output, [0xaa]);
    }
}
#[test]
fn ffi_handles_zero_candidates_and_rejects_unbounded_or_null_input() {
    let f = Fixture::new();
    let mut handle = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            switch_forwarding_diagnose_queue(
                f.directory.as_raw_fd(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                &mut handle,
            )
        },
        0
    );
    assert!(!handle.is_null());
    assert_eq!(unsafe { switch_forwarding_diagnostic_confirm(handle) }, 0);
    unsafe { switch_forwarding_diagnostic_destroy(handle) };
    assert_eq!(
        unsafe {
            switch_forwarding_diagnose_queue(
                f.directory.as_raw_fd(),
                std::ptr::null(),
                1,
                std::ptr::null_mut(),
                &mut handle,
            )
        },
        INVALID
    );
    assert_eq!(
        unsafe {
            switch_forwarding_diagnose_queue(
                f.directory.as_raw_fd(),
                std::ptr::null(),
                MAX_KEYS + 1,
                std::ptr::null_mut(),
                &mut handle,
            )
        },
        INVALID
    );
}

#[test]
fn ffi_pins_original_descriptors_through_the_final_confirmation() {
    let f = Fixture::new();
    let raw = [7; 32];
    f.seed(&raw);
    f.witness(&raw);
    let mut output = [0xaa];
    let mut handle = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            switch_forwarding_diagnose_queue(
                f.directory.as_raw_fd(),
                raw.as_ptr(),
                1,
                output.as_mut_ptr(),
                &mut handle,
            )
        },
        67
    );
    assert_eq!(output, [0x11]);
    assert!(!handle.is_null());
    fs::rename(f.root.join("state.enc"), f.root.join("preserved.enc")).unwrap();
    f.seed(&raw);
    assert_eq!(
        unsafe { switch_forwarding_diagnostic_confirm(handle) },
        UNSAFE
    );
    unsafe { switch_forwarding_diagnostic_destroy(handle) };
    assert!(f.root.join("preserved.enc").exists());
}
