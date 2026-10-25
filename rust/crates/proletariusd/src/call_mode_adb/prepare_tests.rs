use super::*;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

struct FakePreparation {
    events: Vec<&'static str>,
    routes: VecDeque<Result<bool, ProbeError>>,
    deploy_error: Option<ProbeError>,
    rollback_error: Option<ProbeError>,
    final_boot: String,
    alive: Arc<AtomicBool>,
    stale_after: Option<&'static str>,
}
impl FakePreparation {
    fn new(routes: Vec<Result<bool, ProbeError>>) -> Self {
        Self {
            events: Vec::new(),
            routes: routes.into(),
            deploy_error: None,
            rollback_error: None,
            final_boot: "boot-a".into(),
            alive: Arc::new(AtomicBool::new(true)),
            stale_after: None,
        }
    }
    fn event(&mut self, event: &'static str) {
        self.events.push(event);
        if self.stale_after == Some(event) {
            self.alive.store(false, Ordering::SeqCst);
        }
    }
    fn run(&mut self) -> Result<String, PreparationFailure> {
        let alive = self.alive.clone();
        prepare_runtime_pipeline(self, &mut || {
            if alive.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(error(
                    "stale_module_voice_runtime_generation",
                    "generation changed",
                ))
            }
        })
    }
}
impl RuntimePreparationIO for FakePreparation {
    fn deploy(
        &mut self,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<String, ProbeError> {
        current()?;
        self.event("deploy");
        match self.deploy_error.take() {
            Some(error) => Err(error),
            None => Ok("boot-a".into()),
        }
    }
    fn route(
        &mut self,
        action: VoiceRouteAction,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<bool, ProbeError> {
        current()?;
        self.event(match action {
            VoiceRouteAction::Start => "start",
            VoiceRouteAction::Stop => "stop",
            VoiceRouteAction::Status => "status",
        });
        self.routes.pop_front().expect("unexpected route action")
    }
    fn rollback(
        &mut self,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<(), ProbeError> {
        current()?;
        self.event("rollback");
        self.rollback_error.take().map_or(Ok(()), Err)
    }
    fn boot_id(
        &mut self,
        current: &mut dyn FnMut() -> Result<(), ProbeError>,
    ) -> Result<String, ProbeError> {
        current()?;
        self.event("boot");
        Ok(self.final_boot.clone())
    }
}
fn error(code: &'static str, message: &str) -> ProbeError {
    ProbeError::new(code, message)
}

#[test]
fn no_at_preparation_keeps_selection_deferred_and_cannot_claim_a_selected_target() {
    let preparation = NativeRuntimePreparation::new(Path::new("unused-no-at-fixture"), None)
        .expect("no-AT construction performs no USB selection or filesystem access");
    assert!(preparation.plan.is_none());
    assert_eq!(
        preparation.selected_plan().err().unwrap().code,
        "module_adb_target_unconfirmed",
    );
}

#[test]
fn fenced_adb_checks_identity_before_and_after_every_actual_command() {
    struct Shell {
        calls: usize,
        alive: Arc<AtomicBool>,
    }
    impl ModuleVoiceADB for Shell {
        fn shell_checked_timeout(
            &mut self,
            _: &str,
            _: Duration,
        ) -> Result<(Vec<u8>, u32), ProbeError> {
            self.calls += 1;
            self.alive.store(false, Ordering::SeqCst);
            Ok((Vec::new(), 0))
        }
    }
    let alive = Arc::new(AtomicBool::new(true));
    let mut shell = Shell {
        calls: 0,
        alive: alive.clone(),
    };
    let mut current = || {
        if alive.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(error("stale_backend_generation", "producer changed"))
        }
    };
    {
        let mut adb = FencedModuleVoiceADB {
            adb: &mut shell,
            current: &mut current,
        };
        assert_eq!(
            adb.shell_checked_timeout("first", Duration::ZERO)
                .unwrap_err()
                .code,
            "stale_backend_generation"
        );
        assert_eq!(
            adb.shell_checked_timeout("second", Duration::ZERO)
                .unwrap_err()
                .code,
            "stale_backend_generation"
        );
    }
    assert_eq!(
        shell.calls, 1,
        "stale producer must not send another command"
    );
}

#[test]
fn admitted_prepare_invalidates_only_its_current_runtime_generation() {
    let mut readiness = VoiceRuntimeReadiness::default();
    let generation = readiness.advance_next("producer-1").unwrap();
    readiness
        .mark_prepared("producer-1", generation, "boot-a".into())
        .unwrap();
    assert!(readiness.is_prepared("producer-1", generation, "boot-a"));
    assert!(
        readiness
            .invalidate_prepared("producer-2", generation)
            .is_err()
    );
    assert!(readiness.is_prepared("producer-1", generation, "boot-a"));
    readiness
        .invalidate_prepared("producer-1", generation)
        .unwrap();
    assert!(!readiness.is_prepared("producer-1", generation, "boot-a"));
}

#[test]
fn prepare_deploys_once_and_confirms_complete_self_test_before_success() {
    let mut io = FakePreparation::new(vec![Ok(true), Ok(false)]);
    assert_eq!(io.run().unwrap(), "boot-a");
    assert_eq!(io.events, ["deploy", "start", "stop", "boot"]);
}

#[test]
fn prepare_failure_never_starts_route_and_preserves_phase_and_original_code() {
    let mut io = FakePreparation::new(vec![]);
    io.deploy_error = Some(error("module_voice_kernel_mismatch", "wrong kernel"));
    let failure = io.run().unwrap_err();
    assert_eq!(failure.phase, PreparationPhase::Prepare);
    assert_eq!(failure.error.code, "module_voice_kernel_mismatch");
    assert_eq!(io.events, ["deploy"]);
}

#[test]
fn lost_start_reply_requires_positive_status_before_stop() {
    let mut io = FakePreparation::new(vec![
        Err(error("start_failed", "lost start")),
        Ok(true),
        Ok(false),
    ]);
    assert!(io.run().is_ok());
    assert_eq!(io.events, ["deploy", "start", "status", "stop", "boot"]);
}

#[test]
fn lost_stop_reply_requires_negative_status_and_independent_audio_rollback() {
    let mut io = FakePreparation::new(vec![
        Ok(true),
        Err(error("stop_failed", "lost stop")),
        Ok(false),
    ]);
    assert!(io.run().is_ok());
    assert_eq!(
        io.events,
        ["deploy", "start", "stop", "status", "rollback", "boot"]
    );
}

#[test]
fn negative_helper_status_never_hides_failed_audio_rollback() {
    let mut io = FakePreparation::new(vec![
        Ok(true),
        Err(error("stop_failed", "lost stop")),
        Ok(false),
    ]);
    io.rollback_error = Some(error(
        "module_voice_route_cleanup_unconfirmed",
        "audio_enable and T/T/B remain unknown",
    ));
    let failure = io.run().unwrap_err();
    assert_eq!(failure.phase, PreparationPhase::SelfTest);
    assert_eq!(failure.error.code, "module_voice_route_cleanup_unconfirmed");
    assert!(
        failure
            .error
            .message
            .starts_with("语音路由已启动，但安全回滚未通过：")
    );
    assert_eq!(io.events, ["deploy", "start", "stop", "status", "rollback"]);
}

#[test]
fn start_or_stop_and_status_errors_are_both_reported_without_replay() {
    for stop in [false, true] {
        let mut routes = Vec::new();
        if stop {
            routes.push(Ok(true));
        }
        routes.push(Err(error("original_action", "action reply lost")));
        routes.push(Err(error("status_error", "status reply lost")));
        let mut io = FakePreparation::new(routes);
        let failure = io.run().unwrap_err();
        assert_eq!(failure.phase, PreparationPhase::SelfTest);
        assert_eq!(failure.error.code, "original_action");
        assert!(failure.error.message.contains("action reply lost"));
        assert!(failure.error.message.contains("status reply lost"));
        assert_eq!(
            io.events.iter().filter(|event| **event == "deploy").count(),
            1
        );
        assert!(!io.events.contains(&"boot"));
        if stop {
            assert!(
                failure
                    .error
                    .message
                    .starts_with("语音路由已启动，但安全回滚未通过：")
            );
        }
    }
}

#[test]
fn opposite_status_does_not_confirm_start_or_stop() {
    for stop in [false, true] {
        let mut routes = Vec::new();
        if stop {
            routes.push(Ok(true));
        }
        routes.push(Err(error("original_action", "not acknowledged")));
        routes.push(Ok(stop));
        let mut io = FakePreparation::new(routes);
        assert_eq!(io.run().unwrap_err().error.code, "original_action");
        assert!(!io.events.contains(&"rollback"));
        assert!(!io.events.contains(&"boot"));
    }
}

#[test]
fn changed_generation_blocks_each_later_stage() {
    for (stage, expected) in [
        ("deploy", vec!["deploy"]),
        ("start", vec!["deploy", "start"]),
        ("stop", vec!["deploy", "start", "stop"]),
        ("boot", vec!["deploy", "start", "stop", "boot"]),
    ] {
        let mut io = FakePreparation::new(vec![Ok(true), Ok(false)]);
        io.stale_after = Some(stage);
        let failure = io.run().unwrap_err();
        assert_eq!(failure.error.code, "stale_module_voice_runtime_generation");
        assert_eq!(io.events, expected);
    }
}

#[test]
fn changed_boot_after_successful_route_cycle_is_not_prepared() {
    let mut io = FakePreparation::new(vec![Ok(true), Ok(false)]);
    io.final_boot = "boot-b".into();
    let failure = io.run().unwrap_err();
    assert_eq!(failure.phase, PreparationPhase::SelfTest);
    assert_eq!(failure.error.code, "module_voice_boot_id_changed");
}

#[tokio::test]
async fn cleanup_failures_and_stale_commit_never_publish_prepared() {
    let prepared = AtomicBool::new(false);
    let failure = finish_runtime_preparation(
        Ok("boot-a".into()),
        async { Err("USB still closing".into()) },
        |_| {
            prepared.store(true, Ordering::SeqCst);
            Ok(())
        },
    )
    .await
    .unwrap_err();
    assert_eq!(failure.phase, PreparationPhase::SelfTest);
    assert_eq!(failure.error.code, "module_voice_route_cleanup_unconfirmed");
    assert!(!prepared.load(Ordering::SeqCst));
    let failure = finish_runtime_preparation(Ok("boot-a".into()), async { Ok(()) }, |_| {
        Err(error(
            "stale_backend_generation",
            "producer replaced during cleanup",
        ))
    })
    .await
    .unwrap_err();
    assert_eq!(failure.error.code, "stale_backend_generation");
    assert_eq!(failure.phase, PreparationPhase::SelfTest);
}

#[tokio::test]
async fn failed_worker_still_waits_for_cleanup_and_preserves_both_errors() {
    let cleaned = AtomicBool::new(false);
    let failure = finish_runtime_preparation(
        Err(PreparationFailure::at(
            PreparationPhase::Prepare,
            error("module_adb_worker_failed", "worker panic"),
        )),
        async {
            cleaned.store(true, Ordering::SeqCst);
            Err("cleanup failed".into())
        },
        |_| panic!("failed worker must not commit"),
    )
    .await
    .unwrap_err();
    assert!(cleaned.load(Ordering::SeqCst));
    assert_eq!(failure.phase, PreparationPhase::Prepare);
    assert!(failure.error.message.contains("worker panic"));
    assert!(failure.error.message.contains("cleanup failed"));
}

#[tokio::test]
async fn canceled_prepare_observer_keeps_real_lock_and_adb_permit_through_cleanup_and_commit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime/prepare-owner-tests")
        .join(crate::snapshot::new_instance_id());
    std::fs::create_dir_all(&root).unwrap();
    let lock = crate::call_mode_backups::try_call_mode_operation_file_lock(&root).unwrap();
    let permit = ADB_ADMISSION.acquire().await.unwrap();
    let (finish_work, worked) = tokio::sync::oneshot::channel::<()>();
    let (finish_cleanup, cleaned) = tokio::sync::oneshot::channel::<()>();
    let prepared = Arc::new(AtomicBool::new(false));
    let committed = prepared.clone();
    let task = spawn_guarded_voice_task(Some(VoiceRouteGuard::shared(lock)), async move {
        let _permit = permit;
        worked.await.unwrap();
        let result = FakePreparation::new(vec![Ok(true), Ok(false)]).run();
        finish_runtime_preparation(
            result,
            async {
                cleaned.await.unwrap();
                Ok(())
            },
            |_| {
                committed.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap();
    });
    drop(task);
    assert!(ADB_ADMISSION.try_acquire().is_err());
    assert_eq!(
        crate::call_mode_backups::try_call_mode_operation_shared_lock(&root)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    finish_work.send(()).unwrap();
    tokio::task::yield_now().await;
    assert!(!prepared.load(Ordering::SeqCst));
    assert!(ADB_ADMISSION.try_acquire().is_err());
    assert_eq!(
        crate::call_mode_backups::try_call_mode_operation_shared_lock(&root)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    finish_cleanup.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if prepared.load(Ordering::SeqCst)
                && ADB_ADMISSION.available_permits() == 1
                && crate::call_mode_backups::try_call_mode_operation_shared_lock(&root).is_ok()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let _ = std::fs::remove_dir_all(root);
}
