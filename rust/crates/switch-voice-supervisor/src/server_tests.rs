use super::*;
use crate::Exit;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SESSION: &str = "00112233445566778899aabbccddeeff";
const NONCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn directory() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "svs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    root
}
fn key(root: &std::path::Path) -> Key {
    let path = root.join("key");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all("07".repeat(32).as_bytes()).unwrap();
    file.sync_all().unwrap();
    Key::open(&path).unwrap()
}
fn child(script: &str) -> ChildOwner {
    let mut command = ProcessCommand::new("/bin/sh");
    command.args(["-c", script]);
    ChildOwner::spawn(command).unwrap()
}
fn request(root: &std::path::Path, command: Command) -> crate::Response {
    let line = client(ClientConfig {
        socket: root.join("socket"),
        key_file: root.join("key"),
        session: SESSION.into(),
        nonce: NONCE.into(),
        command,
    })
    .unwrap();
    verify_response(line.as_bytes(), SESSION, NONCE, command, &[7; 32]).unwrap()
}
fn run(script: &str) -> (PathBuf, std::thread::JoinHandle<Result<(), Error>>) {
    let root = directory();
    let key = key(&root);
    let (socket, listener) = Socket::bind(&root.join("socket")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut owner = child(script);
    let join = std::thread::spawn(move || run_owner(SESSION, &key, &socket, listener, &mut owner));
    (root, join)
}
fn terminal(root: &std::path::Path) {
    let end = Instant::now() + Duration::from_secs(4);
    loop {
        if request(root, Command::Status).state() == State::Exited {
            return;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn active_release_does_not_retire_owner_and_terminal_release_keeps_evidence() {
    let (root, join) = run("sleep 0.5; exit 3");
    let response = request(&root, Command::Release);
    assert_ne!(response.state(), State::Exited);
    assert_eq!(response.exit(), Exit::None);
    assert!(!join.is_finished());
    terminal(&root);
    assert_eq!(request(&root, Command::Release).state(), State::Exited);
    join.join().unwrap().unwrap();
    assert!(
        root.join("socket")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_socket()
    );
    assert!(root.join("key").exists());
}
#[test]
fn disconnected_stop_and_bad_mac_do_not_abandon_child_or_allow_foreign_commands() {
    let (root, join) = run("sleep 0.6; exit 0");
    let socket = Socket::existing(&root.join("socket")).unwrap();
    let mut bad = socket.connect().unwrap();
    let forged = encode_request(SESSION, NONCE, Command::Stop, &[8; 32]).unwrap();
    write_line(&mut bad, forged.as_bytes(), Instant::now() + IO_BUDGET).unwrap();
    drop(bad);
    assert_eq!(request(&root, Command::Status).state(), State::Running);
    let mut stream = socket.connect().unwrap();
    let stop = encode_request(SESSION, NONCE, Command::Stop, &[7; 32]).unwrap();
    write_line(&mut stream, stop.as_bytes(), Instant::now() + IO_BUDGET).unwrap();
    drop(stream);
    terminal(&root);
    assert!(!join.is_finished());
    assert_eq!(request(&root, Command::Stop).state(), State::Exited);
    request(&root, Command::Release);
    join.join().unwrap().unwrap();
}
#[test]
fn bounded_partial_request_does_not_drop_a_stopping_child() {
    let root = directory();
    let key = key(&root);
    let (socket, listener) = Socket::bind(&root.join("socket")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = ProcessCommand::new("/bin/sh");
    command
        .args([
            "-c",
            "trap '' TERM; : > \"$1\"; sleep 1.8; exit 0",
            "supervisor-test",
        ])
        .arg(root.join("ready"));
    let mut owner = ChildOwner::spawn(command).unwrap();
    let join = std::thread::spawn(move || run_owner(SESSION, &key, &socket, listener, &mut owner));
    let ready_deadline = Instant::now() + Duration::from_secs(3);
    while !root.join("ready").exists() {
        assert!(Instant::now() < ready_deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    // Actual child marker follows trap installation; no scheduler delay is
    // treated as readiness, and this is not the ARM voice-helper fixture.
    assert_eq!(request(&root, Command::Stop).state(), State::Stopping);
    let socket = Socket::existing(&root.join("socket")).unwrap();
    let mut stream = socket.connect().unwrap();
    stream.write_all(b"SVS1").unwrap();
    std::thread::sleep(IO_BUDGET + Duration::from_millis(60));
    drop(stream);
    let state = request(&root, Command::Status).state();
    assert!(matches!(state, State::Stopping | State::Exited));
    assert!(!join.is_finished());
    terminal(&root);
    request(&root, Command::Release);
    join.join().unwrap().unwrap();
}
#[test]
fn protocol_eof_and_extra_records_are_never_success() {
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    sender.write_all(b"SVS1").unwrap();
    sender.shutdown(Shutdown::Write).unwrap();
    assert!(read_line(&mut receiver, Instant::now() + IO_BUDGET).is_err());
    let (mut sender, mut receiver) = UnixStream::pair().unwrap();
    sender.write_all(b"one\ntwo\n").unwrap();
    assert!(read_line(&mut receiver, Instant::now() + IO_BUDGET).is_err());
}
#[test]
fn unsafe_key_and_replaced_socket_names_are_rejected_without_unlinking() {
    let root = directory();
    let key = key(&root);
    let path = root.join("key");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Key::open(&path).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(&path, root.join("old-key")).unwrap();
    std::os::unix::fs::symlink(root.join("old-key"), &path).unwrap();
    assert!(key.validate().is_err());
    assert!(Key::open(&path).is_err());
    let (socket, _listener) = Socket::bind(&root.join("socket")).unwrap();
    fs::rename(root.join("socket"), root.join("old-socket")).unwrap();
    fs::write(root.join("socket"), b"replacement").unwrap();
    assert!(socket.validate().is_err());
    drop(socket);
    assert_eq!(fs::read(root.join("socket")).unwrap(), b"replacement");
    assert!(
        root.join("old-socket")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_socket()
    );
}
#[test]
fn pidfile_is_exclusive_retained_diagnostic_and_cannot_replace_foreign_entry() {
    let root = directory();
    let path = root.join("child.pid");
    let mut pid = PidFile::reserve(&path).unwrap();
    let mut owner = child("sleep 0.3; exit 0");
    pid.write(owner.pid()).unwrap();
    let original = fs::read(&path).unwrap();
    assert!(PidFile::reserve(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    fs::rename(&path, root.join("old-pid")).unwrap();
    fs::write(&path, b"foreign").unwrap();
    assert!(pid.write(owner.pid()).is_err());
    owner.stop_once().unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    while owner.poll().unwrap() != State::Exited {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(pid);
    assert_eq!(fs::read(&path).unwrap(), b"foreign");
    assert_eq!(fs::read(root.join("old-pid")).unwrap(), original);
}

#[test]
fn key_hardlink_fifo_and_nonprivate_control_directory_are_rejected() {
    let root = directory();
    let _key = key(&root);
    fs::hard_link(root.join("key"), root.join("alias")).unwrap();
    assert!(Key::open(&root.join("key")).is_err());
    let fifo = root.join("fifo");
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let start = Instant::now();
    assert!(Key::open(&fifo).is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Socket::bind(&root.join("socket")).is_err());
    assert!(PidFile::reserve(&root.join("child.pid")).is_err());
    assert!(root.join("key").exists());
    assert!(fifo.symlink_metadata().unwrap().file_type().is_fifo());
}
