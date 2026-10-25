//! Local registry and scripted ADB wire only. These fixtures prove original
//! plan selection and executor ordering, never native AT/ADB physical pairing.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

fn inspect(
    ownership: &Arc<Mutex<VoiceRouteOwnership>>,
    producer: &str,
) -> Result<Option<VoiceCleanupObligation>, ProbeError> {
    restore_voice_cleanup_obligation(&ownership.lock().unwrap(), producer)
}

// Exercise the same preflight, bounded physical worker and USB release
// pipeline used by execute_voice_route_action_with_admission. Only the wire
// transport and lease release are fake; no physical target is manufactured.
async fn settle_scripted(
    permit: tokio::sync::SemaphorePermit<'_>,
    obligation: VoiceCleanupObligation,
    ownership: Arc<Mutex<VoiceRouteOwnership>>,
    release: impl std::future::Future<Output = Result<(), String>>,
    panic_worker: bool,
) -> Result<bool, RouteExecutionFailure> {
    let _permit = permit;
    let plan = obligation.target.plan.clone();
    let result = run_voice_route_pipeline(
        (),
        move || {
            let mut session = selected_session(plan);
            let preflight = obligation_stop_preflight(
                quiesce_preflight(&mut session, StopPolicy::Restore)
                    .map_err(RouteExecutionFailure::from_stop)?,
            );
            Ok(((), (session, preflight), true))
        },
        || async { Ok(()) },
        move |(mut session, preflight)| {
            assert!(!panic_worker, "scripted worker panic after lease admission");
            finish_quiesce(&mut session, StopPolicy::Restore, preflight)
                .map(|()| false)
                .map_err(RouteExecutionFailure::from_stop)
        },
        |()| release,
        Some(StopPhase::Status),
    )
    .await;
    let confirmed =
        matches!(result, Ok(false)) || result.as_ref().is_err_and(|error| error.stop_confirmed);
    ownership.lock().unwrap().retire(&obligation, confirmed);
    result
}

#[tokio::test]
async fn restore_empty_or_prepared_only_registry_never_dispatches_factory_or_stop() {
    let gate = crate::module_recovery::Gate::default();
    gate.revoke_for_tests();
    let (plan, trace) = selected_plan(58101);
    let mut readiness = VoiceRuntimeReadiness::default();
    readiness.advance_next("producer-a").unwrap();
    readiness
        .mark_prepared("producer-a", 1, "boot-a".into())
        .unwrap();
    readiness.prepared_target = Some(VoiceBoundTarget {
        plan,
        capture: None,
        managed: None,
    });
    let ownership = Arc::new(Mutex::new(VoiceRouteOwnership::default()));
    let admission = Semaphore::new(1);
    finish_restore_voice_cleanup(
        &admission,
        || inspect(&ownership, "producer-a"),
        |_, _| async { panic!("empty registry must not borrow radio, capture AT or select ADB") },
    )
    .await
    .unwrap();
    assert!(gate.is_admission_revoked());
    assert!(readiness.prepared_target.is_some());
    assert!(trace.lock().unwrap().opened.is_empty());
    assert_eq!(admission.available_permits(), 1);
}

#[tokio::test]
async fn restore_settles_original_plan_and_waits_for_actual_usb_release() {
    let (plan, trace) = selected_plan(58102);
    let original = cleanup_fixture("producer-a", 7, plan);
    let authority = Arc::downgrade(&original.authority);
    let expected_plan = original.target.plan.clone();
    let ownership = Arc::new(Mutex::new(VoiceRouteOwnership::default()));
    ownership.lock().unwrap().begin(original).unwrap();
    let admission = Arc::new(Semaphore::new(1));
    let task_admission = admission.clone();
    let task_ownership = ownership.clone();
    let release_ownership = ownership.clone();
    let release_admission = admission.clone();
    let (release_reached, reached) = tokio::sync::oneshot::channel();
    let (allow_release, release) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
        finish_restore_voice_cleanup(
            &task_admission,
            || inspect(&task_ownership, "producer-a"),
            |permit, obligation| async move {
                assert_eq!(obligation.generation, 7);
                assert!(Arc::ptr_eq(&obligation.target.plan.0, &expected_plan.0));
                settle_scripted(
                    permit,
                    obligation,
                    release_ownership,
                    async move {
                        assert_eq!(release_admission.available_permits(), 0);
                        release_reached.send(()).unwrap();
                        release.await.unwrap();
                        Ok(())
                    },
                    false,
                )
                .await
            },
        )
        .await
    });
    reached.await.unwrap();
    assert!(!worker.is_finished());
    assert_eq!(ownership.lock().unwrap().obligations.len(), 1);
    assert_eq!(admission.available_permits(), 0);
    allow_release.send(()).unwrap();
    worker.await.unwrap().unwrap();
    assert!(ownership.lock().unwrap().obligations.is_empty());
    assert!(authority.upgrade().is_none());
    assert_eq!(admission.available_permits(), 1);
    let trace = trace.lock().unwrap();
    assert!(trace.opened.iter().all(|id| *id == 58102));
    assert!(
        trace
            .commands
            .iter()
            .any(|(_, command)| command.contains("kill -TERM"))
    );
    assert!(
        trace
            .commands
            .iter()
            .any(|(_, command)| command.contains("audio_enable"))
    );
}

#[tokio::test]
async fn restore_inspects_registry_after_late_accepted_worker_publication() {
    let admission = Arc::new(Semaphore::new(1));
    let accepted = admission.clone().acquire_owned().await.unwrap();
    let ownership = Arc::new(Mutex::new(VoiceRouteOwnership::default()));
    let inspected = Arc::new(AtomicBool::new(false));
    let task_admission = admission.clone();
    let task_ownership = ownership.clone();
    let task_inspected = inspected.clone();
    let worker_ownership = ownership.clone();
    let worker = tokio::spawn(async move {
        finish_restore_voice_cleanup(
            &task_admission,
            || {
                task_inspected.store(true, Ordering::Release);
                inspect(&task_ownership, "producer-a")
            },
            |permit, obligation| {
                settle_scripted(
                    permit,
                    obligation,
                    worker_ownership,
                    async { Ok(()) },
                    false,
                )
            },
        )
        .await
    });
    tokio::task::yield_now().await;
    assert!(!inspected.load(Ordering::Acquire));
    let (plan, trace) = selected_plan(58103);
    ownership
        .lock()
        .unwrap()
        .begin(cleanup_fixture("producer-a", 3, plan))
        .unwrap();
    drop(accepted);
    worker.await.unwrap().unwrap();
    assert!(inspected.load(Ordering::Acquire));
    assert!(ownership.lock().unwrap().obligations.is_empty());
    assert!(!trace.lock().unwrap().commands.is_empty());
}

#[tokio::test]
async fn restore_foreign_or_mixed_registry_blocks_without_new_io() {
    for mixed in [false, true] {
        let (plan, trace) = selected_plan(58104);
        let mut registry = VoiceRouteOwnership::default();
        registry
            .begin(cleanup_fixture("old-producer", 4, plan))
            .unwrap();
        if mixed {
            // A corrupted/mixed registry must not pass a producer-only lookup.
            let (plan, _) = selected_plan(58105);
            registry
                .obligations
                .push(cleanup_fixture("producer-a", 5, plan));
        }
        let ownership = Arc::new(Mutex::new(registry));
        let error = finish_restore_voice_cleanup(
            &Semaphore::new(1),
            || inspect(&ownership, "producer-a"),
            |_, _| async { panic!("foreign obligations cannot become an empty/noop receipt") },
        )
        .await
        .unwrap_err();
        assert!(error.1.contains("其他后端"));
        assert_eq!(
            ownership.lock().unwrap().obligations.len(),
            if mixed { 2 } else { 1 }
        );
        assert!(trace.lock().unwrap().opened.is_empty());
    }
}

#[tokio::test]
async fn restore_cancelled_waiter_keeps_admission_and_owned_cleanup_until_release() {
    let admission = Arc::new(Semaphore::new(1));
    let (plan, _) = selected_plan(58110);
    let ownership = Arc::new(Mutex::new(VoiceRouteOwnership::default()));
    ownership
        .lock()
        .unwrap()
        .begin(cleanup_fixture("producer-a", 10, plan))
        .unwrap();
    let task_admission = admission.clone();
    let task_ownership = ownership.clone();
    let worker_ownership = ownership.clone();
    let (release_reached, reached) = tokio::sync::oneshot::channel();
    let (allow_release, release) = tokio::sync::oneshot::channel();
    let (finished, completion) = tokio::sync::oneshot::channel();
    let observer = tokio::spawn(async move {
        retain_restore_voice_cleanup(async move {
            let result = finish_restore_voice_cleanup(
                &task_admission,
                || inspect(&task_ownership, "producer-a"),
                |permit, obligation| {
                    settle_scripted(
                        permit,
                        obligation,
                        worker_ownership,
                        async move {
                            release_reached.send(()).unwrap();
                            release.await.unwrap();
                            Ok(())
                        },
                        false,
                    )
                },
            )
            .await;
            finished.send(()).unwrap();
            result
        })
        .await
    });
    reached.await.unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert_eq!(admission.available_permits(), 0);
    assert_eq!(ownership.lock().unwrap().obligations.len(), 1);
    allow_release.send(()).unwrap();
    completion.await.unwrap();
    assert!(ownership.lock().unwrap().obligations.is_empty());
    assert_eq!(admission.available_permits(), 1);
}

#[tokio::test]
async fn restore_stop_timeout_or_worker_panic_preserves_original_obligation() {
    for panic_worker in [false, true] {
        let (plan, trace) = selected_plan(58106);
        if !panic_worker {
            trace.lock().unwrap().replies.extend([
                Ok(("0".into(), 0)),
                Err(ProbeError::new(
                    "module_voice_route_timeout",
                    "original status timeout",
                )),
            ]);
        }
        let ownership = Arc::new(Mutex::new(VoiceRouteOwnership::default()));
        ownership
            .lock()
            .unwrap()
            .begin(cleanup_fixture("producer-a", 6, plan.clone()))
            .unwrap();
        let released = Arc::new(AtomicBool::new(false));
        let release_seen = released.clone();
        let worker_ownership = ownership.clone();
        let error = finish_restore_voice_cleanup(
            &Semaphore::new(1),
            || inspect(&ownership, "producer-a"),
            |permit, obligation| {
                settle_scripted(
                    permit,
                    obligation,
                    worker_ownership,
                    async move {
                        release_seen.store(true, Ordering::Release);
                        Ok(())
                    },
                    panic_worker,
                )
            },
        )
        .await
        .unwrap_err();
        assert!(error.1.contains(if panic_worker {
            "工作线程"
        } else {
            "original status timeout"
        }));
        assert_eq!(released.load(Ordering::Acquire), panic_worker);
        let retained = inspect(&ownership, "producer-a").unwrap().unwrap();
        assert!(Arc::ptr_eq(&retained.target.plan.0, &plan.0));
        assert_eq!(retained.generation, 6);
    }
}

#[tokio::test]
async fn restore_known_stop_with_unknown_usb_release_remains_owed_until_confirmed_release() {
    let (plan, trace) = selected_plan(58107);
    let ownership = Arc::new(Mutex::new(VoiceRouteOwnership::default()));
    ownership
        .lock()
        .unwrap()
        .begin(cleanup_fixture("producer-a", 7, plan))
        .unwrap();
    let worker_ownership = ownership.clone();
    let error = finish_restore_voice_cleanup(
        &Semaphore::new(1),
        || inspect(&ownership, "producer-a"),
        |permit, obligation| {
            settle_scripted(
                permit,
                obligation,
                worker_ownership,
                async { Err("original USB release unknown".into()) },
                false,
            )
        },
    )
    .await
    .unwrap_err();
    assert!(error.1.contains("original USB release unknown"));
    assert_eq!(ownership.lock().unwrap().obligations.len(), 1);
    let worker_ownership = ownership.clone();
    finish_restore_voice_cleanup(
        &Semaphore::new(1),
        || inspect(&ownership, "producer-a"),
        |permit, obligation| {
            settle_scripted(
                permit,
                obligation,
                worker_ownership,
                async { Ok(()) },
                false,
            )
        },
    )
    .await
    .unwrap();
    assert!(ownership.lock().unwrap().obligations.is_empty());
    assert!(trace.lock().unwrap().opened.iter().all(|id| *id == 58107));
}

#[tokio::test]
async fn restore_lost_original_device_never_substitutes_later_candidate() {
    let (plan, trace) = selected_plan(58108);
    trace
        .lock()
        .unwrap()
        .open_results
        .push_back(Err(ProbeError::new(
            TARGET_DISCONNECTED,
            "original disconnected",
        )));
    let (_later_plan, later_trace) = selected_plan(58109);
    let ownership = Arc::new(Mutex::new(VoiceRouteOwnership::default()));
    ownership
        .lock()
        .unwrap()
        .begin(cleanup_fixture("producer-a", 8, plan))
        .unwrap();
    let worker_ownership = ownership.clone();
    let error = finish_restore_voice_cleanup(
        &Semaphore::new(1),
        || inspect(&ownership, "producer-a"),
        |permit, obligation| {
            settle_scripted(
                permit,
                obligation,
                worker_ownership,
                async { panic!("no USB lease after failed original preflight") },
                false,
            )
        },
    )
    .await
    .unwrap_err();
    assert!(error.1.contains("original disconnected"));
    assert_eq!(ownership.lock().unwrap().obligations.len(), 1);
    assert_eq!(trace.lock().unwrap().opened, [58108]);
    assert!(later_trace.lock().unwrap().opened.is_empty());
}
