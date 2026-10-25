//! Independent shipping-process recovery checks using synthetic historical
//! JSON shapes and the committed Go-produced SWF1 ciphertext. No card, user
//! data, Keychain, provider endpoint, or legacy executable is accessed.
#![cfg(unix)]

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use tokio::net::UnixStream;

const TOKEN: &str = "native-recovery-synthetic-token";
const WAIT: Duration = Duration::from_secs(20);
// Literal fixtures intentionally omit later optional fields. They are not
// generated through the production serializer or derived from actual users.
const ARCHIVE: &str = r#"[
 {"id":"legacy-confirmed","sender":"fixture-accepted","content":"confirmed archive body","timestamp":"2026-09-01T12:00:00Z","direction":"out","device":"reader","send_request_id":"accepted-old","future_field":{"keep":"旧格式 😀"}},
 {"id":"legacy-delete","sender":"fixture-delete","content":"delete only this record","timestamp":"2026-09-01T12:01:00Z","direction":"in","device":"reader"}
]"#;
const LEDGER: &str = r#"[{"schema_version":1,"records":{
 "accepted-old":{"client_request_id":"accepted-old","phone":"fixture-accepted","message":"confirmed archive body","device":"reader","binding":"old-route","state":"accepted","attempt":"old-accepted-attempt","segments":2,"archive_confirmed":true,"created_at_ms":1,"updated_at_ms":2},
 "interrupted-old":{"client_request_id":"interrupted-old","phone":"fixture-unknown","message":"unknown must not replay","device":"reader","binding":"old-route","state":"submitting","attempt":"old-interrupted-attempt","segments":0,"archive_confirmed":false,"created_at_ms":3,"updated_at_ms":4}
}}]"#;
const MULTI: &str = r#"[{"revision":2,"profiles":{"old-profile":{"profile_id":"old-profile","card_id":"absent-card","tombstone":false}},"messages":{
 "old-profile\u0000received":{"message_id":"received","profile_id":"old-profile","direction":"inbound","peer":"fixture-peer","body":"confirmed old multi inbox","created_at_ms":1,"state":"received","revision":1,"deleted":false},
 "old-profile\u0000interrupted":{"message_id":"interrupted","profile_id":"old-profile","direction":"outbound","peer":"fixture-peer","body":"unknown multi send","created_at_ms":2,"state":"submitting","revision":2,"deleted":false,"attempt_token":"old-private-attempt"}
}}]"#;
const OPERATIONS: &str = r#"[{"operations":{
 "old-running":{"operation_id":"old-running","state":"running","detail":"old process interrupted","request_id":"old-request","fingerprint":"synthetic-fingerprint","action":"start","card_id":"absent-card","profile_ids":["old-profile"]},
 "old-completed":{"operation_id":"old-completed","state":"completed","detail":"old confirmed result","request_id":"old-complete-request","fingerprint":"synthetic-completed","action":"stop","card_id":"absent-card","profile_ids":["old-profile"]}
}}]"#;
const FIXTURES: &[(&str, &str)] = &[
    ("sms-archive.json", ARCHIVE),
    ("device-send-ledger.json", LEDGER),
    ("multistandby/store-v1.json", MULTI),
    ("multistandby/operations-v1.json", OPERATIONS),
];
const SWF1: &str = include_str!("../../proletarius-forwarding/tests/fixtures/go-state.swf1.hex");
const QUEUE_ID: &str = "71e946a085e000926dd82c21e14be42ec3a3bb91a76a84b3762e4922bbc1b5cf";
// Key bytes 00..1f, documented by the independent Go golden test.
const QUEUE_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

fn root() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = (0..16)
        .find_map(|_| {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("sw-nr-{}-{sequence}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => Some(Ok(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()
        .unwrap()
        .expect("fresh short native-recovery fixture directory");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn seed(root: &Path) {
    for (name, content) in FIXTURES {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

fn read_json(root: &Path, name: &str) -> Value {
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}

struct Daemon {
    root: PathBuf,
    child: Child,
    log: PathBuf,
}
impl Daemon {
    async fn start(root: &Path, generation: &str) -> Self {
        let log = root.join(format!("{generation}.log"));
        let output = fs::File::create(&log).unwrap();
        let serial = root.join("absent-serial");
        assert!(!serial.exists());
        let child = Command::new(env!("CARGO_BIN_EXE_switchd-rs"))
            .arg("-port")
            .arg(serial)
            .arg("-listen")
            .arg(format!("unix:{}", root.join("s").display()))
            .env("SWITCH_DATA_DIR", root)
            .env("HOME", root)
            .env("TMPDIR", root)
            .env("PROLETARIUS_APP_TOKEN", TOKEN)
            .env("PROLETARIUS_BLE_SOCKET", root.join("absent-ble"))
            .stdin(Stdio::null())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap();
        let mut daemon = Self {
            root: root.to_owned(),
            child,
            log,
        };
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "daemon exited: {}",
                daemon.log()
            );
            if let Ok((StatusCode::OK, value)) =
                daemon.request(Method::GET, "/api/v2/runtime", None).await
            {
                assert_eq!(value["backend"], "rust");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "startup timeout: {}",
                daemon.log()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        daemon
    }
    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value), String> {
        tokio::time::timeout(Duration::from_secs(5), async {
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
                            .method(method)
                            .uri(path)
                            .header("host", "localhost")
                            .header("x-proletarius-app-token", TOKEN)
                            .header("content-type", "application/json")
                            .body(Full::new(Bytes::from(
                                body.map(|v| serde_json::to_vec(&v).unwrap())
                                    .unwrap_or_default(),
                            )))
                            .unwrap(),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let status = response.status();
                let bytes = response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|e| e.to_string())?
                    .to_bytes();
                Ok((
                    status,
                    serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
                ))
            }
            .await;
            drop(sender);
            driver.abort();
            result
        })
        .await
        .map_err(|_| "HTTP timeout".to_owned())?
    }
    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (status, body) = self.request(method, path, body).await.unwrap();
        assert_eq!(status, StatusCode::OK, "{path}: {body}; {}", self.log());
        body
    }
    async fn stop(&mut self, signal: Signal) {
        kill(Pid::from_raw(self.child.id() as i32), signal).unwrap();
        let deadline = Instant::now() + WAIT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "exit timeout: {}", self.log());
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        if signal == Signal::SIGKILL {
            assert_eq!(status.signal(), Some(9));
            assert!(!self.log().contains("switchd-rs: native shutdown complete"));
        } else {
            assert!(status.success(), "{status}: {}", self.log());
            assert!(
                self.log()
                    .lines()
                    .any(|s| s == "switchd-rs: native shutdown complete"),
                "{}",
                self.log()
            );
            for name in ["s", "s.rs", "s.in"] {
                assert!(
                    !self.root.join(name).exists(),
                    "socket {name} survived cleanup"
                );
            }
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // Keep synthetic data and logs inside build for failure diagnosis.
    }
}

async fn assert_recovered(daemon: &Daemon, deleted: bool) {
    let archive = daemon.ok(Method::GET, "/api/v2/sms-archive", None).await;
    let rows = archive.as_array().unwrap();
    assert_eq!(rows.len(), if deleted { 1 } else { 2 });
    let confirmed = rows
        .iter()
        .find(|r| r["sender"] == "fixture-accepted")
        .unwrap();
    assert_eq!(confirmed["content"], "confirmed archive body");
    assert_eq!(confirmed["future_field"], json!({"keep":"旧格式 😀"}));
    assert_eq!(confirmed["send_request_id"], "accepted-old");
    let ledger_before = read_json(&daemon.root, "device-send-ledger.json");
    for (id, state, phone, message) in [
        (
            "accepted-old",
            "accepted",
            "fixture-accepted",
            "confirmed archive body",
        ),
        (
            "interrupted-old",
            "unknown",
            "fixture-unknown",
            "unknown must not replay",
        ),
    ] {
        let record = daemon
            .ok(
                Method::GET,
                &format!("/api/sms/send-intents?client_request_id={id}"),
                None,
            )
            .await;
        assert_eq!(record["state"], state);
        assert_eq!(record["can_submit"], false);
        assert!(
            record.get("subscription_id").is_none(),
            "old optional field defaults to empty and is omitted publicly"
        );
        assert!(record.get("attempt").is_none());
        let (status, result) = daemon
            .request(
                Method::POST,
                "/api/sms/send",
                Some(json!({
                    "client_request_id":id,"phone":phone,"message":message,"device":"reader"
                })),
            )
            .await
            .unwrap();
        if state == "accepted" {
            assert_eq!(status, StatusCode::OK, "{result}");
            assert_eq!(
                result,
                json!({"sent":true,"segments":2,"device":"reader","client_request_id":id})
            );
        } else {
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{result}");
            assert_eq!(result["code"], "sms_send_outcome_unknown");
        }
    }
    // Entire durable record equality proves no fresh claim, changed attempt,
    // new intent, or accepted/submitting transition was caused by replay.
    assert_eq!(
        read_json(&daemon.root, "device-send-ledger.json"),
        ledger_before
    );
    assert_eq!(
        ledger_before[0]["records"]["interrupted-old"]["state"],
        "unknown"
    );
    assert_eq!(
        ledger_before[0]["records"]["interrupted-old"]["attempt"],
        "old-interrupted-attempt"
    );
    let messages = daemon
        .ok(
            Method::GET,
            "/api/multistandby/messages?profile_id=old-profile",
            None,
        )
        .await;
    let rows = messages["messages"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let received = rows.iter().find(|m| m["message_id"] == "received").unwrap();
    assert_eq!(received["body"], "confirmed old multi inbox");
    assert_eq!(received["state"], "received");
    let unknown = rows
        .iter()
        .find(|m| m["message_id"] == "interrupted")
        .unwrap();
    assert_eq!(unknown["state"], "unknown");
    assert_eq!(unknown["error"], "backend_restarted_during_submission");
    assert!(unknown.get("attempt_token").is_none());
    for (id, state) in [("old-running", "unknown"), ("old-completed", "completed")] {
        let op = daemon
            .ok(
                Method::GET,
                &format!("/api/multistandby/operations/{id}"),
                None,
            )
            .await;
        assert_eq!(op["state"], state);
    }
}

async fn delete_acknowledged(daemon: &Daemon) -> Value {
    let result = daemon
        .ok(
            Method::POST,
            "/api/v2/sms-archive/delete-sender",
            Some(json!({
                "client_request_id":"native-recovery-delete-sender",
                "sender":"fixture-delete"
            })),
        )
        .await;
    assert_eq!(result["deleted"], true);
    assert_eq!(result["deleted_count"], 1);
    assert_eq!(
        result["mutation_receipt"]["client_request_id"],
        "native-recovery-delete-sender"
    );
    assert_eq!(result["mutation_receipt"]["outcome"], "completed");
    assert!(
        result["mutation_receipt"]["binding"]
            .as_str()
            .is_some_and(|binding| binding.starts_with("sms-archive-sender-v1:")),
        "{result}"
    );
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn historical_json_survives_clean_restart_and_restored_copy() {
    let active = root();
    seed(&active);
    let backup = root();
    // Copy the historical input before the new daemon can normalize it.
    for (name, _) in FIXTURES {
        let destination = backup.join(name);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(active.join(name), destination).unwrap();
    }
    let mut first = Daemon::start(&active, "first").await;
    assert_recovered(&first, false).await;
    let delete_ack = delete_acknowledged(&first).await;
    let first_pid = first.child.id();
    first.stop(Signal::SIGTERM).await;
    let mut second = Daemon::start(&active, "second").await;
    assert_ne!(first_pid, second.child.id());
    assert_recovered(&second, true).await;
    assert_eq!(
        delete_acknowledged(&second).await,
        delete_ack,
        "clean restart must replay the original durable mutation receipt"
    );
    second.stop(Signal::SIGTERM).await;

    let restored = root();
    for (name, content) in FIXTURES {
        assert_eq!(
            fs::read(backup.join(name)).unwrap(),
            content.as_bytes(),
            "untouched rollback copy"
        );
        let destination = restored.join(name);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(backup.join(name), destination).unwrap();
    }
    let mut rollback = Daemon::start(&restored, "restored").await;
    assert_recovered(&rollback, false).await;
    rollback.stop(Signal::SIGTERM).await;
    // This proves native reads of a restored data copy, not old-binary downgrade.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_archive_mutation_survives_sigkill_without_replaying_unknown_sends() {
    let root = root();
    seed(&root);
    let mut first = Daemon::start(&root, "crashed").await;
    assert_recovered(&first, false).await;
    let pid = first.child.id();
    let delete_ack = delete_acknowledged(&first).await;
    // No application flush or clean shutdown between HTTP acknowledgement and kill.
    first.stop(Signal::SIGKILL).await;
    assert!(
        root.join("s").exists(),
        "SIGKILL leaves a stale socket to recover"
    );
    let mut second = Daemon::start(&root, "recovered").await;
    assert_ne!(pid, second.child.id());
    assert_recovered(&second, true).await;
    assert_eq!(
        delete_acknowledged(&second).await,
        delete_ack,
        "SIGKILL recovery must replay the original durable mutation receipt"
    );
    second.stop(Signal::SIGTERM).await;
}

fn forwarding_config(key: &str) -> Value {
    // Both senders stay absent. Loading, inspecting and cancelling this local
    // queue cannot authorize any provider request, even with inherited proxy env.
    let (client_request_id, client_sequence) = if key == QUEUE_KEY {
        ("native-recovery-forwarding-config", 2)
    } else {
        ("native-recovery-forwarding-wrong-key", 1)
    };
    json!({
        "client_request_id":client_request_id,
        "client_sequence":client_sequence,
        "state_key":key,
        "telegram":{"enabled":false},
        "bark":{"enabled":false}
    })
}
fn task<'a>(status: &'a Value, id: &str) -> &'a Value {
    status["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == id)
        .unwrap_or_else(|| panic!("missing {id}: {status}"))
}
async fn assert_queue(daemon: &Daemon, cancelled: bool) {
    let status = daemon
        .ok(Method::GET, "/api/sms/forwarding/status", None)
        .await;
    assert_eq!(status["configured"], false);
    assert_eq!(status["telegram_enabled"], false);
    assert_eq!(status["bark_enabled"], false);
    let pending = task(&status, QUEUE_ID);
    assert_eq!(
        pending["telegram"]["state"],
        if cancelled {
            "cancelled"
        } else {
            "paused_disabled"
        }
    );
    assert_eq!(pending["telegram"]["attempts"], 2);
    assert_eq!(pending["telegram"]["confirmed_parts"], 1);
    assert_eq!(pending["telegram"]["total_parts"], 2);
    assert_eq!(
        pending["telegram"]["error_code"],
        "telegram_submission_unconfirmed"
    );
    assert_eq!(pending["bark"]["attempts"], 1);
    let legacy = task(&status, "fixture-legacy");
    assert_eq!(legacy["telegram"]["state"], "paused_disabled");
    assert_eq!(legacy["telegram"]["attempts"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn go_swf1_queue_key_and_confirmed_progress_survive_native_process_restart() {
    let root = root();
    let dir = root.join("sms-forwarding");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let hex = SWF1.trim();
    assert_eq!(hex.len() % 2, 0);
    let wire: Vec<u8> = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect();
    assert_eq!(&wire[..4], b"SWF1");
    let path = dir.join("state.enc");
    fs::write(&path, &wire).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut first = Daemon::start(&root, "queue-first").await;
    let (status, error) = first
        .request(
            Method::POST,
            "/api/sms/forwarding/configure",
            Some(forwarding_config(
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            )),
        )
        .await
        .unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], "sms_forwarding_queue_unavailable");
    assert_eq!(
        fs::read(&path).unwrap(),
        wire,
        "wrong key must not replace old ciphertext"
    );
    first
        .ok(
            Method::POST,
            "/api/sms/forwarding/configure",
            Some(forwarding_config(QUEUE_KEY)),
        )
        .await;
    assert_queue(&first, false).await;
    let queue_status = first
        .ok(Method::GET, "/api/sms/forwarding/status", None)
        .await;
    let telegram = &task(&queue_status, QUEUE_ID)["telegram"];
    let queue_action = json!({
        "client_request_id":"native-recovery-queue-cancel",
        "id":QUEUE_ID,"provider":"telegram","action":"cancel",
        "expected_configuration_id":queue_status["configuration_id"],
        "expected_attempts":telegram["attempts"],
        "expected_state":telegram["state"],
        "expected_destination_revision":telegram["revision"]
    });
    let ack = first
        .ok(
            Method::POST,
            "/api/sms/forwarding/queue-action",
            // Historical format fixtures also contain an artificial legacy
            // key; public mutations require the actual 64-hex message digest.
            // QUEUE_ID = SHA256("fixture-inbound-001") in the Go golden.
            Some(queue_action.clone()),
        )
        .await;
    assert_eq!(ack["accepted"], true);
    assert_eq!(ack["mutation_receipt"]["outcome"], "completed");
    assert_eq!(
        first
            .ok(
                Method::POST,
                "/api/sms/forwarding/queue-action",
                Some(queue_action.clone()),
            )
            .await,
        ack,
        "exact retry must return the original durable receipt"
    );
    let mut drift = queue_action.clone();
    drift["action"] = json!("retry");
    let (status, body) = first
        .request(
            Method::POST,
            "/api/sms/forwarding/queue-action",
            Some(drift),
        )
        .await
        .unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "public_mutation_request_conflict");
    assert_queue(&first, true).await;
    first.stop(Signal::SIGTERM).await;
    let mut second = Daemon::start(&root, "queue-second").await;
    second
        .ok(
            Method::POST,
            "/api/sms/forwarding/configure",
            Some(forwarding_config(QUEUE_KEY)),
        )
        .await;
    assert_eq!(
        second
            .ok(
                Method::POST,
                "/api/sms/forwarding/queue-action",
                Some(queue_action),
            )
            .await,
        ack,
        "daemon restart must not reapply the queue action"
    );
    assert_queue(&second, true).await;
    second.stop(Signal::SIGTERM).await;
}
