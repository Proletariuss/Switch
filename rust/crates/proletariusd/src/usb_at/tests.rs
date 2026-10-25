use super::*;
use std::collections::VecDeque;
use std::sync::Condvar;
use std::sync::atomic::{AtomicUsize, Ordering};

const PRODUCER: &str = "owner-test-producer";

#[test]
fn module_smsc_resolution_is_bound_to_active_identity_snapshot() {
    assert_eq!(
        resolve_module_smsc("+447700900123", "sip:smsc@example.test", "+441234567890"),
        (
            "+447700900123".to_owned(),
            "sip:smsc@example.test".to_owned()
        )
    );
    assert_eq!(
        resolve_module_smsc("", "", "+441234567890"),
        ("+441234567890".to_owned(), "tel:+441234567890".to_owned())
    );
    assert_eq!(
        resolve_module_smsc("invalid", "", "invalid"),
        (String::new(), String::new())
    );
    assert_eq!(
        resolve_module_smsc("00000000000", "sip:configured@example.test", ""),
        (String::new(), "sip:configured@example.test".to_owned())
    );
}

#[test]
fn module_smsc_normalization_accepts_common_formatting_and_rejects_bad_values() {
    assert_eq!(
        normalize_module_smsc(" +852 (9123) 4567 "),
        Some("+85291234567".into())
    );
    for invalid in [
        "",
        "00000000000",
        "+44ABC1234567",
        "1234",
        "+1234567890123456",
    ] {
        assert_eq!(normalize_module_smsc(invalid), None, "{invalid:?}");
    }
}

#[test]
fn usb_status_uses_saved_profile_phone_only_when_modem_number_is_missing() {
    let mut status = json!({"phone_number":"unknown"})
        .as_object()
        .unwrap()
        .clone();
    Owner::apply_saved_phone_fallback(&mut status, " +85291234567 ");
    assert_eq!(status["phone_number"], "+85291234567");

    let mut known = json!({"phone_number":"+441234567890"})
        .as_object()
        .unwrap()
        .clone();
    Owner::apply_saved_phone_fallback(&mut known, "+85291234567");
    assert_eq!(known["phone_number"], "+441234567890");

    let mut missing = json!({"phone_number":"0000"}).as_object().unwrap().clone();
    Owner::apply_saved_phone_fallback(&mut missing, "");
    assert_eq!(missing["phone_number"], "");
}

#[test]
fn module_identity_at_commands_are_narrow_and_length_checked() {
    for command in [
        "AT+CNUM",
        "AT+CSCA?",
        "AT+QCCID",
        "AT+CIMI",
        "AT+CSIM=4,\"00A4\"",
    ] {
        assert!(valid_module_identity_at_command(command), "{command}");
    }
    for command in [
        "AT+CMGS=1",
        "AT+CSIM=3,\"00A4\"",
        "AT+CSIM=4,\"00A?\"",
        "AT+CNUM;AT+CFUN=1",
        "AT+CSCA",
        "AT+QCCID;AT+CFUN=1",
        "AT+CIMI?",
    ] {
        assert!(!valid_module_identity_at_command(command), "{command}");
    }
}

#[test]
fn module_profile_identity_parsers_match_legacy_normalization() {
    assert_eq!(
        parse_module_qccid("AT+QCCID\r\n+QCCID: \"8988212345678901234F\"\r\nOK"),
        Some("8988212345678901234".into())
    );
    assert_eq!(
        parse_module_cimi("AT+CIMI\r\n234301234567890\r\nOK"),
        Some("234301234567890".into())
    );
    assert_eq!(parse_module_qccid("+QCCID: bad\r\nOK"), None);
    assert_eq!(parse_module_cimi("+CIMI: 234301234567890\r\nOK"), None);
}

#[test]
fn module_csim_parser_checks_character_length_and_hex_payload() {
    assert_eq!(
        parse_module_csim_response("+CSIM: 4,\"9000\"\r\nOK").unwrap(),
        vec![0x90, 0]
    );
    assert_eq!(
        parse_module_csim_response("+CSIM: 4,9000\r\nOK").unwrap(),
        vec![0x90, 0]
    );
    for response in ["+CSIM: 3,\"9000\"\r\nOK", "+CSIM: 4,\"90X0\"\r\nOK", "OK"] {
        assert!(parse_module_csim_response(response).is_err(), "{response}");
    }
}

#[test]
fn module_sim_number_parsers_keep_the_legacy_quoted_field_order() {
    assert_eq!(
        parse_module_cnum_number("+CNUM: \"65075\",\"+85291234567\",145\r\nOK"),
        Some("+85291234567".into())
    );
    assert_eq!(
        parse_module_csca_number("+CSCA: \"+85291234567\",145\r\nOK"),
        Some("+85291234567".into())
    );
    assert_eq!(parse_module_cnum_number("+CNUM: \"\",\"\",145"), None);
    assert_eq!(
        parse_module_csca_number("+CSCA: \"not-a-number\",145"),
        None
    );
}

#[test]
fn module_sim_identity_has_a_finite_transaction_command_budget() {
    let timeout = identity_command_timeout(
        Instant::now() + Duration::from_secs(5),
        Duration::from_secs(12),
    )
    .unwrap();
    assert!(timeout <= Duration::from_secs(5));
    let failure = identity_command_timeout(Instant::now(), Duration::from_secs(12)).unwrap_err();
    assert_eq!(failure.code, "module_sim_identity_timeout");
}

#[test]
fn module_aka_challenge_decoder_requires_exact_hex_vectors() {
    let decoded = decode_module_challenge(&"aB".repeat(16)).unwrap();
    assert_eq!(&decoded[..], &[0xab; 16]);
    for invalid in [
        "",
        "00",
        &format!("{}gg", "00".repeat(15)),
        &"00".repeat(17),
    ] {
        assert!(decode_module_challenge(invalid).is_none(), "{invalid}");
    }
}

#[test]
fn module_sim_apdu_adapter_uses_csim_character_length_without_replay() {
    struct OneResponse {
        calls: usize,
        command: String,
    }
    impl AtCommandTransport for OneResponse {
        type Error = Failure;

        fn command(
            &mut self,
            command: &str,
            _timeout: Duration,
            _max_response_bytes: usize,
        ) -> Result<String, Self::Error> {
            self.calls += 1;
            self.command = command.to_owned();
            Ok("+CSIM: 4,\"9000\"\r\nOK".into())
        }
    }

    let mut command = OneResponse {
        calls: 0,
        command: String::new(),
    };
    let mut adapter = ModuleSimApduTransport {
        command: &mut command,
        deadline: Instant::now() + Duration::from_secs(10),
    };
    assert_eq!(adapter.transmit(&[0x00, 0xa4]).unwrap(), vec![0x90, 0x00]);
    drop(adapter);
    assert_eq!(command.calls, 1);
    assert_eq!(command.command, "AT+CSIM=4,\"00A4\"");
}

#[derive(Default)]
struct Fake {
    commands: Mutex<Vec<String>>,
    follow_ups: Mutex<Vec<Option<Vec<u8>>>>,
    responses: Mutex<VecDeque<Result<String, TransportError>>>,
    opens: AtomicUsize,
    drops: AtomicUsize,
    close_mode: AtomicUsize,
    gate: Mutex<Option<Arc<Gate>>>,
    gate_command: Mutex<Option<String>>,
    drop_gate: Mutex<Option<Arc<Gate>>>,
    budgets: Mutex<Vec<(Duration, usize)>>,
    physical_target: Mutex<Option<crate::usb_at_transport::PhysicalTarget>>,
}
struct FakeTransport(Arc<Fake>);
impl Transport for FakeTransport {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        let mode = self.0.close_mode.load(Ordering::SeqCst);
        drop(self);
        match mode {
            0 => Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped),
            1 => Err(usb_at_transport::close_unconfirmed()),
            3 => Ok(CloseDisposition::DisconnectedAndHandleDropped),
            _ => panic!("fixture checked close panicked after handle drop"),
        }
    }

    fn physical_target(&self) -> Result<crate::usb_at_transport::PhysicalTarget, TransportError> {
        self.0
            .physical_target
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| TransportError {
                code: "usb_at_target_unconfirmed",
                message: "The opened fake transport has no native connection proof".into(),
                response: String::new(),
                uncertain: false,
            })
    }
    fn command(
        &mut self,
        command: &str,
        follow: Option<&[u8]>,
        timeout: Duration,
        limit: usize,
    ) -> Result<String, TransportError> {
        self.0.commands.lock().unwrap().push(command.into());
        self.0
            .follow_ups
            .lock()
            .unwrap()
            .push(follow.map(ToOwned::to_owned));
        self.0.budgets.lock().unwrap().push((timeout, limit));
        let desired = self.0.gate_command.lock().unwrap().clone();
        let gate = if desired.as_deref().is_none_or(|desired| desired == command) {
            self.0.gate.lock().unwrap().take()
        } else {
            None
        };
        if let Some(gate) = gate {
            gate.entered.notify_one();
            let mut proceed = gate.proceed.lock().unwrap();
            while !*proceed {
                proceed = gate.resume.wait(proceed).unwrap();
            }
        }
        self.0
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok("OK".into()))
    }
    fn description(&self) -> String {
        "isolated fake USB AT".into()
    }
}
impl Drop for FakeTransport {
    fn drop(&mut self) {
        let gate = self.0.drop_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            let mut proceed = gate.proceed.lock().unwrap();
            while !*proceed {
                proceed = gate.resume.wait(proceed).unwrap();
            }
        }
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}
#[derive(Default)]
struct Gate {
    entered: Notify,
    proceed: Mutex<bool>,
    resume: Condvar,
}
impl Gate {
    fn release(&self) {
        *self.proceed.lock().unwrap() = true;
        self.resume.notify_all();
    }
}
fn fixture() -> (Owner, Arc<Fake>) {
    fixture_with_adopt(false)
}
fn fixture_with_adopt(adopt: bool) -> (Owner, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let captured = fake.clone();
    let owner = Owner::with_factory_and_adopt(
        PRODUCER.into(),
        Arc::new(move || {
            captured.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeTransport(captured.clone())))
        }),
        adopt,
    );
    owner.set_cellular_suppressed(false);
    (owner, fake)
}
async fn opened(owner: &Owner) -> String {
    owner
        .open(
            OpenRequest {
                producer_id: PRODUCER.into(),
                lease_id: String::new(),
            },
            false,
        )
        .await
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn live_revocation_rejects_usb_open_and_native_claim_before_factory() {
    let gate = crate::module_recovery::Gate::default();
    let (owner, fake) = fixture();
    let owner = owner.with_recovery_gate_for_tests(gate.clone());
    gate.revoke_for_tests();
    let error = owner
        .open(
            OpenRequest {
                producer_id: PRODUCER.into(),
                lease_id: String::new(),
            },
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, crate::module_recovery::CODE);
    assert_eq!(
        owner.claim_native_management().unwrap_err().code,
        crate::module_recovery::CODE
    );
    assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn live_revocation_keeps_accepted_usb_worker_until_actual_completion() {
    let gate = crate::module_recovery::Gate::default();
    let (owner, fake) = fixture();
    let owner = owner.with_recovery_gate_for_tests(gate.clone());
    let session = opened(&owner).await;
    let block = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(block.clone());
    let task_owner = owner.clone();
    let task_session = session.clone();
    let task = tokio::spawn(async move {
        task_owner
            .command(command(&task_session, "", "AT"), false)
            .await
    });
    block.entered.notified().await;
    gate.revoke_for_tests();
    assert!(owner.inner.state.lock().unwrap().busy);
    block.release();
    task.await.unwrap().unwrap();
    assert!(!owner.inner.state.lock().unwrap().busy);
    let before = fake.commands.lock().unwrap().len();
    assert_eq!(
        owner
            .command(command(&session, "", "AT"), false)
            .await
            .unwrap_err()
            .code,
        crate::module_recovery::CODE
    );
    assert_eq!(fake.commands.lock().unwrap().len(), before);
    owner.close(close(&session, "")).await.unwrap();
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn live_revocation_denies_new_commands_under_old_sim_lease_but_keeps_release() {
    let gate = crate::module_recovery::Gate::default();
    let (owner, fake) = fixture();
    let owner = owner.with_recovery_gate_for_tests(gate.clone());
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "accepted-sim", 1, Purpose::Sim))
        .await
        .unwrap();
    gate.revoke_for_tests();
    assert_eq!(
        owner
            .command(command(&session, "accepted-sim", "AT"), false)
            .await
            .unwrap_err()
            .code,
        crate::module_recovery::CODE
    );
    owner
        .release(release(&session, "accepted-sim", 1))
        .await
        .unwrap();
    let error = owner
        .acquire(acquire(&session, "new-sim", 2, Purpose::Sim))
        .await
        .unwrap_err();
    assert_eq!(error.code, crate::module_recovery::CODE);
    assert!(fake.commands.lock().unwrap().is_empty());
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn scoped_usb_io_requires_same_active_gate_and_producer_after_revocation() {
    let gate = crate::module_recovery::Gate::default();
    let (owner, _) = fixture();
    let owner = owner.with_recovery_gate_for_tests(gate.clone());
    let session = opened(&owner).await;
    let accepted = gate.admit_operation(PRODUCER).unwrap();
    let right = accepted.right();
    let foreign_gate = crate::module_recovery::Gate::default();
    let foreign = foreign_gate.admit_operation(PRODUCER).unwrap();
    let other = gate.admit_operation("other-producer").unwrap();
    gate.revoke_for_tests();
    for invalid in [foreign.right(), other.right()] {
        assert!(
            owner
                .admit_with_right(
                    PRODUCER,
                    Some(&session),
                    "",
                    Instant::now() + Duration::from_secs(1),
                    Some(&invalid)
                )
                .await
                .is_err()
        );
    }
    let permit = owner
        .admit_with_right(
            PRODUCER,
            Some(&session),
            "",
            Instant::now() + Duration::from_secs(1),
            Some(&right),
        )
        .await
        .unwrap();
    assert!(owner.inner.state.lock().unwrap().busy);
    drop(permit);
    drop(accepted);
    assert!(
        owner
            .admit_with_right(
                PRODUCER,
                Some(&session),
                "",
                Instant::now() + Duration::from_secs(1),
                Some(&right)
            )
            .await
            .is_err()
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn voice_cleanup_capture_accepts_same_connection_but_never_replacement() {
    let (owner, fake) = fixture();
    let target = crate::usb_at_transport::PhysicalTarget::synthetic_for_tests();
    *fake.physical_target.lock().unwrap() = Some(target.clone());
    let first = opened(&owner).await;
    let original = owner.capture_voice_target(PRODUCER).await.unwrap();
    owner.close(close(&first, "")).await.unwrap();
    owner
        .validate_voice_cleanup_capture(PRODUCER, &original)
        .await
        .unwrap();
    let second = opened(&owner).await;
    owner
        .validate_voice_cleanup_capture(PRODUCER, &original)
        .await
        .unwrap();
    *fake.physical_target.lock().unwrap() =
        Some(crate::usb_at_transport::PhysicalTarget::synthetic_for_tests());
    let error = owner
        .validate_voice_cleanup_capture(PRODUCER, &original)
        .await
        .unwrap_err();
    assert_eq!(error.code, Failure::stale().code);
    assert_eq!(owner.inner.state.lock().unwrap().session, second);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn voice_cleanup_capture_rejects_connection_acquired_after_empty_capture() {
    let (owner, fake) = fixture();
    let original = owner.capture_voice_target(PRODUCER).await.unwrap();
    *fake.physical_target.lock().unwrap() =
        Some(crate::usb_at_transport::PhysicalTarget::synthetic_for_tests());
    opened(&owner).await;
    assert!(
        owner
            .validate_voice_cleanup_capture(PRODUCER, &original)
            .await
            .is_err()
    );
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn accepted_voice_cleanup_right_can_acquire_and_physically_release_after_revocation() {
    let gate = crate::module_recovery::Gate::default();
    let (owner, fake) = fixture();
    let owner = owner.with_recovery_gate_for_tests(gate.clone());
    *fake.physical_target.lock().unwrap() =
        Some(crate::usb_at_transport::PhysicalTarget::synthetic_for_tests());
    opened(&owner).await;
    let capture = owner.capture_voice_target(PRODUCER).await.unwrap();
    let accepted = gate.admit_operation(PRODUCER).unwrap();
    gate.revoke_for_tests();
    let lease = owner
        .acquire_voice_transition_captured_with_right(PRODUCER, capture, &accepted.right())
        .await
        .unwrap();
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    lease.release_confirmed().await.unwrap();
    assert!(owner.inner.state.lock().unwrap().lease.is_none());
    drop(accepted);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn active_description_tracks_the_single_usb_owner_lifecycle() {
    let (owner, _) = fixture();
    assert_eq!(owner.active_description(), None);
    let session = opened(&owner).await;
    assert_eq!(
        owner.active_description().as_deref(),
        Some("isolated fake USB AT")
    );

    owner.close(close(&session, "")).await.unwrap();
    assert_eq!(owner.active_description(), None);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn voice_target_empty_capture_does_not_open_and_matching_owner_can_settle_lease() {
    let (owner, fake) = fixture();
    let capture = owner.capture_voice_target(PRODUCER).await.unwrap();
    assert!(capture.physical_target().is_none());
    assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
    assert!(fake.commands.lock().unwrap().is_empty());
    let cloned = owner.clone();
    let lease = cloned
        .acquire_voice_transition_captured(PRODUCER, capture)
        .await
        .unwrap();
    assert!(owner.inner.state.lock().unwrap().lease.is_some());
    lease.release_confirmed().await.unwrap();
    let state = owner.inner.state.lock().unwrap();
    assert!(state.lease.is_none() && !state.busy && !state.revoking);
    assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn voice_target_opened_transport_without_native_proof_is_not_a_global_fallback() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    let before = owner.inner.state.lock().unwrap().lease_high_water;
    assert_eq!(
        owner.capture_voice_target(PRODUCER).await.unwrap_err().code,
        "usb_at_target_unconfirmed"
    );
    let state = owner.inner.state.lock().unwrap();
    assert_eq!(state.session, session);
    assert_eq!(state.lease_high_water, before);
    assert!(state.lease.is_none() && !state.revoking);
    drop(state);
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    assert!(fake.commands.lock().unwrap().is_empty());
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn voice_target_read_only_capture_keeps_existing_sim_lease_and_transition_still_rejects() {
    let (owner, fake) = fixture();
    *fake.physical_target.lock().unwrap() =
        Some(crate::usb_at_transport::PhysicalTarget::synthetic_for_tests());
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "existing-sim", 1, Purpose::Sim))
        .await
        .unwrap();
    let captured = owner.capture_voice_target(PRODUCER).await.unwrap();
    assert!(captured.physical_target().is_some());
    let error = match owner
        .acquire_voice_transition_captured(PRODUCER, captured)
        .await
    {
        Ok(_) => panic!("metadata capture must not supersede an existing SIM operation"),
        Err(error) => error,
    };
    assert_eq!(error.code, Failure::busy().code);
    let state = owner.inner.state.lock().unwrap();
    assert_eq!(state.lease.as_ref().unwrap().id, "existing-sim");
    assert_eq!(state.lease_high_water, 1);
    assert_eq!(state.session, session);
    assert!(!state.revoking);
    drop(state);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    assert!(fake.commands.lock().unwrap().is_empty());
    owner
        .release(release(&session, "existing-sim", 1))
        .await
        .unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn voice_target_new_open_between_capture_and_lease_is_rejected_without_close() {
    let (owner, fake) = fixture();
    let capture = owner.capture_voice_target(PRODUCER).await.unwrap();
    let session = opened(&owner).await;
    let error = match owner
        .acquire_voice_transition_captured(PRODUCER, capture)
        .await
    {
        Ok(_) => panic!("a new AT connection cannot satisfy an empty capture"),
        Err(error) => error,
    };
    assert_eq!(error.code, Failure::stale().code);
    let state = owner.inner.state.lock().unwrap();
    assert_eq!(state.session, session);
    assert!(state.lease.is_none() && !state.revoking);
    drop(state);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn voice_target_different_owner_with_same_producer_and_empty_session_cannot_borrow_capture() {
    let (first, first_fake) = fixture();
    let (second, second_fake) = fixture();
    let capture = first.capture_voice_target(PRODUCER).await.unwrap();
    let error = match second
        .acquire_voice_transition_captured(PRODUCER, capture)
        .await
    {
        Ok(_) => panic!("same producer text is not the captured AT owner"),
        Err(error) => error,
    };
    assert_eq!(error.code, Failure::stale().code);
    for (owner, fake) in [(first, first_fake), (second, second_fake)] {
        assert!(owner.inner.state.lock().unwrap().lease.is_none());
        assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
        assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn voice_target_old_empty_capture_is_not_revalidated_by_open_then_close() {
    let (owner, fake) = fixture();
    let capture = owner.capture_voice_target(PRODUCER).await.unwrap();
    let session = opened(&owner).await;
    owner.close(close(&session, "")).await.unwrap();
    assert!(owner.inner.state.lock().unwrap().session.is_empty());
    let error = match owner
        .acquire_voice_transition_captured(PRODUCER, capture)
        .await
    {
        Ok(_) => panic!("an empty session string must not erase connection history"),
        Err(error) => error,
    };
    assert_eq!(error.code, Failure::stale().code);
    assert!(owner.inner.state.lock().unwrap().lease.is_none());
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn voice_target_read_only_capture_during_stop_does_not_admit_a_new_transition() {
    let (owner, fake) = fixture();
    assert_eq!(
        owner
            .capture_voice_target("other-producer")
            .await
            .unwrap_err()
            .code,
        Failure::stale().code
    );
    owner.shutdown().await.unwrap();
    let captured = owner.capture_voice_target(PRODUCER).await.unwrap();
    assert!(captured.physical_target().is_none());
    let error = match owner
        .acquire_voice_transition_captured(PRODUCER, captured)
        .await
    {
        Ok(_) => panic!("read-only capture cannot authorize startup after stopping"),
        Err(error) => error,
    };
    assert_eq!(error.code, Failure::stopped().code);
    assert!(owner.inner.state.lock().unwrap().lease.is_none());
    assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
    assert!(fake.commands.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn voice_target_captured_lease_cancel_waits_for_actual_transport_drop_then_releases() {
    let (owner, fake) = fixture();
    *fake.physical_target.lock().unwrap() =
        Some(crate::usb_at_transport::PhysicalTarget::synthetic_for_tests());
    opened(&owner).await;
    let capture = owner.capture_voice_target(PRODUCER).await.unwrap();
    assert!(capture.physical_target().is_some());
    let gate = Arc::new(Gate::default());
    *fake.drop_gate.lock().unwrap() = Some(gate.clone());
    let copied = owner.clone();
    let task = tokio::spawn(async move {
        copied
            .acquire_voice_transition_captured(PRODUCER, capture)
            .await
    });
    gate.entered.notified().await;
    task.abort();
    assert!(owner.inner.state.lock().unwrap().lease.is_some());
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    gate.release();
    eventually(|| {
        let state = owner.inner.state.lock().unwrap();
        state.lease.is_none() && !state.busy && !state.revoking
    })
    .await;
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert!(fake.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn voice_target_capture_from_retired_producer_cannot_install_in_replacement() {
    let (owner, fake) = fixture();
    let capture = owner.capture_voice_target(PRODUCER).await.unwrap();
    owner
        .replace_producer("replacement-producer".into())
        .await
        .unwrap();
    let error = match owner
        .acquire_voice_transition_captured("replacement-producer", capture)
        .await
    {
        Ok(_) => panic!("retired producer capture cannot authorize its successor"),
        Err(error) => error,
    };
    assert_eq!(error.code, Failure::stale().code);
    assert!(owner.inner.state.lock().unwrap().lease.is_none());
    assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn voice_target_generation_exhaustion_does_not_install_an_unreturnable_lease() {
    for exhaust_epoch in [true, false] {
        let (owner, fake) = fixture();
        {
            let mut state = owner.inner.state.lock().unwrap();
            if exhaust_epoch {
                state.epoch = u64::MAX;
            } else {
                state.identity_generation = u64::MAX;
            }
        }
        let capture = owner.capture_voice_target(PRODUCER).await.unwrap();
        let before = owner.inner.state.lock().unwrap().lease_high_water;
        let error = match owner
            .acquire_voice_transition_captured(PRODUCER, capture)
            .await
        {
            Ok(_) => panic!("exhausted generation must reject before lease installation"),
            Err(error) => error,
        };
        assert_eq!(error.code, Failure::stopped().code);
        let state = owner.inner.state.lock().unwrap();
        assert_eq!(state.lease_high_water, before);
        assert!(state.lease.is_none() && !state.busy && !state.revoking);
        assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn sms_submit_uses_bound_card_and_rust_owned_pdu_mode_and_cmgs() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    fake.responses.lock().unwrap().extend([
        Ok("+QCCID: \"8988212345678901234\"\r\nOK".into()),
        Ok("OK".into()),
        Ok("+CMGS: 42\r\nOK".into()),
    ]);
    owner
        .acquire(acquire(&session, "sms-submit-lease", 1, Purpose::Sim))
        .await
        .unwrap();
    let response = dispatch(
        &format!("{PREFIX}submit-sms"),
        &serde_json::to_vec(&json!({
            "producer_id":PRODUCER,
            "session_id":session,
            "lease_id":"sms-submit-lease",
            "recipient":"+12025550100",
            "message":"hello",
            "expected_iccid":"8988212345678901234"
        }))
        .unwrap(),
        &owner,
        false,
    )
    .await
    .unwrap();
    assert_eq!(response, json!({"accepted_segments":1,"complete":true}));
    let commands = fake.commands.lock().unwrap().clone();
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[0], "AT+QCCID");
    assert_eq!(commands[1], "AT+CMGF=0");
    let tpdu_length: usize = commands[2]
        .strip_prefix("AT+CMGS=")
        .unwrap()
        .parse()
        .unwrap();
    let follow_ups = fake.follow_ups.lock().unwrap();
    let payload = follow_ups[2].as_deref().unwrap();
    assert_eq!(payload.first(), Some(&b'0'));
    assert_eq!(payload.last(), Some(&0x1a));
    assert_eq!((payload.len() - 3) / 2, tpdu_length);
    assert!(
        payload[..payload.len() - 1]
            .iter()
            .all(u8::is_ascii_hexdigit)
    );
    drop(follow_ups);
    owner
        .release(release(&session, "sms-submit-lease", 1))
        .await
        .unwrap();
    owner.close(close(&session, "")).await.unwrap();
}

#[tokio::test]
async fn rust_owned_active_sms_submission_reuses_only_the_current_sim_lease() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    fake.responses.lock().unwrap().extend([
        Ok("+QCCID: \"8988212345678901234\"\r\nOK".into()),
        Ok("OK".into()),
        Ok("+CMGS: 42\r\nOK".into()),
    ]);
    owner
        .acquire(acquire(&session, "sms-active-lease", 1, Purpose::Sim))
        .await
        .unwrap();
    let identity_generation = cache_module_identity(&owner);
    assert_eq!(
        owner.active_sim_identity_generation("8988212345678901234"),
        Some(identity_generation)
    );
    assert_eq!(
        owner.active_sim_identity_generation("8988212345678909999"),
        None
    );

    let outcome = owner
        .submit_active_sms(
            "+12025550100".into(),
            "hello".into(),
            identity_generation,
            false,
        )
        .await
        .unwrap();
    assert_eq!(outcome.accepted_segments, 1);
    assert!(outcome.complete);
    assert_eq!(
        fake.commands.lock().unwrap().as_slice(),
        ["AT+QCCID", "AT+CMGF=0", "AT+CMGS=18"]
    );

    owner
        .release(release(&session, "sms-active-lease", 1))
        .await
        .unwrap();
    owner.close(close(&session, "")).await.unwrap();
}

#[tokio::test]
async fn rust_owned_active_sms_submission_refuses_missing_or_voice_lease_before_io() {
    let (owner, fake) = fixture();
    assert_eq!(
        owner
            .submit_active_sms("+12025550100".into(), "hello".into(), 1, false)
            .await
            .unwrap_err()
            .code,
        "usb_at_stale_session"
    );
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "voice-active-lease", 1, Purpose::Voice))
        .await
        .unwrap();
    assert_eq!(
        owner
            .submit_active_sms("+12025550100".into(), "hello".into(), 1, false)
            .await
            .unwrap_err()
            .code,
        "usb_at_stale_session"
    );
    assert!(fake.commands.lock().unwrap().is_empty());
    owner
        .release(release(&session, "voice-active-lease", 1))
        .await
        .unwrap();
    owner.close(close(&session, "")).await.unwrap();
}

#[tokio::test]
async fn sms_submit_rejects_card_change_and_radio_inhibit_before_sms_write() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    fake.responses
        .lock()
        .unwrap()
        .push_back(Ok("+QCCID: \"8988212345678901234\"\r\nOK".into()));
    owner
        .acquire(acquire(&session, "sms-submit-lease", 1, Purpose::Sim))
        .await
        .unwrap();
    let changed = owner
        .submit_sms(
            SmsSubmitRequest {
                producer_id: PRODUCER.into(),
                session_id: session.clone(),
                lease_id: "sms-submit-lease".into(),
                recipient: "+12025550100".into(),
                message: "hello".into(),
                expected_iccid: "8988212345678909999".into(),
            },
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(changed.code, "usb_sms_card_changed");
    assert_eq!(*fake.commands.lock().unwrap(), ["AT+QCCID"]);

    owner
        .set_radio_inhibit(PRODUCER, "vowifi-inhibit", true)
        .unwrap();
    let inhibited = owner
        .submit_sms(
            SmsSubmitRequest {
                producer_id: PRODUCER.into(),
                session_id: session.clone(),
                lease_id: "sms-submit-lease".into(),
                recipient: "+12025550100".into(),
                message: "hello".into(),
                expected_iccid: "8988212345678901234".into(),
            },
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(inhibited.code, "usb_at_busy");
    assert_eq!(*fake.commands.lock().unwrap(), ["AT+QCCID"]);
    owner
        .set_radio_inhibit(PRODUCER, "vowifi-inhibit", false)
        .unwrap();
    owner
        .release(release(&session, "sms-submit-lease", 1))
        .await
        .unwrap();
    owner.close(close(&session, "")).await.unwrap();
}

#[tokio::test]
async fn sms_submit_is_rejected_with_stopping_code_before_hardware_io() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    let failure = owner
        .submit_sms(
            SmsSubmitRequest {
                producer_id: PRODUCER.into(),
                session_id: session.clone(),
                lease_id: "sms-submit-stop".into(),
                recipient: "+12025550100".into(),
                message: "hello".into(),
                expected_iccid: "8988212345678901234".into(),
            },
            true,
        )
        .await
        .unwrap_err();
    assert_eq!(failure.code, "usb_at_stopping");
    assert!(fake.commands.lock().unwrap().is_empty());
    owner.close(close(&session, "")).await.unwrap();
}

fn configure(owner: &Owner) {
    let config_seq = owner.inner.state.lock().unwrap().config_high_water + 1;
    owner
        .configure(
            ConfigureRequest::Sms {
                producer_id: PRODUCER.into(),
                config_seq,
                serial: false,
                poll_interval_ms: 8000,
            },
            false,
        )
        .unwrap();
}
fn acquire(session: &str, id: &str, seq: u64, purpose: Purpose) -> LeaseRequest {
    LeaseRequest {
        producer_id: PRODUCER.into(),
        session_id: session.into(),
        lease_id: id.into(),
        lease_seq: seq,
        purpose,
    }
}
fn release(session: &str, id: &str, seq: u64) -> ReleaseRequest {
    ReleaseRequest {
        producer_id: PRODUCER.into(),
        session_id: session.into(),
        lease_id: id.into(),
        lease_seq: seq,
    }
}
fn command(session: &str, lease: &str, text: &str) -> CommandRequest {
    CommandRequest {
        producer_id: PRODUCER.into(),
        session_id: session.into(),
        lease_id: lease.into(),
        command: text.into(),
        follow_up_base64: None,
        timeout_ms: 1000,
        max_bytes: 4096,
    }
}
fn close(session: &str, lease: &str) -> CloseRequest {
    CloseRequest {
        producer_id: PRODUCER.into(),
        session_id: session.into(),
        lease_id: lease.into(),
    }
}
async fn eventually(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("deterministic state transition did not complete");
}

#[tokio::test]
async fn handle_is_opened_once_and_close_fences_old_commands() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    assert_eq!(opened(&owner).await, session);
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
    owner.close(close(&session, "")).await.unwrap();
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    owner.close(close(&session, "")).await.unwrap();
    assert_eq!(
        owner
            .command(command(&session, "", "AT"), false)
            .await
            .unwrap_err()
            .code,
        "usb_at_stale_session"
    );
    let fresh = opened(&owner).await;
    assert_ne!(fresh, session);
    assert!(owner.close(close(&session, "")).await.is_err());
    assert_eq!(owner.status(PRODUCER).unwrap()["session_id"], fresh);
    assert!(fake.commands.lock().unwrap().is_empty());
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn lease_conflicts_return_immediately_and_holder_can_complete() {
    let (owner, fake) = fixture();
    configure(&owner);
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "lease-one", 1, Purpose::Sim))
        .await
        .unwrap();
    owner
        .acquire(acquire(&session, "lease-one", 1, Purpose::Sim))
        .await
        .unwrap();
    // More than the private listener's capacity must not retain waiting RPCs.
    for number in 2..80 {
        let reply = tokio::time::timeout(
            Duration::from_millis(100),
            owner.acquire(acquire(
                &session,
                &format!("lease-{number}"),
                number,
                Purpose::Sim,
            )),
        )
        .await
        .unwrap();
        assert_eq!(reply.unwrap_err().code, "usb_at_busy");
    }
    assert_eq!(owner.snapshot(PRODUCER).await.unwrap()["mode"], "busy");
    assert_eq!(
        owner
            .command(command(&session, "", "AT"), false)
            .await
            .unwrap_err()
            .code,
        "usb_at_busy"
    );
    owner
        .command(command(&session, "lease-one", "AT+CCHO=\"A0\""), false)
        .await
        .unwrap();
    owner
        .release(release(&session, "lease-one", 1))
        .await
        .unwrap();
    owner
        .acquire(acquire(&session, "lease-two", 2, Purpose::Sim))
        .await
        .unwrap();
    owner
        .release(release(&session, "lease-two", 2))
        .await
        .unwrap();
    assert_eq!(fake.commands.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn release_before_acquire_prevents_resurrection_without_tombstones() {
    let (owner, _) = fixture();
    let session = opened(&owner).await;
    owner
        .release(release(&session, "never-arrived", 40))
        .await
        .unwrap();
    assert_eq!(
        owner
            .acquire(acquire(&session, "never-arrived", 40, Purpose::Sim))
            .await
            .unwrap_err()
            .code,
        "usb_at_stale_session"
    );
    assert!(
        owner
            .acquire(acquire(&session, "older", 39, Purpose::Sim))
            .await
            .is_err()
    );
    owner
        .acquire(acquire(&session, "next", 41, Purpose::Sim))
        .await
        .unwrap();
    owner.release(release(&session, "next", 41)).await.unwrap();
    assert!(
        owner
            .acquire(acquire(&session, "next", 41, Purpose::Sim))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn newer_pending_release_does_not_revoke_an_existing_holder() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "winner", 1, Purpose::Sim))
        .await
        .unwrap();
    owner
        .release(release(&session, "canceled-waiter", 10))
        .await
        .unwrap();
    owner
        .acquire(acquire(&session, "winner", 1, Purpose::Sim))
        .await
        .unwrap();
    owner
        .command(command(&session, "winner", "AT"), false)
        .await
        .unwrap();
    owner.release(release(&session, "winner", 1)).await.unwrap();
    assert!(
        owner
            .acquire(acquire(&session, "canceled-waiter", 10, Purpose::Sim))
            .await
            .is_err()
    );
    assert_eq!(fake.commands.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn late_close_cannot_revoke_a_new_lease_on_the_same_handle() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "old", 1, Purpose::Sim))
        .await
        .unwrap();
    owner.release(release(&session, "old", 1)).await.unwrap();
    owner
        .acquire(acquire(&session, "current", 2, Purpose::Sim))
        .await
        .unwrap();
    assert!(owner.close(close(&session, "old")).await.is_err());
    assert!(owner.close(close(&session, "")).await.is_err());
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    owner
        .command(command(&session, "current", "AT"), false)
        .await
        .unwrap();
    owner.close(close(&session, "current")).await.unwrap();
    owner
        .release(release(&session, "current", 2))
        .await
        .unwrap();
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn voice_reservation_works_without_handle_and_blocks_even_holder_open() {
    let (owner, fake) = fixture();
    configure(&owner);
    owner
        .acquire(acquire("", "voice", 1, Purpose::Voice))
        .await
        .unwrap();
    assert_eq!(
        owner
            .open(
                OpenRequest {
                    producer_id: PRODUCER.into(),
                    lease_id: "voice".into()
                },
                false
            )
            .await
            .unwrap_err()
            .code,
        "usb_at_busy"
    );
    assert_eq!(owner.snapshot(PRODUCER).await.unwrap()["mode"], "busy");
    assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
    owner.release(release("", "voice", 1)).await.unwrap();
    opened(&owner).await;
}

#[tokio::test]
async fn rust_voice_transition_lease_retires_old_handle_and_blocks_sim_until_release() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;

    let lease = owner.acquire_voice_transition(PRODUCER).await.unwrap();
    assert_eq!(owner.status(PRODUCER).unwrap()["open"], false);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        owner
            .acquire(acquire(
                "",
                "sim-during-voice",
                lease.sequence + 1,
                Purpose::Sim,
            ))
            .await
            .unwrap_err()
            .code,
        "usb_at_busy"
    );

    lease.release().await.unwrap();
    let sequence = owner.inner.state.lock().unwrap().lease_high_water + 1;
    owner
        .acquire(acquire("", "sim-after-voice", sequence, Purpose::Sim))
        .await
        .unwrap();
    owner
        .release(release("", "sim-after-voice", sequence))
        .await
        .unwrap();
    let replacement = opened(&owner).await;
    assert_ne!(session, replacement);
}

#[tokio::test]
async fn active_sim_sms_binding_is_fenced_by_session_generation_and_lease() {
    let (owner, _) = fixture();
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "sms-route", 1, Purpose::Sim))
        .await
        .unwrap();
    let generation = cache_module_identity(&owner);
    let binding = owner
        .active_sim_identity_binding("8988212345678901234")
        .unwrap();
    assert_eq!(binding, (session.clone(), generation));
    assert_eq!(
        owner.active_sim_identity_binding_current(),
        Some((session.clone(), generation))
    );
    assert!(owner.has_active_sim_identity_binding(&session, generation));
    assert!(!owner.has_active_sim_identity_binding(&session, generation + 1));

    owner
        .release(release(&session, "sms-route", 1))
        .await
        .unwrap();
    assert!(!owner.has_active_sim_identity_binding(&session, generation));
    assert!(owner.active_sim_identity_binding_current().is_none());
    owner.close(close(&session, "")).await.unwrap();
    let replacement = opened(&owner).await;
    assert_ne!(replacement, session);
    assert!(!owner.has_active_sim_identity_binding(&session, generation));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canceled_voice_lease_acquire_releases_after_blocked_transport_close() {
    let (owner, fake) = fixture();
    opened(&owner).await;
    let close_gate = Arc::new(Gate::default());
    *fake.drop_gate.lock().unwrap() = Some(close_gate.clone());

    let acquire_owner = owner.clone();
    let acquire_task =
        tokio::spawn(async move { acquire_owner.acquire_voice_transition(PRODUCER).await });
    close_gate.entered.notified().await;
    acquire_task.abort();
    assert!(owner.inner.state.lock().unwrap().lease.is_some());

    close_gate.release();
    eventually(|| {
        let state = owner.inner.state.lock().unwrap();
        state.lease.is_none() && !state.busy && !state.revoking
    })
    .await;
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert!(!owner.status(PRODUCER).unwrap()["leased"].as_bool().unwrap());
}

// Snapshot the installed native lease while acquisition is still closing the
// old handle, so release can race the actual transport Drop in these tests.
fn installed_voice_lease(owner: &Owner) -> VoiceTransitionLease {
    let state = owner.inner.state.lock().unwrap();
    let lease = state.lease.as_ref().unwrap();
    assert_eq!(lease.purpose, Purpose::Voice);
    VoiceTransitionLease {
        owner: owner.clone(),
        producer: state.producer.clone(),
        session: lease.session.clone(),
        lease_id: lease.id.clone(),
        sequence: lease.sequence,
        released: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_voice_release_waits_for_blocked_close_without_observation_deadline() {
    let (owner, fake) = fixture();
    opened(&owner).await;
    let gate = Arc::new(Gate::default());
    *fake.drop_gate.lock().unwrap() = Some(gate.clone());
    let cloned = owner.clone();
    let acquire = tokio::spawn(async move { cloned.acquire_voice_transition(PRODUCER).await });
    gate.entered.notified().await;
    let lease = installed_voice_lease(&owner);
    // An expired RPC observation stops waiting, while the native owner must
    // still join the same live transport close without imposing that deadline.
    assert_eq!(
        owner
            .release_with_deadline(
                release(&lease.session, &lease.lease_id, lease.sequence),
                Some(Instant::now()),
            )
            .await
            .unwrap_err()
            .code,
        "usb_at_wait_timeout"
    );
    let release = tokio::spawn(async move { lease.release_confirmed().await });
    eventually(|| {
        owner
            .inner
            .state
            .lock()
            .unwrap()
            .lease
            .as_ref()
            .unwrap()
            .releasing
    })
    .await;
    assert!(!release.is_finished());
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    gate.release();
    release.await.unwrap().unwrap();
    acquire
        .await
        .unwrap()
        .unwrap()
        .release_confirmed()
        .await
        .unwrap();
    let state = owner.inner.state.lock().unwrap();
    assert!(!state.busy && !state.revoking && state.lease.is_none());
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_voice_release_waits_when_logical_lease_disappears_before_close() {
    let (owner, fake) = fixture();
    opened(&owner).await;
    let gate = Arc::new(Gate::default());
    *fake.drop_gate.lock().unwrap() = Some(gate.clone());
    let cloned = owner.clone();
    let acquire = tokio::spawn(async move { cloned.acquire_voice_transition(PRODUCER).await });
    gate.entered.notified().await;
    let lease = installed_voice_lease(&owner);
    owner.inner.state.lock().unwrap().lease = None;
    let mut release = tokio::spawn(async move { lease.release_confirmed().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut release)
            .await
            .is_err()
    );
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    gate.release();
    release.await.unwrap().unwrap();
    acquire
        .await
        .unwrap()
        .unwrap()
        .release_confirmed()
        .await
        .unwrap();
    let state = owner.inner.state.lock().unwrap();
    assert!(!state.busy && !state.revoking && state.lease.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canceled_confirmed_voice_release_retains_cleanup_until_close_finishes() {
    let (owner, fake) = fixture();
    opened(&owner).await;
    let gate = Arc::new(Gate::default());
    *fake.drop_gate.lock().unwrap() = Some(gate.clone());
    let cloned = owner.clone();
    let acquire = tokio::spawn(async move { cloned.acquire_voice_transition(PRODUCER).await });
    gate.entered.notified().await;
    let lease = installed_voice_lease(&owner);
    let release = tokio::spawn(async move { lease.release_confirmed().await });
    eventually(|| {
        owner
            .inner
            .state
            .lock()
            .unwrap()
            .lease
            .as_ref()
            .unwrap()
            .releasing
    })
    .await;
    release.abort();
    assert!(owner.inner.state.lock().unwrap().busy);
    gate.release();
    acquire
        .await
        .unwrap()
        .unwrap()
        .release_confirmed()
        .await
        .unwrap();
    eventually(|| {
        let state = owner.inner.state.lock().unwrap();
        !state.busy && !state.revoking && state.lease.is_none()
    })
    .await;
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_voice_release_on_retirement_waits_for_old_close_before_error() {
    for shutdown in [false, true] {
        let (owner, fake) = fixture();
        opened(&owner).await;
        let gate = Arc::new(Gate::default());
        *fake.drop_gate.lock().unwrap() = Some(gate.clone());
        let cloned = owner.clone();
        let acquire = tokio::spawn(async move { cloned.acquire_voice_transition(PRODUCER).await });
        gate.entered.notified().await;
        let lease = installed_voice_lease(&owner);
        let cloned = owner.clone();
        let retire = tokio::spawn(async move {
            if shutdown {
                cloned.shutdown().await
            } else {
                cloned.replace_producer("replacement".into()).await
            }
        });
        eventually(|| {
            let state = owner.inner.state.lock().unwrap();
            state.stopping || state.producer != PRODUCER
        })
        .await;
        let mut release = tokio::spawn(async move { lease.release_confirmed().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut release)
                .await
                .is_err()
        );
        assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
        gate.release();
        let error = release.await.unwrap().unwrap_err();
        assert_eq!(
            error.code,
            if shutdown {
                "usb_at_stopping"
            } else {
                "usb_at_stale_session"
            }
        );
        retire.await.unwrap().unwrap();
        // A close already executing can finish successfully after its epoch
        // is retired; any delivered old lease still has a settled stale result.
        if let Ok(old_lease) = acquire.await.unwrap() {
            assert_eq!(
                old_lease.release_confirmed().await.unwrap_err().code,
                error.code
            );
        }
        let state = owner.inner.state.lock().unwrap();
        assert!(!state.busy && !state.revoking);
        assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn explicitly_failed_confirmed_voice_release_does_not_spawn_drop_cleanup() {
    let (owner, _) = fixture();
    let lease = owner.acquire_voice_transition(PRODUCER).await.unwrap();
    owner.replace_producer("replacement".into()).await.unwrap();
    let tasks = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    assert_eq!(
        lease.release_confirmed().await.unwrap_err().code,
        "usb_at_stale_session"
    );
    assert_eq!(
        tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks(),
        tasks
    );
}

#[tokio::test]
async fn voice_acquire_retires_old_handle_before_ack_and_reopen_has_new_session() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "voice", 1, Purpose::Voice))
        .await
        .unwrap();
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert_eq!(owner.status(PRODUCER).unwrap()["open"], false);
    owner
        .acquire(acquire(&session, "voice", 1, Purpose::Voice))
        .await
        .unwrap();
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    owner.release(release(&session, "voice", 1)).await.unwrap();
    let next = opened(&owner).await;
    assert_ne!(session, next);
    assert!(
        owner
            .command(command(&session, "", "AT+CMGS=1"), false)
            .await
            .is_err()
    );
    assert!(fake.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sim_reservation_can_open_handle_and_release_original_empty_session() {
    let (owner, _) = fixture();
    owner
        .acquire(acquire("", "sim", 1, Purpose::Sim))
        .await
        .unwrap();
    let value = owner
        .open(
            OpenRequest {
                producer_id: PRODUCER.into(),
                lease_id: "sim".into(),
            },
            false,
        )
        .await
        .unwrap();
    owner
        .command(
            command(value["session_id"].as_str().unwrap(), "sim", "AT"),
            false,
        )
        .await
        .unwrap();
    owner.release(release("", "sim", 1)).await.unwrap();
    assert!(!owner.status(PRODUCER).unwrap()["leased"].as_bool().unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canceled_command_does_not_release_io_or_ack_release_early() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "sending", 1, Purpose::Sim))
        .await
        .unwrap();
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    let cloned = owner.clone();
    let request = command(&session, "sending", "AT+CMGS=1");
    let task = tokio::spawn(async move { cloned.command(request, false).await });
    gate.entered.notified().await;
    task.abort();
    assert!(owner.inner.state.lock().unwrap().busy);
    let cloned = owner.clone();
    let request = release(&session, "sending", 1);
    let release_task = tokio::spawn(async move { cloned.release(request).await });
    eventually(|| {
        owner
            .inner
            .state
            .lock()
            .unwrap()
            .lease
            .as_ref()
            .is_some_and(|lease| lease.releasing)
    })
    .await;
    assert!(!release_task.is_finished());
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    assert!(
        owner
            .acquire(acquire(&session, "other", 2, Purpose::Sim))
            .await
            .is_err()
    );
    gate.release();
    release_task.await.unwrap().unwrap();
    assert!(!owner.inner.state.lock().unwrap().busy);
    owner
        .acquire(acquire(&session, "other", 2, Purpose::Sim))
        .await
        .unwrap();
    assert_eq!(fake.commands.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn producer_replacement_revokes_immediately_but_joins_real_io() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    let cloned = owner.clone();
    let request = command(&session, "", "AT");
    let command_task = tokio::spawn(async move { cloned.command(request, false).await });
    gate.entered.notified().await;
    let cloned = owner.clone();
    let replacement =
        tokio::spawn(async move { cloned.replace_producer("new-producer".into()).await });
    eventually(|| owner.inner.state.lock().unwrap().producer == "new-producer").await;
    assert!(!replacement.is_finished());
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    assert!(owner.status(PRODUCER).is_err());
    assert!(
        owner
            .open(
                OpenRequest {
                    producer_id: "new-producer".into(),
                    lease_id: String::new()
                },
                false
            )
            .await
            .is_err()
    );
    gate.release();
    let error = command_task.await.unwrap().unwrap_err();
    assert!(error.uncertain);
    replacement.await.unwrap().unwrap();
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert!(owner.inner.state.lock().unwrap().sms.is_none());
    assert_eq!(owner.inner.state.lock().unwrap().lease_high_water, 0);
    owner
        .open(
            OpenRequest {
                producer_id: "new-producer".into(),
                lease_id: String::new(),
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(fake.opens.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_transport_exit_after_http_cancellation() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    let cloned = owner.clone();
    let task =
        tokio::spawn(async move { cloned.command(command(&session, "", "AT"), false).await });
    gate.entered.notified().await;
    task.abort();
    let cloned = owner.clone();
    let shutdown = tokio::spawn(async move { cloned.shutdown().await });
    eventually(|| owner.inner.state.lock().unwrap().stopping).await;
    assert!(!shutdown.is_finished());
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    gate.release();
    shutdown.await.unwrap().unwrap();
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn snapshot_requires_configuration_and_inhibit_only_blocks_collection() {
    let (owner, fake) = fixture();
    assert_eq!(owner.snapshot(PRODUCER).await.unwrap()["mode"], "busy");
    owner
        .configure(
            ConfigureRequest::Sms {
                producer_id: PRODUCER.into(),
                config_seq: 1,
                serial: true,
                poll_interval_ms: 2000,
            },
            false,
        )
        .unwrap();
    assert_eq!(owner.snapshot(PRODUCER).await.unwrap()["mode"], "serial");
    assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
    configure(&owner);
    let session = opened(&owner).await;
    owner
        .configure(
            ConfigureRequest::Inhibit {
                producer_id: PRODUCER.into(),
                inhibit_seq: 1,
                token: "ims-session".into(),
                enabled: true,
            },
            false,
        )
        .unwrap();
    assert_eq!(owner.snapshot(PRODUCER).await.unwrap()["mode"], "busy");
    owner
        .command(command(&session, "", "AT+CSIM=4,\"00A4\""), false)
        .await
        .unwrap();
    owner
        .configure(
            ConfigureRequest::Inhibit {
                producer_id: PRODUCER.into(),
                inhibit_seq: 1,
                token: "ims-session".into(),
                enabled: false,
            },
            false,
        )
        .unwrap();
    assert!(owner.inner.state.lock().unwrap().inhibits.is_empty());
}

#[tokio::test]
async fn native_snapshot_runs_one_atomic_sm_then_me_transaction() {
    let (owner, fake) = fixture_with_adopt(true);
    configure(&owner);
    fake.responses.lock().unwrap().extend([
        Ok("+QCCID: 8988212345678901234\r\nOK".into()),
        Ok("OK".into()),
        Ok("OK".into()),
        Ok("+CMGL: 1,0,,1\r\n00\r\nOK".into()),
        Ok("OK".into()),
        Ok("OK".into()),
        Ok("+QCCID: 8988212345678901234\r\nOK".into()),
    ]);
    let value = owner.snapshot(PRODUCER).await.unwrap();
    assert_eq!(value["mode"], "usb");
    assert_eq!(value["adopt"], true);
    assert_eq!(value["storages"][0]["storage"], "SM");
    assert_eq!(value["storages"][1]["storage"], "ME");
    assert_eq!(value["card_identity"], "8988212345678901234");
    assert_eq!(
        *fake.commands.lock().unwrap(),
        [
            "AT+QCCID",
            "AT+CMGF=0",
            "AT+CPMS=\"SM\",\"SM\",\"SM\"",
            "AT+CMGL=4",
            "AT+CPMS=\"ME\",\"ME\",\"ME\"",
            "AT+CMGL=4",
            "AT+QCCID"
        ]
    );
}

#[tokio::test]
async fn snapshot_preserves_per_storage_errors_and_never_deletes() {
    let (owner, fake) = fixture();
    configure(&owner);
    fake.responses.lock().unwrap().extend([
        Ok("ERROR".into()),
        Ok("OK".into()),
        Ok("ERROR".into()),
        Ok("OK".into()),
        Ok("+CMGL: 1".into()),
        Ok("ERROR".into()),
    ]);
    let value = owner.snapshot(PRODUCER).await.unwrap();
    assert_eq!(value["storages"][0]["error_code"], "select_failed");
    assert_eq!(value["storages"][1]["error_code"], "incomplete_response");
    assert_eq!(value["card_identity"], "");
    // Explicit ERROR replies are terminal and allow the other storage to be
    // inspected; the final partial CMGL prevents the queued QCCID entirely.
    assert_eq!(fake.commands.lock().unwrap().len(), 5);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert!(
        fake.commands
            .lock()
            .unwrap()
            .iter()
            .all(|command| !command.contains("CMGD"))
    );
}

#[tokio::test]
async fn snapshot_keeps_completed_storage_when_later_storage_disconnects() {
    let (owner, fake) = fixture();
    configure(&owner);
    fake.responses.lock().unwrap().extend([
        Ok("+QCCID: 8988212345678901234\r\nOK".into()),
        Ok("OK".into()),
        Ok("OK".into()),
        Ok("+CMGL: 1,0,,1\r\n00\r\nOK".into()),
        Ok("OK".into()),
        Err(TransportError {
            code: "usb_at_disconnected",
            message: "fake disconnected".into(),
            response: String::new(),
            uncertain: true,
        }),
    ]);
    let snapshot = owner.snapshot(PRODUCER).await.unwrap();
    assert_eq!(snapshot["mode"], "usb");
    assert_eq!(
        snapshot["storages"][0]["response"],
        "+CMGL: 1,0,,1\r\n00\r\nOK"
    );
    assert_eq!(snapshot["storages"][1]["error_code"], "read_failed");
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert_eq!(fake.commands.lock().unwrap().len(), 6);
    assert_eq!(snapshot["card_identity"], "");
    assert_eq!(owner.status(PRODUCER).unwrap()["open"], false);
}

#[tokio::test]
async fn disconnect_closes_handle_and_old_command_never_reopens_or_replays() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    fake.responses
        .lock()
        .unwrap()
        .push_back(Err(TransportError {
            code: "usb_at_disconnected",
            message: "fake disconnected".into(),
            response: String::new(),
            uncertain: true,
        }));
    let error = owner
        .command(command(&session, "", "ATD123;"), false)
        .await
        .unwrap_err();
    assert!(error.uncertain);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert!(
        owner
            .command(command(&session, "", "ATD123;"), false)
            .await
            .is_err()
    );
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fake.commands.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn strict_wire_and_shutdown_allow_only_radio_restore() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    for body in [
        json!({"producer_id":PRODUCER,"unexpected":true}),
        json!({"producer_id":""}),
        json!({}),
    ] {
        assert!(
            dispatch(
                &format!("{PREFIX}status"),
                &serde_json::to_vec(&body).unwrap(),
                &owner,
                false
            )
            .await
            .is_err()
        );
    }
    for text in ["AT+CMGS=1", "AT+CGLA=1,4,00A4", "AT+CFUN=1,1", "ATD123;"] {
        assert_eq!(
            owner
                .command(command(&session, "", text), true)
                .await
                .unwrap_err()
                .code,
            "usb_at_stopping"
        );
    }
    assert!(
        owner
            .command(command(&session, "", "AT+CFUN=1\rATD123;"), true)
            .await
            .is_err()
    );
    for text in ["AT+CFUN?", "AT+CFUN=?"] {
        owner
            .command(command(&session, "", text), true)
            .await
            .unwrap();
    }
    assert_eq!(fake.commands.lock().unwrap().len(), 2);
    for text in ["AT+CFUN=0", "AT+CFUN=4"] {
        assert_eq!(
            owner
                .command(command(&session, "", text), true)
                .await
                .unwrap_err()
                .code,
            "radio_authorization_required"
        );
    }
    let mut request = command(&session, "", "AT+CFUN=4");
    request.follow_up_base64 = Some("AA==".into());
    assert!(owner.command(request, true).await.is_err());
    let mut request = command(&session, "", "AT");
    request.timeout_ms = 60_001;
    assert!(owner.command(request, false).await.is_err());
    owner.close(close(&session, "")).await.unwrap();
    assert!(
        owner
            .open(
                OpenRequest {
                    producer_id: PRODUCER.into(),
                    lease_id: String::new()
                },
                true
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn module_profile_check_reads_both_identifiers_under_one_owner_transaction() {
    let (owner, fake) = fixture();
    opened(&owner).await;
    let session_generation = cache_module_identity(&owner);
    fake.responses.lock().unwrap().extend([
        Ok("+QCCID: 8988212345678901234F\r\nOK".into()),
        Ok("234301234567890\r\nOK".into()),
    ]);
    let result = owner
        .sim_profile_check(SimProfileCheckRequest { session_generation }, false)
        .await
        .unwrap();
    assert_eq!(result["outcome"], "identity_matches");
    assert_eq!(
        fake.commands.lock().unwrap().as_slice(),
        &["AT+QCCID".to_string(), "AT+CIMI".to_string()]
    );
}

#[tokio::test]
async fn module_profile_check_waits_for_an_existing_sim_lease_before_reading() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    let session_generation = cache_module_identity(&owner);
    owner
        .acquire(acquire(&session, "profile-handoff", 1, Purpose::Sim))
        .await
        .unwrap();

    let checking_owner = owner.clone();
    let check = tokio::spawn(async move {
        checking_owner
            .sim_profile_check(SimProfileCheckRequest { session_generation }, false)
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !check.is_finished(),
        "identity check bypassed the active SIM lease"
    );
    assert!(fake.commands.lock().unwrap().is_empty());

    fake.responses.lock().unwrap().extend([
        Ok("+QCCID: 8988212345678901234F\r\nOK".into()),
        Ok("234301234567890\r\nOK".into()),
    ]);
    owner
        .release(release(&session, "profile-handoff", 1))
        .await
        .unwrap();
    let result = check.await.unwrap().unwrap();
    assert_eq!(result["outcome"], "identity_matches");
    assert_eq!(
        fake.commands.lock().unwrap().as_slice(),
        &["AT+QCCID".to_string(), "AT+CIMI".to_string()]
    );
}

#[tokio::test]
async fn module_profile_check_rejects_identity_change_and_malformed_requests() {
    let (owner, fake) = fixture();
    opened(&owner).await;
    let session_generation = cache_module_identity(&owner);
    fake.responses.lock().unwrap().extend([
        Ok("+QCCID: 8988212345678901235\r\nOK".into()),
        Ok("234301234567890\r\nOK".into()),
    ]);
    let result = owner
        .sim_profile_check(SimProfileCheckRequest { session_generation }, false)
        .await
        .unwrap();
    assert_eq!(result["outcome"], "identity_changed");
    let command_count = fake.commands.lock().unwrap().len();
    let error = owner
        .sim_profile_check(
            SimProfileCheckRequest {
                session_generation: 0,
            },
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "usb_at_invalid_request");
    assert_eq!(fake.commands.lock().unwrap().len(), command_count);
}

#[tokio::test]
async fn module_profile_check_rejects_a_stale_transport_generation_before_io() {
    let (owner, fake) = fixture();
    opened(&owner).await;
    let (current_epoch, current_generation) = {
        let state = owner.inner.state.lock().unwrap();
        (state.epoch, state.identity_generation)
    };
    cache_module_identity(&owner);
    detached(&owner.inner, current_epoch);
    owner.inner.port.lock().unwrap().take();
    owner.inner.state.lock().unwrap().next_open = None;
    let error = owner
        .sim_profile_check(
            SimProfileCheckRequest {
                session_generation: current_generation,
            },
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "usb_at_stale_session");
    assert!(fake.commands.lock().unwrap().is_empty());
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refreshed_module_identity_commit_invalidates_the_previous_snapshot() {
    let (owner, fake) = fixture();
    opened(&owner).await;
    let previous_generation = cache_module_identity(&owner);
    let current_generation = cache_module_identity(&owner);
    assert!(current_generation > previous_generation);
    let error = owner
        .sim_profile_check(
            SimProfileCheckRequest {
                session_generation: previous_generation,
            },
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "usb_at_stale_session");
    assert!(fake.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn module_aka_rejects_a_stale_transport_generation_before_apdu() {
    let (owner, fake) = fixture();
    opened(&owner).await;
    let stale_generation = cache_module_identity(&owner);
    let epoch = owner.inner.state.lock().unwrap().epoch;
    detached(&owner.inner, epoch);
    let error = owner
        .sim_aka(
            SimAkaRequest {
                application: "usim".into(),
                session_generation: stale_generation,
                rand: "11".repeat(16),
                autn: "22".repeat(16),
            },
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "usb_at_stale_session");
    assert!(fake.commands.lock().unwrap().is_empty());
}

fn cache_module_identity(owner: &Owner) -> u64 {
    let observed_generation = owner.inner.state.lock().unwrap().identity_generation;
    let response = owner
        .commit_module_identity(
            observed_generation,
            json!({"session_generation":observed_generation}),
            Zeroizing::new("8988212345678901234".into()),
            Zeroizing::new("234301234567890".into()),
        )
        .unwrap();
    response["session_generation"].as_u64().unwrap()
}

#[tokio::test]
async fn inhibit_cleanup_before_enable_prevents_late_reactivation() {
    let (owner, _) = fixture();
    configure(&owner);
    let inhibit = |token: &str, inhibit_seq, enabled| ConfigureRequest::Inhibit {
        producer_id: PRODUCER.into(),
        token: token.into(),
        inhibit_seq,
        enabled,
    };
    // A previous session is still active while cleanup for a newer, lost
    // enable arrives. It must not retire the unrelated active session.
    owner.configure(inhibit("active", 1, true), false).unwrap();
    owner.configure(inhibit("late", 2, false), false).unwrap();
    assert_eq!(
        owner
            .configure(inhibit("late", 2, true), false)
            .unwrap_err()
            .code,
        "usb_at_stale_session"
    );
    assert_eq!(
        owner
            .configure(inhibit("older", 1, true), false)
            .unwrap_err()
            .code,
        "usb_at_stale_session"
    );
    owner.configure(inhibit("active", 1, true), false).unwrap();
    assert_eq!(owner.inner.state.lock().unwrap().inhibits.len(), 1);
    assert_eq!(owner.snapshot(PRODUCER).await.unwrap()["mode"], "busy");
    // Reusing the same token with a different sequence is not an update and
    // cannot make its delayed, old cleanup retire this active reservation.
    assert!(owner.configure(inhibit("active", 3, true), false).is_err());
    owner.configure(inhibit("active", 3, false), false).unwrap();
    assert_eq!(
        owner.inner.state.lock().unwrap().inhibits.get("active"),
        Some(&1)
    );
    owner.configure(inhibit("active", 1, false), false).unwrap();
    owner.configure(inhibit("active", 1, false), false).unwrap();
    assert!(owner.inner.state.lock().unwrap().inhibits.is_empty());
    assert!(owner.configure(inhibit("active", 1, true), false).is_err());
}

#[tokio::test]
async fn sms_config_sequence_rejects_late_or_inconsistent_updates() {
    let (owner, fake) = fixture();
    let config = |config_seq, serial, poll_interval_ms| ConfigureRequest::Sms {
        producer_id: PRODUCER.into(),
        config_seq,
        serial,
        poll_interval_ms,
    };
    owner.configure(config(2, true, 2000), false).unwrap();
    owner.configure(config(2, true, 2000), false).unwrap();
    for input in [
        config(1, false, 8000),
        config(2, false, 2000),
        config(2, true, 8000),
    ] {
        assert_eq!(
            owner.configure(input, false).unwrap_err().code,
            "usb_at_stale_session"
        );
    }
    assert!(owner.configure(config(0, true, 2000), false).is_err());
    let snapshot = owner.snapshot(PRODUCER).await.unwrap();
    assert_eq!(snapshot["mode"], "serial");
    assert_eq!(snapshot["adopt"], false);
    assert_eq!(snapshot["poll_interval_ms"], 2000);
    assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
    owner.configure(config(3, true, 3000), false).unwrap();
    assert_eq!(owner.snapshot(PRODUCER).await.unwrap()["adopt"], false);
    owner
        .replace_producer("next-producer".into())
        .await
        .unwrap();
    let state = owner.inner.state.lock().unwrap();
    assert_eq!(state.config_high_water, 0);
    assert_eq!(state.inhibit_high_water, 0);
    assert!(state.inhibits.is_empty());
    assert!(state.sms.is_none());
}

#[tokio::test]
async fn sms_transport_configuration_preserves_rust_owned_adoption_setting() {
    let (owner, _) = fixture_with_adopt(true);
    owner
        .configure(
            ConfigureRequest::Sms {
                producer_id: PRODUCER.into(),
                config_seq: 1,
                serial: true,
                poll_interval_ms: 8000,
            },
            false,
        )
        .unwrap();
    assert_eq!(owner.snapshot(PRODUCER).await.unwrap()["adopt"], true);
}

fn device_status_request(session: &str, suppress_cellular: bool) -> DeviceStatusRequest {
    DeviceStatusRequest {
        producer_id: PRODUCER.into(),
        session_id: session.into(),
        lease_id: "status-lease".into(),
        suppress_cellular,
    }
}
async fn status_fixture() -> (Owner, Arc<Fake>, String) {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "status-lease", 1, Purpose::Sim))
        .await
        .unwrap();
    (owner, fake, session)
}

#[tokio::test]
async fn device_status_wire_requires_live_session_lease_and_explicit_policy() {
    let (owner, fake, session) = status_fixture().await;
    let base = json!({"producer_id":PRODUCER,"session_id":session,"lease_id":"status-lease","suppress_cellular":true});
    for field in ["producer_id", "session_id", "lease_id", "suppress_cellular"] {
        let mut invalid = base.clone();
        invalid.as_object_mut().unwrap().remove(field);
        assert_eq!(
            dispatch(
                &format!("{PREFIX}device-status"),
                &serde_json::to_vec(&invalid).unwrap(),
                &owner,
                false
            )
            .await
            .unwrap_err()
            .code,
            "usb_at_invalid_request"
        );
    }
    let mut extra = base.clone();
    extra["raw"] = json!(true);
    assert!(
        dispatch(
            &format!("{PREFIX}device-status"),
            &serde_json::to_vec(&extra).unwrap(),
            &owner,
            false
        )
        .await
        .is_err()
    );
    assert_eq!(
        owner
            .device_status(device_status_request(&session, true), true)
            .await
            .unwrap_err()
            .code,
        "usb_at_stopping"
    );
    let mut invalid = device_status_request(&session, true);
    invalid.lease_id.clear();
    assert!(owner.device_status(invalid, false).await.is_err());
    let mut invalid = device_status_request(&session, true);
    invalid.session_id = "old-session".into();
    assert!(owner.device_status(invalid, false).await.is_err());
    owner
        .release(release(&session, "status-lease", 1))
        .await
        .unwrap();
    assert!(
        owner
            .device_status(device_status_request(&session, true), false)
            .await
            .is_err()
    );
    assert!(fake.commands.lock().unwrap().is_empty());
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn device_status_is_one_fixed_session_plan_with_original_command_budgets() {
    let (owner, fake, session) = status_fixture().await;
    fake.responses
        .lock()
        .unwrap()
        .push_back(Ok("+CFUN: 1\r\nOK".into()));
    let reply = owner
        .device_status(device_status_request(&session, false), false)
        .await
        .unwrap();
    assert_eq!(reply["session_id"], session);
    assert_eq!(reply["status"].as_object().unwrap().len(), 16);
    let commands = fake.commands.lock().unwrap().clone();
    assert_eq!(
        commands,
        [
            "AT+CFUN?",
            "ATI",
            "AT+CPIN?",
            "AT+CSQ",
            "AT+CEREG?",
            "AT+CREG?",
            "AT+COPS=3,2",
            "AT+COPS?",
            "AT+QNWINFO",
            "AT+QCCID",
            "AT+CIMI",
            "AT+CNUM",
            "AT+CGSN",
            "AT+QCFG=\"usbnet\""
        ]
    );
    let budgets = fake.budgets.lock().unwrap();
    assert!(budgets[0].0 <= Duration::from_secs(4));
    assert!(
        budgets[1..]
            .iter()
            .all(|(duration, limit)| *duration <= Duration::from_secs(3) && *limit <= MAX_BYTES)
    );
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn device_status_owner_resolves_saved_phone_from_the_same_iccid_snapshot() {
    let root = std::path::PathBuf::from(
        std::env::var_os("SWITCH_BUILD_DIR").expect("workspace-env build directory"),
    )
    .join("tmp/usb-at-profile-phone-tests")
    .join(format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("profile-notes.json"),
        json!({"8988212345678901234":{"phone":"+441111111111"}}).to_string(),
    )
    .unwrap();
    let (owner, fake) = fixture();
    let owner = owner.with_profile_notes(crate::profile_notes::Store::new(&root));
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "status-lease", 1, Purpose::Sim))
        .await
        .unwrap();
    fake.responses.lock().unwrap().extend([
        Ok("+CFUN: 4\r\nOK".into()),
        Ok("Switch test firmware".into()),
        Ok("+CPIN: READY".into()),
        Ok("+QCCID: \"8988212345678901234F\"".into()),
        Ok("234150123456789".into()),
        Ok("+CNUM: \"\",\"\",145".into()),
        Ok("123456789012345".into()),
        Ok("+QCFG: \"usbnet\",1".into()),
    ]);
    let response = owner
        .device_status(device_status_request(&session, true), false)
        .await
        .unwrap();
    assert_eq!(response["status"]["iccid"], "\"8988212345678901234F\"");
    assert_eq!(response["status"]["phone_number"], "+441111111111");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn device_status_cfun_off_never_queries_cellular_and_inhibit_prevents_all_io() {
    let (owner, fake, session) = status_fixture().await;
    owner
        .configure(
            ConfigureRequest::Inhibit {
                producer_id: PRODUCER.into(),
                inhibit_seq: 1,
                token: "vowifi".into(),
                enabled: true,
            },
            false,
        )
        .unwrap();
    assert_eq!(
        owner
            .device_status(device_status_request(&session, false), false)
            .await
            .unwrap_err()
            .code,
        "usb_at_busy"
    );
    assert!(fake.commands.lock().unwrap().is_empty());
    owner
        .configure(
            ConfigureRequest::Inhibit {
                producer_id: PRODUCER.into(),
                inhibit_seq: 1,
                token: "vowifi".into(),
                enabled: false,
            },
            false,
        )
        .unwrap();
    fake.responses
        .lock()
        .unwrap()
        .push_back(Ok("+CFUN: 4\r\nOK".into()));
    let reply = owner
        .device_status(device_status_request(&session, false), false)
        .await
        .unwrap();
    assert_eq!(reply["status"]["signal_dbm"], -999);
    assert_eq!(reply["status"]["network_mode"], "飞行模式");
    assert_eq!(
        *fake.commands.lock().unwrap(),
        [
            "AT+CFUN?",
            "ATI",
            "AT+CPIN?",
            "AT+QCCID",
            "AT+CIMI",
            "AT+CNUM",
            "AT+CGSN",
            "AT+QCFG=\"usbnet\""
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_status_cancellation_keeps_io_reserved_until_physical_read_returns() {
    let (owner, fake, session) = status_fixture().await;
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    let cloned = owner.clone();
    let request = device_status_request(&session, true);
    let job = tokio::spawn(async move { cloned.device_status(request, false).await });
    gate.entered.notified().await;
    job.abort();
    let cloned = owner.clone();
    let request = release(&session, "status-lease", 1);
    let release_job = tokio::spawn(async move { cloned.release(request).await });
    eventually(|| {
        owner
            .inner
            .state
            .lock()
            .unwrap()
            .lease
            .as_ref()
            .is_some_and(|lease| lease.releasing)
    })
    .await;
    assert!(!release_job.is_finished());
    assert!(owner.inner.state.lock().unwrap().busy);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    gate.release();
    release_job.await.unwrap().unwrap();
    assert!(!owner.inner.state.lock().unwrap().busy);
    assert_eq!(fake.commands.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_status_rechecks_inhibit_after_the_final_physical_response() {
    let (owner, fake, session) = status_fixture().await;
    let gate = Arc::new(Gate::default());
    *fake.gate_command.lock().unwrap() = Some("AT+QCFG=\"usbnet\"".into());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    let cloned = owner.clone();
    let request = device_status_request(&session, true);
    let job = tokio::spawn(async move { cloned.device_status(request, false).await });
    gate.entered.notified().await;
    owner
        .configure(
            ConfigureRequest::Inhibit {
                producer_id: PRODUCER.into(),
                inhibit_seq: 1,
                token: "vowifi-start".into(),
                enabled: true,
            },
            false,
        )
        .unwrap();
    gate.release();
    assert_eq!(job.await.unwrap().unwrap_err().code, "usb_at_busy");
    assert_eq!(fake.commands.lock().unwrap().len(), 8);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    assert_eq!(owner.status(PRODUCER).unwrap()["session_id"], session);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_status_old_producer_stops_plan_without_touching_the_next_handle() {
    let (owner, fake, session) = status_fixture().await;
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    let cloned = owner.clone();
    let request = device_status_request(&session, true);
    let job = tokio::spawn(async move { cloned.device_status(request, false).await });
    gate.entered.notified().await;
    let cloned = owner.clone();
    let replacement = tokio::spawn(async move { cloned.replace_producer("next".into()).await });
    eventually(|| owner.inner.state.lock().unwrap().producer == "next").await;
    assert!(!replacement.is_finished());
    gate.release();
    assert_eq!(job.await.unwrap().unwrap_err().code, "usb_at_stale_session");
    replacement.await.unwrap().unwrap();
    assert_eq!(fake.commands.lock().unwrap().len(), 1);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    let fresh = owner
        .open(
            OpenRequest {
                producer_id: "next".into(),
                lease_id: String::new(),
            },
            false,
        )
        .await
        .unwrap();
    assert_ne!(fresh["session_id"], session);
    assert_eq!(fake.opens.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_status_transport_failure_waits_for_real_close_and_cools_down() {
    let (owner, fake, session) = status_fixture().await;
    fake.responses
        .lock()
        .unwrap()
        .push_back(Err(TransportError {
            code: "usb_at_read_failed",
            message: "fixture read failed".into(),
            response: String::new(),
            uncertain: true,
        }));
    let gate = Arc::new(Gate::default());
    *fake.drop_gate.lock().unwrap() = Some(gate.clone());
    let cloned = owner.clone();
    let request = device_status_request(&session, true);
    let job = tokio::spawn(async move { cloned.device_status(request, false).await });
    gate.entered.notified().await;
    assert!(!job.is_finished());
    assert!(owner.inner.state.lock().unwrap().busy);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    gate.release();
    assert_eq!(job.await.unwrap().unwrap_err().code, "usb_at_read_failed");
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert_eq!(fake.commands.lock().unwrap().len(), 1);
    assert_eq!(owner.status(PRODUCER).unwrap()["open"], false);
    assert!(owner.inner.state.lock().unwrap().next_open.is_some());
    owner
        .release(release(&session, "status-lease", 1))
        .await
        .unwrap();
    assert!(
        owner
            .open(
                OpenRequest {
                    producer_id: PRODUCER.into(),
                    lease_id: String::new()
                },
                false
            )
            .await
            .is_err()
    );
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
}

const STORAGE_CARD: &str = "8988212345678901234";
fn storage_card_reply() -> String {
    format!("+QCCID: {STORAGE_CARD}\r\nOK")
}
fn storage_selection(storage: &str) -> String {
    format!("+CPMS: \"{storage}\",2,20,\"{storage}\",2,20,\"{storage}\",2,20\r\nOK")
}
fn storage_group(token: &str, index: u32) -> crate::usb_at_storage::DeleteGroup {
    crate::usb_at_storage::DeleteGroup {
        token: token.into(),
        card_identity: STORAGE_CARD.into(),
        sources: vec![crate::usb_at_storage::StoredSource {
            storage: crate::usb_at_storage::Storage::ME,
            index,
            pdu: "001122".into(),
        }],
    }
}
fn storage_clear() -> crate::usb_at_storage::StorageOperation {
    crate::usb_at_storage::StorageOperation::Clear {
        storages: vec![crate::usb_at_storage::Storage::SM],
    }
}
fn storage_transition_replies(fake: &Fake) {
    fake.responses.lock().unwrap().extend([
        Ok(storage_card_reply()),
        Ok(storage_selection("ME")),
        Ok("OK".into()),
        // Cancellation/inhibit after the selecting write permits only cleanup.
        Ok(storage_card_reply()),
        Ok("OK".into()),
        Ok(storage_selection("ME")),
    ]);
}

#[tokio::test]
async fn public_storage_usage_runs_one_rust_owned_read_and_restores_cpms() {
    let (owner, fake) = fixture();
    fake.responses.lock().unwrap().extend([
        Ok("+CPMS: \"MT\",4,64,\"MT\",4,64,\"MT\",4,64\r\nOK".into()),
        Ok("OK".into()),
        Ok("+CPMS: \"SM\",3,30,\"SM\",3,30,\"SM\",3,30\r\nOK".into()),
        Ok("OK".into()),
        Ok("+CPMS: \"ME\",6,60,\"ME\",6,60,\"ME\",6,60\r\nOK".into()),
        Ok("OK".into()),
        Ok("+CPMS: \"MT\",4,64,\"MT\",4,64,\"MT\",4,64\r\nOK".into()),
    ]);

    let outcome = owner.storage_usage(PRODUCER).await.unwrap();
    assert!(
        outcome.complete,
        "error={} commands={:?}",
        outcome.error_code,
        fake.commands.lock().unwrap()
    );
    assert!(outcome.storage_restored);
    assert_eq!(outcome.usage[&crate::usb_at_storage::Storage::SM].used, 3);
    assert_eq!(outcome.usage[&crate::usb_at_storage::Storage::SM].total, 30);
    assert_eq!(outcome.usage[&crate::usb_at_storage::Storage::ME].used, 6);
    assert_eq!(outcome.usage[&crate::usb_at_storage::Storage::ME].total, 60);
    assert_eq!(
        *fake.commands.lock().unwrap(),
        [
            "AT+CPMS?",
            "AT+CPMS=\"SM\",\"SM\",\"SM\"",
            "AT+CPMS?",
            "AT+CPMS=\"ME\",\"ME\",\"ME\"",
            "AT+CPMS?",
            "AT+CPMS=\"MT\",\"MT\",\"MT\"",
            "AT+CPMS?"
        ]
    );
    assert!(
        fake.commands
            .lock()
            .unwrap()
            .iter()
            .all(|command| !command.starts_with("AT+CMGD="))
    );
    let state = owner.inner.state.lock().unwrap();
    assert!(!state.busy);
    assert!(state.lease.is_none());
}

#[tokio::test]
async fn public_storage_usage_refuses_an_existing_sim_lease_without_io() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "active-sim-lease", 1, Purpose::Sim))
        .await
        .unwrap();

    let error = owner.storage_usage(PRODUCER).await.unwrap_err();
    assert_eq!(error.code, "usb_at_busy");
    assert!(fake.commands.lock().unwrap().is_empty());

    owner
        .release(release(&session, "active-sim-lease", 1))
        .await
        .unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn rust_public_storage_mutation_refuses_existing_sim_lease_without_io() {
    let (owner, fake, session) = status_fixture().await;
    let before = fake.commands.lock().unwrap().len();
    let error = owner
        .storage_exclusive(PRODUCER, &session, storage_clear())
        .await
        .unwrap_err();
    assert_eq!(error.code, "usb_at_busy");
    assert_eq!(fake.commands.lock().unwrap().len(), before);

    owner
        .release(release(&session, "status-lease", 1))
        .await
        .unwrap();
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn rust_public_storage_mutation_owns_and_finishes_the_complete_card_transaction() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    fake.responses.lock().unwrap().extend([
        Ok(storage_card_reply()),
        Ok(storage_selection("ME")),
        Ok("OK".into()),
        Ok(storage_selection("SM")),
        Ok(storage_card_reply()),
        Ok("OK".into()),
        Ok(storage_card_reply()),
        Ok("+CPMS: \"SM\",0,20,\"SM\",0,20,\"SM\",0,20\r\nOK".into()),
        Ok(storage_card_reply()),
        Ok("OK".into()),
        Ok(storage_selection("ME")),
    ]);

    let outcome = owner
        .storage_exclusive(PRODUCER, &session, storage_clear())
        .await
        .unwrap();
    assert!(
        outcome.complete,
        "error={} commands={:?}",
        outcome.error_code,
        fake.commands.lock().unwrap()
    );
    assert!(outcome.storage_restored);
    assert_eq!(
        outcome.details[&crate::usb_at_storage::Storage::SM].after,
        Some(0)
    );
    assert!(
        fake.commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command == "AT+CMGD=1,4")
    );
    let state = owner.inner.state.lock().unwrap();
    assert!(!state.busy);
    assert!(state.lease.is_none());
    drop(state);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn public_storage_usage_refuses_radio_inhibit_without_opening_device() {
    let (owner, fake) = fixture();
    owner
        .set_radio_inhibit(PRODUCER, "module-vowifi-session", true)
        .unwrap();

    let error = owner.storage_usage(PRODUCER).await.unwrap_err();
    assert_eq!(error.code, "usb_at_busy");
    assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
    assert!(fake.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn storage_admission_requires_exact_live_lease_and_never_opens_or_replays() {
    use crate::usb_at_storage::StorageOperation;
    let (owner, fake, session) = status_fixture().await;
    for (producer, expected, lease) in [
        ("old-producer", session.as_str(), "status-lease"),
        (PRODUCER, "old-session", "status-lease"),
        (PRODUCER, session.as_str(), "other-lease"),
    ] {
        assert!(
            owner
                .storage(
                    producer.into(),
                    expected.into(),
                    lease.into(),
                    StorageOperation::Usage
                )
                .await
                .is_err()
        );
    }
    owner
        .configure(
            ConfigureRequest::Inhibit {
                producer_id: PRODUCER.into(),
                inhibit_seq: 1,
                token: "radio".into(),
                enabled: true,
            },
            false,
        )
        .unwrap();
    assert!(
        owner
            .storage(
                PRODUCER.into(),
                session.clone(),
                "status-lease".into(),
                StorageOperation::Usage
            )
            .await
            .is_err()
    );
    assert!(fake.commands.lock().unwrap().is_empty());
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
    owner.shutdown().await.unwrap();
    assert!(
        owner
            .storage(
                PRODUCER.into(),
                session,
                "status-lease".into(),
                StorageOperation::Usage
            )
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_cancellation_waits_for_selection_restore_before_release_ack() {
    let (owner, fake, session) = status_fixture().await;
    storage_transition_replies(&fake);
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    *fake.gate_command.lock().unwrap() = Some("AT+CPMS=\"SM\",\"SM\",\"SM\"".into());
    let cloned = owner.clone();
    let selected = session.clone();
    let task = tokio::spawn(async move {
        cloned
            .storage(
                PRODUCER.into(),
                selected,
                "status-lease".into(),
                storage_clear(),
            )
            .await
    });
    gate.entered.notified().await;
    task.abort();
    let cloned = owner.clone();
    let request = release(&session, "status-lease", 1);
    let release_task = tokio::spawn(async move { cloned.release(request).await });
    eventually(|| {
        owner
            .inner
            .state
            .lock()
            .unwrap()
            .lease
            .as_ref()
            .is_some_and(|lease| lease.releasing)
    })
    .await;
    let early_release = release_task.is_finished();
    gate.release();
    release_task.await.unwrap().unwrap();
    assert!(!early_release);
    let commands = fake.commands.lock().unwrap();
    assert_eq!(commands.last().map(String::as_str), Some("AT+CPMS?"));
    assert!(
        commands
            .iter()
            .any(|command| command == "AT+CPMS=\"ME\",\"ME\",\"ME\"")
    );
    assert!(
        !commands
            .iter()
            .any(|command| command.starts_with("AT+CMGD"))
    );
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    assert!(!owner.inner.state.lock().unwrap().busy);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_inhibit_stops_mutation_but_does_not_prevent_same_session_cleanup() {
    let (owner, fake, session) = status_fixture().await;
    storage_transition_replies(&fake);
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    *fake.gate_command.lock().unwrap() = Some("AT+CPMS=\"SM\",\"SM\",\"SM\"".into());
    let cloned = owner.clone();
    let task = tokio::spawn(async move {
        cloned
            .storage(
                PRODUCER.into(),
                session,
                "status-lease".into(),
                storage_clear(),
            )
            .await
    });
    gate.entered.notified().await;
    owner
        .configure(
            ConfigureRequest::Inhibit {
                producer_id: PRODUCER.into(),
                inhibit_seq: 1,
                token: "radio".into(),
                enabled: true,
            },
            false,
        )
        .unwrap();
    gate.release();
    let outcome = task.await.unwrap().unwrap();
    assert!(!outcome.complete && !outcome.uncertain);
    assert!(outcome.storage_restored);
    assert_eq!(outcome.error_code, "usb_at_busy");
    assert!(
        !fake
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command.starts_with("AT+CMGD"))
    );
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_old_producer_after_delete_submit_is_unknown_and_cannot_touch_new_session() {
    use crate::usb_at_storage::StorageOperation;
    let (owner, fake, session) = status_fixture().await;
    fake.responses.lock().unwrap().extend([
        Ok(storage_card_reply()),
        Ok(storage_selection("ME")),
        Ok("OK".into()),
        Ok("+CMGR: 0,,2\r\n001122\r\nOK".into()),
        Ok(storage_card_reply()),
        Ok("+CMGR: 0,,2\r\n001122\r\nOK".into()),
        Ok("OK".into()),
    ]);
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    *fake.gate_command.lock().unwrap() = Some("AT+CMGD=7".into());
    let cloned = owner.clone();
    let task = tokio::spawn(async move {
        cloned
            .storage(
                PRODUCER.into(),
                session,
                "status-lease".into(),
                StorageOperation::Delete {
                    groups: vec![storage_group("record", 7)],
                },
            )
            .await
    });
    gate.entered.notified().await;
    let cloned = owner.clone();
    let replacing =
        tokio::spawn(async move { cloned.replace_producer("new-producer".into()).await });
    eventually(|| owner.inner.state.lock().unwrap().producer == "new-producer").await;
    let premature = replacing.is_finished();
    gate.release();
    let outcome = task.await.unwrap().unwrap();
    replacing.await.unwrap().unwrap();
    assert!(!premature && !outcome.complete && outcome.uncertain && !outcome.storage_restored);
    assert!(outcome.deleted_tokens.is_empty());
    assert_eq!(
        fake.commands.lock().unwrap().last().map(String::as_str),
        Some("AT+CMGD=7")
    );
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    owner
        .open(
            OpenRequest {
                producer_id: "new-producer".into(),
                lease_id: String::new(),
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(fake.opens.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_transport_loss_preserves_prior_whole_group_and_waits_for_actual_drop() {
    use crate::usb_at_storage::StorageOperation;
    let (owner, fake, session) = status_fixture().await;
    fake.responses.lock().unwrap().extend([
        Ok(storage_card_reply()),
        Ok(storage_selection("ME")),
        Ok("OK".into()),
        Ok("+CMGR: 0,,2\r\n001122\r\nOK".into()),
        Ok("+CMGR: 0,,2\r\n001122\r\nOK".into()),
        Ok(storage_card_reply()),
        Ok("+CMGR: 0,,2\r\n001122\r\nOK".into()),
        Ok("OK".into()),
        Ok(storage_card_reply()),
        Ok(storage_card_reply()),
        Ok("+CMGR: 0,,2\r\n001122\r\nOK".into()),
        Err(TransportError {
            code: "usb_at_disconnected",
            message: "isolated disconnect".into(),
            response: String::new(),
            uncertain: true,
        }),
    ]);
    let drop_gate = Arc::new(Gate::default());
    *fake.drop_gate.lock().unwrap() = Some(drop_gate.clone());
    let cloned = owner.clone();
    let selected = session.clone();
    let task = tokio::spawn(async move {
        cloned
            .storage(
                PRODUCER.into(),
                selected,
                "status-lease".into(),
                StorageOperation::Delete {
                    groups: vec![storage_group("second", 2), storage_group("first", 1)],
                },
            )
            .await
    });
    drop_gate.entered.notified().await;
    let finished_early = task.is_finished();
    let busy = owner.inner.state.lock().unwrap().busy;
    drop_gate.release();
    let outcome = task.await.unwrap().unwrap();
    assert!(!finished_early && busy);
    assert!(!outcome.complete && outcome.uncertain && !outcome.storage_restored);
    assert_eq!(outcome.deleted_tokens, vec!["second"]);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert_eq!(owner.status(PRODUCER).unwrap()["open"], false);
    assert_eq!(
        fake.commands
            .lock()
            .unwrap()
            .iter()
            .filter(|command| command.as_str() == "AT+CMGD=1")
            .count(),
        1
    );
    owner
        .release(release(&session, "status-lease", 1))
        .await
        .unwrap();
    assert!(
        owner
            .open(
                OpenRequest {
                    producer_id: PRODUCER.into(),
                    lease_id: String::new()
                },
                false
            )
            .await
            .is_err()
    );
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn snapshot_card_change_preserves_display_but_provides_no_delete_identity() {
    let (owner, fake) = fixture();
    configure(&owner);
    fake.responses.lock().unwrap().extend([
        Ok(storage_card_reply()),
        Ok("OK".into()),
        Ok("OK".into()),
        Ok("OK".into()),
        Ok("OK".into()),
        Ok("OK".into()),
        Ok("+QCCID: 8988212345678901235\r\nOK".into()),
    ]);
    let snapshot = owner.snapshot(PRODUCER).await.unwrap();
    assert_eq!(snapshot["mode"], "usb");
    assert_eq!(snapshot["card_identity"], "");
    assert_eq!(snapshot["storages"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn snapshot_partial_reply_at_any_step_retires_without_later_io_or_delete_proof() {
    let commands = [
        "AT+QCCID",
        "AT+CMGF=0",
        "AT+CPMS=\"SM\",\"SM\",\"SM\"",
        "AT+CMGL=4",
        "AT+CPMS=\"ME\",\"ME\",\"ME\"",
        "AT+CMGL=4",
        "AT+QCCID",
    ];
    let responses = [
        storage_card_reply(),
        "OK".into(),
        "OK".into(),
        "+CMGL: 1,0,,1\r\n00\r\nOK".into(),
        "OK".into(),
        "OK".into(),
        storage_card_reply(),
    ];
    for partial_at in 0..commands.len() {
        let (owner, fake) = fixture();
        configure(&owner);
        fake.responses
            .lock()
            .unwrap()
            .extend(responses[..partial_at].iter().cloned().map(Ok));
        fake.responses
            .lock()
            .unwrap()
            .push_back(Ok("+INCOMPLETE: synthetic".into()));
        let result = owner.snapshot(PRODUCER).await;
        if partial_at <= 1 {
            assert_eq!(
                result.unwrap_err().code,
                "usb_sms_snapshot_incomplete_response"
            );
        } else {
            let value = result.unwrap();
            assert_eq!(value["card_identity"], "");
            if partial_at >= 4 {
                assert_eq!(value["storages"][0]["response"], responses[3]);
            }
            if partial_at == 6 {
                assert_eq!(value["storages"][1]["error_code"], "");
            }
        }
        assert_eq!(
            *fake.commands.lock().unwrap(),
            commands[..=partial_at],
            "partial step {partial_at} dispatched later commands"
        );
        assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
        assert_eq!(owner.status(PRODUCER).unwrap()["open"], false);
    }
}

#[tokio::test]
async fn radio_typed_transaction_preserves_original_binding_and_inhibit_permission() {
    use crate::module_radio_transport::Action;
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    owner
        .set_radio_inhibit(PRODUCER, "vowifi-radio-intent", true)
        .unwrap();
    assert!(
        owner
            .radio_execute(
                PRODUCER.into(),
                session.clone(),
                Action::Observe { cellular: false },
                None
            )
            .await
            .is_err()
    );
    fake.responses.lock().unwrap().extend([
        Ok("+CFUN: 1\r\nOK".into()),
        Ok("OK".into()),
        Ok("OK".into()),
        Ok("+CFUN: 4\r\nOK".into()),
    ]);
    let result = owner
        .radio_execute(
            PRODUCER.into(),
            session,
            Action::SetVerify { target: 4 },
            Some("vowifi-radio-intent".into()),
        )
        .await
        .unwrap();
    assert!(result.complete && result.verified);
    assert_eq!(
        *fake.commands.lock().unwrap(),
        ["AT+CFUN?", "AT+CLCC", "AT+CFUN=4", "AT+CFUN?"]
    );
    owner
        .set_radio_inhibit(PRODUCER, "retired-intent", false)
        .unwrap();
    assert!(owner.inner.state.lock().unwrap().radio_inhibit.is_some());
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn radio_write_ack_loss_drops_real_transport_without_replay() {
    use crate::module_radio_transport::Action;
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    fake.responses.lock().unwrap().extend([
        Ok("+CFUN: 1\r\nOK".into()),
        Ok("OK".into()),
        Ok("+CFUN: 4".into()),
    ]);
    let result = owner
        .radio_execute(
            PRODUCER.into(),
            session.clone(),
            Action::SetVerify { target: 4 },
            None,
        )
        .await
        .unwrap();
    assert!(result.submitted && result.uncertain && !result.complete);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        *fake.commands.lock().unwrap(),
        ["AT+CFUN?", "AT+CLCC", "AT+CFUN=4"]
    );
    assert!(
        owner
            .radio_execute(
                PRODUCER.into(),
                session,
                Action::Observe { cellular: false },
                None
            )
            .await
            .is_err()
    );
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn radio_waiter_cancel_keeps_actual_io_owned_and_blocks_other_lease() {
    use crate::module_radio_transport::Action;
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    fake.responses
        .lock()
        .unwrap()
        .push_back(Ok("+CFUN: 4\r\nOK".into()));
    let io_owner = owner.clone();
    let io_session = session.clone();
    let job = tokio::spawn(async move {
        io_owner
            .radio_execute(
                PRODUCER.into(),
                io_session,
                Action::Observe { cellular: false },
                None,
            )
            .await
    });
    gate.entered.notified().await;
    job.abort();
    assert_eq!(
        owner
            .acquire(acquire(&session, "competing-radio", 1, Purpose::Sim))
            .await
            .unwrap_err()
            .code,
        "usb_at_busy"
    );
    let closer = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.shutdown().await })
    };
    assert!(!closer.is_finished());
    gate.release();
    closer.await.unwrap().unwrap();
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn radio_inhibit_arriving_between_steps_prevents_write() {
    use crate::module_radio_transport::Action;
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    fake.responses
        .lock()
        .unwrap()
        .push_back(Ok("+CFUN: 1\r\nOK".into()));
    let io_owner = owner.clone();
    let job = tokio::spawn(async move {
        io_owner
            .radio_execute(
                PRODUCER.into(),
                session,
                Action::SetVerify { target: 4 },
                None,
            )
            .await
    });
    gate.entered.notified().await;
    owner
        .set_radio_inhibit(PRODUCER, "new-vowifi-intent", true)
        .unwrap();
    gate.release();
    let outcome = job.await.unwrap().unwrap();
    assert!(!outcome.complete && !outcome.submitted);
    assert_eq!(*fake.commands.lock().unwrap(), ["AT+CFUN?"]);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn generic_usb_raw_cfun_and_prompt_escape_cannot_bypass_policy() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    for text in ["AT+CFUN=4", "ATE0+CFUN=1", "AT+TEST=\"x\\\";+CFUN=0\""] {
        assert_eq!(
            owner
                .command(command(&session, "", text), false)
                .await
                .unwrap_err()
                .code,
            "radio_authorization_required"
        );
    }
    let mut prompt = command(&session, "", "AT+CMGS=2");
    prompt.follow_up_base64 =
        Some(base64::engine::general_purpose::STANDARD.encode(b"\x1bAT+CFUN=1\r"));
    assert!(owner.command(prompt, false).await.is_err());
    assert!(fake.commands.lock().unwrap().is_empty());
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn radio_old_producer_stops_at_fence_and_does_not_open_replacement() {
    use crate::module_radio_transport::Action;
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    fake.responses
        .lock()
        .unwrap()
        .push_back(Ok("+CFUN: 1\r\nOK".into()));
    let running_owner = owner.clone();
    let running = tokio::spawn(async move {
        running_owner
            .radio_execute(
                PRODUCER.into(),
                session,
                Action::SetVerify { target: 4 },
                None,
            )
            .await
    });
    gate.entered.notified().await;
    let replacing = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.replace_producer("new-radio-producer".into()).await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while owner.inner.state.lock().unwrap().producer != "new-radio-producer" {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!replacing.is_finished());
    gate.release();
    let outcome = running.await.unwrap().unwrap();
    assert!(!outcome.complete && !outcome.submitted);
    replacing.await.unwrap().unwrap();
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert_eq!(*fake.commands.lock().unwrap(), ["AT+CFUN?"]);
}

#[tokio::test]
async fn native_policy_gate_blocks_stale_status_probe_and_preserves_sim_queries() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    owner.set_cellular_suppressed(true);
    assert_eq!(
        owner
            .command(command(&session, "", "AT+CSQ"), false)
            .await
            .unwrap_err()
            .code,
        "module_flight_mode_blocks_cellular_probe"
    );
    owner
        .command(command(&session, "", "AT+CPIN?"), false)
        .await
        .unwrap();
    owner
        .command(command(&session, "", "AT+CGLA=1,4,0084"), false)
        .await
        .unwrap();
    owner
        .command(command(&session, "", "AT+CFUN?"), false)
        .await
        .unwrap();
    assert_eq!(
        *fake.commands.lock().unwrap(),
        ["AT+CPIN?", "AT+CGLA=1,4,0084", "AT+CFUN?"]
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn device_status_rechecks_native_policy_before_next_cellular_step() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "status-policy-gate", 1, Purpose::Sim))
        .await
        .unwrap();
    fake.responses.lock().unwrap().extend([
        Ok("+CFUN: 1\r\nOK".into()),
        Ok("fixture firmware\r\nOK".into()),
        Ok("+CPIN: READY\r\nOK".into()),
    ]);
    let gate = Arc::new(Gate::default());
    *fake.gate.lock().unwrap() = Some(gate.clone());
    *fake.gate_command.lock().unwrap() = Some("AT+CPIN?".into());
    let collector = owner.clone();
    let job = tokio::spawn(async move {
        collector
            .device_status(
                DeviceStatusRequest {
                    producer_id: PRODUCER.into(),
                    session_id: session,
                    lease_id: "status-policy-gate".into(),
                    suppress_cellular: false,
                },
                false,
            )
            .await
    });
    gate.entered.notified().await;
    owner.set_cellular_suppressed(true);
    gate.release();
    assert_eq!(
        job.await.unwrap().unwrap_err().code,
        "module_flight_mode_blocks_cellular_probe"
    );
    assert_eq!(
        *fake.commands.lock().unwrap(),
        ["AT+CFUN?", "ATI", "AT+CPIN?"]
    );
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn checked_close_failure_stays_bound_to_old_session_across_reopen_and_revoke() {
    let (owner, fake) = fixture();
    let old = opened(&owner).await;
    let original_epoch = owner.inner.state.lock().unwrap().epoch;
    fake.close_mode.store(1, Ordering::SeqCst);
    assert_eq!(
        owner.close(close(&old, "")).await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert!(owner.inner.port.lock().unwrap().is_none());
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        owner.close(close(&old, "")).await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner.capture_voice_target(PRODUCER).await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner
            .release(release(&old, "retired", 1))
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    {
        let state = owner.inner.state.lock().unwrap();
        assert_eq!(state.close_failures.len(), 1);
        assert_eq!(state.close_failures[0].0.session, old);
        assert_eq!(state.close_failures[0].0.epoch, original_epoch);
        assert!(!state.busy && !state.revoking);
    }
    // A distinct actual connection can still be opened/closed successfully.
    fake.close_mode.store(0, Ordering::SeqCst);
    let new = opened(&owner).await;
    assert_ne!(new, old);
    owner.close(close(&new, "")).await.unwrap();
    assert_eq!(
        owner.close(close(&old, "")).await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
}

#[tokio::test]
async fn native_startup_refuses_reopen_after_checked_close_is_unconfirmed() {
    let (owner, fake) = fixture();
    owner.claim_native_management().unwrap();
    let session = owner.open_native_session().await.unwrap();
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
    fake.close_mode.store(1, Ordering::SeqCst);
    assert_eq!(
        owner.close(close(&session, "")).await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert!(owner.inner.port.lock().unwrap().is_none());
    fake.close_mode.store(0, Ordering::SeqCst);
    assert_eq!(
        owner.open_native_session().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(fake.opens.load(Ordering::SeqCst), 1);
    assert_eq!(
        owner.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
}

#[tokio::test]
async fn checked_close_panic_is_retained_after_consumed_port_and_worker_exit() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    fake.close_mode.store(2, Ordering::SeqCst);
    assert_eq!(
        owner.close(close(&session, "")).await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert!(owner.inner.port.lock().unwrap().is_none());
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        owner.close(close(&session, "")).await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    let state = owner.inner.state.lock().unwrap();
    assert!(!state.busy && !state.revoking);
    assert_eq!(state.close_failures.len(), 1);
}

#[tokio::test]
async fn cancelled_close_waiter_and_concurrent_native_release_wait_for_real_close_error() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    owner
        .acquire(acquire(&session, "close-owner", 1, Purpose::Sim))
        .await
        .unwrap();
    fake.close_mode.store(1, Ordering::SeqCst);
    let gate = Arc::new(Gate::default());
    *fake.drop_gate.lock().unwrap() = Some(gate.clone());
    let closing = owner.clone();
    let request = close(&session, "close-owner");
    let waiter = tokio::spawn(async move { closing.close(request).await });
    gate.entered.notified().await;
    let releasing = owner.clone();
    let request = release(&session, "close-owner", 1);
    let release_waiter =
        tokio::spawn(async move { releasing.release_with_deadline(request, None).await });
    tokio::task::yield_now().await;
    assert!(!release_waiter.is_finished());
    assert_eq!(fake.drops.load(Ordering::SeqCst), 0);
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    gate.release();
    assert_eq!(
        release_waiter.await.unwrap().unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        owner.capture_voice_target(PRODUCER).await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert!(
        owner.inner.state.lock().unwrap().lease.is_some(),
        "failed close must not erase pending release"
    );
    assert_eq!(
        owner.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
}

#[tokio::test]
async fn voice_acquire_close_error_never_becomes_same_token_success_or_no_device_capture() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    fake.close_mode.store(1, Ordering::SeqCst);
    assert_eq!(
        owner
            .acquire(acquire(&session, "voice-close", 1, Purpose::Voice))
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner
            .acquire(acquire(&session, "voice-close", 1, Purpose::Voice))
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner
            .release_with_deadline(release(&session, "voice-close", 1), None)
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner.capture_voice_target(PRODUCER).await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
}

#[tokio::test]
async fn close_failure_capacity_stops_before_next_factory_or_native_open_attempt() {
    let (owner, fake) = fixture();
    fake.close_mode.store(1, Ordering::SeqCst);
    for _ in 0..64 {
        let session = opened(&owner).await;
        assert_eq!(
            owner.close(close(&session, "")).await.unwrap_err().code,
            "usb_at_close_unconfirmed"
        );
    }
    assert_eq!(fake.opens.load(Ordering::SeqCst), 64);
    assert_eq!(
        owner
            .open(
                OpenRequest {
                    producer_id: PRODUCER.into(),
                    lease_id: String::new()
                },
                false
            )
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(fake.opens.load(Ordering::SeqCst), 64);
    let state = owner.inner.state.lock().unwrap();
    assert_eq!(state.close_failures.len(), 64);
    assert_eq!(state.port_sequence, 64);
    assert!(state.session.is_empty());
}

#[tokio::test]
async fn candidate_retirement_before_publication_preserves_failed_checked_close() {
    let fake = Arc::new(Fake::default());
    fake.close_mode.store(1, Ordering::SeqCst);
    let factory = fake.clone();
    let owner = Owner::with_factory_and_adopt(
        PRODUCER.into(),
        Arc::new(move || Ok(Box::new(FakeTransport(factory.clone())))),
        false,
    );
    // Force failure after factory creation but before session publication.
    owner.inner.state.lock().unwrap().identity_generation = u64::MAX;
    assert_eq!(
        owner
            .open(
                OpenRequest {
                    producer_id: PRODUCER.into(),
                    lease_id: String::new()
                },
                false
            )
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    let state = owner.inner.state.lock().unwrap();
    assert!(state.session.is_empty() && state.port_identity.is_none());
    assert_eq!(state.close_failures.len(), 1);
    assert!(
        state.close_failures[0].0.session.is_empty(),
        "attempt must not fabricate a published session"
    );
    assert_eq!(state.close_failures[0].0.connection, 1);
}

#[tokio::test]
async fn empty_session_lease_retains_failed_actual_connection_without_poisoning_new_empty_lease() {
    let (owner, fake) = fixture();
    owner
        .acquire(acquire("", "before-open", 1, Purpose::Sim))
        .await
        .unwrap();
    let session = owner
        .open(
            OpenRequest {
                producer_id: PRODUCER.into(),
                lease_id: "before-open".into(),
            },
            false,
        )
        .await
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    fake.close_mode.store(1, Ordering::SeqCst);
    let barrier = Arc::new(Gate::default());
    *fake.drop_gate.lock().unwrap() = Some(barrier.clone());
    let closing = owner.clone();
    let request = close(&session, "before-open");
    let close_waiter = tokio::spawn(async move { closing.close(request).await });
    barrier.entered.notified().await;
    let releasing = owner.clone();
    let release_waiter = tokio::spawn(async move {
        releasing
            .release_with_deadline(release("", "before-open", 1), None)
            .await
    });
    tokio::task::yield_now().await;
    assert!(!release_waiter.is_finished());
    barrier.release();
    assert_eq!(
        close_waiter.await.unwrap().unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        release_waiter.await.unwrap().unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner
            .release(release("", "before-open", 1))
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert!(owner.inner.state.lock().unwrap().lease.is_some());
    assert_eq!(
        owner
            .replace_producer("unrelated-empty-producer".into())
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    fake.close_mode.store(0, Ordering::SeqCst);
    owner
        .acquire(LeaseRequest {
            producer_id: "unrelated-empty-producer".into(),
            session_id: String::new(),
            lease_id: "new-empty".into(),
            lease_seq: 1,
            purpose: Purpose::Sim,
        })
        .await
        .unwrap();
    let new_session = owner
        .open(
            OpenRequest {
                producer_id: "unrelated-empty-producer".into(),
                lease_id: "new-empty".into(),
            },
            false,
        )
        .await
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    owner
        .close(CloseRequest {
            producer_id: "unrelated-empty-producer".into(),
            session_id: new_session,
            lease_id: "new-empty".into(),
        })
        .await
        .unwrap();
    owner
        .release(ReleaseRequest {
            producer_id: "unrelated-empty-producer".into(),
            session_id: String::new(),
            lease_id: "new-empty".into(),
            lease_seq: 1,
        })
        .await
        .unwrap();
    assert_eq!(
        owner
            .release(release("", "before-open", 1))
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(
        owner.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
}

#[tokio::test]
async fn disconnected_resource_close_is_typed_and_is_not_a_restore_receipt() {
    let (owner, fake) = fixture();
    let session = opened(&owner).await;
    fake.close_mode.store(3, Ordering::SeqCst);
    owner.close(close(&session, "")).await.unwrap();
    {
        let state = owner.inner.state.lock().unwrap();
        assert!(matches!(
            state.last_close_result,
            Some((_, Ok(CloseDisposition::DisconnectedAndHandleDropped)))
        ));
        assert!(state.close_failures.is_empty());
        assert!(state.session.is_empty());
    }
    assert_eq!(fake.drops.load(Ordering::SeqCst), 1);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn opening_panic_retains_unknown_attempt_instead_of_no_port_shutdown_success() {
    let owner = Owner::with_factory(
        PRODUCER.into(),
        Arc::new(|| panic!("opening attempt panicked")),
    );
    assert_eq!(
        owner
            .open(
                OpenRequest {
                    producer_id: PRODUCER.into(),
                    lease_id: String::new()
                },
                false
            )
            .await
            .unwrap_err()
            .code,
        "usb_at_close_unconfirmed"
    );
    assert!(owner.inner.port.lock().unwrap().is_none());
    assert_eq!(
        owner.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    let state = owner.inner.state.lock().unwrap();
    assert_eq!(state.close_failures.len(), 1);
    assert!(state.close_failures[0].0.session.is_empty());
}
