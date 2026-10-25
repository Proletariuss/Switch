//! Actual Service -> Owner -> Plan -> NativeTransport byte boundary. Synthetic
//! target/anchor proves only fixture ownership, never native reenumeration.
use super::*;
use crate::call_mode_restore_journal::{self as durable, Intent, Journal, TestPhase};
use crate::module_radio::{CallModeReservation, Service};
use crate::module_radio_transport::{
    Action, CallModeModuleIdentity, Outcome, UsbComposition, VoiceConfiguration,
};
use std::path::PathBuf;
use std::sync::Mutex;

const PRODUCER: &str = "restore-wire-owner";
#[derive(Default)]
struct Wire {
    commands: Vec<String>,
    accepted: Vec<Vec<u8>>,
    reply: Option<Vec<u8>>,
    ims: u8,
    volte: u8,
    partial: Option<String>,
    reject: Option<String>,
    reboot_effective: bool,
    drain_change: Option<(Arc<AtomicBool>, Box<dyn FnOnce() + Send>)>,
}
struct ByteIo {
    target: PhysicalTarget,
    wire: Arc<Mutex<Wire>>,
}
impl Io for ByteIo {
    fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
        Ok(self.target.clone())
    }
    fn capture_reenumeration_anchor(
        &mut self,
        _: Instant,
        guard: crate::module_radio::CallAdmission,
    ) -> Result<ReenumerationAnchor, TransportError> {
        assert!(guard());
        Ok(ReenumerationAnchor::synthetic_for_tests(
            self.target.clone(),
            (0x2c7c, 0x0125),
        ))
    }
    fn read(&mut self, buffer: &mut [u8], _: Duration) -> Result<usize, IoError> {
        let mut wire = self.wire.lock().unwrap();
        let Some(reply) = wire.reply.take() else {
            let change = if wire
                .drain_change
                .as_ref()
                .is_some_and(|(armed, _)| armed.load(Ordering::Acquire))
            {
                wire.drain_change.take().map(|(_, change)| change)
            } else {
                None
            };
            drop(wire);
            if let Some(change) = change {
                change();
            }
            return Err(IoError::Timeout);
        };
        assert!(reply.len() <= buffer.len());
        buffer[..reply.len()].copy_from_slice(&reply);
        Ok(reply.len())
    }
    fn write(&mut self, bytes: &[u8], _: Duration) -> Result<usize, IoError> {
        let command = std::str::from_utf8(bytes).unwrap().trim().to_owned();
        let mut wire = self.wire.lock().unwrap();
        wire.commands.push(command.clone());
        if wire.partial.as_ref() == Some(&command) {
            wire.accepted.push(bytes[..3].to_vec());
            return Ok(3);
        }
        wire.accepted.push(bytes.to_vec());
        let reply = if wire.reject.as_ref() == Some(&command) {
            "\r\nERROR\r\n".into()
        } else {
            match command.as_str() {
                "AT+CGSN" => "123456789012345\r\nOK\r\n".into(),
                "ATI" => "journal fixture\r\nOK\r\n".into(),
                "AT+CLCC" => "\r\nOK\r\n".into(),
                "AT+QCFG=\"ims\"" => format!("+QCFG: \"ims\",{},1\r\nOK\r\n", wire.ims),
                "AT+QCFG=\"volte_disable\"" => {
                    format!("+QCFG: \"volte_disable\",{}\r\nOK\r\n", wire.volte)
                }
                "AT+QCFG=\"volte_disable\",0" => {
                    if !wire.reboot_effective {
                        wire.volte = 0;
                    }
                    "\r\nOK\r\n".into()
                }
                "AT+QCFG=\"ims\",1" => {
                    if !wire.reboot_effective {
                        wire.ims = 1;
                    }
                    "\r\nOK\r\n".into()
                }
                _ => panic!("unexpected actual USB command {command}"),
            }
        };
        wire.reply = Some(reply.into_bytes());
        Ok(bytes.len())
    }
}
struct Fixture {
    root: PathBuf,
    lifetime: crate::daemon_lifetime::Lease,
    usb: crate::usb_at::Owner,
    modem: crate::modem_actor::ModemActor,
    service: Service,
    lease: CallModeReservation,
    wire: Arc<Mutex<Wire>>,
    captured: crate::call_mode_backups::CapturedNativeBackup,
}
fn composition() -> UsbComposition {
    UsbComposition {
        vendor_id: 0x2c7c,
        product_id: 0x2400,
        flags: [0; 7],
    }
}
fn current_voice() -> VoiceConfiguration {
    VoiceConfiguration {
        ims: 0,
        volte_capability: 1,
        volte_disabled: 1,
    }
}
fn target_voice() -> VoiceConfiguration {
    VoiceConfiguration {
        ims: 1,
        volte_capability: 1,
        volte_disabled: 0,
    }
}
impl Fixture {
    async fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/r585-wire")
            .join(crate::snapshot::new_instance_id());
        let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
        let wire = Arc::new(Mutex::new(Wire {
            volte: 1,
            ..Wire::default()
        }));
        let target = PhysicalTarget::synthetic_for_tests();
        let opened = wire.clone();
        let usb = crate::usb_at::Owner::with_factory(
            PRODUCER.into(),
            Arc::new(move || {
                Ok(Box::new(NativeTransport::new(
                    Box::new(ByteIo {
                        target: target.clone(),
                        wire: opened.clone(),
                    }),
                    "actual journal byte fixture".into(),
                    None,
                )))
            }),
        )
        .with_recovery_gate_for_tests(gate.clone());
        usb.claim_native_management().unwrap();
        let session = usb.open_native_session().await.unwrap();
        let modem = crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap();
        let service = Service::new(
            root.clone(),
            usb.clone(),
            modem.clone(),
            crate::availability::BackendAvailability::with_module_recovery(gate),
            PRODUCER.into(),
        );
        service.configure_native_usb(&session).await.unwrap();
        let lease = service.reserve_call_mode_restore_operation().await.unwrap();
        let id = crate::call_mode_backups::create_native_backup(
            &root,
            CallModeModuleIdentity {
                imei: "123456789012345".into(),
                firmware: String::new(),
            },
            composition(),
            target_voice(),
            false,
            &lifetime.root_proof(),
        )
        .await
        .unwrap();
        let captured =
            crate::call_mode_backups::capture_native_backup(&root, &id, &lifetime.root_proof())
                .await
                .unwrap();
        Self {
            root,
            lifetime,
            usb,
            modem,
            service,
            lease,
            wire,
            captured,
        }
    }
    async fn attach(&self) -> Journal {
        let journal = Journal::begin(
            &self.root,
            &self.lifetime.root_proof(),
            &self.captured,
            Intent {
                producer_id: PRODUCER.into(),
                reservation_token: self.lease.token.clone(),
                reservation_sequence: self.lease.sequence,
                operation_id: "actual-restore".into(),
                module_imei: "123456789012345".into(),
                current_usb: composition(),
                current_voice: current_voice(),
                target_usb: composition(),
                target_voice: Some(target_voice()),
                recovery_binding: None,
                voice_recovery_binding: None,
            },
        )
        .await
        .unwrap();
        self.service
            .attach_restore_journal(&self.lease, journal.clone())
            .unwrap();
        let id = crate::call_mode_backups::create_native_backup(
            &self.root,
            CallModeModuleIdentity {
                imei: "123456789012345".into(),
                firmware: String::new(),
            },
            composition(),
            current_voice(),
            false,
            &self.lifetime.root_proof(),
        )
        .await
        .unwrap();
        let safety = crate::call_mode_backups::capture_native_backup(
            &self.root,
            &id,
            &self.lifetime.root_proof(),
        )
        .await
        .unwrap();
        journal
            .safety_checkpoint(safety.id(), &safety.sha256())
            .unwrap();
        journal
    }
    async fn voice(&self) -> Result<Outcome, crate::module_radio::Failure> {
        self.service
            .call_mode_action(
                &self.lease,
                Action::CallModeVoice {
                    target: target_voice(),
                },
            )
            .await
    }
    fn mutations(&self) -> Vec<String> {
        self.wire
            .lock()
            .unwrap()
            .commands
            .iter()
            .filter(|s| {
                s.starts_with("AT+QCFG=\"ims\",") || s.starts_with("AT+QCFG=\"volte_disable\",")
            })
            .cloned()
            .collect()
    }
    fn summary(&self) -> durable::DiagnosticSummary {
        self.lifetime
            .root_proof()
            .with_root(&self.root, durable::diagnostic_summary)
            .unwrap()
            .unwrap()
            .unwrap()
    }
    async fn finish(&self) {
        self.service
            .release_call_mode_operation(&self.lease)
            .await
            .unwrap();
        self.service.finish_shutdown().await.unwrap();
        self.usb.shutdown().await.unwrap();
        self.modem.shutdown().unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.modem.shutdown(); /* retain evidence */
    }
}

#[tokio::test]
async fn actual_restore_without_journal_rejects_before_any_usb_command() {
    let f = Fixture::new().await;
    assert!(f.voice().await.is_err());
    assert!(f.wire.lock().unwrap().commands.is_empty());
    f.finish().await;
}
#[tokio::test]
async fn both_actual_voice_writes_have_distinct_durable_prepare_and_verified_readback() {
    let f = Fixture::new().await;
    f.attach().await;
    let outcome = f.voice().await.unwrap();
    assert!(outcome.submitted && outcome.complete && outcome.verified && !outcome.uncertain);
    assert_eq!(
        f.mutations(),
        vec!["AT+QCFG=\"volte_disable\",0", "AT+QCFG=\"ims\",1"]
    );
    {
        let wire = f.wire.lock().unwrap();
        let first = wire
            .commands
            .iter()
            .position(|s| s == "AT+QCFG=\"volte_disable\",0")
            .unwrap();
        assert_eq!(wire.commands[first + 1], "AT+QCFG=\"volte_disable\"");
    }
    let summary = f.summary();
    assert_eq!(summary.prepared_steps, 2);
    assert_eq!(summary.unknown_steps, 0);
    f.finish().await;
}
#[tokio::test]
async fn partial_first_or_second_actual_usb_write_keeps_unknown_and_never_replays() {
    for partial in ["AT+QCFG=\"volte_disable\",0", "AT+QCFG=\"ims\",1"] {
        let f = Fixture::new().await;
        f.attach().await;
        f.wire.lock().unwrap().partial = Some(partial.into());
        let outcome = f.voice().await.unwrap();
        assert!(outcome.submitted && outcome.uncertain && !outcome.verified);
        let before = f.mutations();
        assert_eq!(before.last().unwrap(), partial);
        assert_eq!(f.wire.lock().unwrap().accepted.last().unwrap().len(), 3);
        assert!(f.voice().await.is_err());
        assert_eq!(f.mutations(), before);
        assert_eq!(f.summary().unknown_steps, 1);
        f.finish().await;
    }
}
#[tokio::test]
async fn original_pre_reboot_readback_is_durable_but_never_a_final_target_receipt() {
    let f = Fixture::new().await;
    f.attach().await;
    f.wire.lock().unwrap().reboot_effective = true;
    let outcome = f.voice().await.unwrap();
    assert!(outcome.complete && outcome.verified && !outcome.uncertain);
    assert_eq!(outcome.voice, Some(current_voice()));
    let summary = f.summary();
    assert_eq!(summary.prepared_steps, 2);
    assert_eq!(summary.unknown_steps, 0);
    assert!(!summary.target_verified && !summary.receipt_present);
    f.finish().await;
}
#[tokio::test]
async fn prepared_namespace_loss_before_native_after_drain_guard_sends_zero_mutations() {
    let f = Fixture::new().await;
    let active = f.root.join(durable::ACTIVE_NAME);
    let retained = f.root.join("retained-journal");
    let armed = Arc::new(AtomicBool::new(false));
    let prepared = armed.clone();
    f.wire.lock().unwrap().drain_change = Some((
        armed,
        Box::new(move || {
            std::fs::rename(&active, &retained).unwrap();
        }),
    ));
    let _hook = durable::set_test_hook(
        &f.root,
        Arc::new(move |phase, _| {
            if phase == TestPhase::Prepared {
                prepared.store(true, Ordering::Release);
            }
        }),
    )
    .unwrap();
    f.attach().await;
    let outcome = f.voice().await.unwrap();
    assert!(!outcome.submitted && !outcome.uncertain);
    assert!(f.mutations().is_empty());
    f.finish().await;
}
#[tokio::test]
async fn actual_terminal_rejection_is_known_rejected_and_does_not_send_the_second_write() {
    let f = Fixture::new().await;
    f.attach().await;
    f.wire.lock().unwrap().reject = Some("AT+QCFG=\"volte_disable\",0".into());
    let outcome = f.voice().await.unwrap();
    assert!(outcome.submitted && !outcome.uncertain && !outcome.verified);
    assert_eq!(f.mutations(), vec!["AT+QCFG=\"volte_disable\",0"]);
    assert_eq!(f.summary().prepared_steps, 1);
    assert_eq!(f.summary().unknown_steps, 0);
    f.finish().await;
}
#[tokio::test]
async fn actual_write_followed_by_outcome_persistence_failure_stops_the_second_write() {
    let f = Fixture::new().await;
    let active = f.root.join(durable::ACTIVE_NAME);
    let retained = f.root.join("retained-journal");
    let once = AtomicBool::new(false);
    let _hook = durable::set_test_hook(
        &f.root,
        Arc::new(move |phase, _| {
            if phase == TestPhase::BeforeOutcomePersist && !once.swap(true, Ordering::AcqRel) {
                std::fs::rename(&active, &retained).unwrap();
            }
        }),
    )
    .unwrap();
    f.attach().await;
    let outcome = f.voice().await.unwrap();
    assert!(outcome.submitted && outcome.uncertain && !outcome.verified);
    assert_eq!(f.mutations(), vec!["AT+QCFG=\"volte_disable\",0"]);
    assert_eq!(outcome.error_code, "call_mode_restore_journal_unavailable");
    f.finish().await;
}
