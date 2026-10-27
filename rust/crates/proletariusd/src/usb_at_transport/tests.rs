#[cfg(test)]
mod tests {
    use super::super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    static OWNER_TEST: Mutex<()> = Mutex::new(());

    enum Action {
        Read(Result<Vec<u8>, IoError>, Duration),
        Write(Vec<u8>, Result<usize, IoError>, Duration),
    }
    struct State {
        actions: VecDeque<Action>,
        writes: Vec<Vec<u8>>,
        timeouts: Vec<Duration>,
        elapsed: Duration,
        reads: usize,
        dropped: bool,
    }
    struct FakeIo {
        state: Arc<Mutex<State>>,
        origin: Instant,
    }
    impl Io for FakeIo {
        fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
            drop(self);
            Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
        }

        fn read(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<usize, IoError> {
            assert!(timeout >= MIN_TIMEOUT);
            assert!(timeout <= Duration::from_millis(u32::MAX as u64));
            let mut state = self.state.lock().unwrap();
            state.timeouts.push(timeout);
            state.reads += 1;
            match state.actions.pop_front() {
                Some(Action::Read(result, elapsed)) => {
                    state.elapsed += elapsed.min(timeout);
                    match result {
                        Ok(data) => {
                            assert!(data.len() <= buffer.len());
                            buffer[..data.len()].copy_from_slice(&data);
                            Ok(data.len())
                        }
                        Err(error) => Err(error),
                    }
                }
                Some(Action::Write(..)) => panic!("unexpected read before expected write"),
                None => {
                    state.elapsed += timeout;
                    Err(IoError::Timeout)
                }
            }
        }
        fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, IoError> {
            assert!(timeout >= MIN_TIMEOUT);
            assert!(timeout <= Duration::from_millis(u32::MAX as u64));
            let mut state = self.state.lock().unwrap();
            state.timeouts.push(timeout);
            state.writes.push(data.to_vec());
            match state.actions.pop_front() {
                Some(Action::Write(expected, result, elapsed)) => {
                    assert_eq!(data, expected);
                    state.elapsed += elapsed.min(timeout);
                    result
                }
                _ => panic!("unexpected write: {} bytes", data.len()),
            }
        }
        fn now(&self) -> Instant {
            self.origin + self.state.lock().unwrap().elapsed
        }
    }
    impl Drop for FakeIo {
        fn drop(&mut self) {
            self.state.lock().unwrap().dropped = true;
        }
    }
    fn fake(actions: Vec<Action>) -> (NativeTransport, Arc<Mutex<State>>) {
        let state = Arc::new(Mutex::new(State {
            actions: actions.into(),
            writes: Vec::new(),
            timeouts: Vec::new(),
            elapsed: Duration::ZERO,
            reads: 0,
            dropped: false,
        }));
        let transport = NativeTransport::new(
            Box::new(FakeIo {
                state: state.clone(),
                origin: Instant::now(),
            }),
            "fixture USB AT".into(),
            None,
        );
        (transport, state)
    }
    fn quiet() -> Action {
        Action::Read(Err(IoError::Timeout), DRAIN_TIMEOUT)
    }
    fn read(bytes: &[u8]) -> Action {
        Action::Read(Ok(bytes.to_vec()), Duration::ZERO)
    }
    fn write(bytes: &[u8]) -> Action {
        Action::Write(bytes.to_vec(), Ok(bytes.len()), Duration::ZERO)
    }
    fn error(bytes: &[u8], error: IoError) -> Action {
        Action::Write(bytes.to_vec(), Err(error), Duration::ZERO)
    }

    #[test]
    fn transport_description_cannot_fabricate_a_physical_target() {
        struct DescriptionOnlyTransport;
        impl Transport for DescriptionOnlyTransport {
            fn command(
                &mut self,
                _: &str,
                _: Option<&[u8]>,
                _: Duration,
                _: usize,
            ) -> Result<String, TransportError> {
                panic!("capturing a target must not issue an AT command")
            }
            fn description(&self) -> String {
                "USB AT · 2ca3:4006 bus 1 address 2 port 3 serial fixture".into()
            }
        }
        let transport: Box<dyn Transport> = Box::new(DescriptionOnlyTransport);
        assert!(transport.description().contains("2ca3:4006"));
        let error = transport.physical_target().unwrap_err();
        assert_eq!(error.code, "usb_at_target_unconfirmed");
        assert!(error.response.is_empty());
        assert!(!error.uncertain);
        assert!(!format!("{error:?}").contains("fixture"));
        assert_eq!(
            transport.checked_close().unwrap_err().code,
            "usb_at_close_unconfirmed"
        );
    }

    #[test]
    fn native_transport_without_native_evidence_fails_without_usb_transfers() {
        let (transport, state) = fake(vec![]);
        let error = transport.physical_target().unwrap_err();
        assert_eq!(error.code, "usb_at_target_unconfirmed");
        let state = state.lock().unwrap();
        assert_eq!(state.reads, 0);
        assert!(state.writes.is_empty());
        assert!(state.timeouts.is_empty());
        assert!(!state.dropped);
    }

    #[test]
    fn native_transport_preserves_target_capture_failure_without_fallback() {
        use std::sync::atomic::AtomicUsize;

        struct UnconfirmedTargetIo(Arc<AtomicUsize>);
        impl Io for UnconfirmedTargetIo {
            fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(TransportError::new(
                    "usb_at_disconnected",
                    "The original USB connection disappeared",
                    &[],
                    false,
                ))
            }
            fn read(&mut self, _: &mut [u8], _: Duration) -> Result<usize, IoError> {
                panic!("target capture must not probe another interface")
            }
            fn write(&mut self, _: &[u8], _: Duration) -> Result<usize, IoError> {
                panic!("target capture must not probe another interface")
            }
        }
        let captures = Arc::new(AtomicUsize::new(0));
        let transport = NativeTransport::new(
            Box::new(UnconfirmedTargetIo(captures.clone())),
            "another 2ca3:4006 device exists".into(),
            None,
        );
        let error = transport.physical_target().unwrap_err();
        assert_eq!(error.code, "usb_at_disconnected");
        assert_eq!(captures.load(Ordering::SeqCst), 1);
        assert!(error.response.is_empty());
        assert!(!error.uncertain);
        // NativeTransport cannot upgrade an Io with no release implementation.
        assert_eq!(
            Box::new(transport).checked_close().unwrap_err().code,
            "usb_at_close_unconfirmed"
        );
    }

    #[test]
    fn synthetic_capture_clone_retains_only_the_same_ownership_fixture() {
        fn anchor(target: &PhysicalTarget) -> &Arc<()> {
            match &target.connection {
                PhysicalConnection::Synthetic(anchor) => anchor,
                #[cfg(target_os = "macos")]
                PhysicalConnection::Native(_) => panic!("test fixture manufactured a USB device"),
            }
        }
        let target = PhysicalTarget::synthetic_for_tests();
        let captured = target.clone();
        assert!(Arc::ptr_eq(anchor(&target), anchor(&captured)));
        assert_eq!(Arc::strong_count(anchor(&captured)), 2);
        assert!(!format!("{target:?}").contains("Synthetic"));
        drop(target);
        assert_eq!(Arc::strong_count(anchor(&captured)), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn synthetic_capture_cannot_supply_a_native_device() {
        let target = PhysicalTarget::synthetic_for_tests();
        let error = target.native_device().unwrap_err();
        assert_eq!(error.code, "usb_at_target_unconfirmed");
        assert!(error.response.is_empty());
        assert!(!error.uncertain);
    }

    #[test]
    fn physical_target_can_outlive_the_transport_and_cross_worker_boundaries() {
        // Compile-time ownership contract; constructing a positive native
        // target still requires an actual opened handle and is not faked here.
        fn assert_owned_target<T: Clone + Send + Sync + 'static>() {}
        assert_owned_target::<PhysicalTarget>();
    }

    #[test]
    fn normal_command_is_normalized_and_final_tokens_can_span_usb_frames() {
        let (mut transport, state) = fake(vec![
            quiet(),
            write(b"AT+CSQ\r"),
            read(b"AT+CSQ\r\r\n\r\n+CSQ: 18,99\r\nO"),
            read(b"K\r\n"),
        ]);
        assert_eq!(
            transport
                .command("  AT+CSQ  ", None, Duration::from_secs(3), 0)
                .unwrap(),
            "AT+CSQ\r\n+CSQ: 18,99\r\nOK"
        );
        assert_eq!(state.lock().unwrap().writes, vec![b"AT+CSQ\r".to_vec()]);
        assert_eq!(transport.description(), "fixture USB AT");
    }

    #[test]
    fn partial_normal_response_is_preserved_at_deadline_but_empty_timeout_is_unknown() {
        let (mut transport, state) = fake(vec![quiet(), write(b"AT+CSQ\r"), read(b"\r\n+CSQ: 18")]);
        assert_eq!(
            transport
                .command("AT+CSQ", None, Duration::from_millis(100), 4096)
                .unwrap(),
            "+CSQ: 18"
        );
        assert_eq!(state.lock().unwrap().elapsed, Duration::from_millis(100));
        let (mut transport, state) = fake(vec![quiet(), write(b"AT+CFUN=1\r")]);
        let error = transport
            .command("AT+CFUN=1", None, Duration::from_millis(100), 4096)
            .unwrap_err();
        assert_eq!(error.code, "usb_at_timeout");
        assert!(error.uncertain);
        assert!(error.response.is_empty());
        assert_eq!(state.lock().unwrap().writes.len(), 1);
    }

    #[test]
    fn normal_and_follow_up_short_writes_never_send_the_remainder_or_replay() {
        let (mut transport, state) = fake(vec![
            quiet(),
            Action::Write(b"AT+CFUN=1\r".to_vec(), Ok(2), Duration::ZERO),
        ]);
        let error = transport
            .command("AT+CFUN=1", None, Duration::from_secs(1), 0)
            .unwrap_err();
        assert_eq!(error.code, "usb_at_short_write");
        assert!(error.uncertain);
        assert_eq!(state.lock().unwrap().writes, vec![b"AT+CFUN=1\r".to_vec()]);
        let body = b"fixture-message\x1a";
        let (mut transport, state) = fake(vec![
            quiet(),
            write(b"AT+CMGS=1\r"),
            read(b"\r\n> "),
            Action::Write(body.to_vec(), Ok(3), Duration::ZERO),
            write(&[0x1b]),
        ]);
        let error = transport
            .command("AT+CMGS=1", Some(body), Duration::from_secs(1), 0)
            .unwrap_err();
        assert_eq!(error.code, "usb_at_short_write");
        assert!(error.uncertain);
        assert_eq!(
            state.lock().unwrap().writes,
            vec![b"AT+CMGS=1\r".to_vec(), body.to_vec(), vec![0x1b]]
        );
    }

    #[test]
    fn timed_out_write_is_not_replayed_and_failure_diagnostics_do_not_leak_input() {
        let secret = "AT+CPIN=private-fixture";
        let command = format!("{secret}\r");
        let (mut transport, state) =
            fake(vec![quiet(), error(command.as_bytes(), IoError::Timeout)]);
        let error = transport
            .command(secret, None, Duration::from_secs(1), 0)
            .unwrap_err();
        assert!(error.uncertain);
        assert_eq!(error.code, "usb_at_timeout");
        assert!(!error.message.contains("private-fixture"));
        assert!(!error.to_string().contains("private-fixture"));
        assert_eq!(state.lock().unwrap().writes.len(), 1);
    }

    #[test]
    fn prompt_send_requires_prompt_and_returns_confirmed_modem_errors_without_sending_body() {
        let body = b"raw-body\x1a";
        let (mut transport, state) = fake(vec![
            quiet(),
            write(b"AT+CMGS=1\r"),
            read(b"AT+CMGS=1\r\n"),
            read(b"> "),
            write(body),
            read(b"\r\n+CMGS: 5\r\nOK\r\n"),
        ]);
        assert_eq!(
            transport
                .command("AT+CMGS=1", Some(body), Duration::from_secs(1), 4096)
                .unwrap(),
            "AT+CMGS=1\r\n> \r\n+CMGS: 5\r\nOK"
        );
        assert_eq!(state.lock().unwrap().writes.len(), 2);
        let (mut transport, state) = fake(vec![
            quiet(),
            write(b"AT+CMGS=1\r"),
            read(b"\r\n+CMS ERROR: 302\r\n"),
        ]);
        assert_eq!(
            transport
                .command("AT+CMGS=1", Some(body), Duration::from_secs(1), 4096)
                .unwrap(),
            "+CMS ERROR: 302"
        );
        assert_eq!(state.lock().unwrap().writes.len(), 1);
    }

    #[test]
    fn prompt_timeout_before_or_after_prompt_sends_one_bounded_escape_and_remains_unknown() {
        for prompted in [false, true] {
            let mut actions = vec![quiet(), write(b"AT+CMGS=1\r")];
            if prompted {
                actions.extend([read(b"\r\n> "), write(b"raw-body\x1a")]);
            }
            actions.push(Action::Read(
                Err(IoError::Timeout),
                Duration::from_millis(20),
            ));
            actions.push(write(&[0x1b]));
            let (mut transport, state) = fake(actions);
            let error = transport
                .command(
                    "AT+CMGS=1",
                    Some(b"raw-body\x1a"),
                    Duration::from_millis(100),
                    4096,
                )
                .unwrap_err();
            assert_eq!(error.code, "usb_at_timeout");
            assert!(error.uncertain);
            let state = state.lock().unwrap();
            assert_eq!(
                state
                    .writes
                    .iter()
                    .filter(|data| data.as_slice() == [0x1b])
                    .count(),
                1
            );
            assert_eq!(state.timeouts.last(), Some(&ESC_TIMEOUT));
        }
    }

    #[test]
    fn failed_prompt_escape_does_not_change_original_timeout_or_submission_uncertainty() {
        let (mut transport, state) = fake(vec![
            quiet(),
            write(b"AT+CMGS=1\r"),
            Action::Read(Err(IoError::Timeout), Duration::from_millis(20)),
            error(&[0x1b], IoError::Disconnected),
        ]);
        let error = transport
            .command(
                "AT+CMGS=1",
                Some(b"body\x1a"),
                Duration::from_millis(100),
                4096,
            )
            .unwrap_err();
        assert_eq!(error.code, "usb_at_timeout");
        assert!(error.uncertain);
        assert_eq!(state.lock().unwrap().writes.len(), 2);
    }

    #[test]
    fn input_drain_has_total_deadline_byte_limit_and_transfer_count_bound() {
        let (mut transport, state) = fake(vec![
            Action::Read(Ok(vec![b'x']), Duration::from_millis(80)),
            Action::Read(Ok(vec![b'y']), Duration::from_millis(20)),
        ]);
        let error = transport
            .command("AT", None, Duration::from_millis(100), 4096)
            .unwrap_err();
        assert_eq!(error.code, "usb_at_timeout");
        assert!(!error.uncertain);
        assert!(state.lock().unwrap().writes.is_empty());
        let (mut transport, state) = fake(vec![read(b"12345"), read(b"67890")]);
        let error = transport
            .command("AT", None, Duration::from_secs(1), 8)
            .unwrap_err();
        assert_eq!(error.code, "usb_at_response_too_large");
        assert!(!error.uncertain);
        assert!(state.lock().unwrap().writes.is_empty());
        let empty = (0..MAX_DRAIN_TRANSFERS).map(|_| read(b"")).collect();
        let (mut transport, state) = fake(empty);
        let error = transport
            .command("AT", None, Duration::from_secs(1), 0)
            .unwrap_err();
        assert_eq!(error.code, "usb_at_drain_failed");
        assert_eq!(state.lock().unwrap().reads, MAX_DRAIN_TRANSFERS);
    }

    #[test]
    fn total_deadline_includes_write_and_never_issues_submillisecond_libusb_timeout() {
        let (mut transport, state) = fake(vec![
            quiet(),
            Action::Write(b"AT\r".to_vec(), Ok(3), Duration::from_micros(19_500)),
        ]);
        let error = transport
            .command("AT", None, Duration::from_millis(100), 0)
            .unwrap_err();
        assert_eq!(error.code, "usb_at_timeout");
        assert!(error.uncertain);
        assert_eq!(state.lock().unwrap().reads, 1);
        assert_eq!(
            state.lock().unwrap().timeouts,
            vec![Duration::from_millis(80), Duration::from_millis(20)]
        );
        let (mut transport, state) = fake(vec![]);
        assert_eq!(
            transport
                .command("AT", None, Duration::from_micros(999), 0)
                .unwrap_err()
                .code,
            "usb_at_invalid_timeout"
        );
        assert!(state.lock().unwrap().timeouts.is_empty());
    }

    #[test]
    fn oversized_response_returns_only_bounded_partial_data_and_never_replays() {
        let (mut transport, state) = fake(vec![
            quiet(),
            write(b"AT\r"),
            read(b"12345"),
            read(b"67890"),
        ]);
        let error = transport
            .command("AT", None, Duration::from_secs(1), 8)
            .unwrap_err();
        assert_eq!(error.code, "usb_at_response_too_large");
        assert_eq!(error.response, "12345");
        assert!(error.uncertain);
        assert_eq!(state.lock().unwrap().writes.len(), 1);
    }

    #[test]
    fn disconnect_before_write_is_certain_but_after_write_preserves_partial_unknown() {
        let (mut transport, state) = fake(vec![Action::Read(
            Err(IoError::Disconnected),
            Duration::ZERO,
        )]);
        let error = transport
            .command("AT", None, Duration::from_secs(1), 0)
            .unwrap_err();
        assert_eq!(error.code, "usb_at_disconnected");
        assert!(!error.uncertain);
        assert!(state.lock().unwrap().writes.is_empty());
        let (mut transport, _) = fake(vec![
            quiet(),
            write(b"AT\r"),
            read(b"\r\npartial-response"),
            Action::Read(Err(IoError::Disconnected), Duration::ZERO),
        ]);
        let error = transport
            .command("AT", None, Duration::from_secs(1), 0)
            .unwrap_err();
        assert_eq!(error.code, "usb_at_disconnected");
        assert!(error.uncertain);
        assert_eq!(error.response, "partial-response");
        assert!(!error.message.contains("partial-response"));
        assert!(!format!("{error:?}").contains("partial-response"));
    }

    #[test]
    fn input_validation_and_hard_limits_fail_before_any_device_io() {
        for (command, payload, timeout, limit, code) in [
            (
                " ".to_owned(),
                None,
                Duration::from_secs(1),
                0,
                "usb_at_invalid_command",
            ),
            (
                "+CSQ".to_owned(),
                None,
                Duration::from_secs(1),
                0,
                "usb_at_invalid_command",
            ),
            (
                format!("AT{}", "x".repeat(MAX_BYTES)),
                None,
                Duration::from_secs(1),
                0,
                "usb_at_invalid_command",
            ),
            (
                "AT".to_owned(),
                Some(Vec::new()),
                Duration::from_secs(1),
                0,
                "usb_at_invalid_payload",
            ),
            (
                "AT".to_owned(),
                Some(vec![b'x'; MAX_BYTES + 1]),
                Duration::from_secs(1),
                0,
                "usb_at_invalid_payload",
            ),
            (
                "AT".to_owned(),
                None,
                Duration::from_secs(1),
                MAX_BYTES + 1,
                "usb_at_invalid_limit",
            ),
        ] {
            let (mut transport, state) = fake(vec![]);
            let error = transport
                .command(&command, payload.as_deref(), timeout, limit)
                .unwrap_err();
            assert_eq!(error.code, code);
            assert!(!error.uncertain);
            assert!(state.lock().unwrap().timeouts.is_empty());
        }
    }

    #[test]
    fn zero_timeout_uses_finite_defaults_and_large_timeout_never_wraps_to_zero() {
        let (mut transport, state) = fake(vec![quiet(), write(b"AT\r"), read(b"\r\nOK\r\n")]);
        transport.command("AT", None, Duration::ZERO, 0).unwrap();
        assert_eq!(
            state.lock().unwrap().timeouts[1],
            Duration::from_millis(2920)
        );
        let (mut transport, state) = fake(vec![
            quiet(),
            write(b"AT+CMGS=1\r"),
            read(b"\r\n+CMS ERROR: 1\r\n"),
        ]);
        transport
            .command("AT+CMGS=1", Some(b"data\x1a"), Duration::ZERO, 0)
            .unwrap();
        assert_eq!(
            state.lock().unwrap().timeouts[1],
            Duration::from_millis(29920)
        );
        let (mut transport, state) = fake(vec![quiet(), write(b"AT\r"), read(b"\r\nOK\r\n")]);
        transport
            .command("AT", None, Duration::from_secs(u32::MAX as u64), 0)
            .unwrap();
        assert_eq!(
            state.lock().unwrap().timeouts[1],
            Duration::from_millis(u32::MAX as u64)
        );
    }

    #[test]
    fn candidate_filter_excludes_diagnostics_nmea_qmi_and_nonbulk_endpoints() {
        assert_eq!(DEVICE_IDS, [(0x2ca3, 0x4006), (0x2c7c, 0x0125)]);
        let endpoints = [(0x81, true), (0x02, true), (0x83, false)];
        for interface in [2, 3] {
            let value = candidate(DJI_QDC507, interface, 0, 0xff, 0, 0, endpoints).unwrap();
            assert_eq!(value.endpoint_in, 0x81);
            assert_eq!(value.endpoint_out, 0x02);
        }
        for (interface, endpoint_in, endpoint_out) in [(4, 0x86, 0x04), (5, 0x88, 0x05)] {
            let pair = [(endpoint_in, true), (endpoint_out, true)];
            assert!(candidate(DJI_QDC507, interface, 0, 0xff, 0, 0, pair).is_some());
            assert!(candidate(DEVICE_IDS[1], interface, 0, 0xff, 0, 0, pair).is_none());
            assert!(candidate(DJI_QDC507, interface, 0, 0xff, 0, 0, endpoints).is_none());
        }
        for interface in [0, 1] {
            assert!(candidate(DJI_QDC507, interface, 0, 0xff, 0, 0, endpoints).is_none());
        }
        for (class, subclass, protocol) in
            [(0xff, 0xff, 0xff), (0xff, 0x42, 1), (2, 0, 0), (0xff, 0, 1)]
        {
            assert!(candidate(DJI_QDC507, 2, 0, class, subclass, protocol, endpoints).is_none());
            assert!(candidate(DJI_QDC507, 4, 0, class, subclass, protocol, endpoints).is_none());
        }
        assert!(candidate(DJI_QDC507, 2, 0, 0xff, 0, 0, [(0x81, true), (0x02, false)]).is_none());
        assert!(candidate(
            DJI_QDC507,
            2,
            0,
            0xff,
            0,
            0,
            [(0x81, true), (0x02, true), (0x83, true)]
        )
        .is_none());
        assert!(candidate(DJI_QDC507, 4, 0, 0x02, 0x06, 0, endpoints).is_none());
        assert!(candidate(DJI_QDC507, 5, 0, 0x0a, 0, 2, endpoints).is_none());
        assert!(candidate(DJI_QDC507, 2, 0, 0xff, 0, 0, [(0x81, true), (0, false)]).is_none());
    }

    #[test]
    fn dji_rndis_at_interfaces_are_preferred_without_reclassifying_network_functions() {
        assert!(usb_at_interface_priority(DJI_QDC507, 4) < usb_at_interface_priority(DJI_QDC507, 2));
        assert!(usb_at_interface_priority(DJI_QDC507, 5) < usb_at_interface_priority(DJI_QDC507, 3));
        assert_eq!(usb_at_interface_priority(DEVICE_IDS[1], 2), 0);
        assert_eq!(usb_at_interface_priority(DEVICE_IDS[1], 3), 1);
    }

    #[test]
    fn probe_requires_ok_closes_failed_transport_and_never_exposes_probe_response() {
        let (transport, state) = fake(vec![
            quiet(),
            write(b"AT\r"),
            read(b"\r\nprivate-unsolicited-data\r\nERROR\r\n"),
        ]);
        let error = match probe(transport) {
            Ok(_) => panic!("ERROR must not admit a candidate"),
            Err(error) => error,
        };
        assert_eq!(error.code, "usb_at_probe_failed");
        assert!(error.response.is_empty());
        assert!(!error.message.contains("private-unsolicited-data"));
        assert!(state.lock().unwrap().dropped);
        let (transport, state) = fake(vec![quiet(), write(b"AT\r"), read(b"\r\nOK\r\n")]);
        let transport = probe(transport).unwrap();
        assert!(!state.lock().unwrap().dropped);
        assert_eq!(
            state.lock().unwrap().timeouts[1],
            Duration::from_millis(820)
        );
        drop(transport);
        assert!(state.lock().unwrap().dropped);
    }

    #[test]
    fn process_owner_is_exclusive_until_the_last_lease_and_handle_are_dropped() {
        let _serial = OWNER_TEST.lock().unwrap();
        let owner = OwnerLease::acquire().unwrap();
        assert!(OwnerLease::acquire().is_err());
        let (mut transport, state) = fake(vec![]);
        transport._owner = Some(owner.clone());
        drop(owner);
        assert!(OwnerLease::acquire().is_err());
        drop(transport);
        assert!(state.lock().unwrap().dropped);
        let new_owner = OwnerLease::acquire().unwrap();
        drop(new_owner);
    }
    #[test]
    fn call_capture_preserves_fragmented_drain_response_and_idle_events_on_one_handle() {
        let (mut transport, state) = fake(vec![
            read(b"RI"),
            read(b"NG\r\n"),
            quiet(),
            write(b"AT+CLCC\r"),
            read(b"+CLIP: \"123\",129\r\nOK\r\n"),
            read(b"NO CARRIER\r\n"),
        ]);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let records = observed.clone();
        transport.set_line_capture(Arc::new(move |line| {
            records.lock().unwrap().push(line.trim().to_owned());
        }));
        assert!(
            transport
                .command("AT+CLCC", None, Duration::from_secs(3), 4096)
                .unwrap()
                .contains("OK")
        );
        transport
            .poll_call_capture(Duration::from_millis(40))
            .unwrap();
        assert_eq!(
            *observed.lock().unwrap(),
            vec!["RING", "+CLIP: \"123\",129", "OK", "NO CARRIER"]
        );
        assert_eq!(state.lock().unwrap().writes, vec![b"AT+CLCC\r".to_vec()]);
    }
    #[test]
    fn call_guard_is_checked_after_drain_capture_and_before_actual_write() {
        let (mut transport, state) = fake(vec![read(b"RING\r\n"), quiet()]);
        let admitted = Arc::new(AtomicBool::new(true));
        let revoked = admitted.clone();
        transport.set_line_capture(Arc::new(move |line| {
            if line.trim() == "RING" {
                revoked.store(false, Ordering::Release);
            }
        }));
        let error = transport
            .command_with_guard(
                "ATD123;",
                None,
                Duration::from_secs(3),
                4096,
                Arc::new(move || admitted.load(Ordering::Acquire)),
            )
            .unwrap_err();
        assert_eq!(error.code, "module_call_admission_changed");
        assert!(!error.uncertain);
        assert!(state.lock().unwrap().writes.is_empty());
    }
    #[test]
    fn cancelled_usb_call_guard_never_submits_and_idle_timeout_is_bounded() {
        let (mut transport, state) = fake(vec![quiet(), quiet()]);
        assert!(
            !transport
                .command_with_guard(
                    "ATH",
                    None,
                    Duration::from_secs(3),
                    4096,
                    Arc::new(|| false)
                )
                .unwrap_err()
                .uncertain
        );
        transport
            .poll_call_capture(Duration::from_millis(40))
            .unwrap();
        assert!(state.lock().unwrap().writes.is_empty());
        assert!(
            state
                .lock()
                .unwrap()
                .timeouts
                .iter()
                .all(|timeout| !timeout.is_zero())
        );
    }
    mod commit_tests {
        include!("commit_tests.rs");
    }

    struct CheckedIo {
        events: Arc<Mutex<Vec<&'static str>>>,
        owner: std::sync::Weak<OwnerLeaseInner>,
        mode: u8,
        barrier: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
        entered: Option<std::sync::mpsc::Sender<()>>,
    }
    impl Drop for CheckedIo {
        fn drop(&mut self) {
            assert!(
                self.owner.upgrade().is_some(),
                "handle must drop before admission lease"
            );
            self.events.lock().unwrap().push("handle_drop");
        }
    }
    impl Io for CheckedIo {
        fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
            self.events.lock().unwrap().push("checked_release");
            assert!(
                OwnerLease::acquire().is_err(),
                "owner must remain held through release"
            );
            if let Some(entered) = &self.entered {
                entered.send(()).unwrap();
            }
            if let Some(barrier) = &self.barrier {
                let (released, changed) = &**barrier;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = changed.wait(released).unwrap();
                }
            }
            let mode = self.mode;
            if mode == 2 {
                panic!("fixture release panic");
            }
            drop(self); // model real handle close after explicit release outcome
            if mode == 1 {
                Err(close_unconfirmed())
            } else if mode == 3 {
                Ok(CloseDisposition::DisconnectedAndHandleDropped)
            } else {
                Ok(CloseDisposition::ReleasedAndHandleDropped)
            }
        }
        fn read(&mut self, _: &mut [u8], _: Duration) -> Result<usize, IoError> {
            panic!("no I/O on close")
        }
        fn write(&mut self, _: &[u8], _: Duration) -> Result<usize, IoError> {
            panic!("no I/O on close")
        }
    }
    #[test]
    fn native_checked_close_keeps_owner_until_handle_drop_on_release_success_error_and_panic() {
        let _serial = OWNER_TEST.lock().unwrap();
        for mode in 0..=3 {
            let owner = OwnerLease::acquire().unwrap();
            let weak = Arc::downgrade(&owner._inner);
            let events = Arc::new(Mutex::new(Vec::new()));
            let port = NativeTransport::new(
                Box::new(CheckedIo {
                    events: events.clone(),
                    owner: weak.clone(),
                    mode,
                    barrier: None,
                    entered: None,
                }),
                "checked close fixture".into(),
                Some(owner),
            );
            let closed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Box::new(port).checked_close()
            }));
            match mode {
                0 => assert_eq!(
                    closed.unwrap().unwrap(),
                    CloseDisposition::ReleasedAndHandleDropped
                ),
                3 => assert_eq!(
                    closed.unwrap().unwrap(),
                    CloseDisposition::DisconnectedAndHandleDropped
                ),
                1 => assert_eq!(
                    closed.unwrap().unwrap_err().code,
                    "usb_at_close_unconfirmed"
                ),
                _ => assert!(closed.is_err()),
            }
            assert_eq!(*events.lock().unwrap(), ["checked_release", "handle_drop"]);
            assert!(weak.upgrade().is_none());
            drop(OwnerLease::acquire().unwrap());
        }
    }
    #[test]
    fn blocking_checked_release_cannot_return_owner_or_close_success_early() {
        let _serial = OWNER_TEST.lock().unwrap();
        let owner = OwnerLease::acquire().unwrap();
        let weak = Arc::downgrade(&owner._inner);
        let events = Arc::new(Mutex::new(Vec::new()));
        let barrier = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (entered, receiver) = std::sync::mpsc::channel();
        let port = NativeTransport::new(
            Box::new(CheckedIo {
                events: events.clone(),
                owner: weak.clone(),
                mode: 0,
                barrier: Some(barrier.clone()),
                entered: Some(entered),
            }),
            "blocked close fixture".into(),
            Some(owner),
        );
        let worker = std::thread::spawn(move || Box::new(port).checked_close());
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(!worker.is_finished());
        assert!(OwnerLease::acquire().is_err());
        assert_eq!(*events.lock().unwrap(), ["checked_release"]);
        *barrier.0.lock().unwrap() = true;
        barrier.1.notify_all();
        worker.join().unwrap().unwrap();
        assert_eq!(*events.lock().unwrap(), ["checked_release", "handle_drop"]);
        assert!(weak.upgrade().is_none());
        drop(OwnerLease::acquire().unwrap());
    }

    #[tokio::test]
    async fn actor_no_device_capture_stays_error_even_when_disconnected_handle_is_closed() {
        struct NoDeviceCaptureIo(Arc<std::sync::atomic::AtomicUsize>);
        impl Io for NoDeviceCaptureIo {
            fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(TransportError::new(
                    "usb_at_disconnected",
                    "Fixture capture reported NoDevice",
                    &[],
                    false,
                ))
            }
            fn checked_close(self: Box<Self>) -> Result<CloseDisposition, TransportError> {
                drop(self);
                Ok(CloseDisposition::DisconnectedAndHandleDropped) // explicit resource outcome, not capture/restore success
            }
            fn read(&mut self, _: &mut [u8], _: Duration) -> Result<usize, IoError> {
                panic!("no capture probe")
            }
            fn write(&mut self, _: &[u8], _: Duration) -> Result<usize, IoError> {
                panic!("no capture probe")
            }
        }
        let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let factory_captures = captures.clone();
        let (owner, _) = crate::usb_at::Owner::native_module_fixture(Arc::new(move || {
            Ok(Box::new(NativeTransport::new(
                Box::new(NoDeviceCaptureIo(factory_captures.clone())),
                "NoDevice byte fixture".into(),
                None,
            )))
        }))
        .await;
        assert_eq!(
            owner
                .capture_voice_target("native-module-test")
                .await
                .unwrap_err()
                .code,
            "usb_at_disconnected"
        );
        assert_eq!(captures.load(Ordering::SeqCst), 1);
        owner.shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
        assert_eq!(
            captures.load(Ordering::SeqCst),
            1,
            "retirement cannot rediscover or capture another device"
        );
    }
    fn framed_step(actions: &mut Vec<Action>, command: &str, response: &[u8]) {
        actions.push(quiet());
        actions.push(write(format!("{command}\r").as_bytes()));
        actions.push(read(response));
    }

    fn sms_identity_steps(actions: &mut Vec<Action>) {
        framed_step(actions, "AT+CPIN?", b"\r\n+CPIN: READY\r\nOK\r\n");
        framed_step(actions, "AT+QCCID", b"\r\n+QCCID: 8988200000000000000\r\nOK\r\n");
        framed_step(actions, "AT+CIMI", b"\r\n234150000000000\r\nOK\r\n");
    }

    fn status_steps(actions: &mut Vec<Action>, cpin: &[u8], changed: bool) {
        framed_step(actions, "AT+CFUN?", b"\r\n+CFUN: 4\r\nOK\r\n");
        framed_step(actions, "ATI", b"ATI\r\r\nFixture firmware\r\nOK\r\n");
        framed_step(actions, "AT+CPIN?", cpin);
        framed_step(actions, "AT+QCCID", if changed {
            b"\r\n+QCCID: 8988212345678901234\r\nOK\r\n+CEREG: 0\r\n"
        } else {
            b"\r\n+QCCID: 8988200000000000000\r\nOK\r\n"
        });
        framed_step(actions, "AT+CIMI", if changed {
            b"\r\n234150123456789\r\nOK\r\n+CEREG: 0\r\n"
        } else {
            b"\r\n234150000000000\r\nOK\r\n"
        });
        framed_step(actions, "AT+CNUM", b"\r\nERROR\r\n");
        framed_step(actions, "AT+CGSN", b"\r\n123456789012345\r\nOK\r\n");
        framed_step(actions, "AT+QCFG=\"usbnet\"", b"\r\n+CME ERROR: 4\r\n");
    }

    async fn byte_owner(actions: Vec<Action>) -> (crate::usb_at::Owner, String, Arc<Mutex<State>>) {
        let (transport, trace) = fake(actions);
        let slot = Arc::new(Mutex::new(Some(transport)));
        let (owner, session) = crate::usb_at::Owner::native_module_fixture(Arc::new(move || {
            Ok(Box::new(slot.lock().unwrap().take().expect("no replacement open")))
        })).await;
        // Keep native_module_fixture's SMS admission enabled. The status
        // transcript's actual CFUN=4 selects the RF-off collection plan.
        (owner, session, trace)
    }

    #[test]
    fn status_terminal_boundary_preserves_tail_urcs_and_fragmented_error_lines() {
        let (mut transport, trace) = fake(vec![
            quiet(),
            write(b"AT+QCCID\r"),
            read(b"\r\n+QCCID: 8988212345678901234\r\nOK\r\nRI"),
            read(b"NG\r\n"),
            quiet(),
            write(b"AT+CSIM=4,\"00A4\"\r"),
            read(b"\r\n+CSIM: 4,\"9000\"\r\nOK\r\n+CEREG: 0\r\n"),
            quiet(),
            write(b"AT+CNUM\r"),
            read(b"\r\n+CME ERROR:"),
            read(b" 10\r"),
            read(b"\nRING\r\n"),
        ]);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let capture = observed.clone();
        transport.set_line_capture(Arc::new(move |line| {
            capture.lock().unwrap().push(line.trim().to_owned());
        }));
        assert_eq!(transport.status_command("AT+QCCID", Duration::from_secs(3), 4096).unwrap(),
            "+QCCID: 8988212345678901234\r\nOK");
        assert_eq!(transport.command("AT+CSIM=4,\"00A4\"", None, Duration::from_secs(3), 4096).unwrap(),
            "+CSIM: 4,\"9000\"\r\nOK");
        assert_eq!(transport.status_command("AT+CNUM", Duration::from_secs(3), 4096).unwrap(),
            "+CME ERROR: 10");
        let lines = observed.lock().unwrap();
        assert_eq!(lines.iter().filter(|line| line.as_str() == "RING").count(), 2);
        assert!(lines.iter().any(|line| line == "+CEREG: 0"));
        assert!(trace.lock().unwrap().actions.is_empty());
    }

    #[test]
    fn strict_status_requires_wire_terminal_but_ordinary_partial_compatibility_remains() {
        for partial in [b"OK".as_slice(), b"+CME ERROR:", b"+CMS ERROR: 500", b"+CPIN: READY"] {
            let (mut transport, trace) = fake(vec![quiet(), write(b"AT+CPIN?\r"), read(partial)]);
            let error = transport.status_command("AT+CPIN?", Duration::from_millis(100), 4096).unwrap_err();
            assert_eq!(error.code, "usb_at_status_incomplete_response");
            assert_eq!(trace.lock().unwrap().writes.len(), 1);
        }
        for complete in [b"\r\nERROR\r\n".as_slice(), b"\r\n+CME ERROR: 10\r\n", b"\r\n+CMS ERROR: 500\r\n"] {
            let (mut transport, _) = fake(vec![quiet(), write(b"AT+CPIN?\r"), read(complete)]);
            assert!(transport.status_command("AT+CPIN?", Duration::from_secs(1), 4096).is_ok());
        }
        let (mut transport, _) = fake(vec![quiet(), write(b"AT+CSQ\r"), read(b"+CSQ: 18")]);
        assert_eq!(transport.command("AT+CSQ", None, Duration::from_millis(100), 4096).unwrap(), "+CSQ: 18");
    }

    #[tokio::test]
    async fn byte_status_tail_urc_identity_change_retires_previous_sms_binding() {
        let mut actions = Vec::new();
        sms_identity_steps(&mut actions);
        status_steps(&mut actions, b"\r\n+CPIN: READY\r\nOK\r\n", true);
        let (owner, session, trace) = byte_owner(actions).await;
        // The framing fixture must not bypass the real SMS policy guard.
        owner.set_cellular_suppressed(true);
        assert_eq!(owner.capture_native_sms(&session).await.unwrap_err().code, "usb_at_busy");
        assert!(trace.lock().unwrap().writes.is_empty());
        owner.set_cellular_suppressed(false);
        let (_, generation) = owner.capture_native_sms(&session).await.unwrap();
        assert!(owner.native_sms_binding_current(&session, generation));
        let observation = owner.native_device_observation(&session).await.unwrap();
        assert_eq!(observation.status["iccid"], "8988212345678901234");
        assert_eq!(observation.status["imsi"], "234150123456789");
        assert!(owner.device_observation_current(&observation));
        assert!(!owner.native_sms_binding_current(&session, generation));
        assert!(!owner.owns_native_sms_binding(&session, generation));
        assert!(trace.lock().unwrap().actions.is_empty());
        owner.shutdown().await.unwrap();
        assert!(trace.lock().unwrap().dropped);
    }

    #[tokio::test]
    async fn byte_status_complete_error_and_pin_absence_preserve_field_and_identity_semantics() {
        for (cpin, ready, revoke) in [
            (b"\r\n+CPIN: READY\r\nOK\r\n".as_slice(), true, false),
            (b"\r\n+CPIN: SIM PIN\r\nOK\r\n+CEREG: 0\r\n", false, true),
            (b"\r\n+CME ERROR: 10\r\n+CEREG: 0\r\n", false, true),
            (b"\r\nERROR\r\n+CEREG: 0\r\n", false, false),
        ] {
            let mut actions = Vec::new();
            sms_identity_steps(&mut actions);
            status_steps(&mut actions, cpin, false);
            let (owner, session, trace) = byte_owner(actions).await;
            let (_, generation) = owner.capture_native_sms(&session).await.unwrap();
            assert!(owner.native_sms_binding_current(&session, generation));
            let observation = owner.native_device_observation(&session).await.unwrap();
            assert_eq!(observation.status["sim_inserted"], ready);
            assert_eq!(observation.status["imei"], "123456789012345");
            assert_eq!(observation.status["iccid"], "8988200000000000000");
            assert_eq!(observation.status["usbnet_mode"], -1);
            assert!(observation.status.get("phone_number").is_none());
            assert_eq!(owner.native_sms_binding_current(&session, generation), !revoke);
            assert!(trace.lock().unwrap().actions.is_empty());
            assert!(!trace.lock().unwrap().dropped);
            owner.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn byte_status_unfinished_reply_or_disconnect_retires_without_next_command() {
        for kind in 0..3 {
            let mut actions = Vec::new();
            framed_step(&mut actions, "AT+CFUN?", b"\r\n+CFUN: 4\r\nOK\r\n");
            actions.push(quiet());
            actions.push(write(b"ATI\r"));
            if kind == 0 {
                actions.push(read(b"ATI\r\r\nFixture firmware\r\n"));
                for _ in 0..4 {
                    actions.push(Action::Read(Err(IoError::Timeout), Duration::from_secs(3)));
                }
                // This late terminal belongs to ATI, never to a subsequent CPIN.
                actions.push(read(b"\r\nOK\r\n"));
            } else if kind == 1 {
                // Empty timeout: the optional metadata command remains unknown.
            } else {
                actions.push(Action::Read(Err(IoError::Disconnected), Duration::ZERO));
            }
            let (owner, session, trace) = byte_owner(actions).await;
            let error = match owner.native_device_observation(&session).await {
                Ok(_) => panic!("unfinished/disconnected status must not publish"),
                Err(error) => error,
            };
            assert_eq!(error.code, match kind {
                0 => "usb_at_status_incomplete_response",
                1 => "usb_at_timeout",
                _ => "usb_at_disconnected",
            });
            let trace = trace.lock().unwrap();
            assert_eq!(trace.writes, vec![b"AT+CFUN?\r".to_vec(), b"ATI\r".to_vec()]);
            assert!(trace.dropped);
            if kind == 0 {
                assert_eq!(trace.actions.len(), 1, "late terminal was never consumed as another reply");
            }
            drop(trace);
            assert!(owner.device_observation_description().is_none());
            assert!(owner.cached_device_observation().is_none());
            owner.shutdown().await.unwrap();
        }
    }

    include!("clcc_wire_tests.rs");

}
