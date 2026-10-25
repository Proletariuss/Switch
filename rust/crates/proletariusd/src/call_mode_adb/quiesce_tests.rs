use super::*;
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

struct FakeADB {
    replies: VecDeque<Result<(Vec<u8>, u32), ProbeError>>,
    commands: Arc<Mutex<Vec<String>>>,
    alive: Arc<AtomicBool>,
    retire_after: Option<usize>,
}
impl FakeADB {
    fn new(replies: Vec<Result<(Vec<u8>, u32), ProbeError>>) -> Self {
        Self {
            replies: replies.into(),
            commands: Arc::new(Mutex::new(vec![])),
            alive: Arc::new(AtomicBool::new(true)),
            retire_after: None,
        }
    }
    fn run(&mut self, policy: StopPolicy) -> Result<bool, RouteExecutionFailure> {
        let alive = self.alive.clone();
        module_voice_route_with_adb(self, VoiceRouteAction::Stop, policy, &mut || {
            if alive.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(ProbeError::new(
                    "stale_backend_generation",
                    "producer retired",
                ))
            }
        })
    }
}
impl ModuleVoiceADB for FakeADB {
    fn shell_checked_timeout(
        &mut self,
        command: &str,
        _: Duration,
    ) -> Result<(Vec<u8>, u32), ProbeError> {
        let mut commands = self.commands.lock().unwrap();
        commands.push(command.into());
        if self.retire_after == Some(commands.len()) {
            self.alive.store(false, Ordering::SeqCst);
        }
        self.replies
            .pop_front()
            .expect("unexpected actual ADB shell")
    }
}
fn root() -> Result<(Vec<u8>, u32), ProbeError> {
    Ok((b"0\n".to_vec(), 0))
}
fn exit(status: u32) -> Result<(Vec<u8>, u32), ProbeError> {
    Ok((vec![], status))
}
fn lost() -> Result<(Vec<u8>, u32), ProbeError> {
    Err(ProbeError::new(
        "module_voice_route_timeout",
        "original stop reply lost",
    ))
}

#[test]
fn stop_policy_boundary_rejects_null_unknown_and_other_actions() {
    for value in ["null", "\"unknown\"", "5", "true"] {
        let body = format!(r#"{{"producer_id":"producer-1","stop_policy":{value}}}"#);
        assert!(serde_json::from_str::<ProbeRequest>(&body).is_err());
    }
    let missing: ProbeRequest = serde_json::from_str(r#"{"producer_id":"producer-1"}"#).unwrap();
    assert_eq!(
        validate_stop_policy(&missing, Some(VoiceRouteAction::Stop)).unwrap(),
        StopPolicy::Always
    );
    for name in ["always", "hangup", "shutdown", "restore"] {
        let body = format!(r#"{{"producer_id":"producer-1","stop_policy":"{name}"}}"#);
        let input: ProbeRequest = serde_json::from_str(&body).unwrap();
        assert!(validate_stop_policy(&input, Some(VoiceRouteAction::Stop)).is_ok());
        for other in [
            None,
            Some(VoiceRouteAction::Start),
            Some(VoiceRouteAction::Status),
        ] {
            assert!(validate_stop_policy(&input, other).is_err());
        }
    }
}

#[test]
fn inactive_policy_preflight_issues_no_stop_or_rollback_shell() {
    for policy in [
        StopPolicy::Hangup,
        StopPolicy::Shutdown,
        StopPolicy::Restore,
    ] {
        let mut adb = FakeADB::new(vec![root(), exit(1), exit(0)]);
        assert_eq!(adb.run(policy).unwrap(), false);
        assert_eq!(adb.commands.lock().unwrap().len(), 3);
    }
}

#[test]
fn active_policies_execute_real_helper_stop_and_audio_rollback() {
    for policy in [
        StopPolicy::Hangup,
        StopPolicy::Shutdown,
        StopPolicy::Restore,
    ] {
        let mut adb = FakeADB::new(vec![root(), exit(0), exit(0), exit(0), exit(0)]);
        assert_eq!(adb.run(policy).unwrap(), false);
        let commands = adb.commands.lock().unwrap();
        assert!(commands[2].contains("kill -TERM"));
        assert!(commands[4].contains("audio_enable"));
        assert!(commands[4].contains("printf 'T\\n'"));
        assert!(commands[4].contains("printf 'B\\n'"));
    }
}

#[test]
fn preflight_failure_obeys_hangup_shutdown_and_restore_contract() {
    for policy in [
        StopPolicy::Hangup,
        StopPolicy::Shutdown,
        StopPolicy::Restore,
    ] {
        let mut adb = FakeADB::new(vec![root(), lost(), root(), exit(0), exit(0), exit(0)]);
        let result = adb.run(policy);
        match policy {
            StopPolicy::Hangup => assert!(!result.unwrap()),
            StopPolicy::Shutdown | StopPolicy::Restore => {
                let error = result.unwrap_err();
                assert_eq!(error.stop_phase, Some(StopPhase::Status));
                assert_eq!(error.stop_confirmed, policy == StopPolicy::Shutdown);
                assert_eq!(error.failure.code, "module_voice_route_timeout");
                assert_eq!(error.failure.status, StatusCode::GATEWAY_TIMEOUT);
                assert!(error.failure.message.contains("original stop reply lost"));
            }
            _ => unreachable!(),
        }
        assert_eq!(
            adb.commands.lock().unwrap().len(),
            if policy == StopPolicy::Restore { 2 } else { 6 }
        );
    }
}

#[test]
fn lost_stop_reply_reconciles_only_after_actual_audio_cleanup() {
    let mut adb = FakeADB::new(vec![root(), lost(), exit(0), exit(0), exit(1), exit(0)]);
    assert!(!adb.run(StopPolicy::Always).unwrap());
    let commands = adb.commands.lock().unwrap();
    assert!(commands[1].contains("kill -TERM"));
    assert!(commands[2].contains("owned=0"));
    assert!(commands[3].contains("audio_enable"));
    assert!(commands[4].contains("RUNNING"));
    assert!(commands[5].contains("owned=0"));
}

#[test]
fn lost_stop_reply_preserves_original_failure_if_rollback_or_status_fails() {
    let mut rollback = FakeADB::new(vec![
        root(),
        lost(),
        exit(0),
        exit(1),
        exit(1),
        exit(1),
        exit(1),
        exit(1),
    ]);
    let error = rollback.run(StopPolicy::Always).unwrap_err();
    assert_eq!(error.failure.code, "module_voice_route_timeout");
    assert_eq!(error.failure.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(error.stop_phase, Some(StopPhase::Stop));
    assert!(error.failure.message.contains("音频回滚未确认"));
    assert_eq!(rollback.commands.lock().unwrap().len(), 8);
    for final_status in [exit(0), lost()] {
        let mut unknown = FakeADB::new(vec![root(), lost(), exit(0), exit(0), final_status]);
        let error = unknown.run(StopPolicy::Always).unwrap_err();
        assert_eq!(error.failure.code, "module_voice_route_timeout");
        assert!(error.failure.message.contains("original stop reply lost"));
        assert_eq!(unknown.commands.lock().unwrap().len(), 5);
    }
}

#[test]
fn stale_producer_after_each_actual_shell_prevents_further_commands() {
    for command in 1..=6 {
        let mut adb = FakeADB::new(vec![root(), lost(), exit(0), exit(0), exit(1), exit(0)]);
        adb.retire_after = Some(command);
        let error = adb.run(StopPolicy::Always).unwrap_err();
        assert_eq!(error.failure.code, "stale_backend_generation");
        assert_eq!(error.failure.status, StatusCode::CONFLICT);
        assert_eq!(adb.commands.lock().unwrap().len(), command);
    }
}

struct PhysicalGuard(Arc<AtomicBool>);
impl Drop for PhysicalGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn inactive_policy_skips_usb_acquisition_and_release_in_real_executor() {
    for policy in [
        StopPolicy::Hangup,
        StopPolicy::Shutdown,
        StopPolicy::Restore,
    ] {
        let admission = Arc::new(Semaphore::new(1));
        let permit = admission.clone().acquire_owned().await.unwrap();
        let result = run_voice_route_pipeline(
            permit,
            move || {
                let mut adb = FakeADB::new(vec![root(), exit(1), exit(0)]);
                let preflight = quiesce_preflight(&mut adb, policy).unwrap();
                Ok(((), adb, !matches!(preflight, StopPreflight::Inactive)))
            },
            || async {
                panic!("inactive route must not acquire USB lease");
                #[allow(unreachable_code)]
                Ok::<(), RouteExecutionFailure>(())
            },
            |_| panic!("inactive route must not run stop"),
            |_| async {
                panic!("inactive route must not close USB");
                #[allow(unreachable_code)]
                Ok(())
            },
            Some(StopPhase::Stop),
        )
        .await;
        assert!(!result.unwrap());
        assert!(admission.try_acquire().is_ok());
    }
}

#[tokio::test]
async fn cancellation_retains_admission_and_both_guards_until_usb_cleanup_settles() {
    let data_root = std::path::PathBuf::from(std::env::var("SWITCH_DATA_DIR").unwrap())
        .join(format!("quiesce-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&data_root).unwrap();
    let operation =
        crate::call_mode_backups::try_call_mode_operation_shared_lock(&data_root).unwrap();
    let radio = crate::module_radio::Service::new(
        data_root.clone(),
        crate::usb_at::Owner::new("quiesce-producer".into()),
        crate::modem_actor::ModemActor::new().unwrap(),
        crate::availability::BackendAvailability::new(),
        "quiesce-producer".into(),
    );
    let mut voice_guard = VoiceRouteGuard::shared(operation);
    voice_guard._radio_shared = Some(
        radio
            .borrow_shared_voice_operation("quiesce-producer", true)
            .unwrap(),
    );
    let admission = Arc::new(Semaphore::new(1));
    let permit = admission.clone().acquire_owned().await.unwrap();
    let physical_alive = Arc::new(AtomicBool::new(true));
    let acquire_started = Arc::new(tokio::sync::Notify::new());
    let acquire_continue = Arc::new(tokio::sync::Notify::new());
    let cleanup_started = Arc::new(tokio::sync::Notify::new());
    let cleanup_continue = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(tokio::sync::Notify::new());
    let acquire_notice = acquire_started.notified();
    let cleanup_notice = cleanup_started.notified();
    let completion = completed.notified();
    let alive = physical_alive.clone();
    let started = acquire_started.clone();
    let resume = acquire_continue.clone();
    let cleaning = cleanup_started.clone();
    let release = cleanup_continue.clone();
    let done = completed.clone();
    let owner = spawn_guarded_voice_task(Some(voice_guard), async move {
        let result = run_voice_route_pipeline(
            permit,
            move || {
                Ok((
                    PhysicalGuard(alive),
                    FakeADB::new(vec![root(), exit(0), exit(0), exit(0)]),
                    true,
                ))
            },
            || async move {
                started.notify_one();
                resume.notified().await;
                Ok(())
            },
            |mut adb| adb.run(StopPolicy::Always),
            |_| async move {
                cleaning.notify_one();
                release.notified().await;
                Ok(())
            },
            Some(StopPhase::Stop),
        )
        .await;
        done.notify_one();
        result
    });
    let waiter = tokio::spawn(async move { owner.await });
    tokio::time::timeout(Duration::from_secs(3), acquire_notice)
        .await
        .unwrap();
    waiter.abort();
    let retiring_radio = radio.clone();
    let retirement = tokio::spawn(async move {
        retiring_radio
            .retire_producer("quiesce-next-producer".into())
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let code = radio
                .borrow_shared_voice_operation("quiesce-producer", true)
                .err()
                .unwrap()
                .code;
            if code == "radio_stale_generation" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!retirement.is_finished());
    assert!(admission.try_acquire().is_err());
    assert!(physical_alive.load(Ordering::SeqCst));
    assert!(crate::call_mode_backups::try_call_mode_operation_file_lock(&data_root).is_err());
    acquire_continue.notify_one();
    tokio::time::timeout(Duration::from_secs(3), cleanup_notice)
        .await
        .unwrap();
    assert!(!retirement.is_finished());
    assert!(admission.try_acquire().is_err());
    assert!(physical_alive.load(Ordering::SeqCst));
    assert!(crate::call_mode_backups::try_call_mode_operation_file_lock(&data_root).is_err());
    cleanup_continue.notify_one();
    tokio::time::timeout(Duration::from_secs(3), completion)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), retirement)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::task::yield_now().await;
    assert!(admission.try_acquire().is_ok());
    assert!(!physical_alive.load(Ordering::SeqCst));
    // The radio pin drops after the shared File, so retirement above proves
    // owner cleanup. Parallel process-spawn tests can briefly retain an
    // inherited flock descriptor until exec; wait only for Busy, never hide I/O
    // errors or a lock that remains held beyond the bounded completion check.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match crate::call_mode_backups::try_call_mode_operation_file_lock(&data_root) {
                Ok(lock) => {
                    drop(lock);
                    break;
                }
                Err(crate::call_mode_backups::OperationFileLockError::Busy) => {
                    tokio::task::yield_now().await;
                }
                Err(error) => {
                    panic!("operation lock failed after real radio retirement: {error:?}")
                }
            }
        }
    })
    .await
    .expect("operation lock remained busy after real radio retirement");
    std::fs::remove_dir_all(&data_root).unwrap();
}

#[tokio::test]
async fn blocking_worker_panic_still_physically_releases_before_admission() {
    let admission = Arc::new(Semaphore::new(1));
    let permit = admission.clone().acquire_owned().await.unwrap();
    let cleanup_admission = admission.clone();
    let result = run_voice_route_pipeline(
        permit,
        || Ok(((), (), true)),
        || async { Ok(()) },
        |_| panic!("fake ADB worker panic"),
        |_| async move {
            assert!(cleanup_admission.try_acquire().is_err());
            Err("physical transport close failed".into())
        },
        Some(StopPhase::Stop),
    )
    .await;
    let error = result.unwrap_err();
    assert_eq!(error.failure.code, "module_adb_worker_failed");
    assert_eq!(error.stop_phase, Some(StopPhase::Stop));
    assert!(
        error
            .failure
            .message
            .contains("physical transport close failed")
    );
    assert!(admission.try_acquire().is_ok());
}

#[test]
fn response_stop_phase_exists_only_on_failure() {
    let failure = RouteExecutionFailure::probe(
        ProbeError::new("module_voice_route_timeout", "lost"),
        Some(StopPhase::Status),
    );
    let response = voice_route_failure_response(failure);
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    // Serialization is shared by real HTTP responses.
    let success = serde_json::to_value(ProbeResponse {
        ready: true,
        code: "module_voice_route_stopped",
        error: String::new(),
        active: Some(false),
        prepared: None,
        failure_phase: None,
        stop_phase: None,
    })
    .unwrap();
    assert!(success.get("stop_phase").is_none());
}

#[tokio::test]
async fn final_receipt_keeps_confirmed_stop_diagnostic_only_after_usb_release() {
    let (release, released) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let result = run_voice_route_worker(
            (),
            || {
                let mut adb = FakeADB::new(vec![root(), lost(), root(), exit(0), exit(0), exit(0)]);
                let result = adb.run(StopPolicy::Shutdown);
                assert_eq!(adb.commands.lock().unwrap().len(), 6);
                result
            },
            async move { released.await.unwrap() },
            Some(StopPhase::Stop),
        )
        .await;
        shutdown_route_receipt(result)
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    release.send(Ok(())).unwrap();
    let errors = task.await.unwrap().unwrap();
    assert_eq!(errors, vec!["original stop reply lost"]);
}

#[tokio::test]
async fn failed_usb_release_revokes_otherwise_confirmed_stop_receipt() {
    let result = run_voice_route_worker(
        (),
        || {
            FakeADB::new(vec![root(), lost(), root(), exit(0), exit(0), exit(0)])
                .run(StopPolicy::Shutdown)
        },
        async { Err("injected USB release failure".into()) },
        Some(StopPhase::Stop),
    )
    .await;
    assert!(!result.as_ref().unwrap_err().stop_confirmed);
    let error = shutdown_route_receipt(result).unwrap_err();
    assert!(error.contains("original stop reply lost"));
    assert!(error.contains("injected USB release failure"));
}

#[test]
fn final_receipt_rejects_unknown_stop_and_stale_generation() {
    let mut unknown = FakeADB::new(vec![root(), lost(), exit(0), exit(0), lost()]);
    let error = unknown.run(StopPolicy::Always).unwrap_err();
    assert!(!error.stop_confirmed);
    assert!(shutdown_route_receipt(Err(error)).is_err());
    let mut stale = FakeADB::new(vec![root(), lost(), root(), exit(0), exit(0), exit(0)]);
    stale.retire_after = Some(6);
    let error = stale.run(StopPolicy::Shutdown).unwrap_err();
    assert_eq!(error.failure.code, "stale_backend_generation");
    assert!(!error.stop_confirmed);
    assert!(shutdown_route_receipt(Err(error)).is_err());
}

#[tokio::test]
async fn final_stop_waits_for_cancelled_direct_worker_and_its_usb_cleanup() {
    let data_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime")
        .join(format!(
            "final-direct-voice-{}",
            crate::snapshot::new_instance_id()
        ));
    std::fs::create_dir_all(&data_root).unwrap();
    let modem = crate::modem_actor::ModemActor::new().unwrap();
    let radio = crate::module_radio::Service::new(
        data_root.clone(),
        crate::usb_at::Owner::unavailable_for_tests("final-direct".into()),
        modem.clone(),
        crate::availability::BackendAvailability::new(),
        "final-direct".into(),
    );
    let guard = VoiceRouteGuard {
        _shared: None,
        _call_mode: None,
        _radio_shared: Some(
            radio
                .borrow_shared_voice_operation("final-direct", false)
                .unwrap(),
        ),
    };
    let admission = Arc::new(Semaphore::new(1));
    let permit = admission.clone().acquire_owned().await.unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release_work, work_wait) = std::sync::mpsc::channel();
    let (cleaning, cleanup_started) = tokio::sync::oneshot::channel();
    let (release_cleanup, cleanup_wait) = tokio::sync::oneshot::channel();
    let direct = spawn_guarded_voice_task(Some(guard), async move {
        run_voice_route_worker(
            permit,
            move || {
                started.send(()).unwrap();
                work_wait.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(true) // The already accepted direct Start completes after its waiter is gone.
            },
            async move {
                cleaning.send(()).unwrap();
                cleanup_wait.await.unwrap();
                Ok(())
            },
            None,
        )
        .await
    });
    let waiter = tokio::spawn(async move { direct.await });
    ready.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    radio.begin_shutdown();
    assert!(
        radio
            .borrow_shared_voice_operation("final-direct", true)
            .is_err()
    );
    let closing_radio = radio.clone();
    let closing_admission = admission.clone();
    let mut final_stop = tokio::spawn(async move {
        let _pin = closing_radio
            .borrow_shared_voice_shutdown("final-direct")
            .await
            .unwrap();
        let permit = VoiceAdmission::FinalShutdown
            .acquire_adb(&closing_admission)
            .await
            .unwrap();
        let adb = FakeADB::new(vec![root(), exit(0), exit(0), exit(0), exit(0)]);
        let commands = adb.commands.clone();
        let result = run_voice_route_worker(
            permit,
            move || {
                let mut adb = adb;
                adb.run(StopPolicy::Shutdown)
            },
            async { Ok(()) },
            Some(StopPhase::Stop),
        )
        .await;
        assert_eq!(commands.lock().unwrap().len(), 5);
        shutdown_route_receipt(result)
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut final_stop)
            .await
            .is_err()
    );
    release_work.send(()).unwrap();
    cleanup_started.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut final_stop)
            .await
            .is_err()
    );
    release_cleanup.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), final_stop)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert_eq!(admission.available_permits(), 1);
    modem.shutdown().unwrap();
    std::fs::remove_dir_all(data_root).unwrap();
}

#[tokio::test]
async fn final_adb_admission_waits_while_request_admission_remains_immediate() {
    let admission = Semaphore::new(1);
    let occupied = admission.acquire().await.unwrap();
    assert!(
        VoiceAdmission::Immediate
            .acquire_adb(&admission)
            .await
            .is_err()
    );
    let mut final_admission = Box::pin(VoiceAdmission::FinalShutdown.acquire_adb(&admission));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut final_admission)
            .await
            .is_err()
    );
    drop(occupied);
    drop(final_admission.await.unwrap());
    assert_eq!(admission.available_permits(), 1);
}

#[tokio::test]
async fn final_file_admission_waits_for_exclusive_preparation_without_replaying_work() {
    let data_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime")
        .join(format!(
            "final-voice-lock-{}",
            crate::snapshot::new_instance_id()
        ));
    std::fs::create_dir_all(&data_root).unwrap();
    let exclusive =
        crate::call_mode_backups::try_call_mode_operation_file_lock(&data_root).unwrap();
    assert!(voice_route_guard(&data_root, VoiceRouteAction::Stop, None).is_err());
    let mut final_guard = Box::pin(final_voice_route_guard(&data_root));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut final_guard)
            .await
            .is_err()
    );
    drop(exclusive);
    drop(
        tokio::time::timeout(Duration::from_secs(3), final_guard)
            .await
            .unwrap()
            .unwrap(),
    );
    std::fs::remove_dir_all(data_root).unwrap();
}

#[test]
fn retained_obligation_inactive_status_still_runs_real_stop_audio_cleanup() {
    for policy in [
        StopPolicy::Hangup,
        StopPolicy::Shutdown,
        StopPolicy::Restore,
    ] {
        let mut adb = FakeADB::new(vec![root(), exit(1), exit(0), exit(0), exit(0), exit(0)]);
        let preflight = quiesce_preflight(&mut adb, policy).unwrap();
        assert!(matches!(preflight, StopPreflight::Inactive));
        finish_quiesce(&mut adb, policy, obligation_stop_preflight(preflight)).unwrap();
        let commands = adb.commands.lock().unwrap();
        assert!(
            commands
                .iter()
                .any(|command| command.contains("kill -TERM"))
        );
        assert!(commands.last().unwrap().contains("audio_enable"));
        assert_eq!(commands.len(), 6);
    }
}
