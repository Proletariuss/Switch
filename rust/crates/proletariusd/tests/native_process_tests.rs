//! Shipping executable lifecycle checks. No cfg(test) daemon entrypoint or Go
//! fixture is used. An explicit nonexistent serial path confines discovery to
//! build/runtime; production explicit-port selection must never fall back to USB.
#![cfg(unix)]

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use tokio::net::UnixStream;

const TOKEN: &str = "native-process-fixture-token";
const WAIT: Duration = Duration::from_secs(15);

struct Fixture {
    root: PathBuf,
    process: Child,
    daemon_pid: Option<Pid>,
}
impl Fixture {
    fn start(parent_shell: bool) -> Self {
        Self::start_with_output(parent_shell, false)
    }

    fn start_with_output(parent_shell: bool, piped_output: bool) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = (0..16)
            .find_map(|_| {
                let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
                let root =
                    std::env::temp_dir().join(format!("sw-np-{}-{sequence}", std::process::id()));
                match fs::create_dir(&root) {
                    Ok(()) => Some(Ok(root)),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .transpose()
            .unwrap()
            .expect("fresh short native-process fixture directory");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.join("s");
        let port = root.join("absent-serial-port");
        assert!(!port.exists());
        let binary = env!("CARGO_BIN_EXE_switchd-rs");
        let mut command = if parent_shell {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "pid_file=$1; shift; \"$@\" & daemon=$!; printf '%s\\n' \"$daemon\" > \"$pid_file\"; wait \"$daemon\"", "native-process-parent"])
                .arg(root.join("pid"))
                .arg(binary);
            command
        } else {
            Command::new(binary)
        };
        let log = fs::File::create(root.join("daemon.log")).unwrap();
        let process = command
            .arg("-port")
            .arg(&port)
            .arg("-listen")
            .arg(format!("unix:{}", socket.display()))
            .env("SWITCH_DATA_DIR", &root)
            .env("HOME", &root)
            .env("TMPDIR", &root)
            .env("PROLETARIUS_APP_TOKEN", TOKEN)
            .env(
                "PROLETARIUS_GO_BACKEND_PATH",
                root.join("nonexistent-go-executable"),
            )
            .env("PROLETARIUS_BLE_SOCKET", root.join("absent-ble-socket"))
            .stdin(Stdio::null())
            .stdout(if piped_output {
                Stdio::piped()
            } else {
                Stdio::from(log.try_clone().unwrap())
            })
            .stderr(if piped_output {
                Stdio::piped()
            } else {
                Stdio::from(log)
            })
            .spawn()
            .expect("start actual shipping daemon executable");
        let daemon_pid = (!parent_shell).then(|| Pid::from_raw(process.id() as i32));
        Self {
            root,
            process,
            daemon_pid,
        }
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.join("daemon.log")).unwrap_or_default()
    }

    fn close_output_readers(&mut self) {
        // Keep both read ends alive through ready(); dropping them here makes
        // subsequent production diagnostics hit the actual closed pipe.
        drop(self.process.stdout.take().expect("piped stdout reader"));
        drop(self.process.stderr.take().expect("piped stderr reader"));
    }

    async fn get(&self, path: &str) -> Result<(StatusCode, Value), String> {
        tokio::time::timeout(Duration::from_secs(3), async {
            let stream = UnixStream::connect(self.root.join("s"))
                .await
                .map_err(|e| e.to_string())?;
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(|e| e.to_string())?;
            let driver = tokio::spawn(connection);
            let result = async {
                let response = sender
                    .send_request(
                        Request::builder()
                            .uri(path)
                            .header("host", "localhost")
                            .header("x-proletarius-app-token", TOKEN)
                            .body(Full::new(Bytes::new()))
                            .unwrap(),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let status = response.status();
                let body = response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|e| e.to_string())?
                    .to_bytes();
                let value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
                Ok((status, value))
            }
            .await;
            drop(sender);
            driver.abort();
            result
        })
        .await
        .map_err(|_| "HTTP request timed out".to_owned())?
    }

    async fn ready(&mut self) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(
                self.process.try_wait().unwrap().is_none(),
                "daemon parent exited: {}",
                self.log()
            );
            if let Ok((StatusCode::OK, value)) = self.get("/api/v2/runtime").await {
                if self.daemon_pid.is_none() {
                    let pid = fs::read_to_string(self.root.join("pid")).unwrap();
                    self.daemon_pid = Some(Pid::from_raw(pid.trim().parse().unwrap()));
                }
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "runtime did not become ready: {}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn wait_child_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.process.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "child failed to exit: {}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn assert_clean_shutdown_log(&self) {
        let log = self.log();
        assert!(
            log.lines()
                .any(|line| line == "switchd-rs: native shutdown complete"),
            "daemon exited without a successful native cleanup receipt: {log}"
        );
        for failure in [
            "I/O failure:",
            "shutdown unconfirmed",
            "cleanup failed:",
            "shutdown failed:",
            "panicked at",
            "Go backend restart",
            "restart budget exhausted",
        ] {
            assert!(
                !log.contains(failure),
                "daemon shutdown reported {failure}: {log}"
            );
        }
    }

    fn assert_sockets_removed(&self) {
        for name in ["s", "s.rs", "s.in", "s.go"] {
            assert!(
                !self.root.join(name).exists(),
                "socket {name} survived shutdown: {}",
                self.log()
            );
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Only PIDs created by this fixture are targeted; no shared daemon or
        // global process-name cleanup is allowed, including on assertion panic.
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
        if let Some(pid) = self.daemon_pid.take() {
            let _ = kill(pid, Signal::SIGKILL);
        } else if let Ok(pid) = fs::read_to_string(self.root.join("pid")) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            }
        }
        // Preserve the isolated log for failed-run diagnostics in the private
        // short temporary root. The socket path must remain below SUN_LEN even
        // when the source capsule itself has a deliberately long path.
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shipping_daemon_without_go_reports_native_no_device_and_exits_cleanly() {
    let mut fixture = Fixture::start(false);
    let runtime = fixture.ready().await;
    assert_eq!(runtime["ok"], true, "{runtime}");
    assert_eq!(runtime["backend"], "rust");
    assert_eq!(runtime["hardware_owner"], "rust");
    assert_eq!(runtime["default_runtime"], "rust-native");
    assert_eq!(runtime["go_backend_connected"], false);
    assert_eq!(runtime["go_backend_restarting"], false);
    assert_eq!(runtime["go_backend_phase"], "retired");
    assert_eq!(runtime["go_backend_generation"], 0);
    assert_eq!(runtime["go_restart_attempts"], 0);
    assert_eq!(
        runtime["cutover_ready"], false,
        "offline startup cannot certify hardware acceptance"
    );
    assert!(!fixture.root.join("s.go").exists());

    let (status, health) = fixture.get("/api/health").await.unwrap();
    assert_eq!(status, StatusCode::OK, "{health}");
    assert_eq!(health["ok"], true);
    assert_eq!(health["esim_available"], false);
    assert!(
        health["usb_device"].is_null(),
        "explicit missing serial port must never select USB: {health}"
    );
    let (status, device) = fixture.get("/api/status").await.unwrap();
    assert_eq!(status, StatusCode::OK, "{device}");
    assert_eq!(device["sim_inserted"], false, "{device}");
    let (status, readiness) = fixture.get("/api/v2/migration-readiness").await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(readiness["ready_to_remove_go"], false);

    kill(fixture.daemon_pid.unwrap(), Signal::SIGTERM).unwrap();
    let status = fixture.wait_child_exit().await;
    assert!(
        status.success(),
        "SIGTERM shutdown failed: {status}: {}",
        fixture.log()
    );
    fixture.daemon_pid = None;
    fixture.assert_sockets_removed();
    fixture.assert_clean_shutdown_log();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shipping_daemon_joins_native_shutdown_after_its_parent_disappears() {
    let mut fixture = Fixture::start(true);
    fixture.ready().await;
    let daemon = fixture.daemon_pid.unwrap();
    // Kill only the launcher. The daemon must detect reparenting and run its
    // own shutdown, rather than being terminated by a process-group signal.
    kill(Pid::from_raw(fixture.process.id() as i32), Signal::SIGKILL).unwrap();
    fixture.wait_child_exit().await;
    let deadline = Instant::now() + WAIT;
    while kill(daemon, None) != Err(Errno::ESRCH) {
        assert!(
            Instant::now() < deadline,
            "daemon survived parent loss: {}",
            fixture.log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    fixture.daemon_pid = None;
    fs::remove_file(fixture.root.join("pid")).unwrap();
    fixture.assert_sockets_removed();
    fixture.assert_clean_shutdown_log();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shipping_daemon_read_only_call_mode_probe_does_not_hold_shutdown_forever() {
    let mut fixture = Fixture::start(false);
    fixture.ready().await;
    let (status, _) = fixture.get("/api/call-mode/status").await.unwrap();
    assert_eq!(status, StatusCode::OK);
    // A connected module's read-only status probes create these empty lock
    // files. Reproduce their disk footprint without using physical hardware.
    fs::File::create(fixture.root.join("call-mode-operation.lock")).unwrap();
    fs::File::create(fixture.root.join(".module-adb-interface.lock")).unwrap();
    assert!(fixture.root.join("call-mode-operation.lock").exists());
    assert!(fixture.root.join(".module-adb-interface.lock").exists());
    assert!(fixture.root.join(".call-mode-pristine-v1").exists());

    kill(fixture.daemon_pid.unwrap(), Signal::SIGTERM).unwrap();
    let status = fixture.wait_child_exit().await;
    assert!(
        status.success(),
        "read-only shutdown failed: {}",
        fixture.log()
    );
    fixture.daemon_pid = None;
    fixture.assert_sockets_removed();
    fixture.assert_clean_shutdown_log();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shipping_daemon_sigterm_with_closed_output_pipes_exits_zero_and_removes_sockets() {
    let mut fixture = Fixture::start_with_output(false, true);
    let runtime = fixture.ready().await;
    assert_eq!(runtime["backend"], "rust");
    fixture.close_output_readers();
    kill(fixture.daemon_pid.unwrap(), Signal::SIGTERM).unwrap();
    let status = fixture.wait_child_exit().await;
    assert!(
        status.success(),
        "closed-pipe SIGTERM exit status: {status}"
    );
    fixture.daemon_pid = None;
    fixture.assert_sockets_removed();
    // The closed output streams cannot carry a success receipt. The directly
    // reaped shipping process exit status and socket cleanup are the evidence.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shipping_daemon_parent_loss_with_closed_output_pipes_removes_sockets() {
    let mut fixture = Fixture::start_with_output(true, true);
    fixture.ready().await;
    let daemon = fixture.daemon_pid.unwrap();
    fixture.close_output_readers();
    kill(Pid::from_raw(fixture.process.id() as i32), Signal::SIGKILL).unwrap();
    fixture.wait_child_exit().await;
    let deadline = Instant::now() + WAIT;
    while kill(daemon, None) != Err(Errno::ESRCH) {
        assert!(
            Instant::now() < deadline,
            "closed-pipe daemon survived parent loss"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    fixture.daemon_pid = None;
    fs::remove_file(fixture.root.join("pid")).unwrap();
    fixture.assert_sockets_removed();
    // This orphan is reaped by the OS, so its exit code is not observable here.
    // The direct-child test above separately requires a successful exit code.
}
