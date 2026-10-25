//! Actual standalone CLI, real executable fixture, UDS and child wait. The
//! copied host echo binary is not the ARM voice helper or DSP acceptance.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};
use switch_voice_supervisor::{ClientConfig, Command, Exit, State, client, verify_response};

const SESSION: &str = "00112233445566778899aabbccddeeff";
const NONCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[test]
fn standalone_serve_client_reap_and_release_preserve_session_artifacts() {
    let root = std::env::temp_dir().join(format!("svcli-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let key_path = root.join("key");
    let socket = root.join("socket");
    let pid_path = root.join("child.pid");
    let helper = root.join("host-echo-fixture");
    fs::copy("/bin/echo", &helper).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut key = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)
        .unwrap();
    key.write_all("07".repeat(32).as_bytes()).unwrap();
    key.sync_all().unwrap();
    let server_log = fs::File::create(root.join("server-fixed-error.log")).unwrap();
    let mut server = ProcessCommand::new(env!("CARGO_BIN_EXE_switch-voice-supervisor"))
        .arg("serve")
        .arg("--socket")
        .arg(&socket)
        .arg("--key-file")
        .arg(&key_path)
        .args(["--session", SESSION])
        .arg("--helper")
        .arg(&helper)
        .arg("--child-pid-file")
        .arg(&pid_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(server_log))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_error = None;
    let terminal = loop {
        let result = client(ClientConfig {
            socket: socket.clone(),
            key_file: key_path.clone(),
            session: SESSION.into(),
            nonce: NONCE.into(),
            command: Command::Status,
        });
        if let Err(error) = &result {
            last_error = Some(error.code());
        }
        if let Ok(line) = result {
            let response =
                verify_response(line.as_bytes(), SESSION, NONCE, Command::Status, &[7; 32])
                    .unwrap();
            if response.state() == State::Exited {
                break response;
            }
        }
        assert!(
            server.try_wait().unwrap().is_none(),
            "supervisor exited before RELEASE"
        );
        assert!(
            Instant::now() < deadline,
            "standalone terminal state not observed; fixed error {:?}",
            last_error
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(terminal.exit(), Exit::Code(0));
    let diagnostic = fs::read_to_string(&pid_path).unwrap();
    assert!(diagnostic.starts_with(&format!("{} ", terminal.pid())));
    #[cfg(target_os = "linux")]
    assert!(
        diagnostic
            .split_ascii_whitespace()
            .nth(1)
            .unwrap()
            .bytes()
            .all(|b| b.is_ascii_digit())
    );
    #[cfg(not(target_os = "linux"))]
    assert!(diagnostic.ends_with(" host-unavailable\n"));
    // The mandatory macOS sandbox allows fixture files, not anonymous pipe writes.
    let reply_path = root.join("client-signed-response");
    let error_path = root.join("client-fixed-error");
    let reply_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&reply_path)
        .unwrap();
    let error_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&error_path)
        .unwrap();
    let status = ProcessCommand::new(env!("CARGO_BIN_EXE_switch-voice-supervisor"))
        .arg("client")
        .arg("--socket")
        .arg(&socket)
        .arg("--key-file")
        .arg(&key_path)
        .args([
            "--session",
            SESSION,
            "--nonce",
            NONCE,
            "--command",
            "release",
        ])
        .stdout(Stdio::from(reply_file))
        .stderr(Stdio::from(error_file))
        .status()
        .unwrap();
    let reply = fs::read(&reply_path).unwrap();
    let fixed_error = fs::read(&error_path).unwrap();
    assert!(
        status.success(),
        "client exit {:?}, fixed stderr {}",
        status.code(),
        String::from_utf8_lossy(&fixed_error)
    );
    assert!(fixed_error.is_empty());
    assert_eq!(
        verify_response(&reply, SESSION, NONCE, Command::Release, &[7; 32])
            .unwrap()
            .state(),
        State::Exited
    );
    while server.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.symlink_metadata().unwrap().file_type().is_socket());
    assert!(key_path.exists());
    assert_eq!(fs::read_to_string(pid_path).unwrap(), diagnostic);
}
