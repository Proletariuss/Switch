use super::*;
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UdpSocket},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Cli {
    root: PathBuf,
    path: PathBuf,
}
impl Cli {
    fn new(body: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "native-startup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("cli");
        std::fs::write(
            &path,
            format!("#!/bin/sh\ncd \"$(dirname \"$0\")\" || exit 1\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, path }
    }
    async fn started_pid(&self) -> String {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(self.root.join("pid")) {
                    if !pid.trim().is_empty() {
                        break pid.trim().to_owned();
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("actual child must publish PID")
    }
}
impl Drop for Cli {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn direct() -> Result<SelectedTransport, SessionSelectionError> {
    Ok(SelectedTransport {
        mode: proletarius_vowifi::ProxyMode::Direct("127.0.0.1".parse().unwrap()),
        proxy: None,
        proxy_error: None,
        proxy_source: Some("surge-effective-profile"),
    })
}
fn code(result: Result<SelectedTransport, PlanError>) -> &'static str {
    result.unwrap_err().code()
}
async fn assert_reaped(pid: &str) {
    let status = tokio::process::Command::new("/bin/kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(
        !status.success(),
        "command returned before actual process exited"
    );
}

/// Private executable fixture shared with the real module preparation caller.
pub(in crate::native_vowifi_session) struct HeldCli {
    cli: Cli,
    ack: std::sync::Arc<ReapAck>,
}
impl HeldCli {
    pub(in crate::native_vowifi_session) fn new() -> Self {
        Self {
            cli: Cli::new(
                r#"
case "$*" in
 'feature get system-proxy') echo 'system-proxy: on';;
 'feature get enhanced-mode') echo 'enhanced-mode: off';;
 'feature set enhanced-mode on') echo $$ > pid; exec /bin/sleep 60;;
esac"#,
            ),
            ack: std::sync::Arc::new(ReapAck::default()),
        }
    }
    pub(in crate::native_vowifi_session) async fn select(
        &self,
        stop: watch::Receiver<bool>,
    ) -> Result<SelectedTransport, PlanError> {
        select_with_control(
            true,
            Control {
                stop: Some(stop),
                deadline: None,
                reap_ack: Some(self.ack.clone()),
            },
            &self.cli.path,
            |_| async { direct() },
        )
        .await
    }
    pub(in crate::native_vowifi_session) async fn started_pid(&self) -> String {
        self.cli.started_pid().await
    }
    pub(in crate::native_vowifi_session) async fn waiting_reap(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.ack.entered.notified())
            .await
            .unwrap();
    }
    pub(in crate::native_vowifi_session) fn release_reap(&self) {
        self.ack.release.notify_one();
    }
    pub(in crate::native_vowifi_session) async fn assert_reaped(&self, pid: &str) {
        assert_reaped(pid).await;
    }
}
async fn socks() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = listener.local_addr().unwrap();
    let relay = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server = tokio::spawn(async move {
        for connection in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, proletarius_vowifi::NO_AUTH_GREETING);
            stream.write_all(&[5, 0]).await.unwrap();
            if connection == 1 {
                let mut associate = [0; 10];
                stream.read_exact(&mut associate).await.unwrap();
                assert_eq!(associate, proletarius_vowifi::UDP_ASSOCIATE_REQUEST);
                let mut reply = vec![5, 0, 0, 1, 127, 0, 0, 1];
                reply.extend_from_slice(&relay.local_addr().unwrap().port().to_be_bytes());
                stream.write_all(&reply).await.unwrap();
                let mut byte = [0];
                assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
            }
        }
    });
    (proxy, server)
}

#[tokio::test]
async fn actual_cli_stderr_enable_and_real_socks_reselection() {
    let cli = Cli::new(
        r#"
printf '%s\n' "$*" >> trace
case "$*" in
 'feature get system-proxy') echo 'system-proxy: on' >&2;;
 'feature get enhanced-mode') if [ -f enabled ]; then echo 'enhanced-mode: on' >&2; else echo 'enhanced-mode: off' >&2; fi;;
 'feature set enhanced-mode on') touch enabled;;
 *) exit 1;;
esac"#,
    );
    let (proxy, server) = socks().await;
    let mut calls = 0;
    let selected = select_with(true, None, &cli.path, || {
        calls += 1;
        let first = calls == 1;
        async move {
            if first {
                direct()
            } else {
                // Discovery authority is injected; SOCKS control/UDP ASSOCIATE
                // are real I/O through the production selection implementation.
                let mut selected = crate::transport_selection::select_for_session(
                    &proxy.to_string(),
                    "epdg.invalid",
                )
                .await?;
                assert_eq!(selected.proxy_source, Some("explicit"));
                selected.proxy_source = Some("surge-effective-profile");
                Ok(selected)
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        selected.mode,
        proletarius_vowifi::ProxyMode::Socks5UdpDomain
    );
    assert_eq!(selected.proxy, Some(proxy));
    assert_eq!(calls, 2);
    assert_eq!(
        std::fs::read_to_string(cli.root.join("trace")).unwrap(),
        "feature get system-proxy\nfeature get enhanced-mode\nfeature set enhanced-mode on\nfeature get enhanced-mode\n"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn explicit_domain_proxy_uses_real_socks_without_cli_mutation() {
    let (proxy, server) = socks().await;
    let selected = select(&format!("socks5h://{proxy}"), "epdg.invalid", None)
        .await
        .unwrap();
    assert_eq!(
        selected.mode,
        proletarius_vowifi::ProxyMode::Socks5UdpDomain
    );
    assert_eq!(selected.proxy_source, Some("explicit"));
    assert_eq!(selected.proxy, Some(proxy));
    server.await.unwrap();
}

#[tokio::test]
async fn stopped_before_selection_never_spawns_cli() {
    let cli = Cli::new("touch unexpected");
    let (_tx, stop) = watch::channel(true);
    assert_eq!(
        code(
            select_with(true, Some(stop), &cli.path, || async {
                panic!("no selection after stop")
            })
            .await
        ),
        CANCELLED
    );
    assert!(!cli.root.join("unexpected").exists());
}

#[tokio::test]
async fn cancel_during_enable_reaps_real_child_without_reselection() {
    let cli = Cli::new(
        r#"
printf '%s\n' "$*" >> trace
case "$*" in
 'feature get system-proxy') echo 'system-proxy: on';;
 'feature get enhanced-mode') echo 'enhanced-mode: off';;
 'feature set enhanced-mode on') echo $$ > pid; exec /bin/sleep 60;;
esac"#,
    );
    let (tx, stop) = watch::channel(false);
    let path = cli.path.clone();
    let task = tokio::spawn(async move {
        let mut calls = 0;
        let result = select_with(true, Some(stop), &path, || {
            calls += 1;
            assert_eq!(calls, 1);
            async { direct() }
        })
        .await;
        code(result)
    });
    let pid = cli.started_pid().await;
    tx.send(true).unwrap();
    assert_eq!(task.await.unwrap(), CANCELLED);
    assert_reaped(&pid).await;
    assert_eq!(
        std::fs::read_to_string(cli.root.join("trace"))
            .unwrap()
            .lines()
            .count(),
        3
    );
}

async fn delayed_preparation_and_cli_reap_case(inner_stop_first: bool) {
    use super::super::module::{PreparationEnd, prepare_until};
    use std::sync::{Arc, atomic::AtomicUsize};
    let cli = Cli::new(
        r#"
printf '%s\n' "$*" >> trace
case "$*" in
 'feature get system-proxy') echo 'system-proxy: on';;
 'feature get enhanced-mode') echo 'enhanced-mode: off';;
 'feature set enhanced-mode on') echo $$ > pid; exec /bin/sleep 60;;
esac"#,
    );
    let ack = Arc::new(ReapAck::default());
    let restored = Arc::new(AtomicUsize::new(0));
    let retries = Arc::new(AtomicUsize::new(0));
    let (record_stop, stop) = watch::channel(false);
    let (inner_stop, inner_signal) = watch::channel(false);
    let (card_ready, delayed_card) = tokio::sync::oneshot::channel();
    let (elapsed, attempt_deadline) = tokio::sync::oneshot::channel();
    let path = cli.path.clone();
    let pid_path = cli.root.join("pid");
    let owned_ack = ack.clone();
    let owned_restored = restored.clone();
    let owned_retries = retries.clone();
    let owner = tokio::spawn(async move {
        // The attempt's deadline authority already exists while the card is
        // delayed. Releasing this boundary leaves transport control consuming
        // the same attempt budget, exactly as module.run_prepared does.
        delayed_card.await.unwrap();
        let (end, result) = prepare_until(
            async { attempt_deadline.await.unwrap() },
            stop,
            |attempt_stop| {
                let control = Control {
                    stop: Some(if inner_stop_first {
                        inner_signal
                    } else {
                        attempt_stop
                    }),
                    deadline: None,
                    reap_ack: Some(owned_ack),
                };
                select_with_control(true, control, &path, |_| async { direct() })
            },
        )
        .await;
        let pid = std::fs::read_to_string(pid_path).unwrap();
        assert_reaped(pid.trim()).await;
        // These counters measure helper continuation only; actual module RF
        // restoration is covered by the run_prepared_with caller test.
        owned_restored.fetch_add(1, Ordering::SeqCst);
        if end == PreparationEnd::Deadline {
            owned_retries.fetch_add(1, Ordering::SeqCst);
        }
        (end, code(result))
    });
    tokio::task::yield_now().await;
    assert!(
        !cli.root.join("pid").exists(),
        "no CLI before the delayed card is ready"
    );
    card_ready.send(()).unwrap();
    let pid = cli.started_pid().await;
    if inner_stop_first {
        inner_stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(3), ack.entered.notified())
            .await
            .unwrap();
    }
    elapsed.send(()).unwrap();
    if !inner_stop_first {
        tokio::time::timeout(Duration::from_secs(3), ack.entered.notified())
            .await
            .unwrap();
    } else {
        // The command is already in its post-kill wait when the outer module
        // deadline fires. A subsequent public stop must not drop that wait.
        tokio::task::yield_now().await;
        record_stop.send(true).unwrap();
        tokio::task::yield_now().await;
    }
    assert!(
        !owner.is_finished(),
        "preparation must retain its actual Child wait"
    );
    assert_eq!(restored.load(Ordering::SeqCst), 0);
    assert_eq!(retries.load(Ordering::SeqCst), 0);
    if !inner_stop_first {
        assert!(
            !*record_stop.borrow(),
            "attempt timeout must preserve the public retry intent"
        );
    }
    ack.release.notify_one();
    let (end, failure) = tokio::time::timeout(Duration::from_secs(3), owner)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failure, CANCELLED);
    assert_eq!(restored.load(Ordering::SeqCst), 1);
    assert_eq!(
        retries.load(Ordering::SeqCst),
        usize::from(!inner_stop_first)
    );
    assert_eq!(
        end,
        if inner_stop_first {
            PreparationEnd::Stopped
        } else {
            PreparationEnd::Deadline
        }
    );
    assert_reaped(&pid).await;
    assert_eq!(
        std::fs::read_to_string(cli.root.join("trace"))
            .unwrap()
            .lines()
            .count(),
        3,
        "an expired preparation cannot reselect or launch a successor CLI"
    );
}

#[tokio::test]
async fn preparation_deadline_retains_cli_join_before_helper_continuation() {
    delayed_preparation_and_cli_reap_case(false).await;
}

#[tokio::test]
async fn preparation_deadline_and_public_stop_retain_already_waiting_cli_join() {
    delayed_preparation_and_cli_reap_case(true).await;
}

#[tokio::test]
async fn selection_observer_retains_shared_discovery_child_join_after_stop() {
    use crate::system_proxy::{StartupCommandControl, StartupReapAck, tests::PrivateCli};
    let cli = PrivateCli::new("echo $$ >> pids; exec /bin/sleep 60");
    let ack = std::sync::Arc::new(StartupReapAck::default());
    let (stop, rx) = watch::channel(false);
    let mut selection_control =
        StartupCommandControl::new(Duration::from_secs(30), Some(rx.clone()));
    selection_control.reap_ack = Some(ack.clone());
    let path = cli.path.clone();
    let task = tokio::spawn(async move {
        let mut control = Control::new(Some(rx));
        let work = async {
            crate::system_proxy::run_bounded_bytes(
                &path,
                &[],
                64 * 1024,
                Duration::from_secs(30),
                &selection_control,
            )
            .await
        };
        control.observe(work).await
    });
    cli.pids(1).await;
    stop.send_replace(true);
    tokio::time::timeout(Duration::from_secs(3), ack.entered.notified())
        .await
        .unwrap();
    assert!(
        !task.is_finished(),
        "observer cannot drop the shared child's active wait"
    );
    ack.release.add_permits(1);
    let result = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err().code(), CANCELLED);
    cli.assert_reaped(1).await;
}

#[tokio::test]
async fn command_timeout_reaps_child_and_closed_cancel_fails_closed() {
    let cli = Cli::new("echo $$ > pid; exec /bin/sleep 60");
    let mut control = Control {
        stop: None,
        deadline: None,
        reap_ack: None,
    };
    let result = control
        .command(&cli.path, &[], Duration::from_millis(150))
        .await;
    assert_eq!(result.unwrap_err().code(), "surge_control_timeout");
    assert_reaped(&cli.started_pid().await).await;
    let (tx, rx) = watch::channel(false);
    drop(tx);
    control.stop = Some(rx);
    assert_eq!(
        control
            .command(Path::new("/does/not/exist"), &[], Duration::from_secs(1))
            .await
            .unwrap_err()
            .code(),
        CANCELLED
    );
}

#[tokio::test]
async fn reselect_policy_error_still_reports_changed_owner() {
    let cli = Cli::new("echo \"$3: on\"");
    let mut calls = 0;
    assert_eq!(
        code(
            select_with(true, None, &cli.path, || {
                calls += 1;
                let first = calls == 1;
                async move {
                    if first {
                        direct()
                    } else {
                        Err(SessionSelectionError::Policy {
                            error: proletarius_vowifi::ProxyPolicyError::Socks5UdpUnavailable,
                            proxy_error: None,
                            proxy_source: Some("macos-socks"),
                        })
                    }
                }
            })
            .await
        ),
        "surge_proxy_owner_changed"
    );
    assert_eq!(calls, 2);
}

#[tokio::test]
async fn cancel_during_feature_probe_never_enables_or_reselects() {
    let cli = Cli::new(
        r#"printf '%s\n' "$*" >> trace
echo $$ > pid
exec /bin/sleep 60"#,
    );
    let (tx, stop) = watch::channel(false);
    let path = cli.path.clone();
    let task = tokio::spawn(async move {
        let mut calls = 0;
        code(
            select_with(true, Some(stop), &path, || {
                calls += 1;
                assert_eq!(calls, 1);
                async { direct() }
            })
            .await,
        )
    });
    let pid = cli.started_pid().await;
    tx.send(true).unwrap();
    assert_eq!(task.await.unwrap(), CANCELLED);
    assert_reaped(&pid).await;
    assert_eq!(
        std::fs::read_to_string(cli.root.join("trace")).unwrap(),
        "feature get system-proxy\n"
    );
}

#[tokio::test(start_paused = true)]
async fn recovery_deadline_reaps_inflight_command() {
    // Deliberately start slower than the recovery deadline. A real child must
    // publish its PID before the test advances the virtual recovery clock.
    let cli = Cli::new("/bin/sleep 0.25; echo $$ > pid; exec /bin/sleep 60");
    let mut control = Control {
        stop: None,
        deadline: Some(Instant::now() + Duration::from_millis(150)),
        reap_ack: None,
    };
    let path = cli.path.clone();
    let command =
        tokio::spawn(async move { control.command(&path, &[], Duration::from_secs(20)).await });
    // Tokio timeout/sleep would auto-advance the paused clock before the OS
    // schedules the shell. Keep a runnable observer and a real wall-clock
    // deadlock guard until the actual process reaches its startup barrier.
    let startup_deadline = std::time::Instant::now() + Duration::from_secs(3);
    let pid = loop {
        if let Ok(pid) = std::fs::read_to_string(cli.root.join("pid")) {
            if !pid.trim().is_empty() {
                break pid.trim().to_owned();
            }
        }
        assert!(
            std::time::Instant::now() < startup_deadline,
            "actual child must publish PID before the wall-clock startup deadline"
        );
        std::thread::sleep(Duration::from_millis(1));
        tokio::task::yield_now().await;
    };
    assert!(
        !command.is_finished(),
        "recovery returned before its deadline"
    );
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid])
            .status()
            .unwrap()
            .success(),
        "startup barrier must identify a live process"
    );
    tokio::time::advance(Duration::from_millis(150)).await;
    assert_eq!(
        command.await.unwrap().unwrap_err().code(),
        "surge_recovery_timeout"
    );
    assert_reaped(&pid).await;
}
