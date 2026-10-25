use super::*;
use crate::usb_at::reconnect_tests::{self as ports, IMEI, TestTransport, Trace};
use crate::usb_at_transport::{PhysicalTarget, TransportError};
use std::sync::atomic::AtomicUsize;

struct Fixture {
    service: Service,
    lease: CallModeReservation,
    root: PathBuf,
    _lifetime: crate::daemon_lifetime::Lease,
    trace: Arc<Trace>,
    factory: Arc<AtomicUsize>,
}
impl Fixture {
    async fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/r584-owner")
            .join(crate::snapshot::new_instance_id());
        let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
        let factory = Arc::new(AtomicUsize::new(0));
        let factory_copy = factory.clone();
        let usb = crate::usb_at::Owner::with_factory(
            "reconnect-owner".into(),
            Arc::new(move || {
                factory_copy.fetch_add(1, Ordering::AcqRel);
                Err(TransportError {
                    code: "usb_at_target_unconfirmed",
                    message: "Generic discovery must not run".into(),
                    response: String::new(),
                    uncertain: false,
                })
            }),
        )
        .with_recovery_gate_for_tests(gate.clone());
        let trace = ports::install_default(&usb, "original-session");
        let service = Service::new(
            root.clone(),
            usb,
            crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap(),
            BackendAvailability::with_module_recovery(gate),
            "reconnect-owner".into(),
        );
        service
            .0
            .store
            .save(Preferences {
                enabled: false,
                restore_cfun: 1,
                user_configured: true,
            })
            .unwrap();
        service
            .submit(Job::Configure(Configure {
                producer_id: "reconnect-owner".into(),
                config_seq: 1,
                transport: "usb".into(),
                session_id: "original-session".into(),
            }))
            .await
            .unwrap();
        let lease = service.reserve_call_mode_operation().await.unwrap();
        Self {
            service,
            lease,
            root,
            _lifetime: lifetime,
            trace,
            factory,
        }
    }
    async fn prepare(&self) {
        let outcome = self
            .service
            .call_mode_action(
                &self.lease,
                Action::CallModeVoice {
                    target: VoiceConfiguration {
                        ims: 1,
                        volte_capability: 1,
                        volte_disabled: 0,
                    },
                },
            )
            .await
            .unwrap();
        assert!(outcome.complete && outcome.verified && !outcome.uncertain);
    }
    fn anchor(&self) -> crate::usb_at::CallModeUsbAnchor {
        self.service
            .0
            .state
            .lock()
            .unwrap()
            .reservation
            .as_ref()
            .unwrap()
            .reconnect
            .clone()
            .unwrap()
    }
    async fn release(&self) {
        self.service
            .release_call_mode_operation(&self.lease)
            .await
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.begin_shutdown();
        let _ = self.service.0.modem.shutdown();
        // Test artifacts are retained for inspection by the validation owner.
    }
}
fn mutating_commands(trace: &Trace) -> Vec<String> {
    trace
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|command| {
            command.starts_with("AT+QCFG=\"ims\",")
                || command.starts_with("AT+QCFG=\"volte_disable\",")
                || command.starts_with("AT+QCFG=\"USBCFG\",")
                || *command == "AT+CFUN=1,1"
        })
        .cloned()
        .collect()
}

#[tokio::test]
async fn first_persistent_write_requires_anchor_from_the_actual_open_transport() {
    let f = Fixture::new().await;
    f.trace.anchor_enabled.store(false, Ordering::Release);
    let result = f
        .service
        .call_mode_action(
            &f.lease,
            Action::CallModeVoice {
                target: VoiceConfiguration {
                    ims: 1,
                    volte_capability: 1,
                    volte_disabled: 0,
                },
            },
        )
        .await;
    assert!(result.is_err());
    assert!(f.trace.commands.lock().unwrap().is_empty());
    assert!(mutating_commands(&f.trace).is_empty());
    assert_eq!(f.factory.load(Ordering::Acquire), 0);
    let state = f.service.0.state.lock().unwrap();
    let attempt = state
        .reservation
        .as_ref()
        .unwrap()
        .attempts
        .values()
        .next()
        .unwrap();
    assert!(matches!(&attempt.result, Some(Err(_))));
    assert!(!state.reservation.as_ref().unwrap().unknown);
    drop(state);
    f.release().await;
}

#[tokio::test]
async fn foreign_current_session_with_equal_imei_is_neither_closed_nor_adopted() {
    let f = Fixture::new().await;
    f.prepare().await;
    let anchor = f.anchor();
    let foreign = Arc::new(Trace::default());
    ports::install(
        &f.service.0.usb,
        "foreign-session",
        PhysicalTarget::synthetic_for_tests(),
        foreign.clone(),
    );
    assert!(f.service.call_mode_rebind(&f.lease, IMEI).await.is_err());
    assert_eq!(ports::current_session(&f.service.0.usb), "foreign-session");
    assert_eq!(foreign.drops.load(Ordering::Acquire), 0);
    assert!(foreign.commands.lock().unwrap().is_empty());
    assert_eq!(
        f.service.usb_transport_session().as_deref(),
        Some("original-session")
    );
    assert_eq!(f.factory.load(Ordering::Acquire), 0);
    f.release().await;
    assert!(
        anchor
            .validate_for_write(&Action::CallModeIdentityRead)
            .is_err()
    );
}

#[tokio::test]
async fn exact_candidate_wrong_or_malformed_identity_is_closed_without_publication() {
    for identity in ["987654321098765", "not-an-imei", "broken-firmware"] {
        let f = Fixture::new().await;
        f.prepare().await;
        let candidate = Arc::new(Trace::default());
        if identity == "broken-firmware" {
            candidate.firmware_broken.store(true, Ordering::Release);
        } else {
            *candidate.imei.lock().unwrap() = identity.into();
        }
        let candidate_copy = candidate.clone();
        ports::set_opener(
            &f.service.0.usb,
            Arc::new(move |source, successor, _, guard| {
                assert!(successor.is_none());
                assert!(guard());
                Ok(Box::new(TestTransport {
                    target: source.clone(),
                    trace: candidate_copy.clone(),
                }))
            }),
        );
        assert!(f.service.call_mode_rebind(&f.lease, IMEI).await.is_err());
        assert!(ports::current_session(&f.service.0.usb).is_empty());
        assert_eq!(
            f.service.usb_transport_session().as_deref(),
            Some("original-session")
        );
        assert_eq!(candidate.drops.load(Ordering::Acquire), 1);
        assert_eq!(f.trace.drops.load(Ordering::Acquire), 1);
        assert!(mutating_commands(&candidate).is_empty());
        assert_eq!(f.factory.load(Ordering::Acquire), 0);
        f.release().await;
    }
}

#[tokio::test]
async fn blocked_candidate_open_keeps_original_operation_and_lock_until_actual_drop() {
    let f = Fixture::new().await;
    f.prepare().await;
    let entered = Arc::new(Notify::new());
    let block = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let candidate = Arc::new(Trace::default());
    let entered_copy = entered.clone();
    let block_copy = block.clone();
    let candidate_copy = candidate.clone();
    ports::set_opener(
        &f.service.0.usb,
        Arc::new(move |source, _, _, guard| {
            assert!(guard());
            entered_copy.notify_one();
            let (lock, changed) = &*block_copy;
            let (done, timeout) = changed
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |done| !*done)
                .unwrap();
            assert!(*done && !timeout.timed_out());
            Ok(Box::new(TestTransport {
                target: source.clone(),
                trace: candidate_copy.clone(),
            }))
        }),
    );
    let worker = f.service.clone();
    let lease = f.lease.clone();
    let request = tokio::spawn(async move { worker.call_mode_rebind(&lease, IMEI).await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let proof = f
        .service
        .mint_call_mode_rebind_proof(None, f.lease.epoch)
        .unwrap();
    let clone = proof.clone();
    request.abort();
    let _ = request.await;
    f.service
        .0
        .state
        .lock()
        .unwrap()
        .epoch_cancel
        .store(true, Ordering::Release);
    assert_eq!(
        f.service
            .0
            .state
            .lock()
            .unwrap()
            .reservation
            .as_ref()
            .unwrap()
            .active_reconnect_pins,
        1
    );
    assert!(matches!(
        crate::call_mode_backups::try_call_mode_operation_file_lock(&f.root),
        Err(crate::call_mode_backups::OperationFileLockError::Busy)
    ));
    let releaser = f.service.clone();
    let lease = f.lease.clone();
    let release = tokio::spawn(async move { releaser.release_call_mode_operation(&lease).await });
    tokio::task::yield_now().await;
    assert!(!release.is_finished());
    assert_eq!(candidate.drops.load(Ordering::Acquire), 0);
    *block.0.lock().unwrap() = true;
    block.1.notify_all();
    tokio::time::timeout(Duration::from_secs(3), release)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(candidate.drops.load(Ordering::Acquire), 1);
    assert!(candidate.commands.lock().unwrap().is_empty());
    assert!(ports::current_session(&f.service.0.usb).is_empty());
    assert!(proof.validate().is_err() && clone.validate().is_err());
    // A proof created by the actual blocked reconnect worker cannot reopen its
    // fifth job/pin registration site after original release and final seal.
    assert!(
        f.service
            .finish_shutdown_receipt()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(f.service.0.state.lock().unwrap().finalized);
    assert!(proof.pin_worker().is_err() && clone.pin_worker().is_err());
    assert_eq!(f.service.0.state.lock().unwrap().jobs, 0);
    assert!(
        crate::call_mode_backups::try_call_mode_operation_file_lock(&f.root).is_ok(),
        "idle proof clones must not retain the kernel lock"
    );
    assert_eq!(f.factory.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn exact_reopen_and_retry_keep_the_same_original_anchor_without_generic_discovery() {
    let f = Fixture::new().await;
    f.prepare().await;
    let original = f.anchor();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_copy = attempts.clone();
    let trace = Arc::new(Trace::default());
    let trace_copy = trace.clone();
    ports::set_opener(
        &f.service.0.usb,
        Arc::new(move |source, successor, _, guard| {
            assert!(successor.is_none());
            assert!(guard());
            if attempts_copy.fetch_add(1, Ordering::AcqRel) == 0 {
                return Err(TransportError {
                    code: "usb_at_successor_pending",
                    message: "not ready".into(),
                    response: String::new(),
                    uncertain: false,
                });
            }
            Ok(Box::new(TestTransport {
                target: source.clone(),
                trace: trace_copy.clone(),
            }))
        }),
    );
    assert!(f.service.call_mode_rebind(&f.lease, IMEI).await.is_err());
    assert!(f.anchor().same_instance(&original));
    assert_eq!(
        f.service
            .call_mode_rebind(&f.lease, IMEI)
            .await
            .unwrap()
            .imei,
        IMEI
    );
    assert!(f.anchor().same_instance(&original));
    assert_eq!(attempts.load(Ordering::Acquire), 2);
    assert_eq!(f.factory.load(Ordering::Acquire), 0);
    assert!(mutating_commands(&trace).is_empty());
    f.release().await;
    assert!(
        original
            .validate_for_write(&Action::CallModeIdentityRead)
            .is_err()
    );
    assert!(crate::call_mode_backups::try_call_mode_operation_file_lock(&f.root).is_ok());
}

#[tokio::test]
async fn only_the_accepted_reboot_successor_can_settle_its_unknown_and_retry_is_read_only() {
    for disconnected_close in [false, true] {
        let f = Fixture::new().await;
        f.trace
            .close_disconnected
            .store(disconnected_close, Ordering::Release);
        f.trace.reboot_uncertain.store(true, Ordering::Release);
        let reboot = f
            .service
            .call_mode_action(&f.lease, Action::Reboot)
            .await
            .unwrap();
        assert!(reboot.submitted && reboot.uncertain);
        assert_eq!(
            ports::last_close_disposition(&f.service.0.usb),
            Some(if disconnected_close {
                crate::usb_at_transport::CloseDisposition::DisconnectedAndHandleDropped
            } else {
                crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped
            })
        );
        assert!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .unknown
        );
        let opens = Arc::new(AtomicUsize::new(0));
        let opens_copy = opens.clone();
        let candidate = Arc::new(Trace::default());
        let candidate_copy = candidate.clone();
        ports::set_opener(
            &f.service.0.usb,
            Arc::new(move |source, successor, _, guard| {
                let successor =
                    successor.expect("accepted reboot must use its original successor anchor");
                assert!(successor.same_source(source));
                assert!(guard());
                opens_copy.fetch_add(1, Ordering::AcqRel);
                Ok(Box::new(TestTransport {
                    target: PhysicalTarget::synthetic_for_tests(),
                    trace: candidate_copy.clone(),
                }))
            }),
        );
        f.service.call_mode_rebind(&f.lease, IMEI).await.unwrap();
        assert!(
            !f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .unknown
        );
        assert!(!f.service.0.state.lock().unwrap().observed.uncertain);
        let session = ports::current_session(&f.service.0.usb);
        f.service.call_mode_rebind(&f.lease, IMEI).await.unwrap();
        assert_eq!(ports::current_session(&f.service.0.usb), session);
        assert_eq!(opens.load(Ordering::Acquire), 1);
        assert_eq!(f.factory.load(Ordering::Acquire), 0);
        assert!(mutating_commands(&candidate).is_empty());
        assert!(
            f.anchor()
                .validate_for_write(&Action::CallModeAdbAuthorize)
                .is_ok(),
            "post-reboot authorization remains permitted by the same finite operation"
        );
        assert!(
            f.anchor().validate_for_write(&Action::Reboot).is_err(),
            "a second reboot needs a new finite operation"
        );
        f.release().await;
    }
}

#[tokio::test]
async fn observed_lock_namespace_failure_poisoning_survives_name_restoration() {
    let f = Fixture::new().await;
    f.prepare().await;
    let entered = Arc::new(Notify::new());
    let block = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let opens = Arc::new(AtomicUsize::new(0));
    let opens_copy = opens.clone();
    let block_copy = block.clone();
    let entered_copy = entered.clone();
    ports::set_opener(
        &f.service.0.usb,
        Arc::new(move |source, _, _, guard| {
            assert!(guard());
            opens_copy.fetch_add(1, Ordering::AcqRel);
            entered_copy.notify_one();
            let (lock, changed) = &*block_copy;
            let (done, timeout) = changed
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |done| !*done)
                .unwrap();
            assert!(*done && !timeout.timed_out());
            Ok(Box::new(TestTransport {
                target: source.clone(),
                trace: Arc::new(Trace::default()),
            }))
        }),
    );
    let worker = f.service.clone();
    let lease = f.lease.clone();
    let task = tokio::spawn(async move { worker.call_mode_rebind(&lease, IMEI).await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let proof = f
        .service
        .mint_call_mode_rebind_proof(None, f.lease.epoch)
        .unwrap();
    let clone = proof.clone();
    let path = f.root.join("call-mode-operation.lock");
    let retained = f.root.join("operation-lock-retained");
    std::fs::rename(&path, &retained).unwrap();
    assert!(proof.validate().is_err());
    std::fs::rename(&retained, &path).unwrap();
    assert!(clone.validate().is_err());
    let failed = f
        .service
        .0
        .usb
        .reconnect_call_mode(
            f.anchor(),
            clone,
            IMEI,
            Instant::now() + Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
        )
        .await;
    assert!(failed.is_err());
    assert_eq!(
        opens.load(Ordering::Acquire),
        1,
        "the failed proof must never reach the opener again"
    );
    *block.0.lock().unwrap() = true;
    block.1.notify_all();
    task.await.unwrap().unwrap();
    f.release().await;
}

#[tokio::test]
async fn original_reboot_rebind_cannot_bypass_failed_checked_close_with_absent_port() {
    let f = Fixture::new().await;
    f.trace.reboot_uncertain.store(true, Ordering::Release);
    f.trace.close_error.store(true, Ordering::Release);
    let reboot = f
        .service
        .call_mode_action(&f.lease, Action::Reboot)
        .await
        .err()
        .expect("checked close must fail the original action");
    assert_eq!(reboot.public_code(), "usb_at_close_unconfirmed");
    let opens = Arc::new(AtomicUsize::new(0));
    let opened = opens.clone();
    ports::set_opener(
        &f.service.0.usb,
        Arc::new(move |_, _, _, _| {
            opened.fetch_add(1, Ordering::AcqRel);
            panic!("failed original close must reject before successor opening")
        }),
    );
    assert!(f.service.call_mode_rebind(&f.lease, IMEI).await.is_err());
    assert!(f.service.call_mode_rebind(&f.lease, IMEI).await.is_err());
    assert_eq!(opens.load(Ordering::Acquire), 0);
    assert_eq!(f.factory.load(Ordering::Acquire), 0);
    assert!(ports::current_session(&f.service.0.usb).is_empty());
    assert_eq!(
        f.service.0.usb.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
}

#[tokio::test]
async fn unpublished_candidate_close_debt_blocks_same_anchor_retry_but_not_new_operation() {
    let f = Fixture::new().await;
    f.prepare().await;
    let original_anchor = f.anchor();
    let candidate = Arc::new(Trace::default());
    candidate.firmware_broken.store(true, Ordering::Release);
    candidate.close_error.store(true, Ordering::Release);
    let candidate_copy = candidate.clone();
    let opens = Arc::new(AtomicUsize::new(0));
    let opened = opens.clone();
    ports::set_opener(
        &f.service.0.usb,
        Arc::new(move |source, successor, _, guard| {
            assert!(successor.is_none());
            assert!(guard());
            opened.fetch_add(1, Ordering::AcqRel);
            Ok(Box::new(TestTransport {
                target: source.clone(),
                trace: candidate_copy.clone(),
            }))
        }),
    );
    let first = f
        .service
        .call_mode_rebind(&f.lease, IMEI)
        .await
        .err()
        .expect("rejected candidate close must fail");
    assert_eq!(first.public_code(), "usb_at_close_unconfirmed");
    assert!(ports::current_session(&f.service.0.usb).is_empty());
    assert_eq!(candidate.drops.load(Ordering::Acquire), 1);
    let reads = candidate.commands.lock().unwrap().clone();
    assert!(!reads.is_empty());
    assert!(mutating_commands(&candidate).is_empty());
    assert!(f.service.call_mode_rebind(&f.lease, IMEI).await.is_err());
    assert!(f.service.call_mode_rebind(&f.lease, IMEI).await.is_err());
    assert_eq!(
        opens.load(Ordering::Acquire),
        1,
        "same anchor must fail before opener"
    );
    assert_eq!(
        *candidate.commands.lock().unwrap(),
        reads,
        "retry must not read back on debt"
    );
    assert_eq!(
        f.service.usb_transport_session().as_deref(),
        Some("original-session")
    );
    assert_eq!(f.factory.load(Ordering::Acquire), 0);
    f.release().await;

    // Independent explicit fixture connection/operation does not inherit the
    // old anchor's debt. This is software ownership evidence, never native USB.
    let fresh = ports::install_default(&f.service.0.usb, "independent-session");
    f.service
        .submit(Job::Configure(Configure {
            producer_id: "reconnect-owner".into(),
            config_seq: 2,
            transport: "usb".into(),
            session_id: "independent-session".into(),
        }))
        .await
        .unwrap();
    let independent = f.service.reserve_call_mode_operation().await.unwrap();
    let prepared = f
        .service
        .call_mode_action(
            &independent,
            Action::CallModeVoice {
                target: VoiceConfiguration {
                    ims: 1,
                    volte_capability: 1,
                    volte_disabled: 0,
                },
            },
        )
        .await
        .unwrap();
    assert!(prepared.complete && prepared.verified && !prepared.uncertain);
    assert!(!f.anchor().same_instance(&original_anchor));
    f.service
        .call_mode_rebind(&independent, IMEI)
        .await
        .unwrap();
    assert!(fresh.drops.load(Ordering::Acquire) >= 1);
    assert_eq!(f.factory.load(Ordering::Acquire), 0);
    f.service
        .release_call_mode_operation(&independent)
        .await
        .unwrap();
    assert!(
        original_anchor
            .validate_for_write(&Action::CallModeIdentityRead)
            .is_err()
    );
    // A new operation's readback is not permission to forget the old resource
    // debt or claim a full restore receipt/clean daemon shutdown.
    assert_eq!(
        f.service.0.usb.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
}

#[tokio::test]
async fn opener_cleanup_error_or_panic_latches_exact_anchor_debt_before_candidate_exists() {
    for panic_in_opener in [false, true] {
        let f = Fixture::new().await;
        f.prepare().await;
        let opens = Arc::new(AtomicUsize::new(0));
        let opened = opens.clone();
        ports::set_opener(
            &f.service.0.usb,
            Arc::new(move |_, _, _, guard| {
                assert!(guard());
                opened.fetch_add(1, Ordering::AcqRel);
                if panic_in_opener {
                    panic!("fixture opener cleanup panicked before returning candidate");
                }
                Err(crate::usb_at_transport::close_unconfirmed())
            }),
        );
        let first = f
            .service
            .call_mode_rebind(&f.lease, IMEI)
            .await
            .err()
            .expect("unconfirmed opening attempt");
        assert_eq!(first.public_code(), "usb_at_close_unconfirmed");
        assert!(ports::current_session(&f.service.0.usb).is_empty());
        assert!(f.service.call_mode_rebind(&f.lease, IMEI).await.is_err());
        assert_eq!(
            opens.load(Ordering::Acquire),
            1,
            "sticky original attempt must reject before another open"
        );
        assert_eq!(f.factory.load(Ordering::Acquire), 0);
        assert_eq!(
            f.service.0.usb.shutdown().await.unwrap_err().code,
            "usb_at_close_unconfirmed"
        );
    }
}
