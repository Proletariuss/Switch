//! Real separate-process SWF1 sibling-lock checks; no Go peer or provider I/O.

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use proletarius_forwarding::{QueueStore, StateKey, StateLimits, StateStoreError};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CHILD_ROOT: &str = "SWITCH_NATIVE_LOCK_CHILD_ROOT";
const LIMIT: Duration = Duration::from_secs(12);

enum HeldLock {
    Queue(QueueStore),
    Raw(Flock<File>),
}

impl HeldLock {
    fn check(&self) {
        match self {
            Self::Queue(store) => {
                store.inspect().unwrap();
            }
            Self::Raw(file) => {
                file.metadata().unwrap();
            }
        }
    }
}

fn acquire(root: &Path, kind: &str) -> Result<HeldLock, StateStoreError> {
    match kind {
        "queue" => QueueStore::open(
            root.join("queue.swf1"),
            StateKey::from_slice(&(0_u8..32).collect::<Vec<_>>()).unwrap(),
            StateLimits::default(),
        )
        .map(HeldLock::Queue),
        "raw" => {
            // Independent protocol oracle: the historical lock name is the
            // state path plus '.lock', with a nonblocking exclusive flock.
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(root.join("queue.swf1.lock"))
                .unwrap();
            Flock::lock(file, FlockArg::LockExclusiveNonblock)
                .map(HeldLock::Raw)
                .map_err(|(_, error)| {
                    assert!(error == Errno::EWOULDBLOCK || error == Errno::EAGAIN);
                    StateStoreError::Busy
                })
        }
        "bootstrap" => {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .open(root.join("queue.swf1.bootstrap.lock"))
                .unwrap();
            Flock::lock(file, FlockArg::LockExclusiveNonblock)
                .map(HeldLock::Raw)
                .map_err(|(_, _)| StateStoreError::Busy)
        }
        _ => panic!("unknown peer kind"),
    }
}

fn mark(root: &Path, name: &str) {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join(name))
        .unwrap()
        .write_all(b"confirmed")
        .unwrap();
}

fn key_identity(byte: u8) -> [u8; 40] {
    let mut hash = Sha256::new();
    hash.update(b"Switch.SMSForwarding.QueueKey.v1\0");
    hash.update([byte; 32]);
    let mut value = [0; 40];
    value[..8].copy_from_slice(b"SWFKID1\0");
    value[8..].copy_from_slice(&hash.finalize());
    value
}

fn publish_identity(root: &Path, byte: u8) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("queue.swf1.keyid"))
        .unwrap();
    file.write_all(&key_identity(byte)).unwrap();
    file.sync_all().unwrap();
    File::open(root).unwrap().sync_all().unwrap();
}

fn await_marker(root: &Path, name: &str) {
    let deadline = Instant::now() + LIMIT;
    while !root.join(name).is_file() {
        assert!(Instant::now() < deadline, "timed out awaiting {name}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn native_lock_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    let root = PathBuf::from(root).canonicalize().unwrap();
    let build = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build")
        .canonicalize()
        .unwrap();
    assert!(root.starts_with(build));
    let kind = std::env::var("SWITCH_NATIVE_LOCK_KIND").unwrap();
    match std::env::var("SWITCH_NATIVE_LOCK_ROLE").unwrap().as_str() {
        "holder" => {
            let held = acquire(&root, &kind).unwrap();
            held.check();
            mark(&root, "holder-ready");
            await_marker(&root, "release-holder");
            held.check();
            drop(held);
        }
        "contender" => {
            assert!(matches!(acquire(&root, &kind), Err(StateStoreError::Busy)));
            mark(&root, "contender-blocked");
            await_marker(&root, "holder-exited");
            let recovered = acquire(&root, &kind).unwrap();
            recovered.check();
            mark(&root, "contender-recovered");
            drop(recovered);
        }
        "bootstrap-publisher" => {
            let held = acquire(&root, "bootstrap").unwrap();
            mark(&root, "publisher-ready");
            await_marker(&root, "publish-now");
            publish_identity(&root, 7);
            mark(&root, "witness-published");
            // Parent kills this child while it still owns the bootstrap lock.
            await_marker(&root, "never-release");
            drop(held);
        }
        "swift-check" => {
            let held = acquire(&root, "bootstrap").unwrap();
            let actual = fs::read(root.join("queue.swf1.keyid")).unwrap();
            assert_ne!(actual, key_identity(7));
            mark(&root, "swift-rejected");
            drop(held);
        }
        "stale-configure-probe" => {
            let expected = std::env::var("SWITCH_NATIVE_LOCK_EXPECT").unwrap();
            let result = acquire(&root, "queue");
            match expected.as_str() {
                "busy" => assert!(matches!(result, Err(StateStoreError::Busy))),
                "mismatch" => assert!(matches!(result, Err(StateStoreError::State(_)))),
                _ => panic!("unknown expected result"),
            }
            mark(&root, &format!("rust-{expected}"));
        }
        _ => panic!("unknown peer role"),
    }
}

#[test]
fn key_bootstrap_orders_and_crash_are_process_fenced() {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/tmp/native-forwarding-key");
    fs::create_dir_all(&parent).unwrap();
    for swift_first in [true, false] {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = parent.join(format!("{}-{nonce}-{swift_first}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("queue.swf1");
        if swift_first {
            let mut publisher = Peer::start(&root, "bootstrap-publisher", "bootstrap");
            publisher.await_marker(&root, "publisher-ready");
            assert!(matches!(
                QueueStore::open(
                    &path,
                    StateKey::from_slice(&[1; 32]).unwrap(),
                    StateLimits::default()
                ),
                Err(StateStoreError::Busy)
            ));
            assert!(!path.exists());
            mark(&root, "publish-now");
            publisher.await_marker(&root, "witness-published");
            publisher.child.kill().unwrap();
            assert_eq!(publisher.finish().signal(), Some(9));
            assert!(matches!(
                QueueStore::open(
                    &path,
                    StateKey::from_slice(&[1; 32]).unwrap(),
                    StateLimits::default()
                ),
                Err(StateStoreError::State(_))
            ));
            assert!(
                QueueStore::open(
                    &path,
                    StateKey::from_slice(&[7; 32]).unwrap(),
                    StateLimits::default()
                )
                .is_ok()
            );
        } else {
            let owner = QueueStore::open(
                &path,
                StateKey::from_slice(&[1; 32]).unwrap(),
                StateLimits::default(),
            )
            .unwrap();
            let mut checker = Peer::start(&root, "swift-check", "bootstrap");
            checker.await_marker(&root, "swift-rejected");
            assert!(checker.finish().success());
            owner.inspect().unwrap();
            drop(owner);
            assert!(matches!(
                QueueStore::open(
                    &path,
                    StateKey::from_slice(&[7; 32]).unwrap(),
                    StateLimits::default()
                ),
                Err(StateStoreError::State(_))
            ));
        }
        fs::remove_dir_all(root).unwrap();
    }
}

/// Uses the compiled production Swift credential path with mocked Keychain IO.
/// Set SWITCH_FORWARDING_SWIFT_TEST_BINARY to the isolated Swift harness.
#[test]
fn swift_and_rust_bootstrap_orders_and_crash() {
    let Some(swift_binary) = std::env::var_os("SWITCH_FORWARDING_SWIFT_TEST_BINARY") else {
        return;
    };
    let build = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build");
    fs::create_dir_all(&build).unwrap();
    let parent = build.join("tmp/swift-rust-key-protocol");
    fs::create_dir_all(&parent).unwrap();
    fn new_root(parent: &Path, label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = parent.join(format!("{}-{label}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        root
    }
    fn spawn_swift(binary: &std::ffi::OsStr, build: &Path, root: &Path, mode: &str) -> Child {
        Command::new(binary)
            .env("SWITCH_BUILD_DIR", build)
            .env("SWITCH_FORWARDING_CHECKPOINT_DIR", root)
            .env("SWITCH_FORWARDING_KEY_CROSS_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap()
    }
    fn line(child: &mut Child, expected: &[u8]) {
        let mut bytes = vec![0; expected.len()];
        child
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut bytes)
            .unwrap();
        assert_eq!(bytes, expected);
    }
    let old_key = || StateKey::from_slice(&(0_u8..32).collect::<Vec<_>>()).unwrap();

    // Swift observes an absent queue, persists K', then pauses before its
    // Keychain active-pointer write. Rust's old K must first see Busy, then
    // reject the durable witness after Swift commits.
    let root = new_root(&parent, "swift-first");
    let mut swift = spawn_swift(&swift_binary, &build, &root, "swift-bootstrap-pause");
    line(&mut swift, b"stage\n");
    let queue = root.join("sms-forwarding/state.enc");
    assert!(matches!(
        QueueStore::open(&queue, old_key(), StateLimits::default()),
        Err(StateStoreError::Busy)
    ));
    swift.stdin.as_mut().unwrap().write_all(&[1]).unwrap();
    line(&mut swift, b"committed\n");
    assert!(swift.wait().unwrap().success());
    assert!(matches!(
        QueueStore::open(&queue, old_key(), StateLimits::default()),
        Err(StateStoreError::State(_))
    ));
    assert!(!queue.exists());
    fs::remove_dir_all(root).unwrap();

    // Rust K opens first. A new GUI with an absent Keychain must not generate
    // K', even while the queue still has no ciphertext.
    let root = new_root(&parent, "rust-first");
    fs::create_dir(root.join("sms-forwarding")).unwrap();
    let queue = root.join("sms-forwarding/state.enc");
    let owner = QueueStore::open(&queue, old_key(), StateLimits::default()).unwrap();
    let mut swift = spawn_swift(&swift_binary, &build, &root, "swift-reject");
    line(&mut swift, b"rejected\n");
    assert!(swift.wait().unwrap().success());
    drop(owner);
    let mut restarted = spawn_swift(&swift_binary, &build, &root, "swift-reject");
    line(&mut restarted, b"rejected\n");
    assert!(restarted.wait().unwrap().success());
    fs::remove_dir_all(root).unwrap();

    // Kill Swift after its witness is durable but before active Keychain
    // publication. Neither a fresh GUI nor an old Rust configure may run.
    let root = new_root(&parent, "swift-crash");
    let mut swift = spawn_swift(&swift_binary, &build, &root, "swift-bootstrap-pause");
    line(&mut swift, b"stage\n");
    swift.kill().unwrap();
    assert_eq!(swift.wait().unwrap().signal(), Some(9));
    let mut restarted = spawn_swift(&swift_binary, &build, &root, "swift-reject");
    line(&mut restarted, b"rejected\n");
    assert!(restarted.wait().unwrap().success());
    let queue = root.join("sms-forwarding/state.enc");
    assert!(matches!(
        QueueStore::open(&queue, old_key(), StateLimits::default()),
        Err(StateStoreError::State(_))
    ));
    assert!(!queue.exists());
    fs::remove_dir_all(root).unwrap();
}

struct Peer {
    child: Child,
    log: PathBuf,
}

impl Peer {
    fn start(root: &Path, role: &str, kind: &str) -> Self {
        let log = root.join(format!("{role}.log"));
        let output = File::create(&log).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_lock_child", "--nocapture"])
            .env(CHILD_ROOT, root)
            .env("SWITCH_NATIVE_LOCK_ROLE", role)
            .env("SWITCH_NATIVE_LOCK_KIND", kind)
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap();
        assert_ne!(child.id(), std::process::id());
        Self { child, log }
    }

    fn await_marker(&mut self, root: &Path, marker: &str) {
        let deadline = Instant::now() + LIMIT;
        while !root.join(marker).is_file() {
            let exited = self.child.try_wait().unwrap();
            assert!(
                exited.is_none() && Instant::now() < deadline,
                "peer exited {exited:?} or timed out before {marker}: {}",
                fs::read_to_string(&self.log).unwrap()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(&mut self) -> ExitStatus {
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "peer did not exit: {}",
                fs::read_to_string(&self.log).unwrap()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        // Only this owned child is terminated; no process-name/global cleanup.
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn exercise(holder_kind: &str, contender_kind: &str, terminate_holder: bool) {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/tmp/native-forwarding-lock");
    fs::create_dir_all(&parent).unwrap();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = parent.join(format!("{}-{holder_kind}-{nonce}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let hex = include_str!("fixtures/go-state.swf1.hex").trim();
    let golden: Vec<u8> = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    assert_eq!(hex.len() % 2, 0);
    let mut queue = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("queue.swf1"))
        .unwrap();
    queue.write_all(&golden).unwrap();
    drop(queue);
    let mut holder = Peer::start(&root, "holder", holder_kind);
    holder.await_marker(&root, "holder-ready");
    let mut contender = Peer::start(&root, "contender", contender_kind);
    assert_ne!(holder.child.id(), contender.child.id());
    contender.await_marker(&root, "contender-blocked");
    assert!(holder.child.try_wait().unwrap().is_none());
    assert_eq!(fs::read(root.join("queue.swf1")).unwrap(), golden);
    if terminate_holder {
        holder.child.kill().unwrap();
        assert_eq!(holder.finish().signal(), Some(9));
    } else {
        mark(&root, "release-holder");
        assert!(holder.finish().success());
    }
    mark(&root, "holder-exited");
    contender.await_marker(&root, "contender-recovered");
    assert!(contender.finish().success());
    assert_eq!(fs::read(root.join("queue.swf1")).unwrap(), golden);
    // Keep only failed-case logs; all peers have been reaped on success.
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn queue_writer_excludes_independent_raw_peer_until_graceful_exit() {
    exercise("queue", "raw", false);
}

#[test]
fn raw_peer_excludes_independent_queue_writer_until_owner_process_dies() {
    exercise("raw", "queue", true);
}
