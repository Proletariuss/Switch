// Included in the existing FakeIo test module: production NativeTransport
// owns framing and terminal recognition before the shared CLCC parser runs.
const DATA_ONLY_CLCC: &str = "AT+CLCC\r\n+CLCC: 1,0,0,1,0,\"\",128\r\nOK\r\n";

fn fragmented_clcc(response: &str) -> crate::module_radio::CallStateSnapshot {
    let mut actions = vec![quiet(), write(b"AT+CLCC\r")];
    actions.extend(response.as_bytes().chunks(3).map(read));
    let (mut transport, trace) = fake(actions);
    let reply = transport
        .command("AT+CLCC", None, Duration::from_secs(3), 4096)
        .unwrap();
    let observed = crate::module_radio::call_snapshot(reply);
    let trace = trace.lock().unwrap();
    assert!(trace.actions.is_empty(), "all response fragments were consumed");
    assert_eq!(trace.writes, vec![b"AT+CLCC\r".to_vec()]);
    observed
}

#[test]
fn fragmented_standard_packet_data_clcc_is_voice_idle_after_native_transport() {
    let observed = fragmented_clcc(DATA_ONLY_CLCC);
    assert!(observed.safe_idle);
    assert_eq!(observed.display["state"], "idle");
    assert!(!observed.display["number"].as_str().is_some_and(|number| !number.is_empty()));
}

#[test]
fn fragmented_voice_mixed_and_unknown_clcc_keep_native_admission_closed() {
    for (response, expected) in [
        ("+CLCC: 1,0,0,0,0,\"\",128\r\nOK\r\n", "active"),
        ("+CLCC: 1,0,1,0,0,\"\",128\r\nOK\r\n", "active"),
        ("+CLCC: 1,1,4,0,0,\"\",128\r\nOK\r\n", "incoming"),
        (
            "+CLCC: 1,0,0,1,0,\"\",128\r\n+CLCC: 2,1,4,0,0,\"\",128\r\nOK\r\n",
            "incoming",
        ),
        ("+CLCC: 1,0,0,9,0,\"\",128\r\nOK\r\n", "unknown"),
        ("+CLCC: 1,0,0,1,0,\"\",128\r\nRING\r\nOK\r\n", "unknown"),
    ] {
        let observed = fragmented_clcc(response);
        assert!(!observed.safe_idle, "{response:?}");
        assert_eq!(observed.display["state"], expected, "{response:?}");
        assert!(!observed.display["number"].as_str().is_some_and(|number| number.contains(",128")));
    }
}

#[test]
fn incomplete_packet_data_reply_cannot_become_idle_after_native_deadline() {
    let response = "AT+CLCC\r\n+CLCC: 1,0,0,1,0,\"\",128\r\n";
    let mut actions = vec![quiet(), write(b"AT+CLCC\r")];
    actions.extend(response.as_bytes().chunks(3).map(read));
    let (mut transport, trace) = fake(actions);
    let error = transport
        .command("AT+CLCC", None, Duration::from_secs(3), 4096)
        .unwrap_err();
    assert_eq!(error.code, "usb_at_status_incomplete_response");
    assert!(error.uncertain);
    assert_eq!(trace.lock().unwrap().writes, vec![b"AT+CLCC\r".to_vec()]);
}

#[test]
fn guarded_clcc_requires_the_final_line_ending_before_accepting_ok() {
    for partial in [
        "OK",
        "AT+CLCC\r\n+CLCC: 1,0,0,1,0,\"\",128\r\nOK",
    ] {
        let (mut transport, trace) = fake(vec![
            quiet(),
            write(b"AT+CLCC\r"),
            read(partial.as_bytes()),
        ]);
        let error = transport
            .command_with_guard(
                "AT+CLCC",
                None,
                Duration::from_millis(100),
                4096,
                Arc::new(|| true),
            )
            .unwrap_err();
        assert_eq!(error.code, "usb_at_status_incomplete_response");
        assert!(error.uncertain);
        assert_eq!(trace.lock().unwrap().writes, vec![b"AT+CLCC\r".to_vec()]);
    }
}

#[test]
fn strict_clcc_keeps_the_guard_after_drain_and_before_the_physical_write() {
    let (mut transport, trace) = fake(vec![read(b"RING\r\n"), quiet()]);
    let admitted = Arc::new(AtomicBool::new(true));
    let revoked = admitted.clone();
    transport.set_line_capture(Arc::new(move |line| {
        if line.trim() == "RING" {
            revoked.store(false, Ordering::Release);
        }
    }));
    let error = transport
        .command_with_guard(
            "AT+CLCC",
            None,
            Duration::from_secs(3),
            4096,
            Arc::new(move || admitted.load(Ordering::Acquire)),
        )
        .unwrap_err();
    assert_eq!(error.code, "module_call_admission_changed");
    assert!(!error.uncertain);
    assert!(trace.lock().unwrap().writes.is_empty());
}

#[test]
fn non_clcc_ordinary_commands_keep_partial_response_compatibility() {
    let (mut transport, trace) = fake(vec![
        quiet(),
        write(b"AT+CSQ\r"),
        read(b"+CSQ: 18"),
    ]);
    assert_eq!(
        transport
            .command("AT+CSQ", None, Duration::from_millis(100), 4096)
            .unwrap(),
        "+CSQ: 18"
    );
    assert_eq!(trace.lock().unwrap().writes, vec![b"AT+CSQ\r".to_vec()]);
}

#[tokio::test]
async fn native_call_read_and_mode_idle_retire_incomplete_clcc_without_consuming_late_ok() {
    use crate::module_radio_transport::Action as RadioAction;
    for action in [RadioAction::CellularCallRead, RadioAction::CallModeIdleRead] {
        for partial in [
            "OK",
            "AT+CLCC\r\n+CLCC: 1,0,0,1,0,\"\",128\r\nOK",
        ] {
            let mut actions = vec![quiet(), write(b"AT+CLCC\r"), read(partial.as_bytes())];
            // CLCC's production plan has a four-second command budget.
            // Five bounded reads exhaust it before the delayed line ending.
            for _ in 0..5 {
                actions.push(Action::Read(Err(IoError::Timeout), Duration::from_secs(4)));
            }
            actions.push(read(b"\r\n"));
            let (owner, session, trace) = byte_owner(actions).await;
            let outcome = owner
                .radio_execute(
                    "native-module-test".into(),
                    session.clone(),
                    action.clone(),
                    None,
                )
                .await
                .unwrap();
            assert!(!outcome.complete && !outcome.verified && !outcome.submitted);
            assert!(!outcome.error_code.is_empty());
            assert!(owner.validate_radio_binding("native-module-test", &session).is_err());
            {
                let trace = trace.lock().unwrap();
                assert!(trace.dropped, "uncertain CLCC retires the actual byte transport");
                assert_eq!(trace.writes, vec![b"AT+CLCC\r".to_vec()]);
                assert_eq!(trace.actions.len(), 1, "the late terminal remains unread");
            }
            // The old session cannot issue another command or reinterpret the
            // delayed terminator as the response to a subsequent query.
            assert!(
                owner
                    .radio_execute(
                        "native-module-test".into(),
                        session,
                        RadioAction::CellularCallRead,
                        None,
                    )
                    .await
                    .is_err()
            );
            assert_eq!(trace.lock().unwrap().writes, vec![b"AT+CLCC\r".to_vec()]);
            assert_eq!(trace.lock().unwrap().actions.len(), 1);
            owner.shutdown().await.unwrap();
        }
    }
}
