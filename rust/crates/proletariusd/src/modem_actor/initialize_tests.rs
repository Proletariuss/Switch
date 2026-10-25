use super::*;

#[test]
fn native_initialization_requires_a_separate_open_capability_opt_in() {
    // Previous helpers already owned SMS reception, but still ran their own
    // startup sequence. That wire payload must not silently acquire a gate
    // which the older helper cannot complete.
    let old: OpenRequest = serde_json::from_str(
        r#"{"port":"/dev/fixture","baud_rate":115200,"sms_receive_owned":true}"#,
    )
    .unwrap();
    assert!(old.sms_receive_owned);
    assert!(!old.initialization_owned);
    let native: OpenRequest = serde_json::from_str(
        r#"{"port":"/dev/fixture","baud_rate":115200,"sms_receive_owned":true,"initialization_owned":true}"#,
    ).unwrap();
    assert!(native.sms_receive_owned && native.initialization_owned);
}

#[cfg(unix)]
mod unix {
    use super::*;
    use serialport::SerialPort as _;
    use std::sync::Mutex;

    const COMMANDS: [&str; 6] = [
        "AT",
        "ATE0",
        "AT+CMGF=0",
        "AT+CNMI=2,1,0,0,0",
        "AT+CLIP=1",
        "AT+QPCMV=1,2",
    ];

    fn fixture(required: bool) -> (ActorState, serialport::TTYPort) {
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(2)).unwrap();
        let mut state = ActorState::new();
        state
            .cellular_suppressed
            .store(false, std::sync::atomic::Ordering::Release);
        state.serial_session = 17;
        state.current_path = slave.name();
        state.current_baud = 115_200;
        state.port = Some(Box::new(slave));
        state.initialization.required = required;
        (state, master)
    }

    fn unused_opener() -> PortOpener {
        Box::new(|_, _| panic!("fixture unexpectedly opened a real port"))
    }

    fn begin(state: &mut ActorState) -> Receiver<Result<InitializationResult, ActorError>> {
        let (reply, result) = mpsc::sync_channel(1);
        state.process(Command::Initialize { reply }, &unused_opener());
        result
    }

    fn due(state: &mut ActorState) {
        state
            .initialization
            .deferred
            .as_mut()
            .expect("initialization must retain its delayed step")
            .ready_at = Instant::now() - Duration::from_millis(1);
    }

    fn command(master: &mut serialport::TTYPort) -> String {
        let mut line = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            assert_eq!(master.read(&mut byte).expect("read fixture AT command"), 1);
            line.push(byte[0]);
            if byte[0] == b'\n' {
                return String::from_utf8(line).unwrap().trim().to_owned();
            }
            assert!(line.len() < 1024, "AT command exceeded fixture bound");
        }
    }

    fn no_command(master: &mut serialport::TTYPort) {
        master.set_timeout(Duration::from_millis(20)).unwrap();
        let mut byte = [0_u8; 1];
        assert!(matches!(
            master.read(&mut byte),
            Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
        ));
        master.set_timeout(Duration::from_secs(2)).unwrap();
    }

    fn step(state: &mut ActorState, master: &mut serialport::TTYPort, response: &str) -> String {
        due(state);
        state.start_next_command();
        assert!(
            state.pending_command.is_some(),
            "initialization failed to reach the serial writer"
        );
        let sent = command(master);
        state.frame_input(response.as_bytes()).unwrap();
        state.complete_pending_command();
        sent
    }

    fn complete(state: &mut ActorState, master: &mut serialport::TTYPort, results: [&str; 6]) {
        for (expected, response) in COMMANDS.into_iter().zip(results) {
            assert_eq!(step(state, master, response), expected);
        }
        assert!(!state.initialization.sms_ready());
        due(state);
        state.poll_initialization();
    }

    fn json(result: &InitializationResult) -> Value {
        serde_json::to_value(result).unwrap()
    }

    #[test]
    fn native_rebind_cancellation_stops_followup_initialization_writes() {
        let (mut state, mut master) = fixture(true);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (reply, result) = mpsc::sync_channel(1);
        state.process(
            Command::CallModeRadioInitialize {
                serial_session: 17,
                deadline: Instant::now() + Duration::from_secs(2),
                cancelled: cancelled.clone(),
                reply,
            },
            &unused_opener(),
        );
        assert_eq!(step(&mut state, &mut master, "OK\r\n"), "AT");
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        due(&mut state);
        state.start_next_command();
        assert_eq!(
            result.recv().unwrap().unwrap_err().code,
            "radio_owner_revoked"
        );
        assert!(!state.initialization.sms_ready());
        no_command(&mut master);
    }
    #[test]
    fn deferred_probe_does_not_write_or_block_control_until_its_deadline() {
        let (mut state, mut master) = fixture(true);
        let result = begin(&mut state);
        state.initialization.deferred.as_mut().unwrap().ready_at =
            Instant::now() + Duration::from_secs(60);
        state.start_next_command();
        assert!(state.pending_command.is_none());
        assert!(state.pending_normal_commands.is_empty());
        assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
        no_command(&mut master);

        // Advance the actor's deadline explicitly; never sleep to let it pass.
        assert_eq!(step(&mut state, &mut master, "OK\r\n"), "AT");
        assert!(state.initialization.running);
    }

    #[test]
    fn failed_probe_is_one_cached_result_and_never_replays_initialization_io() {
        let (mut state, mut master) = fixture(true);
        let result = begin(&mut state);
        assert_eq!(step(&mut state, &mut master, "ERROR\r\n"), "AT");
        let first = result.recv().unwrap().unwrap();
        assert!(first.completed);
        assert!(!first.probe_succeeded && !first.sms_pdu_ready);
        assert_eq!(first.steps.len(), 1);
        assert_eq!(first.steps[0].name, "probe");
        assert_eq!(first.steps[0].result, "error");
        assert!(!state.initialization.running);
        assert!(!state.initialization.sms_ready());
        let cached = begin(&mut state).recv().unwrap().unwrap();
        assert_eq!(json(&cached), json(&first));
        state.start_next_command();
        no_command(&mut master);
    }

    #[test]
    fn pdu_failure_finishes_remaining_steps_but_retains_cmti_and_blocks_storage_io() {
        let (mut state, mut master) = fixture(true);
        state.configure_receive(true).unwrap();
        state.frame_input(b"+CMTI: \"ME\",7\r\n").unwrap();
        assert!(state.next_receive().is_none());
        let result = begin(&mut state);
        complete(
            &mut state,
            &mut master,
            [
                "OK\r\n",
                "OK\r\n",
                "ERROR\r\n",
                "OK\r\n",
                "OK\r\n",
                "OK\r\n",
            ],
        );
        let result = result.recv().unwrap().unwrap();
        assert!(result.completed && result.probe_succeeded);
        assert!(!result.sms_pdu_ready);
        assert_eq!(result.steps.len(), 6);
        assert_eq!(result.steps[2].result, "error");
        assert_eq!(result.steps[5].name, "usb_audio");
        assert!(state.next_receive().is_none());
        assert_eq!(state.scan_receive().unwrap_err().code, "sms_not_ready");
        assert_storage_gate(&mut state, true);
        state.start_next_command();
        no_command(&mut master);

        // Inspect retention by removing only the readiness requirement. If
        // the gate consumed/dropped the event, no work would remain here.
        state.initialization.required = false;
        assert!(
            state.next_receive().is_some(),
            "failed init lost the original CMTI"
        );
    }

    #[test]
    fn optional_setup_failures_are_reported_without_disabling_confirmed_pdu_mode() {
        let (mut state, mut master) = fixture(true);
        let result = begin(&mut state);
        complete(
            &mut state,
            &mut master,
            [
                "OK\r\n",
                "ERROR\r\n",
                "OK\r\n",
                "ERROR\r\n",
                "ERROR\r\n",
                "ERROR\r\n",
            ],
        );
        let result = result.recv().unwrap().unwrap();
        assert!(result.completed && result.probe_succeeded && result.sms_pdu_ready);
        assert!(state.initialization.sms_ready());
        assert_eq!(
            result
                .steps
                .iter()
                .filter(|step| step.result == "error")
                .count(),
            4
        );
        assert!(state.scan_receive().is_ok());
        assert!(state.next_receive().is_some());
    }

    fn assert_storage_gate(state: &mut ActorState, gated: bool) {
        let generation = state.sms_generation;
        let (reply, read) = mpsc::sync_channel(1);
        state.process(
            Command::ReadStoredSms {
                storage: Some("ME".into()),
                index: Some(7),
                expected_sms_generation: generation,
                receive_fence: None,
                reply,
            },
            &unused_opener(),
        );
        let (reply, confirm) = mpsc::sync_channel(1);
        state.process(
            Command::ConfirmSmsGeneration {
                expected_sms_generation: generation,
                receive_fence: None,
                reply,
            },
            &unused_opener(),
        );
        let (reply, delete) = mpsc::sync_channel(1);
        state.process(
            Command::DeleteArchivedSms {
                index: 7,
                pdu: "001122".into(),
                expected_sms_generation: generation,
                receive_fence: None,
                reply,
            },
            &unused_opener(),
        );
        if gated {
            assert_eq!(read.recv().unwrap().unwrap_err().code, "sms_not_ready");
            assert_eq!(confirm.recv().unwrap().unwrap_err().code, "sms_not_ready");
            assert_eq!(delete.recv().unwrap().unwrap_err().code, "sms_not_ready");
            assert!(state.pending_normal_commands.is_empty());
        } else {
            assert!(confirm.recv().unwrap().is_ok());
            assert!(matches!(read.try_recv(), Err(mpsc::TryRecvError::Empty)));
            assert!(matches!(delete.try_recv(), Err(mpsc::TryRecvError::Empty)));
            assert_eq!(state.pending_normal_commands.len(), 2);
            assert!(matches!(
                state
                    .pending_normal_commands
                    .pop_front()
                    .unwrap()
                    .completion,
                CommandCompletion::ReadStoredSms(_)
            ));
            assert!(matches!(
                state
                    .pending_normal_commands
                    .pop_front()
                    .unwrap()
                    .completion,
                CommandCompletion::DeleteArchivedSms(_)
            ));
        }
    }

    #[test]
    fn direct_read_confirm_delete_require_ready_but_legacy_open_remains_compatible() {
        for required in [true, false] {
            let (mut state, mut master) = fixture(required);
            assert_storage_gate(&mut state, required);
            no_command(&mut master);
        }
    }

    #[test]
    fn duplicate_initialize_is_busy_in_deferred_queued_and_pending_states() {
        for phase in ["deferred", "queued", "pending"] {
            let (mut state, mut master) = fixture(true);
            let _first = begin(&mut state);
            if phase != "deferred" {
                due(&mut state);
                state.poll_initialization();
            }
            if phase == "pending" {
                state.start_next_command();
                assert_eq!(command(&mut master), "AT");
            }
            assert_eq!(
                begin(&mut state).recv().unwrap().unwrap_err().code,
                "serial_initialization_busy",
                "{phase}"
            );
            assert!(state.initialization.running);
            no_command(&mut master);
        }
    }

    #[test]
    fn close_cancels_deferred_queued_and_pending_initialization_and_reopen_is_not_ready() {
        for phase in ["deferred", "queued", "pending"] {
            let (mut state, mut master) = fixture(true);
            let result = begin(&mut state);
            if phase != "deferred" {
                due(&mut state);
                state.poll_initialization();
            }
            if phase == "pending" {
                state.start_next_command();
                assert_eq!(command(&mut master), "AT");
            }
            let previous_session = state.serial_session;
            let (reply, closed) = mpsc::sync_channel(1);
            state.process(Command::Close { reply }, &unused_opener());
            assert!(closed.recv().unwrap().is_ok());
            assert!(
                result.recv().unwrap().is_err(),
                "{phase} init survived Close"
            );
            assert!(!state.initialization.running);
            assert!(state.initialization.deferred.is_none());
            assert!(state.pending_command.is_none());
            assert!(state.pending_normal_commands.is_empty());
            assert!(state.port.is_none());

            let (_new_master, new_slave) = serialport::TTYPort::pair().unwrap();
            let path = new_slave.name().unwrap();
            let new_port = Mutex::new(Some(new_slave));
            let opener: PortOpener =
                Box::new(move |_, _| Ok(Box::new(new_port.lock().unwrap().take().unwrap())));
            let (reply, opened) = mpsc::sync_channel(1);
            state.process(
                Command::Open {
                    path,
                    baud_rate: 115_200,
                    initialization_required: true,
                    reply,
                },
                &opener,
            );
            assert!(opened.recv().unwrap().is_ok());
            assert!(state.serial_session > previous_session);
            assert!(state.initialization.cached.is_none());
            assert!(!state.initialization.sms_ready());
        }
    }

    #[test]
    fn retired_completion_cannot_finish_or_cancel_new_session_initialization() {
        let (mut state, _master) = fixture(true);
        let old_result = begin(&mut state);
        let old = state.initialization.deferred.take().unwrap().transaction;
        // Represent an already retired serial transaction delivered late.
        state.serial_session += 1;
        state.initialization = InitializationState {
            required: true,
            ..Default::default()
        };
        let current_result = begin(&mut state);
        state.advance_initialization(old, "ok");
        assert_eq!(
            old_result.recv().unwrap().unwrap_err().code,
            "serial_session_changed"
        );
        assert!(state.initialization.running);
        assert!(state.initialization.cached.is_none());
        assert_eq!(
            state
                .initialization
                .deferred
                .as_ref()
                .unwrap()
                .transaction
                .serial_session,
            state.serial_session
        );
        assert!(matches!(
            current_result.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(!state.initialization.sms_ready());
    }

    #[test]
    fn timeout_watchdog_cancels_initialization_and_queued_work_without_reporting_ready() {
        let (mut state, mut master) = fixture(true);
        let result = begin(&mut state);
        due(&mut state);
        state.start_next_command();
        assert_eq!(command(&mut master), "AT");
        state.timeout_streak = COMMAND_TIMEOUT_WATCHDOG_THRESHOLD - 1;
        let (reply, queued) = mpsc::sync_channel(1);
        state.pending_normal_commands.push_back(QueuedCommand {
            bytes: b"AT+CSQ\r\n".to_vec(),
            timeout: Duration::from_secs(2),
            follow_up: None,
            expected_sms_generation: None,
            high_priority: false,
            completion: CommandCompletion::Execute(reply),
        });

        state.finish_command("timeout", None);

        let error = result.recv().unwrap().unwrap_err();
        assert_eq!(error.code, "serial_timeout_watchdog");
        assert!(error.message.contains("device disconnected"));
        assert_eq!(
            state
                .initialization
                .cached
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap_err()
                .code,
            "serial_timeout_watchdog"
        );
        assert!(queued.recv().unwrap().is_err());
        assert!(state.port.is_none());
        assert!(state.pending_command.is_none());
        assert!(state.pending_normal_commands.is_empty());
        assert!(state.initialization.deferred.is_none());
        assert!(!state.initialization.running);
        assert!(!state.initialization.sms_ready());
        assert_eq!(
            state.disconnected_error.as_ref().unwrap().code,
            "serial_timeout_watchdog"
        );
    }

    #[test]
    fn timeout_watchdog_delivers_typed_error_to_live_or_replacement_pending_reader() {
        for drop_reader in [false, true] {
            let (mut state, mut master) = fixture(true);
            let result = begin(&mut state);
            due(&mut state);
            state.start_next_command();
            assert_eq!(command(&mut master), "AT");

            let (reply, reader) = mpsc::sync_channel(1);
            state.process(
                Command::Read {
                    maximum: MAX_READ_BYTES,
                    timeout: Duration::from_secs(60),
                    reply,
                },
                &unused_opener(),
            );
            assert!(state.pending_read.is_some());
            let reader = if drop_reader {
                drop(reader);
                None
            } else {
                Some(reader)
            };
            state.timeout_streak = COMMAND_TIMEOUT_WATCHDOG_THRESHOLD - 1;
            state.finish_command("timeout", None);

            assert_eq!(
                result.recv().unwrap().unwrap_err().code,
                "serial_timeout_watchdog"
            );
            assert!(state.pending_read.is_none());
            assert!(state.port.is_none());
            if let Some(reader) = reader {
                assert_eq!(
                    reader.recv().unwrap().unwrap_err().code,
                    "serial_timeout_watchdog"
                );
                assert!(state.disconnected_error.is_none());
            } else {
                assert_eq!(
                    state.disconnected_error.as_ref().unwrap().code,
                    "serial_timeout_watchdog"
                );
                let (reply, replacement) = mpsc::sync_channel(1);
                state.process(
                    Command::Read {
                        maximum: MAX_READ_BYTES,
                        timeout: Duration::from_secs(60),
                        reply,
                    },
                    &unused_opener(),
                );
                assert_eq!(
                    replacement.recv().unwrap().unwrap_err().code,
                    "serial_timeout_watchdog"
                );
                assert!(state.disconnected_error.is_none());
                assert!(state.pending_read.is_none());
            }
        }
    }

    #[test]
    fn abandoned_initialize_reply_still_caches_success_without_replaying_at_commands() {
        let (mut state, mut master) = fixture(true);
        drop(begin(&mut state));
        complete(&mut state, &mut master, ["OK\r\n"; 6]);
        assert!(state.initialization.sms_ready());
        let cached = begin(&mut state).recv().unwrap().unwrap();
        assert!(cached.completed && cached.probe_succeeded && cached.sms_pdu_ready);
        assert_eq!(cached.steps.len(), 6);
        state.start_next_command();
        no_command(&mut master);
    }

    #[test]
    fn full_normal_queue_defers_at_step_but_does_not_delay_final_completion() {
        let (mut state, mut master) = fixture(true);
        let result = begin(&mut state);
        let fill_queue = |state: &mut ActorState| {
            for _ in 0..NORMAL_COMMAND_QUEUE_CAPACITY {
                let (reply, _result) = mpsc::sync_channel(1);
                state.pending_normal_commands.push_back(QueuedCommand {
                    bytes: b"AT+CSQ\r\n".to_vec(),
                    timeout: Duration::from_secs(2),
                    follow_up: None,
                    expected_sms_generation: None,
                    high_priority: false,
                    completion: CommandCompletion::Execute(reply),
                });
            }
        };
        fill_queue(&mut state);
        due(&mut state);
        state.poll_initialization();
        assert_eq!(
            state.pending_normal_commands.len(),
            NORMAL_COMMAND_QUEUE_CAPACITY
        );
        assert_eq!(
            state
                .initialization
                .deferred
                .as_ref()
                .unwrap()
                .transaction
                .step,
            0
        );
        assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
        no_command(&mut master);
        state.pending_normal_commands.clear();
        for expected in COMMANDS {
            assert_eq!(step(&mut state, &mut master, "OK\r\n"), expected);
        }
        fill_queue(&mut state);
        due(&mut state);
        state.poll_initialization();
        assert!(result.recv().unwrap().unwrap().completed);
        assert!(state.initialization.sms_ready());
        assert_eq!(
            state.pending_normal_commands.len(),
            NORMAL_COMMAND_QUEUE_CAPACITY
        );
        no_command(&mut master);
    }

    #[test]
    fn real_actor_initializes_in_order_before_gated_cmti_scan_and_storage_read() {
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_secs(3)).unwrap();
        let path = slave.name().unwrap();
        let port = Mutex::new(Some(slave));
        let actor = ModemActor::with_port_opener(move |_, _| {
            Ok(Box::new(port.lock().unwrap().take().unwrap()))
        })
        .unwrap();
        actor.open_with_initialization(path, 115_200, true).unwrap();
        actor.configure_sms_receive(true).unwrap();
        master.write_all(b"+CMTI: \"ME\",7\r\n").unwrap();
        let observed = actor.read(MAX_READ_BYTES, 250).unwrap();
        assert!(observed.sms_ingest_owned);
        assert_eq!(observed.sms_event.as_ref().unwrap().index, "7");
        actor.scan_sms_receive().unwrap();
        assert!(
            actor
                .submit(|reply| Command::NextSmsReceive { reply }, RPC_TIMEOUT)
                .unwrap()
                .is_none()
        );
        no_command(&mut master);

        let initializing = actor.clone();
        let initialization = thread::spawn(move || initializing.initialize());
        for (index, expected) in COMMANDS.into_iter().enumerate() {
            assert_eq!(command(&mut master), expected);
            if index == 0 {
                assert_eq!(
                    actor.initialize().unwrap_err().code,
                    "serial_initialization_busy"
                );
            }
            assert!(
                actor
                    .submit(|reply| Command::NextSmsReceive { reply }, RPC_TIMEOUT)
                    .unwrap()
                    .is_none()
            );
            master.write_all(b"OK\r\n").unwrap();
        }
        let result = initialization.join().unwrap().unwrap();
        assert!(result.completed && result.probe_succeeded && result.sms_pdu_ready);
        assert_eq!(result.steps.len(), 6);
        let cached = actor.initialize().unwrap();
        assert_eq!(json(&cached), json(&result));
        no_command(&mut master);
        actor.scan_sms_receive().unwrap();
        assert!(
            actor
                .submit(|reply| Command::NextSmsReceive { reply }, RPC_TIMEOUT)
                .unwrap()
                .is_some()
        );

        let reading = actor.clone();
        let read = thread::spawn(move || reading.read_stored_sms(Some("ME".into()), None, 0));
        assert_eq!(command(&mut master), "AT+CPMS?");
        master
            .write_all(b"+CPMS: \"ME\",0,10,\"ME\",0,10,\"ME\",0,10\r\nOK\r\n")
            .unwrap();
        assert_eq!(command(&mut master), "AT+CMGL=4");
        master.write_all(b"OK\r\n").unwrap();
        let snapshot = read.join().unwrap().unwrap();
        assert_eq!(snapshot.storage, "ME");
        assert!(snapshot.storage_restored);
        no_command(&mut master);
        actor.close().unwrap();
    }
}
