//! Shipping-process CLI compatibility. All ports/data are synthetic and local.
#![cfg(unix)]

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::Value;
use std::{
    fs,
    ops::Deref,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::net::UnixStream;

const TOKEN: &str = "cli-compatibility-synthetic-token";
const WAIT: Duration = Duration::from_secs(20);
static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct FixtureRoot(PathBuf);
impl Deref for FixtureRoot {
    type Target = Path;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn root(case: usize) -> FixtureRoot {
    let root = (0..16)
        .find_map(|_| {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "switch-cli-{}-{case}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&root) {
                Ok(()) => Some(Ok(root)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()
        .unwrap()
        .expect("fresh private CLI fixture directory");
    fs::create_dir(root.join("empty-bin")).unwrap();
    FixtureRoot(root)
}
fn start(root: &Path, args: &[String]) -> Process {
    start_with_token(root, args, Some(TOKEN))
}
fn start_with_token(root: &Path, args: &[String], token: Option<&str>) -> Process {
    let log = fs::File::create(root.join("daemon.log")).unwrap();
    let absent = root.join("absent-port");
    assert!(!absent.exists());
    let mut command = Command::new(env!("CARGO_BIN_EXE_switchd-rs"));
    command
        .arg("-port")
        .arg(absent)
        .args(args)
        .env_clear()
        .current_dir(root)
        .env("PATH", root.join("empty-bin"))
        .env("CFFIXED_USER_HOME", root)
        .env("SWITCH_DATA_DIR", root)
        .env("HOME", root)
        .env("TMPDIR", root)
        .env("PROLETARIUS_BLE_SOCKET", root.join("absent-ble"))
        .env("PROLETARIUS_VOWIFI_CONFIG", root.join("absent-config.yaml"))
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    if let Some(token) = token {
        command.env("PROLETARIUS_APP_TOKEN", token);
    }
    Process(command.spawn().unwrap())
}
async fn request(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let socket = UnixStream::connect(path).await?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(socket)).await?;
    let driver = tokio::spawn(connection);
    let result = async {
        let response = sender
            .send_request(
                Request::builder()
                    .uri("/api/v2/runtime")
                    .header("host", "localhost")
                    .header("x-proletarius-app-token", TOKEN)
                    .header("connection", "close")
                    .body(Full::new(Bytes::new()))?,
            )
            .await?;
        if response.status() != 200 {
            return Err("unexpected runtime status".into());
        }
        let bytes = response.into_body().collect().await?.to_bytes();
        Ok(serde_json::from_slice(&bytes)?)
    }
    .await;
    driver.abort();
    result
}
async fn stopped(child: &mut Process) -> std::process::ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "CLI process did not exit");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn shipping_cli_uses_go_value_boundaries_without_opening_a_device() {
    for case in 0..5 {
        let root = root(case);
        let socket = root.join("s");
        let other = root.join("wrong");
        let good = format!("unix:{}", socket.display());
        let wrong = format!("unix:{}", other.display());
        let args = match case {
            0 => vec![format!("--listen={good}"), "--backend=at".into()],
            1 => vec![format!("-listen={wrong}"), "--listen".into(), good],
            2 => vec![
                "-vowifi-config".into(),
                "-backend=invalid-fixture-mode".into(),
                "-listen".into(),
                good,
            ],
            3 => vec![
                "-listen".into(),
                good,
                "--".into(),
                "-listen".into(),
                wrong,
                "-backend=invalid-fixture-mode".into(),
            ],
            _ => vec![
                "-listen".into(),
                good,
                "positional".into(),
                "-listen".into(),
                wrong,
                "-backend=invalid-fixture-mode".into(),
            ],
        };
        let mut child = start(&root, &args);
        let deadline = Instant::now() + WAIT;
        let status = loop {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "CLI exited early: {}",
                fs::read_to_string(root.join("daemon.log")).unwrap()
            );
            if let Ok(Ok(status)) =
                tokio::time::timeout(Duration::from_secs(2), request(&socket)).await
            {
                break status;
            }
            assert!(Instant::now() < deadline, "CLI readiness timeout");
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        assert_eq!(status["default_runtime"], "rust-native");
        assert_eq!(status["go_backend_connected"], false);
        assert!(!other.exists(), "ignored listen value created a socket");
        kill(Pid::from_raw(child.0.id() as i32), Signal::SIGTERM).unwrap();
        assert!(stopped(&mut child).await.success());
        assert!(!socket.exists());
        assert!(
            fs::read_to_string(root.join("daemon.log"))
                .unwrap()
                .lines()
                .any(|line| line == "switchd-rs: native shutdown complete")
        );
    }
}

#[tokio::test]
async fn malformed_cli_fails_without_creating_a_listener() {
    for case in 10..13 {
        let root = root(case);
        let socket = root.join("s");
        let good = format!("unix:{}", socket.display());
        let args = match case {
            10 => vec!["-listen".into(), good, "--vowifi-config".into()],
            11 => vec!["-listen".into(), good, "--listen=127.0.0.1:8080".into()],
            _ => vec!["--".into(), "-listen".into(), good],
        };
        let mut child = start(&root, &args);
        assert!(!stopped(&mut child).await.success());
        assert!(!socket.exists());
    }
}

#[tokio::test]
async fn shipping_daemon_rejects_missing_or_invalid_app_token_before_state_or_listener() {
    for (case, token) in [
        (20, None),
        (21, Some("short")),
        (22, Some("contains space token")),
    ] {
        let root = root(case);
        let socket = root.join("s");
        let args = vec![
            format!("--listen=unix:{}", socket.display()),
            "--backend=at".into(),
        ];
        let mut child = start_with_token(&root, &args, token);
        assert!(!stopped(&mut child).await.success());
        assert!(
            !socket.exists(),
            "invalid credential created the public listener"
        );
        assert!(
            !root.join(".switchd-lifetime.lock").exists(),
            "invalid credential acquired the data-root lifetime lease"
        );
        let log = fs::read_to_string(root.join("daemon.log")).unwrap();
        assert!(log.contains("app_token_missing_or_invalid"));
    }

    let root = root(23);
    let log = fs::File::create(root.join("daemon.log")).unwrap();
    let mut child = Process(
        Command::new(env!("CARGO_BIN_EXE_switchd-rs"))
            .arg("--internal-adb-probe")
            .env_clear()
            .current_dir(&*root)
            .env("SWITCH_DATA_DIR", &*root)
            .env("HOME", &*root)
            .env("TMPDIR", &*root)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    assert!(!stopped(&mut child).await.success());
    assert!(
        fs::read_to_string(root.join("daemon.log"))
            .unwrap()
            .contains("app_token_missing_or_invalid")
    );
    let names = fs::read_dir(&*root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        ["daemon.log", "empty-bin"]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect(),
        "unauthenticated physical helper touched its data root"
    );
}
