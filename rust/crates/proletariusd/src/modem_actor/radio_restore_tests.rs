// The existing Fixture owns the real PTY actor and actual native reservation.
impl Fixture {
    async fn restore_reservation(&mut self) {
        self.service
            .release_call_mode_operation(&self.reservation)
            .await
            .unwrap();
        self.reservation = self
            .service
            .reserve_call_mode_restore_operation()
            .await
            .unwrap();
    }
    async fn attach_restore_journal(&self) -> crate::call_mode_restore_journal::Journal {
        use crate::call_mode_restore_journal::{Intent, Journal};
        let proof = self.lifetime.as_ref().unwrap().root_proof();
        let id = crate::call_mode_backups::create_native_backup(
            &self.root,
            crate::module_radio_transport::CallModeModuleIdentity {
                imei: "123456789012345".into(),
                firmware: String::new(),
            },
            journal_usb(),
            journal_target_voice(),
            false,
            &proof,
        )
        .await
        .unwrap();
        let captured = crate::call_mode_backups::capture_native_backup(&self.root, &id, &proof)
            .await
            .unwrap();
        let journal = Journal::begin(
            &self.root,
            &proof,
            &captured,
            Intent {
                producer_id: PRODUCER.into(),
                reservation_token: self.reservation.token.clone(),
                reservation_sequence: self.reservation.sequence,
                operation_id: "serial-restore".into(),
                module_imei: "123456789012345".into(),
                current_usb: journal_usb(),
                current_voice: VoiceConfiguration {
                    ims: 0,
                    volte_capability: 1,
                    volte_disabled: 1,
                },
                target_usb: journal_usb(),
                target_voice: Some(journal_target_voice()),
                recovery_binding: None,
                voice_recovery_binding: None,
            },
        )
        .await
        .unwrap();
        self.service
            .attach_restore_journal(&self.reservation, journal.clone())
            .unwrap();
        let id = crate::call_mode_backups::create_native_backup(
            &self.root,
            crate::module_radio_transport::CallModeModuleIdentity {
                imei: "123456789012345".into(),
                firmware: String::new(),
            },
            journal_usb(),
            VoiceConfiguration {
                ims: 0,
                volte_capability: 1,
                volte_disabled: 1,
            },
            false,
            &proof,
        )
        .await
        .unwrap();
        let safety = crate::call_mode_backups::capture_native_backup(&self.root, &id, &proof)
            .await
            .unwrap();
        journal
            .safety_checkpoint(safety.id(), &safety.sha256())
            .unwrap();
        journal
    }
    fn restore_voice_reads(&mut self) {
        self.answer("AT+CLCC", b"OK\r\n");
        self.answer("AT+QCFG=\"ims\"", b"+QCFG: \"ims\",0,1\r\nOK\r\n");
        self.answer(
            "AT+QCFG=\"volte_disable\"",
            b"+QCFG: \"volte_disable\",1\r\nOK\r\n",
        );
    }
    fn journal_summary(&self) -> crate::call_mode_restore_journal::DiagnosticSummary {
        self.lifetime
            .as_ref()
            .unwrap()
            .root_proof()
            .with_root(
                &self.root,
                crate::call_mode_restore_journal::diagnostic_summary,
            )
            .unwrap()
            .unwrap()
            .unwrap()
    }
}
fn journal_usb() -> crate::module_radio_transport::UsbComposition {
    crate::module_radio_transport::UsbComposition {
        vendor_id: 0x2c7c,
        product_id: 0x2400,
        flags: [0; 7],
    }
}
fn journal_target_voice() -> VoiceConfiguration {
    VoiceConfiguration {
        ims: 1,
        volte_capability: 1,
        volte_disabled: 0,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_serial_missing_journal_never_sends_any_configuration_command() {
    let mut f = Fixture::new().await;
    f.restore_reservation().await;
    let result = f.begin_voice().await.unwrap();
    assert!(result.is_err());
    f.no_more_bytes();
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_serial_actual_both_writes_have_separate_prepare_and_typed_readback() {
    let mut f = Fixture::new().await;
    f.restore_reservation().await;
    f.attach_restore_journal().await;
    let task = f.begin_voice();
    f.restore_voice_reads();
    f.answer("AT+QCFG=\"volte_disable\",0", b"OK\r\n");
    f.answer(
        "AT+QCFG=\"volte_disable\"",
        b"+QCFG: \"volte_disable\",0\r\nOK\r\n",
    );
    f.answer("AT+QCFG=\"ims\",1", b"OK\r\n");
    f.answer("AT+QCFG=\"ims\"", b"+QCFG: \"ims\",1,1\r\nOK\r\n");
    f.answer(
        "AT+QCFG=\"volte_disable\"",
        b"+QCFG: \"volte_disable\",0\r\nOK\r\n",
    );
    let result = outcome(task).await;
    assert!(result.submitted && result.complete && result.verified && !result.uncertain);
    assert_eq!(f.journal_summary().prepared_steps, 2);
    assert_eq!(f.journal_summary().unknown_steps, 0);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_serial_lost_second_ack_preserves_unknown_and_rejects_later_mutation() {
    let mut f = Fixture::new().await;
    f.restore_reservation().await;
    f.attach_restore_journal().await;
    let task = f.begin_voice();
    f.restore_voice_reads();
    f.answer("AT+QCFG=\"volte_disable\",0", b"OK\r\n");
    f.answer(
        "AT+QCFG=\"volte_disable\"",
        b"+QCFG: \"volte_disable\",0\r\nOK\r\n",
    );
    assert_eq!(line(&mut f.peer), "AT+QCFG=\"ims\",1");
    f.actor.close_native_serial(&f.session).await.unwrap();
    let result = outcome(task).await;
    assert!(result.submitted && result.uncertain && !result.verified);
    assert_eq!(f.journal_summary().prepared_steps, 2);
    assert_eq!(f.journal_summary().unknown_steps, 1);
    assert!(f.begin_voice().await.unwrap().is_err());
    f.service
        .release_call_mode_operation(&f.reservation)
        .await
        .unwrap();
    f.actor.shutdown().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_serial_outcome_storage_failure_after_first_write_prevents_second_write() {
    use crate::call_mode_restore_journal::{self as durable, TestPhase};
    let mut f = Fixture::new().await;
    f.restore_reservation().await;
    let active = f.root.join(durable::ACTIVE_NAME);
    let retained = f.root.join("retained-serial-journal");
    let once = std::sync::atomic::AtomicBool::new(false);
    let _hook = durable::set_test_hook(
        &f.root,
        Arc::new(move |phase, _| {
            if phase == TestPhase::BeforeOutcomePersist
                && !once.swap(true, std::sync::atomic::Ordering::AcqRel)
            {
                std::fs::rename(&active, &retained).unwrap();
            }
        }),
    )
    .unwrap();
    f.attach_restore_journal().await;
    let task = f.begin_voice();
    f.restore_voice_reads();
    f.answer("AT+QCFG=\"volte_disable\",0", b"OK\r\n");
    let result = outcome(task).await;
    assert!(result.submitted && result.uncertain && !result.verified);
    assert_eq!(result.error_code, "call_mode_restore_journal_unavailable");
    f.no_more_bytes();
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serial_same_imei_rebind_cannot_clear_restore_reboot_unknown_without_native_successor() {
    let mut f = Fixture::new_with_reopen(true).await;
    f.restore_reservation().await;
    f.attach_restore_journal().await;
    crate::module_radio_store::Store::new(f.root.clone())
        .save(crate::module_radio_store::Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        })
        .unwrap();
    let owner = f.service.clone();
    let lease = f.reservation.clone();
    let reboot = tokio::spawn(async move { owner.call_mode_action(&lease, Action::Reboot).await });
    f.answer("AT+CFUN?", b"+CFUN: 1\r\nOK\r\n");
    f.answer("AT+CLCC", b"OK\r\n");
    assert_eq!(line(&mut f.peer), "AT+CFUN=1,1");
    f.actor.close_native_serial(&f.session).await.unwrap();
    let result = outcome(reboot).await;
    assert!(result.submitted && result.uncertain);
    let owner = f.service.clone();
    let lease = f.reservation.clone();
    let rebind =
        tokio::spawn(async move { owner.call_mode_rebind(&lease, "123456789012345").await });
    // Follow the actual fresh PTY handed to the injected opener. All ensuing
    // commands/ACKs/readbacks still cross the actor's real byte transport.
    f.peer = f
        .reopened
        .recv_timeout(WAIT)
        .expect("actual replacement PTY did not open");
    f.answer("AT+CGSN", b"123456789012345\r\nOK\r\n");
    for command in [
        "AT",
        "ATE0",
        "AT+CMGF=0",
        "AT+CNMI=2,1,0,0,0",
        "AT+CLIP=1",
        "AT+QPCMV=1,2",
    ] {
        f.answer(command, b"OK\r\n");
    }
    f.answer("AT+CGSN", b"123456789012345\r\nOK\r\n");
    f.answer("ATI", b"PTY reconnected\r\nOK\r\n");
    tokio::time::timeout(WAIT, rebind)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(f.journal_summary().unknown_steps, 1);
    let rejected = f.begin_voice().await.unwrap().unwrap_err();
    assert_eq!(rejected.code, "radio_outcome_unknown");
    f.no_more_bytes();
    f.service
        .release_call_mode_operation(&f.reservation)
        .await
        .unwrap();
    f.service.finish_shutdown().await.unwrap();
    f.actor.shutdown().unwrap();
}
