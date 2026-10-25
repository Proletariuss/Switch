use super::*;
#[cfg(unix)]
mod unix {
    use super::*;
    use serialport::SerialPort as _;
    use std::sync::Mutex;

    fn fixture() -> (ActorState, serialport::TTYPort) {
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let mut state = ActorState::new();
        state
            .cellular_suppressed
            .store(false, std::sync::atomic::Ordering::Release);
        state.serial_session = 17;
        state.port = Some(Box::new(slave));
        (state, master)
    }
    fn command(master: &mut serialport::TTYPort) -> String {
        let mut line = Vec::new();
        loop {
            let mut byte = [0];
            assert_eq!(master.read(&mut byte).unwrap(), 1);
            line.push(byte[0]);
            if byte[0] == b'\n' {
                return String::from_utf8(line).unwrap().trim().into();
            }
            assert!(line.len() < 4096);
        }
    }
    fn begin(state: &mut ActorState, action: Action) -> Receiver<Result<Outcome, ActorError>> {
        let (reply, result) = mpsc::sync_channel(1);
        state.begin_radio(state.serial_session, action, reply);
        result
    }
    fn respond(state: &mut ActorState, text: &[u8]) {
        state.frame_input(text).unwrap();
        state.complete_pending_command();
    }
    fn unused_opener() -> PortOpener {
        Box::new(|_, _| panic!("unexpected real port open"))
    }

    #[test]
    fn serial_rf_preserves_priority_queue_until_plan_is_confirmed() {
        let (mut state, mut master) = fixture();
        let result = begin(&mut state, Action::SetVerify { target: 4 });
        state.start_next_command();
        assert_eq!(command(&mut master), "AT+CFUN?");
        let (reply, queued_result) = mpsc::sync_channel(1);
        state.process(
            Command::Execute {
                command: "AT+CSQ".into(),
                timeout: Duration::from_secs(1),
                high_priority: true,
                follow_up: None,
                expected_sms_generation: None,
                reply,
            },
            &unused_opener(),
        );
        for (response, next) in [
            (b"+CFUN: 1\r\nOK\r\n".as_slice(), "AT+CLCC"),
            (b"OK\r\n".as_slice(), "AT+CFUN=4"),
            (b"OK\r\n".as_slice(), "AT+CFUN?"),
        ] {
            respond(&mut state, response);
            assert!(queued_result.try_recv().is_err());
            state.start_next_command();
            assert_eq!(command(&mut master), next);
        }
        respond(&mut state, b"+CFUN: 4\r\nOK\r\n");
        let outcome = result.recv().unwrap().unwrap();
        assert!(outcome.complete && outcome.verified && outcome.submitted);
        state.start_next_command();
        assert_eq!(command(&mut master), "AT+CSQ");
        respond(&mut state, b"+CSQ: 20,99\r\nOK\r\n");
        assert_eq!(queued_result.recv().unwrap().unwrap().result, "ok");
    }
    #[test]
    fn serial_incoming_urc_stops_rf_before_write_and_remains_readable() {
        let (mut state, mut master) = fixture();
        let result = begin(&mut state, Action::SetVerify { target: 4 });
        state.start_next_command();
        assert_eq!(command(&mut master), "AT+CFUN?");
        respond(&mut state, b"RING\r\n+CFUN: 1\r\nOK\r\n");
        let outcome = result.recv().unwrap().unwrap();
        assert!(!outcome.complete && !outcome.submitted);
        assert_eq!(outcome.error_code, "radio_call_busy");
        let frames: Vec<_> = state
            .command_events
            .iter()
            .flat_map(|frame| frame.data.clone())
            .collect();
        assert!(String::from_utf8_lossy(&frames).contains("RING"));
    }
    #[test]
    fn serial_close_during_write_reports_unknown_and_old_binding_never_reopens() {
        let (mut state, mut master) = fixture();
        let result = begin(&mut state, Action::SetVerify { target: 4 });
        state.start_next_command();
        assert_eq!(command(&mut master), "AT+CFUN?");
        respond(&mut state, b"+CFUN: 1\r\nOK\r\n");
        state.start_next_command();
        assert_eq!(command(&mut master), "AT+CLCC");
        respond(&mut state, b"OK\r\n");
        state.start_next_command();
        assert_eq!(command(&mut master), "AT+CFUN=4");
        state.close_session();
        let outcome = result.recv().unwrap().unwrap();
        assert!(outcome.uncertain && outcome.submitted && !outcome.complete);
        let (_master, slave) = serialport::TTYPort::pair().unwrap();
        state.serial_session += 1;
        state.port = Some(Box::new(slave));
        let (reply, stale) = mpsc::sync_channel(1);
        state.begin_radio(17, Action::Observe { cellular: false }, reply);
        assert_eq!(
            stale.recv().unwrap().unwrap_err().code,
            "serial_session_changed"
        );
        assert!(state.deferred_radio.is_none() && state.pending_command.is_none());
    }
    #[test]
    fn serial_timeout_after_write_retires_instead_of_querying_late_ack() {
        let (mut state, mut master) = fixture();
        let result = begin(&mut state, Action::SetVerify { target: 4 });
        state.start_next_command();
        assert_eq!(command(&mut master), "AT+CFUN?");
        respond(&mut state, b"+CFUN: 1\r\nOK\r\n");
        state.start_next_command();
        assert_eq!(command(&mut master), "AT+CLCC");
        respond(&mut state, b"OK\r\n");
        state.start_next_command();
        assert_eq!(command(&mut master), "AT+CFUN=4");
        state.finish_command("timeout", None);
        let outcome = result.recv().unwrap().unwrap();
        assert!(outcome.uncertain && !outcome.complete);
        assert!(
            state.port.is_none()
                && state.pending_command.is_none()
                && state.deferred_radio.is_none()
        );
    }
    #[test]
    fn serial_retired_policy_epoch_cancels_refresh_gap_without_a_restore_write() {
        let (mut state, mut master) = fixture();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (reply, result) = mpsc::sync_channel(1);
        state.begin_radio_with_deadline(
            state.serial_session,
            Action::RefreshAfterProfileSwitch { target: 4 },
            Instant::now() + Duration::from_secs(55),
            cancelled.clone(),
            reply,
            None,
        );
        for (expected, response) in [
            ("AT+CFUN?", "+CFUN: 1\r\nOK\r\n"),
            ("AT+CLCC", "OK\r\n"),
            ("AT+CFUN=0", "OK\r\n"),
            ("AT+CFUN?", "+CFUN: 0\r\nOK\r\n"),
        ] {
            state.start_next_command();
            assert_eq!(command(&mut master), expected);
            respond(&mut state, response.as_bytes());
        }
        assert!(state.deferred_radio.is_some() && state.pending_command.is_none());
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        state.start_next_command();
        let outcome = result.recv().unwrap().unwrap();
        assert!(!outcome.complete && outcome.submitted && outcome.uncertain);
        assert_eq!(outcome.error_code, "radio_owner_revoked");
        assert!(
            state.deferred_radio.is_none()
                && state.pending_command.is_none()
                && state.port.is_some()
        );
        master.set_timeout(Duration::from_millis(20)).unwrap();
        let mut byte = [0];
        assert!(master.read(&mut byte).is_err());
    }
    #[test]
    fn serial_policy_change_rejects_queued_probe_without_blocking_sim() {
        let (mut state, mut master) = fixture();
        let (reply, probe) = mpsc::sync_channel(1);
        state.process(
            Command::Execute {
                command: "AT+CSQ".into(),
                timeout: Duration::from_secs(1),
                high_priority: false,
                follow_up: None,
                expected_sms_generation: None,
                reply,
            },
            &unused_opener(),
        );
        state
            .cellular_suppressed
            .store(true, std::sync::atomic::Ordering::Release);
        state.start_next_command();
        assert_eq!(
            probe.recv().unwrap().unwrap_err().code,
            "module_flight_mode_blocks_cellular_probe"
        );
        let (reply, sim) = mpsc::sync_channel(1);
        state.process(
            Command::Execute {
                command: "AT+CPIN?".into(),
                timeout: Duration::from_secs(1),
                high_priority: false,
                follow_up: None,
                expected_sms_generation: None,
                reply,
            },
            &unused_opener(),
        );
        state.start_next_command();
        assert_eq!(command(&mut master), "AT+CPIN?");
        respond(&mut state, b"+CPIN: READY\r\nOK\r\n");
        assert_eq!(sim.recv().unwrap().unwrap().result, "ok");
    }
    #[test]
    fn generic_serial_write_rejects_split_and_compound_cfun() {
        let (mut state, _master) = fixture();
        for data in [
            b"AT+CF".as_slice(),
            b"AT+CFUN=4\r\n".as_slice(),
            b"ATE0+CFUN=1\r\n".as_slice(),
            b"AT+TEST=\"unfinished\rAT+CFUN=1\r".as_slice(),
        ] {
            let (reply, result) = mpsc::sync_channel(1);
            state.process(
                Command::Write {
                    data: data.into(),
                    reply,
                },
                &unused_opener(),
            );
            assert!(result.recv().unwrap().is_err());
        }
        assert!(state.port.is_some());
    }
    #[tokio::test]
    async fn real_actor_radio_binding_and_transaction_work_without_go_command_rpc() {
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(3)).unwrap();
        let path = slave.name().unwrap();
        let port = Mutex::new(Some(slave));
        let actor = ModemActor::with_port_opener(move |_, _| {
            Ok(Box::new(port.lock().unwrap().take().unwrap()))
        })
        .unwrap();
        actor
            .open_with_initialization(path, 115_200, false)
            .unwrap();
        let session = actor.radio_binding().await.unwrap();
        let peer = thread::spawn(move || {
            for (expected, response) in [
                ("AT+CFUN?", "+CFUN: 1\r\nOK\r\n"),
                ("AT+CLCC", "OK\r\n"),
                ("AT+CFUN=4", "OK\r\n"),
                ("AT+CFUN?", "+CFUN: 4\r\nOK\r\n"),
            ] {
                assert_eq!(command(&mut master), expected);
                master.write_all(response.as_bytes()).unwrap();
            }
            // Keep the peer alive until the actor consumes the final reply.
            master
        });
        let outcome = actor
            .radio_execute(session, Action::SetVerify { target: 4 })
            .await
            .unwrap();
        assert!(outcome.complete && outcome.verified);
        let _master = peer.join().unwrap();
        actor.close().unwrap();
    }
    #[test]
    fn native_cs_hangup_uses_active_call_transport_and_captures_before_wire_read() {
        let (mut state, mut master) = fixture();
        let events = Arc::new(Mutex::new(Vec::new()));
        let records = events.clone();
        *state.call_capture.lock().unwrap() = Some(Arc::new(move |binding, line| {
            records
                .lock()
                .unwrap()
                .push((binding, line.trim().to_owned()));
            true
        }));
        state.frame_input(b"RING\r\n").unwrap();
        assert!(state.radio_call_active);
        let result = begin(
            &mut state,
            Action::CellularCall(crate::module_radio_transport::CallCommand {
                action: crate::module_radio::CallAction::Hangup,
                value: String::new(),
                admission: Arc::new(|| true),
            }),
        );
        state.start_next_command();
        assert_eq!(command(&mut master), "ATH");
        respond(&mut state, b"NO CARRIER\r\nOK\r\n");
        let result = result.recv().unwrap().unwrap();
        assert!(result.complete && result.verified && result.submitted);
        assert_eq!(
            events.lock().unwrap()[0].0,
            crate::module_radio_transport::Binding::Serial { serial_session: 17 }
        );
        assert!(
            state
                .command_events
                .iter()
                .any(|frame| frame.call_event_owned
                    && String::from_utf8_lossy(&frame.data).contains("NO CARRIER"))
        );
        state.close_session();
        assert_eq!(
            state
                .live_serial_session
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
    }
    #[test]
    fn registered_native_observer_blocks_raw_cs_bypass_without_submitting() {
        let (mut state, _master) = fixture();
        *state.call_capture.lock().unwrap() = Some(Arc::new(|_, _| true));
        let (reply, result) = mpsc::sync_channel(1);
        state.process(
            Command::Execute {
                command: "ATD123;".into(),
                timeout: Duration::from_secs(1),
                high_priority: false,
                follow_up: None,
                expected_sms_generation: None,
                reply,
            },
            &unused_opener(),
        );
        assert_eq!(
            result.recv().unwrap().unwrap_err().code,
            "radio_authorization_required"
        );
        assert!(state.pending_command.is_none());
    }
}

#[cfg(unix)]
mod call_mode_reconnect {
    use super::*;
    use serialport::SerialPort as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    fn read_command(master: &mut serialport::TTYPort) -> String {
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0u8];
            master.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
            if byte[0] == b'\n' {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }
    #[test]
    fn late_fenced_close_does_not_close_new_stream_or_interrupt_initialization() {
        let (_master, slave) = serialport::TTYPort::pair().unwrap();
        let mut state = ActorState::new();
        state.serial_session = 18;
        state.port = Some(Box::new(slave));
        let (init_reply, init_result) = mpsc::sync_channel(1);
        state.initialization.required = true;
        state.begin_initialization(init_reply);
        let (read_reply, _read_result) = mpsc::sync_channel(1);
        state.pending_read = Some(PendingRead {
            maximum: 128,
            deadline: Instant::now() + Duration::from_secs(1),
            reply: read_reply,
        });
        let opener: PortOpener = Box::new(|_, _| panic!("close must not open"));
        let (reply, result) = mpsc::sync_channel(1);
        state.process(
            Command::FencedClose {
                serial_session: 17,
                reply,
            },
            &opener,
        );
        result.recv().unwrap().unwrap();
        assert!(state.port.is_some());
        assert!(state.pending_read.is_some());
        assert!(state.initialization.running);
        assert!(init_result.try_recv().is_err());
        let (reply, result) = mpsc::sync_channel(1);
        state.process(
            Command::FencedClose {
                serial_session: 18,
                reply,
            },
            &opener,
        );
        result.recv().unwrap().unwrap();
        assert!(state.port.is_none());
        assert!(!state.initialization.running);
        assert!(init_result.recv().unwrap().is_err());
    }
    #[test]
    fn open_command_acknowledges_its_generation_atomically() {
        let (_master, slave) = serialport::TTYPort::pair().unwrap();
        let candidate = std::sync::Mutex::new(Some(slave));
        let opener: PortOpener =
            Box::new(move |_, _| Ok(Box::new(candidate.lock().unwrap().take().unwrap())));
        let mut state = ActorState::new();
        state.serial_session = 17;
        let (reply, result) = mpsc::sync_channel(1);
        state.process(
            Command::Open {
                path: "/dev/cu.fake-call-mode".into(),
                baud_rate: 115200,
                initialization_required: true,
                reply,
            },
            &opener,
        );
        assert_eq!(result.recv().unwrap().unwrap(), 18);
        assert_eq!(state.serial_session, 18);
    }
    #[test]
    fn serial_reconnect_installs_only_matching_proven_imei_after_terminal() {
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let peer = std::thread::spawn(move || {
            assert_eq!(read_command(&mut master), "AT+CGSN\r\n");
            master.write_all(b"123456789012345\r\n").unwrap();
            std::thread::sleep(Duration::from_millis(50));
            master.write_all(b"OK\r\n").unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });
        let candidate = std::sync::Mutex::new(Some(slave));
        let opener: PortOpener = Box::new(move |path, baud| {
            assert_eq!(path, "/dev/cu.fake-call-mode");
            assert_eq!(baud, 115200);
            Ok(Box::new(candidate.lock().unwrap().take().unwrap()))
        });
        let mut state = ActorState::new();
        state.serial_session = 17;
        state.last_radio_port = Some(("/dev/cu.fake-call-mode".into(), 115200));
        let result = state
            .call_mode_radio_reconnect(
                17,
                "123456789012345",
                Instant::now() + Duration::from_secs(2),
                &AtomicBool::new(false),
                &opener,
            )
            .unwrap();
        assert_eq!(result, 18);
        assert!(state.port.is_some());
        assert_eq!(
            state.current_path.as_deref(),
            Some("/dev/cu.fake-call-mode")
        );
        assert!(!state.initialization.required);
        peer.join().unwrap();
    }
    #[test]
    fn serial_reconnect_never_retains_wrong_device() {
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let peer = std::thread::spawn(move || {
            assert_eq!(read_command(&mut master), "AT+CGSN\r\n");
            master.write_all(b"987654321098765\r\nOK\r\n").unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });
        let candidate = std::sync::Mutex::new(Some(slave));
        let opener: PortOpener = Box::new(move |path, _| {
            if path != "/dev/cu.fake-call-mode" {
                return Err(ActorError::unavailable("fake candidate absent"));
            }
            Ok(Box::new(candidate.lock().unwrap().take().unwrap()))
        });
        let mut state = ActorState::new();
        state.serial_session = 17;
        state.last_radio_port = Some(("/dev/cu.fake-call-mode".into(), 115200));
        assert_eq!(
            state
                .call_mode_radio_reconnect(
                    17,
                    "123456789012345",
                    Instant::now() + Duration::from_secs(2),
                    &AtomicBool::new(false),
                    &opener
                )
                .unwrap_err()
                .code,
            "call_mode_module_identity_unconfirmed"
        );
        assert!(state.port.is_none());
        assert_eq!(state.serial_session, 17);
        peer.join().unwrap();
    }
    #[test]
    fn canceled_or_expired_serial_reconnect_never_opens_or_closes_existing_port() {
        for cancelled in [false, true] {
            let (_master, slave) = serialport::TTYPort::pair().unwrap();
            let mut state = ActorState::new();
            state.serial_session = 17;
            state.port = Some(Box::new(slave));
            state.last_radio_port = Some(("/dev/cu.fake-call-mode".into(), 115200));
            let opener: PortOpener = Box::new(|_, _| panic!("revoked worker attempted open"));
            let deadline = if cancelled {
                Instant::now() + Duration::from_secs(2)
            } else {
                Instant::now()
            };
            assert_eq!(
                state
                    .call_mode_radio_reconnect(
                        17,
                        "123456789012345",
                        deadline,
                        &AtomicBool::new(cancelled),
                        &opener
                    )
                    .unwrap_err()
                    .code,
                "radio_owner_revoked"
            );
            assert!(state.port.is_some());
            assert_eq!(state.serial_session, 17);
        }
    }
    #[test]
    fn cancellation_during_serial_open_drops_candidate_without_installing() {
        let (_master, slave) = serialport::TTYPort::pair().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let candidate = std::sync::Mutex::new(Some(slave));
        let opener: PortOpener = Box::new(move |_, _| {
            flag.store(true, Ordering::Release);
            Ok(Box::new(candidate.lock().unwrap().take().unwrap()))
        });
        let mut state = ActorState::new();
        state.serial_session = 17;
        state.last_radio_port = Some(("/dev/cu.fake-call-mode".into(), 115200));
        let error = state
            .call_mode_radio_reconnect(
                17,
                "123456789012345",
                Instant::now() + Duration::from_secs(2),
                &cancelled,
                &opener,
            )
            .unwrap_err();
        assert!(matches!(
            error.code,
            "radio_owner_revoked" | "call_mode_module_identity_unconfirmed"
        ));
        assert!(state.port.is_none());
        assert_eq!(state.serial_session, 17);
    }
}
