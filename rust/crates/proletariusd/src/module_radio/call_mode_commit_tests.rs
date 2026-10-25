use super::*;
use tokio::sync::mpsc;

struct Pending {
    service: Service,
    root: PathBuf,
    lifetime: Option<crate::daemon_lifetime::Lease>,
    lease: CallModeReservation,
    proof: CallModeCommitProof,
    action: Action,
    reply: Option<oneshot::Sender<Result<Outcome, Failure>>>,
    task: Option<JoinHandle<Result<Outcome, Failure>>>,
    requests: mpsc::UnboundedReceiver<(Action, oneshot::Sender<Result<Outcome, Failure>>)>,
}
impl Pending {
    async fn new(action: Action) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/r582-commit")
            .join(crate::snapshot::new_instance_id());
        std::fs::create_dir_all(&root).unwrap();
        let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
        let service = Service::new(
            root.clone(),
            crate::usb_at::Owner::unavailable_for_tests("commit-owner".into())
                .with_recovery_gate_for_tests(gate.clone()),
            crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap(),
            BackendAvailability::with_module_recovery(gate),
            "commit-owner".into(),
        );
        let (send, mut receive) = mpsc::unbounded_channel();
        crate::usb_at::reconnect_tests::install_default(&service.0.usb, "commit-session");
        *service.0.physical_fixture.lock().unwrap() = Some(send);
        service.0.store.save(Preferences::default()).unwrap();
        service
            .submit(Job::Configure(Configure {
                producer_id: "commit-owner".into(),
                config_seq: 1,
                transport: "usb".into(),
                session_id: "commit-session".into(),
            }))
            .await
            .unwrap();
        let lease = service.reserve_call_mode_operation().await.unwrap();
        let worker = service.clone();
        let worker_lease = lease.clone();
        let worker_action = action.clone();
        let task =
            tokio::spawn(
                async move { worker.call_mode_action(&worker_lease, worker_action).await },
            );
        let (_, reply) = tokio::time::timeout(Duration::from_secs(3), receive.recv())
            .await
            .unwrap()
            .unwrap();
        let proof = service.pending_call_mode_commit_for_tests(&action).unwrap();
        Self {
            service,
            root,
            lifetime: Some(lifetime),
            lease,
            proof,
            action,
            reply: Some(reply),
            task: Some(task),
            requests: receive,
        }
    }
    async fn finish(&mut self, outcome: Outcome) -> Result<Outcome, Failure> {
        self.reply.take().unwrap().send(Ok(outcome)).unwrap();
        self.task.take().unwrap().await.unwrap()
    }
    async fn release(&self) {
        self.service
            .release_call_mode_operation(&self.lease)
            .await
            .unwrap();
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.reply.take();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let _ = self.service.0.modem.shutdown();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn voice() -> Action {
    Action::CallModeVoice {
        target: VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        },
    }
}
fn complete() -> Outcome {
    Outcome {
        complete: true,
        verified: true,
        ..Outcome::default()
    }
}

#[tokio::test]
async fn actual_pending_owner_mints_exact_action_binding_and_clone_expires_with_attempt() {
    let mut pending = Pending::new(voice()).await;
    pending
        .proof
        .validate_for_binding(&pending.action, &pending.proof.binding)
        .unwrap();
    let clone = pending.proof.clone();
    let strong = {
        let state = pending.service.0.state.lock().unwrap();
        Arc::strong_count(&state.reservation.as_ref().unwrap().operation)
    };
    let extra = pending.proof.clone();
    assert_eq!(strong, {
        let state = pending.service.0.state.lock().unwrap();
        Arc::strong_count(&state.reservation.as_ref().unwrap().operation)
    });
    pending.finish(complete()).await.unwrap();
    assert!(clone.validate(&pending.action).is_err());
    assert!(extra.validate(&pending.action).is_err());
    pending.release().await;
}

#[tokio::test]
async fn different_target_or_transport_never_reuses_valid_owner_proof() {
    for foreign_binding in [false, true] {
        let mut pending = Pending::new(voice()).await;
        if foreign_binding {
            assert!(
                pending
                    .proof
                    .validate_for_binding(&pending.action, &Binding::Serial { serial_session: 1 })
                    .is_err()
            );
        } else {
            let other = Action::CallModeVoice {
                target: VoiceConfiguration {
                    ims: 2,
                    volte_capability: 1,
                    volte_disabled: 1,
                },
            };
            assert!(pending.proof.validate(&other).is_err());
        }
        assert!(
            pending.proof.validate(&pending.action).is_err(),
            "failed proof clones never revive"
        );
        pending.finish(complete()).await.unwrap();
        pending.release().await;
    }
}

#[tokio::test]
async fn usbcfg_expected_and_target_are_both_sealed() {
    let expected = UsbComposition {
        vendor_id: 0x2c7c,
        product_id: 0x0125,
        flags: [0, 0, 0, 0, 0, 0, 0],
    };
    let target = UsbComposition {
        flags: [0, 0, 0, 0, 0, 1, 1],
        ..expected
    };
    let mut pending = Pending::new(Action::CallModeUsbCompositionWrite { expected, target }).await;
    let changed_expected = UsbComposition {
        flags: [1, 0, 0, 0, 0, 0, 0],
        ..expected
    };
    assert!(
        pending
            .proof
            .validate(&Action::CallModeUsbCompositionWrite {
                expected: changed_expected,
                target
            })
            .is_err()
    );
    pending.finish(complete()).await.unwrap();
    pending.release().await;
}

#[tokio::test]
async fn replaced_operation_lock_fails_without_releasing_kernel_owner_and_release_still_settles() {
    let mut pending = Pending::new(voice()).await;
    let path = pending.root.join("call-mode-operation.lock");
    let original = pending.root.join("original-lock");
    std::fs::rename(&path, &original).unwrap();
    std::fs::write(&path, []).unwrap();
    assert_eq!(
        pending.proof.validate(&pending.action).unwrap_err().code,
        "call_mode_operation_lock_unavailable"
    );
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&original)
        .unwrap();
    assert!(matches!(
        file.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    // A real prior write remains unknown; namespace rejection cannot erase it.
    let outcome = pending
        .finish(Outcome {
            submitted: true,
            uncertain: true,
            ..Outcome::default()
        })
        .await
        .unwrap();
    assert!(outcome.submitted && outcome.uncertain);
    assert!(
        pending
            .service
            .0
            .state
            .lock()
            .unwrap()
            .reservation
            .as_ref()
            .unwrap()
            .unknown
    );
    pending.release().await;
    assert!(pending.proof.validate(&pending.action).is_err());
    // Retained proof holds the original kernel lock, never a replacement lock.
    assert!(matches!(
        file.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
}

#[tokio::test]
async fn root_rebase_fails_and_restoring_names_does_not_revive_the_plan() {
    let mut pending = Pending::new(voice()).await;
    let moved = pending.root.with_extension("moved");
    std::fs::rename(&pending.root, &moved).unwrap();
    std::fs::create_dir(&pending.root).unwrap();
    assert!(pending.proof.validate(&pending.action).is_err());
    std::fs::remove_dir(&pending.root).unwrap();
    std::fs::rename(&moved, &pending.root).unwrap();
    assert!(pending.proof.validate(&pending.action).is_err());
    pending.finish(complete()).await.unwrap();
    pending.release().await;
}

#[tokio::test]
async fn live_revoke_preserves_finite_owner_but_ended_root_authority_denies_new_configuration() {
    let mut pending = Pending::new(voice()).await;
    pending.service.module_recovery().revoke_for_tests();
    pending.proof.validate(&pending.action).unwrap();
    pending.lifetime.take();
    assert!(pending.proof.validate(&pending.action).is_err());
    pending.finish(complete()).await.unwrap();
    pending.release().await;
}

#[tokio::test]
async fn changed_epoch_binding_and_owner_are_rejected_even_with_same_action() {
    for change in ["epoch", "binding", "owner"] {
        let mut pending = Pending::new(voice()).await;
        {
            let mut state = pending.service.0.state.lock().unwrap();
            match change {
                "epoch" => state.epoch += 1,
                "binding" => state.binding = Some(Binding::Serial { serial_session: 99 }),
                "owner" => state.reservation.as_mut().unwrap().identity.sequence += 1,
                _ => unreachable!(),
            }
        }
        assert!(pending.proof.validate(&pending.action).is_err());
        // Restore test-owned bookkeeping only so the original real worker can
        // settle; the invalid proof itself stays poisoned and cannot revive.
        {
            let mut state = pending.service.0.state.lock().unwrap();
            state.epoch = pending.lease.epoch;
            state.binding = Some(pending.proof.binding.clone());
            state.reservation.as_mut().unwrap().identity.sequence = pending.lease.sequence;
        }
        assert!(pending.proof.validate(&pending.action).is_err());
        pending.finish(complete()).await.unwrap();
        pending.release().await;
    }
}

#[tokio::test]
async fn no_pending_attempt_cannot_mint_a_new_capability() {
    let mut pending = Pending::new(voice()).await;
    pending.finish(complete()).await.unwrap();
    assert!(
        pending
            .service
            .pending_call_mode_commit_for_tests(&pending.action)
            .is_err()
    );
    pending.release().await;
}

#[tokio::test]
async fn compatible_internal_action_mints_from_actual_reservation_without_an_owned_json_flag() {
    let mut pending = Pending::new(voice()).await;
    pending.finish(complete()).await.unwrap();
    let mut input = pending.proof.input.clone();
    input.operation_id = "compatible-internal-action".into();
    let service = pending.service.clone();
    let task = tokio::spawn(async move { service.submit(Job::Action(input)).await });
    let (_, reply) = tokio::time::timeout(Duration::from_secs(3), pending.requests.recv())
        .await
        .unwrap()
        .unwrap();
    let proof = pending
        .service
        .pending_call_mode_commit_for_tests(&pending.action)
        .unwrap();
    proof.validate(&pending.action).unwrap();
    reply.send(Ok(complete())).unwrap();
    assert_eq!(task.await.unwrap().unwrap()["verified"], true);
    assert!(proof.validate(&pending.action).is_err());
    pending.release().await;
}

#[tokio::test]
async fn failed_owner_mint_records_terminal_attempt_without_sending_a_configuration() {
    let mut pending = Pending::new(voice()).await;
    pending.finish(complete()).await.unwrap();
    let path = pending.root.join("call-mode-operation.lock");
    std::fs::rename(&path, pending.root.join("old-lock")).unwrap();
    std::fs::write(path, []).unwrap();
    let mut input = pending.proof.input.clone();
    input.operation_id = "replaced-lock-attempt".into();
    assert_eq!(
        pending
            .service
            .submit(Job::Action(input))
            .await
            .unwrap_err()
            .code,
        "call_mode_operation_lock_unavailable"
    );
    let state = pending.service.0.state.lock().unwrap();
    let attempt = state
        .reservation
        .as_ref()
        .unwrap()
        .attempts
        .get("replaced-lock-attempt")
        .unwrap();
    assert!(matches!(&attempt.result, Some(Err(_))));
    drop(state);
    assert!(pending.requests.try_recv().is_err());
    pending.release().await;
}

#[tokio::test]
async fn foreign_gate_cannot_reuse_the_live_owner_proof() {
    let mut pending = Pending::new(voice()).await;
    pending
        .proof
        .validate_for_gate(pending.service.module_recovery())
        .unwrap();
    assert!(
        pending
            .proof
            .validate_for_gate(&crate::module_recovery::Gate::default())
            .is_err()
    );
    assert!(pending.proof.validate(&pending.action).is_err());
    pending.finish(complete()).await.unwrap();
    pending.release().await;
}

#[tokio::test]
async fn same_gate_wrong_usb_owner_cannot_reuse_a_live_proof() {
    let mut pending = Pending::new(voice()).await;
    pending
        .proof
        .validate_for_usb_owner(&pending.service.0.usb)
        .unwrap();
    let wrong = crate::usb_at::Owner::unavailable_for_tests("commit-owner".into())
        .with_recovery_gate_for_tests(pending.service.module_recovery().clone());
    pending
        .proof
        .validate_for_gate(pending.service.module_recovery())
        .unwrap();
    assert!(pending.proof.validate_for_usb_owner(&wrong).is_err());
    assert!(pending.proof.clone().validate(&pending.action).is_err());
    pending.finish(complete()).await.unwrap();
    pending.release().await;
}

#[tokio::test]
async fn same_gate_wrong_serial_actor_cannot_reuse_a_live_proof() {
    let mut pending = Pending::new(voice()).await;
    pending
        .proof
        .validate_for_modem_actor(&pending.service.0.modem)
        .unwrap();
    let wrong = crate::modem_actor::ModemActor::new_with_module_recovery(
        pending.service.module_recovery().clone(),
    )
    .unwrap();
    pending
        .proof
        .validate_for_gate(pending.service.module_recovery())
        .unwrap();
    assert!(pending.proof.validate_for_modem_actor(&wrong).is_err());
    assert!(pending.proof.clone().validate(&pending.action).is_err());
    pending.finish(complete()).await.unwrap();
    pending.release().await;
    wrong.shutdown().unwrap();
}
