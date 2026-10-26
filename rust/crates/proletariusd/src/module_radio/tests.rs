#[cfg(test)]
pub(crate) mod tests {
    use super::super::*;
    use http_body_util::BodyExt;
    use std::convert::Infallible;
    use tokio::sync::mpsc;
    pub(crate) struct Fixture {
        pub(crate) service: Service,
        requests: mpsc::UnboundedReceiver<(Action, oneshot::Sender<Result<Outcome, Failure>>)>,
        pub(crate) root: PathBuf,
        _lifetime: crate::daemon_lifetime::Lease,
    }
    impl Fixture {
        pub(crate) fn new() -> Self {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .unwrap()
                .join("build/runtime/module-radio-tests")
                .join(crate::snapshot::new_instance_id());
            std::fs::create_dir_all(&root).unwrap();
            let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
            let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
            let service = Service::new(
                root.clone(),
                crate::usb_at::Owner::new("producer-1".into())
                    .with_recovery_gate_for_tests(gate.clone()),
                crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap(),
                BackendAvailability::with_module_recovery(gate),
                "producer-1".into(),
            );
            let (send, requests) = mpsc::unbounded_channel();
            crate::usb_at::reconnect_tests::install_default(&service.0.usb, "usb-session-1");
            *service.0.physical_fixture.lock().unwrap() = Some(send);
            Self {
                service,
                requests,
                root,
                _lifetime: lifetime,
            }
        }
        pub(crate) async fn configure(&self) {
            self.service
                .submit(Job::Configure(Configure {
                    producer_id: "producer-1".into(),
                    config_seq: 1,
                    transport: "usb".into(),
                    session_id: "usb-session-1".into(),
                }))
                .await
                .unwrap();
        }
        fn preferences(&self, value: Preferences) {
            self.service.0.store.save(value).unwrap();
        }
        pub(crate) async fn next(&mut self) -> (Action, oneshot::Sender<Result<Outcome, Failure>>) {
            tokio::time::timeout(Duration::from_secs(3), self.requests.recv())
                .await
                .unwrap()
                .unwrap()
        }
        fn begin(&self, job: Job) -> JoinHandle<Result<Value, Failure>> {
            let service = self.service.clone();
            tokio::spawn(async move { service.submit(job).await })
        }
        async fn answer(&mut self, mode: u8) -> Action {
            let (action, reply) = self.next().await;
            reply.send(Ok(observed(mode))).unwrap();
            action
        }
        async fn answer_profile_note(&mut self, expected: ProfileNoteCommand, response: &str) {
            let (action, reply) = self.next().await;
            match action {
                Action::ProfileNotes(actual) => assert_eq!(actual, expected),
                other => panic!("expected a module profile note action, got {other:?}"),
            }
            reply
                .send(Ok(Outcome {
                    complete: at_command_succeeded(response),
                    verified: at_command_succeeded(response),
                    error_code: if at_command_succeeded(response) {
                        String::new()
                    } else {
                        "radio_command_rejected".into()
                    },
                    response: response.into(),
                    ..Outcome::default()
                }))
                .unwrap();
        }
        async fn answer_profile_note_outcome(
            &mut self,
            expected: ProfileNoteCommand,
            outcome: Outcome,
        ) {
            let (action, reply) = self.next().await;
            match action {
                Action::ProfileNotes(actual) => assert_eq!(actual, expected),
                other => panic!("expected a module profile note action, got {other:?}"),
            }
            reply.send(Ok(outcome)).unwrap();
        }
        pub(crate) fn empty(&mut self) {
            assert!(self.requests.try_recv().is_err());
        }
        pub(crate) fn assert_esim_drained(&self) {
            let state = self.service.0.state.lock().unwrap();
            assert!(state.reservation.is_none());
            assert!(state.waters.get(&Purpose::Esim).unwrap().released);
            assert_eq!(state.jobs, 0);
            assert!(!state.mutating_in_flight);
            assert!(!state.running);
            assert!(self.service.0.task.lock().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn sms_storage_busy_tracks_module_radio_intent_through_release() {
        let fixture = Fixture::new();
        fixture.configure().await;
        assert!(!fixture.service.sim_channel_busy());
        assert!(!fixture.service.vowifi_owns_sim_channel());

        fixture.service.submit(intent("prepare", 1)).await.unwrap();
        assert!(fixture.service.sim_channel_busy());
        assert!(fixture.service.vowifi_owns_sim_channel());

        fixture.service.submit(intent("restore", 1)).await.unwrap();
        assert!(!fixture.service.sim_channel_busy());
        assert!(!fixture.service.vowifi_owns_sim_channel());
    }

    async fn request_module_notes(service: Service, method: &str, body: &str) -> (u16, Value) {
        request_public_route(service, method, "/api/esim/module-notes", body).await
    }
    async fn request_public_route(
        service: Service,
        method: &str,
        path: &str,
        body: &str,
    ) -> (u16, Value) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let serving = tokio::spawn(async move {
            crate::http_server::builder()
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(server),
                        hyper::service::service_fn(move |request: Request<Incoming>| {
                            let service = service.clone();
                            async move {
                                Ok::<_, Infallible>(handle_public(request, service, false).await)
                            }
                        }),
                    )
                    .await
                    .unwrap();
        });
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        serving.await.unwrap();
        let boundary = response
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap();
        let headers = std::str::from_utf8(&response[..boundary]).unwrap();
        let status = headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let body = &response[boundary + 4..];
        (status, serde_json::from_slice(body).unwrap())
    }

    async fn request_at_console(
        fixture: &mut Fixture,
        body: &str,
        response: Option<&str>,
    ) -> (u16, Value) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let service = fixture.service.clone();
        let serving = tokio::spawn(async move {
            crate::http_server::builder()
                .serve_connection(
                    hyper_util::rt::TokioIo::new(server),
                    hyper::service::service_fn(move |request: Request<Incoming>| {
                        let service = service.clone();
                        async move {
                            Ok::<_, Infallible>(
                                handle_at_console_public(request, service, false).await,
                            )
                        }
                    }),
                )
                .await
                .unwrap();
        });
        let request = format!(
            "POST /api/at HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        client.write_all(request.as_bytes()).await.unwrap();
        if let Some(response) = response {
            let (action, reply) = fixture.next().await;
            match action {
                Action::Diagnostic { command, .. } => {
                    let decoded = serde_json::from_str::<Value>(body).unwrap();
                    assert_eq!(command, decoded["command"].as_str().unwrap());
                }
                other => panic!("expected direct Rust diagnostic action, got {other:?}"),
            }
            reply
                .send(Ok(Outcome {
                    response: response.into(),
                    complete: at_command_succeeded(response),
                    verified: at_command_succeeded(response),
                    error_code: if at_command_succeeded(response) {
                        String::new()
                    } else {
                        "radio_command_rejected".into()
                    },
                    ..Outcome::default()
                }))
                .unwrap();
        }
        let mut response_bytes = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(3),
            client.read_to_end(&mut response_bytes),
        )
        .await
        .unwrap()
        .unwrap();
        serving.await.unwrap();
        let boundary = response_bytes
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap();
        let headers = std::str::from_utf8(&response_bytes[..boundary]).unwrap();
        let status = headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let output = serde_json::from_slice(&response_bytes[boundary + 4..]).unwrap();
        (status, output)
    }

    #[tokio::test]
    async fn public_at_console_executes_through_rust_module_actor_and_preserves_result_shape() {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        let (status, body) = request_at_console(
            &mut fixture,
            r#"{"command":"AT+CPIN?"}"#,
            Some("+CPIN: READY\r\nOK\r\n"),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["response"], "+CPIN: READY\r\nOK\r\n");
        assert_eq!(body["outcome"], "success");
        assert!(body.get("error_kind").is_none());
        fixture.empty();
    }

    #[tokio::test]
    async fn public_at_mutation_http_requires_identity_and_replays_exact_receipt_across_successor()
    {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        for body in [
            r#"{"command":"AT+CFUN=1,1"}"#,
            r#"{"command":"ATD123;","client_request_id":"bad id"}"#,
            r#"{"command":"AT+CSIM=4,FFFF","client_request_id":null}"#,
        ] {
            let (status, _) = request_at_console(&mut fixture, body, None).await;
            assert_eq!(status, 400);
            fixture.empty();
        }
        let body = r#"{"command":"AT+UNKNOWNWRITE=1","client_request_id":"raw-write-original"}"#;
        let (status, receipt) = request_at_console(&mut fixture, body, Some("OK\r\n")).await;
        assert_eq!(status, 200);
        {
            let mut state = fixture.service.0.state.lock().unwrap();
            state.epoch += 1;
        }
        let (status, replay) = request_at_console(&mut fixture, body, None).await;
        assert_eq!(status, 200);
        assert_eq!(receipt, replay);
        fixture.empty();
        let (status, _) = request_at_console(
            &mut fixture,
            r#"{"command":"AT+UNKNOWNWRITE=2","client_request_id":"raw-write-original"}"#,
            None,
        )
        .await;
        assert_eq!(status, 409);
        fixture.empty();
    }
    #[tokio::test]
    async fn public_radio_configuration_and_phonebook_http_reject_missing_identity_before_io() {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        for (method, path, body) in [
            (
                "POST",
                "/api/module/flight-mode",
                r#"{"enabled":true,"user_initiated":true}"#,
            ),
            ("POST", "/api/network/usbnet", r#"{"mode":1}"#),
            (
                "PUT",
                "/api/esim/module-notes",
                r#"{"iccid":"8985200000000000007","label":"x"}"#,
            ),
        ] {
            let (status, _) =
                request_public_route(fixture.service.clone(), method, path, body).await;
            assert_eq!(status, 400);
            fixture.empty();
        }
        let binding = fixture.service.public_mutation_binding();
        {
            fixture.service.0.state.lock().unwrap().epoch += 1;
        }
        let error = fixture
            .service
            .submit_bound(
                Job::PublicUSBNetMode {
                    mode: 1,
                    deadline: Instant::now() + USBNET_DEADLINE,
                    expected_binding: binding.clone(),
                    read_wait: None,
                },
                &binding,
            )
            .await
            .unwrap_err();
        assert_eq!(error.public_status(), StatusCode::CONFLICT);
        fixture.empty();
    }

    #[tokio::test]
    async fn public_usbnet_http_receipt_replays_after_new_module_and_conflicts_on_mode_drift() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let service = f.service.clone();
        let body = r#"{"client_request_id":"usbnet-original","mode":1}"#;
        let task = tokio::spawn(async move {
            request_public_route(service, "POST", "/api/network/usbnet", body).await
        });
        let (action, reply) = f.next().await;
        assert!(
            matches!(action,Action::Diagnostic {ref command,..} if command==r#"AT+QCFG="usbnet",1"#)
        );
        reply
            .send(Ok(Outcome {
                complete: true,
                submitted: true,
                verified: true,
                response: "OK".into(),
                ..Outcome::default()
            }))
            .unwrap();
        let (status, receipt) = task.await.unwrap();
        assert_eq!(status, 200);
        {
            let mut state = f.service.0.state.lock().unwrap();
            state.epoch += 1;
        }
        let (status, replay) =
            request_public_route(f.service.clone(), "POST", "/api/network/usbnet", body).await;
        assert_eq!(status, 200);
        assert_eq!(receipt, replay);
        f.empty();
        let (status, _) = request_public_route(
            f.service.clone(),
            "POST",
            "/api/network/usbnet",
            r#"{"client_request_id":"usbnet-original","mode":0}"#,
        )
        .await;
        assert_eq!(status, 409);
        f.empty();
    }
    #[tokio::test]
    async fn public_flight_mode_http_receipt_cannot_rollback_later_configuration() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let service = f.service.clone();
        let body =
            r#"{"client_request_id":"flight-original","enabled":true,"user_initiated":true}"#;
        let task = tokio::spawn(async move {
            request_public_route(service, "POST", "/api/module/flight-mode", body).await
        });
        f.answer(1).await;
        f.answer(4).await;
        f.answer(4).await;
        let (status, receipt) = task.await.unwrap();
        assert_eq!(status, 200);
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        {
            let mut state = f.service.0.state.lock().unwrap();
            state.epoch += 1;
        }
        let (status, replay) =
            request_public_route(f.service.clone(), "POST", "/api/module/flight-mode", body).await;
        assert_eq!(status, 200);
        assert_eq!(receipt, replay);
        assert!(!f.service.0.store.load().unwrap().enabled);
        f.empty();
        let (status, _) = request_public_route(
            f.service.clone(),
            "POST",
            "/api/module/flight-mode",
            r#"{"client_request_id":"flight-original","enabled":false,"user_initiated":true}"#,
        )
        .await;
        assert_eq!(status, 409);
        f.empty();
    }
    #[tokio::test]
    async fn public_module_note_http_lost_response_does_not_repeat_cpbw_on_successor() {
        let mut f = Fixture::new();
        f.configure().await;
        let note = crate::module_profile_notes::Note {
            iccid: "8985200000000000003".into(),
            label: "original note".into(),
            ..Default::default()
        };
        let record = crate::module_profile_notes::encode_note(note).unwrap();
        let body = r#"{"client_request_id":"phonebook-original","iccid":"8985200000000000003","label":"original note"}"#;
        let service = f.service.clone();
        let task = tokio::spawn(async move { request_module_notes(service, "PUT", body).await });
        f.answer_profile_note(ProfileNoteCommand::SelectME, "OK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Status, "+CPBS: \"ME\",0,4\r\nOK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Write { index: 1, record }, "OK")
            .await;
        let (status, receipt) = task.await.unwrap();
        assert_eq!(status, 200);
        {
            let mut state = f.service.0.state.lock().unwrap();
            state.epoch += 1;
        }
        let (status, replay) = request_module_notes(f.service.clone(), "PUT", body).await;
        assert_eq!(status, 200);
        assert_eq!(receipt, replay);
        f.empty();
        let (status,_)=request_module_notes(f.service.clone(),"PUT",r#"{"client_request_id":"phonebook-original","iccid":"8985200000000000004","label":"new card"}"#).await;
        assert_eq!(status, 409);
        f.empty();
    }

    #[tokio::test]
    async fn public_at_console_keeps_module_vowifi_sim_exclusion() {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        fixture.service.submit(intent("prepare", 1)).await.unwrap();
        let (status, body) =
            request_at_console(&mut fixture, r#"{"command":"AT+CPIN?"}"#, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "module_vowifi_owns_sim");
        assert_eq!(
            body["error"],
            "模块 VoWiFi 正在占用 AT/SIM 安全通道，暂不接受手动 AT 命令"
        );
        fixture.empty();
    }

    #[tokio::test]
    async fn public_at_console_keeps_invalid_command_error() {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        let (status, body) = request_at_console(&mut fixture, r#"{"command":"+CSQ"}"#, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "at_command_invalid");
        assert_eq!(body["error"], "命令必须以 AT 开头");
        assert_eq!(body["recoverable"], false);
        fixture.empty();
    }

    #[tokio::test]
    async fn public_at_console_classifies_module_error_in_rust() {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        let (status, body) = request_at_console(
            &mut fixture,
            r#"{"command":"AT+CPIN?"}"#,
            Some("AT+CPIN?\r\nERROR\r\n"),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["response"], "AT+CPIN?\r\nERROR\r\n");
        assert_eq!(body["outcome"], "module_error");
        assert_eq!(body["error_kind"], "generic");
        fixture.empty();
    }

    #[tokio::test]
    async fn public_at_console_blocks_cellular_probe_in_flight_mode_before_transport() {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        fixture.preferences(Preferences {
            enabled: true,
            ..Preferences::default()
        });
        let (status, body) =
            request_at_console(&mut fixture, r#"{"command":"AT+CSQ"}"#, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "module_flight_mode_blocks_cellular_probe");
        assert_eq!(
            body["error"],
            "模块飞行模式已开启，已阻止基站扫描、蜂窝信号查询及蜂窝注册/附着命令"
        );
        fixture.empty();
    }

    #[tokio::test]
    async fn uncertain_at_console_result_is_not_replayed_and_releases_the_user_operation_lease() {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        let service = fixture.service.clone();
        let command =
            tokio::spawn(async move { service.execute_at_console("AT+CPIN?".to_owned()).await });
        let (action, reply) = fixture.next().await;
        assert!(matches!(action, Action::Diagnostic { .. }));
        let mut error = Failure::new(
            StatusCode::GATEWAY_TIMEOUT,
            "radio_deadline",
            "command reply was not observed",
        );
        error.uncertain = true;
        reply.send(Err(error)).unwrap();
        let result = command.await.unwrap().unwrap_err();
        assert!(result.uncertain);
        assert!(
            !fixture.service.sim_channel_busy(),
            "an unknown manual command is reported without replay and does not strand the console reservation"
        );
        fixture.empty();
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.service.begin_shutdown();
            if let Err(error) = self.service.0.modem.shutdown() {
                if std::thread::panicking() {
                    crate::diagnostics::write_line(format_args!(
                        "fixture modem shutdown unconfirmed during unwind: {error:?}"
                    ));
                } else {
                    panic!("fixture modem shutdown unconfirmed: {error:?}");
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn observed(mode: u8) -> Outcome {
        Outcome {
            complete: true,
            verified: true,
            cfun: Some(mode),
            ..Outcome::default()
        }
    }
    fn producer() -> Producer {
        Producer {
            producer_id: "producer-1".into(),
        }
    }
    #[test]
    fn call_mode_operation_subprocess_lock_probe() {
        let Some(root) = std::env::var_os("SWITCH_CALL_MODE_LOCK_PROBE_ROOT") else {
            return;
        };
        let expected_busy = std::env::var("SWITCH_CALL_MODE_LOCK_PROBE_BUSY").unwrap() == "1";
        let result = crate::call_mode_backups::try_call_mode_operation_shared_lock(
            std::path::Path::new(&root),
        );
        if expected_busy {
            assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        } else {
            drop(result.unwrap());
        }
    }

    fn probe_operation_lock_in_child(root: &std::path::Path, busy: bool) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "module_radio::tests::tests::call_mode_operation_subprocess_lock_probe",
            ])
            .env("SWITCH_CALL_MODE_LOCK_PROBE_ROOT", root)
            .env(
                "SWITCH_CALL_MODE_LOCK_PROBE_BUSY",
                if busy { "1" } else { "0" },
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "subprocess probe did not execute"
        );
    }

    #[tokio::test]
    async fn call_mode_operation_guard_proves_exclusive_lock_until_worker_and_reservation_exit() {
        let fixture = Fixture::new();
        fixture.configure().await;
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "operation",
                1,
            ))
            .await
            .unwrap();
        probe_operation_lock_in_child(&fixture.root, true);
        assert_eq!(
            crate::call_mode_adb::voice_route_guard(
                &fixture.root,
                crate::call_mode_adb::VoiceRouteAction::Start,
                None,
            )
            .err()
            .unwrap()
            .code,
            "call_mode_operation_busy",
        );
        let guard = fixture
            .service
            .borrow_call_mode_voice_operation("producer-1", "operation", 1)
            .unwrap();
        let guard = crate::call_mode_adb::voice_route_guard(
            &fixture.root,
            crate::call_mode_adb::VoiceRouteAction::Start,
            Some(crate::call_mode_adb::VoiceRouteGuard::call_mode(guard)),
        )
        .map_err(|error| error.message)
        .unwrap();
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 1);
        assert_eq!(
            fixture
                .service
                .borrow_call_mode_voice_operation("producer-1", "operation", 1)
                .err()
                .unwrap()
                .code,
            "module_radio_busy"
        );
        assert_eq!(
            fixture
                .service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "release",
                    "operation",
                    1
                ))
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        probe_operation_lock_in_child(&fixture.root, true);
        drop(guard);
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 0);
        probe_operation_lock_in_child(&fixture.root, true);
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "operation",
                1,
            ))
            .await
            .unwrap();
        probe_operation_lock_in_child(&fixture.root, false);
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[tokio::test]
    async fn call_mode_operation_guard_rejects_wrong_identity_and_short_read_upgrade() {
        let fixture = Fixture::new();
        fixture.configure().await;
        assert_eq!(
            fixture
                .service
                .submit(reservation_for(
                    Purpose::Diagnostic,
                    "acquire_call_mode_operation",
                    "operation",
                    1
                ))
                .await
                .unwrap_err()
                .code,
            "invalid_radio_request"
        );
        fixture
            .service
            .submit(reservation_for(Purpose::CallMode, "acquire", "read", 1))
            .await
            .unwrap();
        assert_eq!(
            fixture
                .service
                .borrow_call_mode_voice_operation("producer-1", "read", 1)
                .err()
                .unwrap()
                .code,
            "radio_stale_generation"
        );
        assert!(
            fixture
                .service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "acquire_call_mode_operation",
                    "read",
                    1
                ))
                .await
                .is_err()
        );
        fixture
            .service
            .submit(reservation_for(Purpose::CallMode, "release", "read", 1))
            .await
            .unwrap();
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "operation",
                2,
            ))
            .await
            .unwrap();
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "operation",
                2,
            ))
            .await
            .unwrap();
        assert!(
            fixture
                .service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "acquire",
                    "operation",
                    2
                ))
                .await
                .is_err()
        );
        for (producer, token, sequence) in [
            ("producer-2", "operation", 2),
            ("producer-1", "wrong", 2),
            ("producer-1", "operation", 1),
        ] {
            assert_eq!(
                fixture
                    .service
                    .borrow_call_mode_voice_operation(producer, token, sequence)
                    .err()
                    .unwrap()
                    .code,
                "radio_stale_generation"
            );
        }
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 0);
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "operation",
                2,
            ))
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[tokio::test]
    async fn call_mode_short_read_reservation_preserves_existing_shared_audio_guard() {
        let fixture = Fixture::new();
        fixture.configure().await;
        std::fs::create_dir_all(&fixture.root).unwrap();
        let shared =
            crate::call_mode_backups::try_call_mode_operation_shared_lock(&fixture.root).unwrap();
        fixture
            .service
            .submit(reservation_for(Purpose::CallMode, "acquire", "read", 1))
            .await
            .unwrap();
        fixture
            .service
            .submit(reservation_for(Purpose::CallMode, "release", "read", 1))
            .await
            .unwrap();
        assert_eq!(
            fixture
                .service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "acquire_call_mode_operation",
                    "operation",
                    2
                ))
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        assert!(
            fixture
                .service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .is_none()
        );
        drop(shared);
        // Failed OS-lock admission must not consume the reservation sequence.
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "operation",
                2,
            ))
            .await
            .unwrap();
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "operation",
                2,
            ))
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[tokio::test]
    async fn call_mode_operation_guard_survives_caller_abort_and_rejects_late_release() {
        let fixture = Fixture::new();
        fixture.configure().await;
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "operation",
                2,
            ))
            .await
            .unwrap();
        let guard = fixture
            .service
            .borrow_call_mode_voice_operation("producer-1", "operation", 2)
            .unwrap();
        let guard = crate::call_mode_adb::voice_route_guard(
            &fixture.root,
            crate::call_mode_adb::VoiceRouteAction::Start,
            Some(crate::call_mode_adb::VoiceRouteGuard::call_mode(guard)),
        )
        .map_err(|error| error.message)
        .unwrap();
        let (finish, finished) = oneshot::channel::<()>();
        let (cleanup, cleaned) = oneshot::channel::<()>();
        let worker = crate::call_mode_adb::spawn_guarded_voice_task(guard, async move {
            finished.await.unwrap();
            cleaned.await.unwrap();
        });
        // HTTP cancellation only drops its task handle; real work keeps guard.
        drop(worker);
        assert_eq!(
            fixture
                .service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "release",
                    "old-operation",
                    1
                ))
                .await
                .unwrap_err()
                .code,
            "radio_stale_generation"
        );
        assert_eq!(
            fixture
                .service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "release",
                    "operation",
                    2
                ))
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        probe_operation_lock_in_child(&fixture.root, true);
        finish.send(()).unwrap();
        assert_eq!(
            fixture
                .service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "release",
                    "operation",
                    2
                ))
                .await
                .unwrap_err()
                .code,
            "module_radio_busy",
        );
        probe_operation_lock_in_child(&fixture.root, true);
        cleanup.send(()).unwrap();
        fixture.service.wait_idle(false).await;
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "operation",
                2,
            ))
            .await
            .unwrap();
        probe_operation_lock_in_child(&fixture.root, false);
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[tokio::test]
    async fn call_mode_operation_retirement_waits_for_actual_guard_drop() {
        let fixture = Fixture::new();
        fixture.configure().await;
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "operation",
                1,
            ))
            .await
            .unwrap();
        let guard = fixture
            .service
            .borrow_call_mode_voice_operation("producer-1", "operation", 1)
            .unwrap();
        let guard = crate::call_mode_adb::voice_route_guard(
            &fixture.root,
            crate::call_mode_adb::VoiceRouteAction::Start,
            Some(crate::call_mode_adb::VoiceRouteGuard::call_mode(guard)),
        )
        .map_err(|error| error.message)
        .unwrap();
        let (finish, finished) = oneshot::channel::<()>();
        let (cleanup, cleaned) = oneshot::channel::<()>();
        drop(crate::call_mode_adb::spawn_guarded_voice_task(
            guard,
            async move {
                finished.await.unwrap();
                cleaned.await.unwrap();
            },
        ));
        let service = fixture.service.clone();
        let retirement =
            tokio::spawn(async move { service.retire_producer("producer-2".into()).await });
        tokio::task::yield_now().await;
        assert!(!retirement.is_finished());
        assert!(
            fixture
                .service
                .borrow_call_mode_voice_operation("producer-1", "operation", 1)
                .is_err()
        );
        probe_operation_lock_in_child(&fixture.root, true);
        finish.send(()).unwrap();
        tokio::task::yield_now().await;
        assert!(
            !retirement.is_finished(),
            "retired before USB cleanup completed"
        );
        cleanup.send(()).unwrap();
        retirement.await.unwrap().unwrap();
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 0);
        assert!(
            fixture
                .service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .is_none()
        );
        probe_operation_lock_in_child(&fixture.root, false);
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[tokio::test]
    async fn call_mode_operation_guard_waits_for_complete_radio_job_admission() {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        fixture.preferences(Preferences {
            user_configured: true,
            ..Preferences::default()
        });
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "operation",
                1,
            ))
            .await
            .unwrap();
        let mut reboot = action("reboot-call-mode");
        reboot.purpose = Purpose::CallMode;
        reboot.token = "operation".into();
        reboot.kind = "reboot".into();
        reboot.target = None;
        let job = fixture.begin(Job::Action(reboot));
        let (probe, reply) = fixture.next().await;
        assert!(matches!(probe, Action::Observe { cellular: false }));
        assert_eq!(
            fixture
                .service
                .borrow_call_mode_voice_operation("producer-1", "operation", 1)
                .err()
                .unwrap()
                .code,
            "module_radio_busy"
        );
        assert_eq!(
            fixture
                .service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .active_voice_pins,
            0
        );
        reply.send(Ok(observed(1))).unwrap();
        let (write, reply) = fixture.next().await;
        assert!(matches!(write, Action::Reboot));
        assert_eq!(
            fixture
                .service
                .borrow_call_mode_voice_operation("producer-1", "operation", 1)
                .err()
                .unwrap()
                .code,
            "module_radio_busy"
        );
        reply.send(Ok(observed(1))).unwrap();
        job.await.unwrap().unwrap();
        let guard = fixture
            .service
            .borrow_call_mode_voice_operation("producer-1", "operation", 1)
            .unwrap();
        let mut blocked = action("later-radio-action");
        blocked.purpose = Purpose::CallMode;
        blocked.token = "operation".into();
        blocked.kind = "reboot".into();
        blocked.target = None;
        assert_eq!(
            fixture
                .service
                .submit(Job::Action(blocked))
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        fixture
            .service
            .submit(Job::Refresh(producer()))
            .await
            .unwrap();
        fixture.empty();
        drop(guard);
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "operation",
                1,
            ))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shared_voice_operation_retirement_waits_for_physical_owner_drop() {
        let fixture = Fixture::new();
        fixture.configure().await;
        let guard = fixture
            .service
            .borrow_shared_voice_operation("producer-1", false)
            .unwrap();
        assert!(fixture.service.sim_channel_busy());
        let service = fixture.service.clone();
        let retirement =
            tokio::spawn(async move { service.retire_producer("producer-2".into()).await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !fixture.service.0.state.lock().unwrap().retiring {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!retirement.is_finished());
        assert_eq!(
            fixture
                .service
                .borrow_shared_voice_operation("producer-1", true)
                .err()
                .unwrap()
                .code,
            "radio_stale_generation"
        );
        drop(guard);
        retirement.await.unwrap().unwrap();
        let state = fixture.service.0.state.lock().unwrap();
        assert_eq!(state.jobs, 0);
        assert_eq!(state.active_shared_voice_pins, 0);
        drop(state);
        assert!(!fixture.service.sim_channel_busy());
    }

    #[tokio::test]
    async fn shared_voice_operation_shutdown_drains_admitted_cleanup_and_rejects_startup() {
        let fixture = Fixture::new();
        fixture.configure().await;
        assert_eq!(
            fixture
                .service
                .borrow_shared_voice_operation("producer-old", false)
                .err()
                .unwrap()
                .code,
            "radio_stale_generation"
        );
        fixture.service.begin_shutdown();
        assert_eq!(
            fixture
                .service
                .borrow_shared_voice_operation("producer-1", false)
                .err()
                .unwrap()
                .code,
            "backend_stopping"
        );
        let guard = fixture
            .service
            .borrow_shared_voice_operation("producer-1", true)
            .unwrap();
        let service = fixture.service.clone();
        let shutdown = tokio::spawn(async move { service.finish_shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        drop(guard);
        shutdown.await.unwrap().unwrap();
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 0);
    }

    #[tokio::test]
    async fn shared_voice_operation_excludes_radio_configuration_and_physical_work() {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        let guard = fixture
            .service
            .borrow_shared_voice_operation("producer-1", false)
            .unwrap();
        assert_eq!(
            fixture
                .service
                .borrow_shared_voice_operation("producer-1", true)
                .err()
                .unwrap()
                .code,
            "module_radio_busy"
        );
        assert_eq!(
            fixture
                .service
                .submit(Job::Refresh(producer()))
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        assert_eq!(
            fixture
                .service
                .submit(Job::Configure(Configure {
                    producer_id: "producer-1".into(),
                    config_seq: 2,
                    transport: "none".into(),
                    session_id: String::new(),
                }))
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        let epoch = fixture.service.0.state.lock().unwrap().epoch;
        assert_eq!(
            fixture
                .service
                .physical(Action::Observe { cellular: false }, epoch)
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        fixture.empty();
        drop(guard);
        fixture
            .service
            .submit(Job::Configure(Configure {
                producer_id: "producer-1".into(),
                config_seq: 2,
                transport: "none".into(),
                session_id: String::new(),
            }))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shared_voice_operation_rejects_exclusive_call_mode_owner() {
        let fixture = Fixture::new();
        fixture.configure().await;
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "operation",
                1,
            ))
            .await
            .unwrap();
        assert_eq!(
            fixture
                .service
                .borrow_shared_voice_operation("producer-1", true)
                .err()
                .unwrap()
                .code,
            "module_radio_busy"
        );
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 0);
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "operation",
                1,
            ))
            .await
            .unwrap();
        drop(
            fixture
                .service
                .borrow_shared_voice_operation("producer-1", true)
                .unwrap(),
        );
    }

    #[tokio::test]
    async fn restore_stop_borrows_genuine_exclusive_operation_without_shared_lock_conflict() {
        let fixture = Fixture::new();
        fixture.configure().await;
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "restore-operation",
                1,
            ))
            .await
            .unwrap();
        for (producer, token, sequence) in [
            ("producer-old", "restore-operation", 1),
            ("producer-1", "wrong-token", 1),
            ("producer-1", "restore-operation", 2),
        ] {
            assert_eq!(
                fixture
                    .service
                    .borrow_call_mode_voice_operation(producer, token, sequence)
                    .err()
                    .unwrap()
                    .code,
                "radio_stale_generation"
            );
        }
        let exclusive = fixture
            .service
            .borrow_call_mode_voice_operation("producer-1", "restore-operation", 1)
            .unwrap();
        let voice = crate::call_mode_adb::voice_route_guard(
            &fixture.root,
            crate::call_mode_adb::VoiceRouteAction::Stop,
            Some(crate::call_mode_adb::VoiceRouteGuard::call_mode(exclusive)),
        )
        .map_err(|error| error.message)
        .unwrap();
        assert!(voice.is_some());
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 1);
        probe_operation_lock_in_child(&fixture.root, true);
        assert_eq!(
            fixture
                .service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "release",
                    "restore-operation",
                    1,
                ))
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        drop(voice);
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "restore-operation",
                1,
            ))
            .await
            .unwrap();
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 0);
        probe_operation_lock_in_child(&fixture.root, false);
    }

    #[tokio::test]
    async fn shared_voice_operation_rejects_already_admitted_radio_job() {
        let mut fixture = Fixture::new();
        fixture.configure().await;
        fixture.preferences(Preferences::default());
        let job = fixture.begin(Job::Refresh(producer()));
        let (_, reply) = fixture.next().await;
        assert_eq!(
            fixture
                .service
                .borrow_shared_voice_operation("producer-1", false)
                .err()
                .unwrap()
                .code,
            "module_radio_busy"
        );
        reply.send(Ok(observed(1))).unwrap();
        job.await.unwrap().unwrap();
        drop(
            fixture
                .service
                .borrow_shared_voice_operation("producer-1", false)
                .unwrap(),
        );
    }

    #[tokio::test]
    async fn call_mode_operation_concurrent_borrows_grant_only_one_real_pin() {
        let fixture = Fixture::new();
        fixture.configure().await;
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "operation",
                1,
            ))
            .await
            .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let barrier = barrier.clone();
            let service = fixture.service.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                service.borrow_call_mode_voice_operation("producer-1", "operation", 1)
            }));
        }
        barrier.wait();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .find_map(|result| result.as_ref().err())
                .unwrap()
                .code,
            "module_radio_busy"
        );
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 1);
        drop(results);
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 0);
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "operation",
                1,
            ))
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[tokio::test]
    async fn call_mode_operation_unknown_outcome_never_borrows_or_adds_pins() {
        let fixture = Fixture::new();
        fixture.configure().await;
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "operation",
                1,
            ))
            .await
            .unwrap();
        for reservation_unknown in [true, false] {
            {
                let mut state = fixture.service.0.state.lock().unwrap();
                state.reservation.as_mut().unwrap().unknown = reservation_unknown;
                state.observed.uncertain = !reservation_unknown;
            }
            let error = fixture
                .service
                .borrow_call_mode_voice_operation("producer-1", "operation", 1)
                .err()
                .unwrap();
            assert_eq!(error.code, "radio_outcome_unknown");
            assert_eq!(error.status, StatusCode::CONFLICT);
            let state = fixture.service.0.state.lock().unwrap();
            assert_eq!(state.jobs, 0);
            assert_eq!(state.reservation.as_ref().unwrap().active_voice_pins, 0);
        }
        fixture
            .service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "operation",
                1,
            ))
            .await
            .unwrap();
        probe_operation_lock_in_child(&fixture.root, false);
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    #[tokio::test]
    async fn call_mode_operation_lock_symlink_fails_closed_without_reservation() {
        let fixture = Fixture::new();
        fixture.configure().await;
        std::fs::create_dir_all(&fixture.root).unwrap();
        let target = fixture.root.join("unrelated-file");
        std::fs::write(&target, b"preserve").unwrap();
        std::os::unix::fs::symlink(&target, fixture.root.join("call-mode-operation.lock")).unwrap();
        assert_eq!(
            fixture
                .service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "acquire_call_mode_operation",
                    "operation",
                    1
                ))
                .await
                .unwrap_err()
                .code,
            "call_mode_operation_lock_unavailable"
        );
        assert!(
            fixture
                .service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .is_none()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    fn reservation(action: &str, token: &str, sequence: u64) -> Job {
        reservation_for(Purpose::Diagnostic, action, token, sequence)
    }
    fn reservation_for(purpose: Purpose, action: &str, token: &str, sequence: u64) -> Job {
        Job::Reservation(ReservationRequest {
            producer_id: "producer-1".into(),
            action: action.into(),
            purpose,
            token: token.into(),
            sequence,
        })
    }
    fn intent(action: &str, sequence: u64) -> Job {
        Job::Intent(IntentRequest {
            producer_id: "producer-1".into(),
            action: action.into(),
            token: format!("intent-{sequence}"),
            sequence,
        })
    }
    fn action(operation_id: &str) -> RadioAction {
        RadioAction {
            producer_id: "producer-1".into(),
            purpose: Purpose::Diagnostic,
            token: "reservation".into(),
            sequence: 1,
            operation_id: operation_id.into(),
            kind: "set_mode".into(),
            target: Some(4),
            command: None,
            timeout_ms: None,
            voice_target: None,
            usb_expected: None,
            usb_target: None,
        }
    }
    async fn until(mut predicate: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn bootstrap_before_go_ready_persists_once_and_tick_on_four_only_reads() {
        let mut f = Fixture::new();
        f.configure().await;
        assert_eq!(
            f.service.0._availability.snapshot().phase,
            crate::availability::BackendPhase::Starting
        );
        let task = f.begin(Job::Bootstrap(producer()));
        assert!(matches!(
            f.answer(1).await,
            Action::Observe { cellular: false }
        ));
        assert!(matches!(f.answer(4).await, Action::SetVerify { target: 4 }));
        assert!(matches!(
            f.answer(4).await,
            Action::Observe { cellular: false }
        ));
        assert_eq!(task.await.unwrap().unwrap()["verified"], true);
        let bytes = std::fs::read(f.root.join("module-flight-mode.json")).unwrap();
        let modified = std::fs::metadata(f.root.join("module-flight-mode.json"))
            .unwrap()
            .modified()
            .unwrap();
        let task = f.begin(Job::Tick);
        assert!(matches!(
            f.answer(4).await,
            Action::Observe { cellular: false }
        ));
        task.await.unwrap().unwrap();
        f.empty();
        assert_eq!(
            std::fs::read(f.root.join("module-flight-mode.json")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::metadata(f.root.join("module-flight-mode.json"))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
    }
    #[tokio::test]
    async fn explicit_optout_and_corrupt_preferences_never_start_default_rf_work() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let bootstrap = f.begin(Job::Bootstrap(producer()));
        assert!(matches!(
            f.answer(1).await,
            Action::Observe { cellular: false }
        ));
        bootstrap.await.unwrap().unwrap();
        f.empty();
        std::fs::write(f.root.join("module-flight-mode.json"), b"corrupt").unwrap();
        assert_eq!(
            f.service
                .submit(Job::Bootstrap(producer()))
                .await
                .unwrap_err()
                .code,
            "flight_mode_state_read_failed"
        );
        let value = f.service.submit(Job::Query(producer())).await.unwrap();
        assert_eq!(value["suppressed"], true);
        assert_eq!(value["status"]["can_toggle"], false);
        f.empty();
    }

    #[tokio::test]
    async fn public_module_profile_note_list_uses_bounded_batches_and_releases_owner() {
        let mut f = Fixture::new();
        f.configure().await;
        let note_a = crate::module_profile_notes::encode_note(crate::module_profile_notes::Note {
            iccid: "8985200000000000001".into(),
            label: "primary".into(),
            ..Default::default()
        })
        .unwrap();
        let note_b = crate::module_profile_notes::encode_note(crate::module_profile_notes::Note {
            iccid: "8985200000000000002".into(),
            label: "travel".into(),
            ..Default::default()
        })
        .unwrap();
        let task = f.begin(Job::PublicModuleProfileNotes(
            ModuleProfileNotesRequest::List,
        ));
        f.answer_profile_note(ProfileNoteCommand::SelectME, "OK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Status, "+CPBS: \"ME\",2,40\r\nOK")
            .await;
        f.answer_profile_note(
            ProfileNoteCommand::Read { first: 1, last: 32 },
            &format!("+CPBR: 1,\"\",129,\"{note_a}\"\r\nOK"),
        )
        .await;
        f.answer_profile_note(
            ProfileNoteCommand::Read {
                first: 33,
                last: 40,
            },
            &format!("+CPBR: 34,\"\",129,\"{note_b}\"\r\nOK"),
        )
        .await;
        let result = task.await.unwrap().unwrap();
        assert_eq!(result["used"], 2);
        assert_eq!(result["total"], 40);
        assert_eq!(result["notes"]["8985200000000000001"]["label"], "primary");
        assert_eq!(result["notes"]["8985200000000000002"]["index"], 34);
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn public_module_profile_note_save_writes_first_free_slot_and_delete_uses_same_owner() {
        let mut f = Fixture::new();
        f.configure().await;
        let note = crate::module_profile_notes::Note {
            iccid: "8985200000000000003".into(),
            label: "new profile".into(),
            ..Default::default()
        };
        let record = crate::module_profile_notes::encode_note(note.clone()).unwrap();
        let save = f.begin(Job::PublicModuleProfileNotes(
            ModuleProfileNotesRequest::Save(note),
        ));
        f.answer_profile_note(ProfileNoteCommand::SelectME, "OK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Status, "+CPBS: \"ME\",0,4\r\nOK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Write { index: 1, record }, "OK")
            .await;
        let saved = save.await.unwrap().unwrap();
        assert_eq!(saved["message"], "模块资料已保存");
        assert_eq!(saved["index"], 1);
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());

        let encoded = crate::module_profile_notes::encode_note(crate::module_profile_notes::Note {
            index: 3,
            iccid: "8985200000000000004".into(),
            label: "to delete".into(),
            ..Default::default()
        })
        .unwrap();
        let delete = f.begin(Job::PublicModuleProfileNotes(
            ModuleProfileNotesRequest::Save(crate::module_profile_notes::Note {
                iccid: "8985200000000000004".into(),
                ..Default::default()
            }),
        ));
        f.answer_profile_note(ProfileNoteCommand::SelectME, "OK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Status, "+CPBS: \"ME\",1,4\r\nOK")
            .await;
        f.answer_profile_note(
            ProfileNoteCommand::Read { first: 1, last: 4 },
            &format!("+CPBR: 3,\"\",129,\"{encoded}\"\r\nOK"),
        )
        .await;
        f.answer_profile_note(ProfileNoteCommand::Delete { index: 3 }, "OK")
            .await;
        let deleted = delete.await.unwrap().unwrap();
        assert_eq!(deleted["message"], "模块资料已删除");
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn public_module_profile_note_waits_for_existing_esim_reservation() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation_for(Purpose::Esim, "acquire", "esim-1", 1))
            .await
            .unwrap();
        let error = f
            .service
            .submit(Job::PublicModuleProfileNotes(
                ModuleProfileNotesRequest::List,
            ))
            .await
            .unwrap_err();
        assert_eq!(error.code, "module_radio_busy");
        f.empty();
        f.service
            .submit(reservation_for(Purpose::Esim, "release", "esim-1", 1))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn public_module_profile_note_http_errors_keep_legacy_shape() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation_for(Purpose::Esim, "acquire", "esim-2", 1))
            .await
            .unwrap();

        let (status, body) = request_module_notes(f.service.clone(), "GET", "").await;
        assert_eq!(status, StatusCode::CONFLICT.as_u16());
        assert_eq!(body["error"], "已有 eSIM 操作正在进行，请等待完成后重试");
        assert_eq!(body["code"], "esim_operation_busy");
        assert_eq!(body["recoverable"], true);
        assert!(body.get("retryable").is_none());

        f.service
            .submit(reservation_for(Purpose::Esim, "release", "esim-2", 1))
            .await
            .unwrap();
        f.empty();
    }

    #[tokio::test]
    async fn public_module_profile_note_http_rejects_unknown_body_fields() {
        let mut f = Fixture::new();
        f.configure().await;
        let (status, body) = request_module_notes(
            f.service.clone(),
            "PUT",
            r#"{"client_request_id":"module-note-unknown-field","iccid":"8985200000000000007","label":"x","phone":"","tags":"","unexpected":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST.as_u16());
        assert!(body["error"].as_str().is_some());
        assert!(f.requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn public_module_profile_note_rejected_write_is_not_reported_as_saved() {
        let mut f = Fixture::new();
        f.configure().await;
        let note = crate::module_profile_notes::Note {
            iccid: "8985200000000000005".into(),
            label: "write rejected".into(),
            ..Default::default()
        };
        let record = crate::module_profile_notes::encode_note(note.clone()).unwrap();
        let echoed_record = record.clone();
        let save = f.begin(Job::PublicModuleProfileNotes(
            ModuleProfileNotesRequest::Save(note),
        ));
        f.answer_profile_note(ProfileNoteCommand::SelectME, "OK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Status, "+CPBS: \"ME\",0,4\r\nOK")
            .await;
        f.answer_profile_note_outcome(
            ProfileNoteCommand::Write { index: 1, record },
            Outcome {
                submitted: true,
                response: format!("AT+CPBW=1,{echoed_record}\r\nERROR"),
                error_code: "radio_command_rejected".into(),
                ..Outcome::default()
            },
        )
        .await;

        let error = save.await.unwrap().unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.code, "module_profile_notes_command_failed");
        assert!(error.submitted);
        assert!(!error.uncertain);
        assert!(!error.message.contains(&echoed_record));
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn public_module_phonebook_probe_preserves_partial_results_and_command_order() {
        let mut f = Fixture::new();
        f.configure().await;
        let task = f.begin(Job::PublicModulePhonebookProbe);
        f.answer_profile_note(
            ProfileNoteCommand::TestStorage,
            "+CPBS: (\"SM\",\"ME\")\r\nOK",
        )
        .await;
        f.answer_profile_note(ProfileNoteCommand::SelectME, "OK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Status, "ERROR")
            .await;
        f.answer_profile_note(ProfileNoteCommand::ReadSupport, "+CPBR: (1-40)\r\nOK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::WriteSupport, "ERROR")
            .await;
        let result = task.await.unwrap().unwrap();
        assert_eq!(result["storage"], "ME");
        assert_eq!(result["storage_supported"], true);
        assert_eq!(result["storage_selected"], true);
        assert_eq!(result["read_supported"], true);
        assert_eq!(result["write_supported"], false);
        assert_eq!(result["storage_status"], "");
        assert_eq!(result["responses"]["AT+CPBS?"], "ERROR");
        assert_eq!(result["responses"]["AT+CPBW=?"], "ERROR");
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn public_module_profile_note_preserves_uncertain_write_fence() {
        let mut f = Fixture::new();
        f.configure().await;
        let note = crate::module_profile_notes::Note {
            iccid: "8985200000000000006".into(),
            label: "write timed out".into(),
            ..Default::default()
        };
        let record = crate::module_profile_notes::encode_note(note.clone()).unwrap();
        let save = f.begin(Job::PublicModuleProfileNotes(
            ModuleProfileNotesRequest::Save(note),
        ));
        f.answer_profile_note(ProfileNoteCommand::SelectME, "OK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Status, "+CPBS: \"ME\",0,4\r\nOK")
            .await;
        f.answer_profile_note_outcome(
            ProfileNoteCommand::Write { index: 1, record },
            Outcome {
                submitted: true,
                uncertain: true,
                error_code: "radio_response_incomplete".into(),
                ..Outcome::default()
            },
        )
        .await;

        let error = save.await.unwrap().unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.code, "module_profile_notes_command_failed");
        assert!(error.submitted);
        assert!(error.uncertain);
        let state = f.service.0.state.lock().unwrap();
        assert!(state.observed.uncertain);
        assert_eq!(state.observed.error_code, "radio_response_incomplete");
        assert!(state.profile_notes_unknown);
        assert!(state.reservation.is_none());
        drop(state);
        f.empty();

        let retry = f
            .service
            .submit(Job::PublicModuleProfileNotes(
                ModuleProfileNotesRequest::Save(crate::module_profile_notes::Note {
                    iccid: "8985200000000000006".into(),
                    label: "write timed out".into(),
                    ..Default::default()
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(retry.code, "module_profile_notes_outcome_unknown");
        f.empty();

        let list = f.begin(Job::PublicModuleProfileNotes(
            ModuleProfileNotesRequest::List,
        ));
        f.answer_profile_note(ProfileNoteCommand::SelectME, "OK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Status, "+CPBS: \"ME\",0,4\r\nOK")
            .await;
        assert_eq!(list.await.unwrap().unwrap()["used"], 0);
        let state = f.service.0.state.lock().unwrap();
        assert!(!state.profile_notes_unknown);
        assert!(!state.observed.uncertain);
        drop(state);
        f.empty();
    }

    #[tokio::test]
    async fn public_module_phonebook_probe_stops_when_me_storage_is_unsupported() {
        let mut f = Fixture::new();
        f.configure().await;
        let task = f.begin(Job::PublicModulePhonebookProbe);
        f.answer_profile_note(ProfileNoteCommand::TestStorage, "+CPBS: (\"SM\")\r\nOK")
            .await;
        let result = task.await.unwrap().unwrap();
        assert_eq!(result["storage_supported"], false);
        assert_eq!(result["storage_selected"], false);
        assert_eq!(result["responses"].as_object().unwrap().len(), 1);
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn module_profile_note_delete_rejection_is_not_reported_as_success() {
        let mut f = Fixture::new();
        f.configure().await;
        let record = crate::module_profile_notes::encode_note(crate::module_profile_notes::Note {
            index: 2,
            iccid: "8985200000000000005".into(),
            label: "to delete".into(),
            ..Default::default()
        })
        .unwrap();
        let delete = f.begin(Job::PublicModuleProfileNotes(
            ModuleProfileNotesRequest::Save(crate::module_profile_notes::Note {
                iccid: "8985200000000000005".into(),
                ..Default::default()
            }),
        ));
        f.answer_profile_note(ProfileNoteCommand::SelectME, "OK")
            .await;
        f.answer_profile_note(ProfileNoteCommand::Status, "+CPBS: \"ME\",1,4\r\nOK")
            .await;
        f.answer_profile_note(
            ProfileNoteCommand::Read { first: 1, last: 4 },
            &format!("+CPBR: 2,\"\",129,\"{record}\"\r\nOK"),
        )
        .await;
        f.answer_profile_note(ProfileNoteCommand::Delete { index: 2 }, "ERROR")
            .await;
        let error = delete.await.unwrap().unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.code, "module_profile_notes_command_failed");
        assert!(!error.message.contains(&record));
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn module_profile_notes_busy_error_keeps_legacy_coded_payload() {
        let response = module_profile_notes_busy_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value,
            json!({
                "error":"已有 eSIM 操作正在进行，请等待完成后重试",
                "code":"esim_operation_busy",
                "recoverable":true
            })
        );
    }
    #[test]
    fn call_mode_backup_delete_guard_excludes_call_mode_reservation_both_ways() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "switch-radio-backup-delete-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let service = crate::test_module_radio(&root);
        let guard = service.try_call_mode_backup_delete().unwrap();
        let error = service
            .reservation(ReservationRequest {
                producer_id: "producer-1".into(),
                action: "acquire".into(),
                purpose: Purpose::CallMode,
                token: "call-mode".into(),
                sequence: 1,
            })
            .unwrap_err();
        assert_eq!(error.code, "module_radio_busy");
        drop(guard);

        let unavailable = service
            .reservation(ReservationRequest {
                producer_id: "producer-1".into(),
                action: "acquire".into(),
                purpose: Purpose::CallMode,
                token: "call-mode".into(),
                sequence: 1,
            })
            .unwrap_err();
        assert_eq!(unavailable.code, "module_unavailable");

        service.0.state.lock().unwrap().reservation = Some(Reservation {
            operation: Arc::new(
                service
                    .module_recovery()
                    .admit_operation("producer-1")
                    .unwrap(),
            ),
            purpose: Purpose::CallMode,
            identity: Identity {
                token: "ongoing-call-mode".into(),
                sequence: 1,
            },
            attempts: BTreeMap::new(),
            unknown: false,
            operation_lock: None,
            active_voice_pins: 0,
            active_reconnect_pins: 0,
            reconnect: None,
            reconnect_reboot: None,
            restore_journal_required: false,
            restore_journal: None,
            restore_reboot: None,
            restore_reboot_verified: false,
        });
        assert!(service.try_call_mode_backup_delete().is_none());
        let _ = std::fs::remove_dir_all(root);
    }
    #[tokio::test]
    async fn reservations_release_before_acquire_and_configuration_watermarks() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation("release", "future", 4))
            .await
            .unwrap();
        assert_eq!(
            f.service
                .submit(reservation("acquire", "future", 4))
                .await
                .unwrap_err()
                .code,
            "radio_stale_generation"
        );
        f.service
            .submit(reservation("acquire", "current", 5))
            .await
            .unwrap();
        f.service
            .submit(reservation("acquire", "current", 5))
            .await
            .unwrap();
        assert!(
            f.service
                .submit(reservation("release", "old", 3))
                .await
                .is_err()
        );
        assert!(f.service.0.state.lock().unwrap().reservation.is_some());
        std::fs::write(f.root.join("module-flight-mode.json"), b"broken").unwrap();
        f.service
            .submit(reservation("release", "current", 5))
            .await
            .unwrap();
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        assert!(
            f.service
                .submit(Job::Configure(Configure {
                    producer_id: "producer-1".into(),
                    config_seq: 1,
                    transport: "none".into(),
                    session_id: String::new()
                }))
                .await
                .is_err()
        );
        assert!(f.service.0.state.lock().unwrap().binding.is_some());
        f.empty();
    }

    #[tokio::test]
    async fn device_sms_reservation_excludes_other_module_sim_workflows() {
        let f = Fixture::new();
        f.configure().await;
        let reservation = f.service.reserve_device_sms().await.unwrap();
        assert_eq!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .purpose,
            Purpose::DeviceSms
        );
        assert_eq!(
            f.service.reserve_esim().await.err().unwrap().code,
            "module_radio_busy"
        );
        f.service
            .reservation(ReservationRequest {
                producer_id: reservation.producer_id,
                action: "release".into(),
                purpose: Purpose::DeviceSms,
                token: reservation.token,
                sequence: reservation.sequence,
            })
            .unwrap();
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
    }
    #[tokio::test]
    async fn action_retry_is_exact_and_unknown_requires_readonly_confirmation() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation("acquire", "reservation", 1))
            .await
            .unwrap();
        let first = action("operation-1");
        let task = f.begin(Job::Action(first.clone()));
        let (_, reply) = f.next().await;
        reply
            .send(Ok(Outcome {
                submitted: true,
                uncertain: true,
                error_code: "radio_timeout".into(),
                ..Outcome::default()
            }))
            .unwrap();
        let original = task.await.unwrap().unwrap();
        assert_eq!(
            f.service.submit(Job::Action(first.clone())).await.unwrap(),
            original
        );
        let mut conflict = first;
        conflict.target = Some(1);
        assert!(f.service.submit(Job::Action(conflict)).await.is_err());
        assert_eq!(
            f.service
                .submit(Job::Action(action("operation-2")))
                .await
                .unwrap_err()
                .code,
            "radio_outcome_unknown"
        );
        f.empty();
        let refresh = f.begin(Job::Refresh(producer()));
        assert!(matches!(
            f.answer(4).await,
            Action::Observe { cellular: false }
        ));
        refresh.await.unwrap().unwrap();
        let retry = f.begin(Job::Action(action("operation-2")));
        assert!(matches!(f.answer(4).await, Action::SetVerify { target: 4 }));
        retry.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn cancellation_shutdown_and_retirement_wait_for_real_io() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation("acquire", "reservation", 1))
            .await
            .unwrap();
        let task = f.begin(Job::Action(action("operation")));
        let (_, reply) = f.next().await;
        task.abort();
        let service = f.service.clone();
        let retirement =
            tokio::spawn(async move { service.retire_producer("producer-2".into()).await });
        until(|| f.service.0.state.lock().unwrap().retiring).await;
        assert!(!retirement.is_finished());
        reply.send(Ok(observed(4))).unwrap();
        retirement.await.unwrap().unwrap();
        assert_eq!(
            f.service
                .submit(Job::Query(producer()))
                .await
                .unwrap_err()
                .code,
            "radio_stale_generation"
        );
        assert_eq!(f.service.0.state.lock().unwrap().jobs, 0);
        f.empty();
        let mut g = Fixture::new();
        g.configure().await;
        g.service
            .submit(reservation("acquire", "reservation", 1))
            .await
            .unwrap();
        let task = g.begin(Job::Action(action("operation")));
        let (_, reply) = g.next().await;
        task.abort();
        g.service.begin_shutdown();
        let service = g.service.clone();
        let shutdown = tokio::spawn(async move { service.finish_shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        reply.send(Ok(observed(4))).unwrap();
        shutdown.await.unwrap().unwrap();
        assert_eq!(g.service.0.state.lock().unwrap().jobs, 0);
    }
    #[tokio::test]
    async fn intent_restore_unknown_keeps_ownership_and_read_confirms_before_release() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        f.service.submit(intent("restore", 1)).await.unwrap();
        assert!(f.service.submit(intent("prepare", 1)).await.is_err());
        f.service.submit(intent("prepare", 2)).await.unwrap();
        let enter = f.begin(intent("enter", 2));
        f.answer(1).await;
        f.answer(4).await;
        enter.await.unwrap().unwrap();
        let restore = f.begin(intent("restore", 2));
        assert!(matches!(
            f.answer(4).await,
            Action::Observe { cellular: false }
        ));
        let (_, reply) = f.next().await;
        reply
            .send(Ok(Outcome {
                submitted: true,
                uncertain: true,
                ..Outcome::default()
            }))
            .unwrap();
        assert!(restore.await.unwrap().is_err());
        assert!(f.service.0.state.lock().unwrap().intent.is_some());
        let restore = f.begin(intent("restore", 2));
        assert!(matches!(
            f.answer(1).await,
            Action::Observe { cellular: false }
        ));
        assert_eq!(restore.await.unwrap().unwrap()["restored"], true);
        assert!(f.service.0.state.lock().unwrap().intent.is_none());
        f.empty();
    }
    #[tokio::test]
    async fn short_call_mode_reservation_rejects_persistent_config_without_io_or_unknown_attempt() {
        let expected = UsbComposition {
            vendor_id: 0x2ca3,
            product_id: 0x4006,
            flags: [1, 1, 1, 1, 1, 0, 0],
        };
        for kind in [
            "call_mode_voice",
            "call_mode_usb_composition_write",
            "call_mode_adb_authorize",
        ] {
            let mut f = Fixture::new();
            f.configure().await;
            f.service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "acquire",
                    "short-read-reservation",
                    1,
                ))
                .await
                .unwrap();
            let mut input = action("short-reservation-config");
            input.purpose = Purpose::CallMode;
            input.token = "short-read-reservation".into();
            input.kind = kind.into();
            input.target = None;
            match kind {
                "call_mode_voice" => {
                    input.voice_target = Some(VoiceConfiguration {
                        ims: 1,
                        volte_capability: 1,
                        volte_disabled: 0,
                    })
                }
                "call_mode_usb_composition_write" => {
                    input.usb_expected = Some(expected);
                    input.usb_target = Some(expected.call_mode_target());
                }
                "call_mode_adb_authorize" => {}
                _ => unreachable!(),
            }
            // A short read reservation is not an exclusive configuration
            // operation. The real mint must finish with a recorded failure,
            // rather than send fake physical work or strand a pending attempt.
            let failure = tokio::time::timeout(
                Duration::from_secs(3),
                f.service.submit(Job::Action(input.clone())),
            )
            .await
            .expect("short reservation config rejection did not settle")
            .unwrap_err();
            assert_eq!(failure.code, "call_mode_operation_lock_unavailable");
            assert!(!failure.uncertain && !failure.submitted);
            {
                let state = f.service.0.state.lock().unwrap();
                let reservation = state.reservation.as_ref().unwrap();
                assert!(reservation.operation_lock.is_none());
                assert!(!reservation.unknown && !state.observed.uncertain);
                let attempt = reservation.attempts.get(&input.operation_id).unwrap();
                assert_eq!(attempt.input, input);
                assert!(
                    matches!(&attempt.result, Some(Err(error)) if error.code == "call_mode_operation_lock_unavailable" && !error.uncertain && !error.submitted)
                );
            }
            f.empty();
            assert_eq!(
                f.service.submit(Job::Action(input)).await.unwrap_err().code,
                "call_mode_operation_lock_unavailable"
            );
            f.empty();
            f.service
                .submit(reservation_for(
                    Purpose::CallMode,
                    "release",
                    "short-read-reservation",
                    1,
                ))
                .await
                .unwrap();
            assert!(f.service.0.state.lock().unwrap().reservation.is_none());
            f.empty();
        }
    }

    #[tokio::test]
    async fn call_mode_voice_action_requires_call_mode_lease_and_is_idempotent() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "voice-reservation",
                1,
            ))
            .await
            .unwrap();
        std::fs::write(f.root.join("module-flight-mode.json"), b"not-json").unwrap();
        let target = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        let mut input = action("voice-operation");
        input.purpose = Purpose::CallMode;
        input.token = "voice-reservation".into();
        input.kind = "call_mode_voice".into();
        input.target = None;
        input.voice_target = Some(target);
        let task = f.begin(Job::Action(input.clone()));
        let (planned, reply) = f.next().await;
        assert!(matches!(planned, Action::CallModeVoice { target: actual } if actual == target));
        f.service
            .pending_call_mode_commit_for_tests(&planned)
            .unwrap()
            .validate(&planned)
            .unwrap();
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                voice: Some(target),
                ..Outcome::default()
            }))
            .unwrap();
        let first = task.await.unwrap().unwrap();
        assert_eq!(first["voice"]["ims"], 1);
        assert_eq!(f.service.submit(Job::Action(input)).await.unwrap(), first);
        let mut read = action("voice-read-operation");
        read.purpose = Purpose::CallMode;
        read.token = "voice-reservation".into();
        read.kind = "call_mode_voice_read".into();
        read.target = None;
        let task = f.begin(Job::Action(read.clone()));
        let (planned, reply) = f.next().await;
        assert!(matches!(planned, Action::CallModeVoiceRead));
        let current = VoiceConfiguration {
            ims: 2,
            volte_capability: 0,
            volte_disabled: 1,
        };
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                voice: Some(current),
                ..Outcome::default()
            }))
            .unwrap();
        let result = task.await.unwrap().unwrap();
        assert_eq!(result["voice"]["ims"], 2);
        assert_eq!(f.service.submit(Job::Action(read)).await.unwrap(), result);
        f.empty();
    }
    #[tokio::test]
    async fn call_mode_usb_composition_read_requires_business_lease_and_returns_verified_snapshot()
    {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire",
                "usb-composition-reservation",
                1,
            ))
            .await
            .unwrap();
        std::fs::write(f.root.join("module-flight-mode.json"), b"not-json").unwrap();
        let mut input = action("usb-composition-read");
        input.purpose = Purpose::CallMode;
        input.token = "usb-composition-reservation".into();
        input.kind = "call_mode_usb_composition_read".into();
        input.target = None;
        let task = f.begin(Job::Action(input.clone()));
        let (planned, reply) = f.next().await;
        assert!(matches!(planned, Action::CallModeUsbCompositionRead));
        let composition = crate::module_radio_transport::UsbComposition {
            vendor_id: 0x2ca3,
            product_id: 0x4006,
            flags: [1, 1, 1, 1, 1, 0, 1],
        };
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                usb_composition: Some(composition),
                usb_composition_target: Some(crate::module_radio_transport::UsbComposition {
                    vendor_id: 0x2ca3,
                    product_id: 0x4006,
                    flags: [1, 1, 1, 1, 1, 1, 1],
                }),
                ..Outcome::default()
            }))
            .unwrap();
        let first = task.await.unwrap().unwrap();
        assert_eq!(first["usb_composition"]["vendor_id"], 0x2ca3);
        assert_eq!(first["usb_composition"]["flags"][5], 0);
        assert_eq!(first["usb_composition_target"]["vendor_id"], 0x2ca3);
        assert_eq!(first["usb_composition_target"]["product_id"], 0x4006);
        assert_eq!(first["usb_composition_target"]["flags"][5], 1);
        assert_eq!(first["usb_composition_target"]["flags"][6], 1);
        assert_eq!(f.service.submit(Job::Action(input)).await.unwrap(), first);

        f.service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "usb-composition-reservation",
                1,
            ))
            .await
            .unwrap();
        std::fs::remove_file(f.root.join("module-flight-mode.json")).unwrap();
        f.service
            .submit(reservation_for(
                Purpose::Diagnostic,
                "acquire",
                "diagnostic-reservation",
                2,
            ))
            .await
            .unwrap();
        let mut invalid_purpose = action("usb-composition-outside-call-mode");
        invalid_purpose.purpose = Purpose::Diagnostic;
        invalid_purpose.token = "diagnostic-reservation".into();
        invalid_purpose.sequence = 2;
        invalid_purpose.kind = "call_mode_usb_composition_read".into();
        invalid_purpose.target = None;
        assert_eq!(
            f.service
                .submit(Job::Action(invalid_purpose))
                .await
                .unwrap_err()
                .code,
            "invalid_radio_request"
        );
        f.service
            .submit(reservation_for(
                Purpose::Diagnostic,
                "release",
                "diagnostic-reservation",
                2,
            ))
            .await
            .unwrap();
        f.empty();
    }

    #[tokio::test]
    async fn call_mode_readiness_skips_voice_read_until_usb_call_interfaces_are_ready() {
        let mut f = Fixture::new();
        f.configure().await;
        let service = f.service.clone();
        let task = tokio::spawn(async move { service.call_mode_readiness_facts().await });
        let (planned, reply) = f.next().await;
        assert!(matches!(planned, Action::CallModeUsbCompositionRead));
        let usb = UsbComposition {
            vendor_id: 0x2ca3,
            product_id: 0x4006,
            flags: [1, 1, 1, 1, 1, 0, 1],
        };
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                usb_composition: Some(usb),
                usb_composition_target: Some(UsbComposition {
                    flags: [1, 1, 1, 1, 1, 1, 1],
                    ..usb
                }),
                ..Outcome::default()
            }))
            .unwrap();
        let facts = task.await.unwrap().unwrap();
        assert_eq!(facts.usb, usb);
        assert_eq!(facts.voice, None);
        f.empty();
    }

    #[tokio::test]
    async fn call_mode_readiness_reads_voice_only_after_usb_is_ready() {
        let mut f = Fixture::new();
        f.configure().await;
        let service = f.service.clone();
        let task = tokio::spawn(async move { service.call_mode_readiness_facts().await });
        let (planned, reply) = f.next().await;
        assert!(matches!(planned, Action::CallModeUsbCompositionRead));
        let usb = UsbComposition {
            vendor_id: 0x2ca3,
            product_id: 0x4006,
            flags: [1, 1, 1, 1, 1, 1, 1],
        };
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                usb_composition: Some(usb),
                usb_composition_target: Some(usb),
                ..Outcome::default()
            }))
            .unwrap();
        let (planned, reply) = f.next().await;
        assert!(matches!(planned, Action::CallModeVoiceRead));
        let voice = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                voice: Some(voice),
                ..Outcome::default()
            }))
            .unwrap();
        let facts = task.await.unwrap().unwrap();
        assert_eq!(facts.usb, usb);
        assert_eq!(facts.voice, Some(voice));
        f.empty();
    }

    #[tokio::test]
    async fn call_mode_adb_actions_are_typed_and_authorization_requires_call_mode_lease() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "adb-reservation",
                1,
            ))
            .await
            .unwrap();

        let mut probe = action("adb-probe");
        probe.purpose = Purpose::CallMode;
        probe.token = "adb-reservation".into();
        probe.kind = "call_mode_adb_probe".into();
        probe.target = None;
        let task = f.begin(Job::Action(probe.clone()));
        let (planned, reply) = f.next().await;
        assert!(matches!(planned, Action::CallModeAdbProbe));
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                adb_authorization_supported: Some(true),
                ..Outcome::default()
            }))
            .unwrap();
        let probe_result = task.await.unwrap().unwrap();
        assert_eq!(probe_result["adb_authorization_supported"], true);
        assert_eq!(
            f.service.submit(Job::Action(probe)).await.unwrap(),
            probe_result
        );

        let mut authorize = action("adb-authorize");
        authorize.purpose = Purpose::CallMode;
        authorize.token = "adb-reservation".into();
        authorize.kind = "call_mode_adb_authorize".into();
        authorize.target = None;
        let task = f.begin(Job::Action(authorize.clone()));
        let (planned, reply) = f.next().await;
        assert!(matches!(planned, Action::CallModeAdbAuthorize));
        f.service
            .pending_call_mode_commit_for_tests(&planned)
            .unwrap()
            .validate(&planned)
            .unwrap();
        reply
            .send(Ok(Outcome {
                complete: true,
                submitted: true,
                verified: true,
                ..Outcome::default()
            }))
            .unwrap();
        let authorize_result = task.await.unwrap().unwrap();
        assert_eq!(authorize_result["submitted"], true);
        assert_eq!(authorize_result["verified"], true);
        assert_eq!(
            f.service.submit(Job::Action(authorize)).await.unwrap(),
            authorize_result
        );

        f.service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "adb-reservation",
                1,
            ))
            .await
            .unwrap();
        f.empty();
    }

    #[tokio::test]
    async fn call_mode_adb_authorization_is_rejected_under_call_lease() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let bootstrap = f.begin(Job::Bootstrap(producer()));
        assert!(matches!(
            f.answer(1).await,
            Action::Observe { cellular: false }
        ));
        bootstrap.await.unwrap().unwrap();
        f.service
            .submit(reservation_for(
                Purpose::Call,
                "acquire",
                "active-call-reservation",
                1,
            ))
            .await
            .unwrap();

        let mut authorize = action("adb-authorize-during-call");
        authorize.purpose = Purpose::Call;
        authorize.token = "active-call-reservation".into();
        authorize.kind = "call_mode_adb_authorize".into();
        authorize.target = None;
        assert_eq!(
            f.service
                .submit(Job::Action(authorize))
                .await
                .unwrap_err()
                .code,
            "invalid_radio_request"
        );
        f.empty();
    }

    #[tokio::test]
    async fn call_mode_usb_composition_write_requires_call_mode_lease_and_typed_snapshot() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation_for(
                Purpose::CallMode,
                "acquire_call_mode_operation",
                "usb-write-reservation",
                1,
            ))
            .await
            .unwrap();
        let expected = UsbComposition {
            vendor_id: 0x2ca3,
            product_id: 0x4006,
            flags: [1, 1, 1, 1, 1, 0, 0],
        };
        let target = UsbComposition {
            flags: [1, 1, 1, 1, 1, 1, 1],
            ..expected
        };
        let mut input = action("usb-composition-write");
        input.purpose = Purpose::CallMode;
        input.token = "usb-write-reservation".into();
        input.kind = "call_mode_usb_composition_write".into();
        input.target = None;
        input.usb_expected = Some(expected);
        input.usb_target = Some(target);
        let task = f.begin(Job::Action(input.clone()));
        let (planned, reply) = f.next().await;
        assert!(matches!(
            planned,
            Action::CallModeUsbCompositionWrite {
                expected: actual_expected,
                target: actual_target
            } if actual_expected == expected && actual_target == target
        ));
        f.service
            .pending_call_mode_commit_for_tests(&planned)
            .unwrap()
            .validate(&planned)
            .unwrap();
        reply
            .send(Ok(Outcome {
                complete: true,
                submitted: true,
                verified: true,
                usb_composition: Some(target),
                ..Outcome::default()
            }))
            .unwrap();
        let first = task.await.unwrap().unwrap();
        assert_eq!(first["usb_composition"]["flags"][5], 1);
        assert_eq!(f.service.submit(Job::Action(input)).await.unwrap(), first);

        f.service
            .submit(reservation_for(
                Purpose::CallMode,
                "release",
                "usb-write-reservation",
                1,
            ))
            .await
            .unwrap();
        f.empty();
    }
    #[tokio::test]
    async fn read_only_call_mode_voice_action_uses_existing_call_lease() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let bootstrap = f.begin(Job::Bootstrap(producer()));
        assert!(matches!(
            f.answer(1).await,
            Action::Observe { cellular: false }
        ));
        bootstrap.await.unwrap().unwrap();
        f.service
            .submit(reservation_for(
                Purpose::Call,
                "acquire",
                "active-call-reservation",
                1,
            ))
            .await
            .unwrap();
        std::fs::write(f.root.join("module-flight-mode.json"), b"not-json").unwrap();
        let mut input = action("voice-read-during-call");
        input.purpose = Purpose::Call;
        input.token = "active-call-reservation".into();
        input.kind = "call_mode_voice_read".into();
        input.target = None;
        let task = f.begin(Job::Action(input));
        let (planned, reply) = f.next().await;
        assert!(matches!(planned, Action::CallModeVoiceRead));
        let voice = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        reply
            .send(Ok(Outcome {
                complete: true,
                verified: true,
                voice: Some(voice),
                ..Outcome::default()
            }))
            .unwrap();
        let result = task.await.unwrap().unwrap();
        assert_eq!(result["voice"]["volte_capability"], 1);
        f.empty();
    }
    #[tokio::test]
    async fn definite_enable_failure_restores_but_unknown_never_replays() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let task = f.begin(Job::PublicSet(SetRequest {
            enabled: true,
            user_initiated: true,
        }));
        f.answer(1).await;
        let (_, reply) = f.next().await;
        reply
            .send(Ok(Outcome {
                cfun: Some(1),
                error_code: "radio_command_rejected".into(),
                ..Outcome::default()
            }))
            .unwrap();
        assert!(matches!(f.answer(1).await, Action::SetVerify { target: 1 }));
        assert!(task.await.unwrap().is_err());
        assert!(!f.service.0.store.load().unwrap().enabled);
        let task = f.begin(Job::PublicSet(SetRequest {
            enabled: true,
            user_initiated: true,
        }));
        f.answer(1).await;
        let (_, reply) = f.next().await;
        reply
            .send(Ok(Outcome {
                submitted: true,
                uncertain: true,
                ..Outcome::default()
            }))
            .unwrap();
        assert!(task.await.unwrap().unwrap_err().uncertain);
        assert!(f.service.0.store.load().unwrap().enabled);
        f.service.submit(Job::Tick).await.unwrap();
        f.empty();
    }

    #[tokio::test]
    async fn public_reboot_uses_the_radio_owner_and_releases_its_temporary_lease() {
        let mut f = Fixture::new();
        if let Some(task) = f.service.0.task.lock().unwrap().take() {
            task.abort();
        }
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        f.service.0.state.lock().unwrap().observed = observed(1);

        let task = f.begin(Job::PublicReboot);
        assert!(matches!(
            f.answer(1).await,
            Action::Observe { cellular: false }
        ));
        let (action, reply) = f.next().await;
        assert!(matches!(action, Action::Reboot));
        let reboot_response = "AT+CFUN=1,1\r\nOK";
        reply
            .send(Ok(Outcome {
                complete: true,
                submitted: true,
                verified: true,
                response: reboot_response.into(),
                ..Outcome::default()
            }))
            .unwrap();

        let result = task.await.unwrap().unwrap();
        assert_eq!(result["response"], reboot_response);
        let state = f.service.0.state.lock().unwrap();
        assert!(state.reservation.is_none());
        assert_eq!(state.waters.get(&Purpose::Reboot).unwrap().sequence, 1);
        drop(state);
        f.empty();
    }

    #[tokio::test]
    async fn network_recovery_checks_mode_and_calls_under_the_reboot_reservation() {
        for (mode, busy_call) in [(0, false), (1, true), (1, false)] {
            let mut f = Fixture::new();
            if let Some(task) = f.service.0.task.lock().unwrap().take() {
                task.abort();
            }
            f.configure().await;
            f.preferences(Preferences {
                enabled: false,
                restore_cfun: 1,
                user_configured: true,
            });
            f.service.0.state.lock().unwrap().observed = observed(1);
            let task = f.begin(Job::PublicNetworkRecoveryReboot);
            let (action, reply) = f.next().await;
            assert!(
                matches!(action, Action::Diagnostic { ref command, .. } if command == r#"AT+QCFG="usbnet""#)
            );
            assert_eq!(
                f.service
                    .0
                    .state
                    .lock()
                    .unwrap()
                    .reservation
                    .as_ref()
                    .unwrap()
                    .purpose,
                Purpose::Reboot
            );
            reply
                .send(Ok(Outcome {
                    complete: true,
                    verified: true,
                    response: format!("+QCFG: \"usbnet\",{mode}\r\nOK"),
                    ..Outcome::default()
                }))
                .unwrap();
            if mode == 0 {
                assert_eq!(
                    task.await.unwrap().unwrap_err().code,
                    "network_usbnet_not_enabled"
                );
            } else {
                let (action, reply) = f.next().await;
                assert!(matches!(action, Action::CellularCallRead));
                reply
                    .send(Ok(Outcome {
                        complete: true,
                        verified: true,
                        response: if busy_call {
                            "+CLCC: 1,1,4,0,0,\"123\"\r\nOK"
                        } else {
                            "OK"
                        }
                        .into(),
                        ..Outcome::default()
                    }))
                    .unwrap();
                if busy_call {
                    assert_eq!(task.await.unwrap().unwrap_err().code, "radio_call_busy");
                } else {
                    assert!(matches!(
                        f.answer(1).await,
                        Action::Observe { cellular: false }
                    ));
                    let (action, reply) = f.next().await;
                    assert!(matches!(action, Action::Reboot));
                    // A call arriving after the earlier idle read is rejected
                    // by the real Reboot plan's fresh CLCC/URC guard.
                    reply
                        .send(Err(Failure::new(
                            StatusCode::CONFLICT,
                            "radio_call_busy",
                            "fixture incoming call",
                        )))
                        .unwrap();
                    assert_eq!(task.await.unwrap().unwrap_err().code, "radio_call_busy");
                }
            }
            assert!(f.service.0.state.lock().unwrap().reservation.is_none());
            f.empty();
        }
    }

    #[tokio::test]
    async fn public_reboot_is_rejected_when_radio_policy_suppresses_cellular() {
        let mut f = Fixture::new();
        if let Some(task) = f.service.0.task.lock().unwrap().take() {
            task.abort();
        }
        f.configure().await;
        f.preferences(Preferences {
            enabled: true,
            restore_cfun: 1,
            user_configured: true,
        });
        f.service.0.state.lock().unwrap().observed = observed(1);

        let error = f.service.submit(Job::PublicReboot).await.unwrap_err();
        assert_eq!(error.code, "module_flight_mode_enabled");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(
            error.message,
            "模块飞行模式已开启；请先关闭飞行模式后再重启模块"
        );
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn public_reboot_errors_keep_the_legacy_coded_error_shape() {
        let response = Failure {
            status: StatusCode::CONFLICT,
            code: "module_flight_mode_enabled",
            message: "模块飞行模式已开启；请先关闭飞行模式后再重启模块".into(),
            uncertain: false,
            submitted: false,
        }
        .module_radio_compat_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["code"], "module_flight_mode_enabled");
        assert_eq!(
            value["error"],
            "模块飞行模式已开启；请先关闭飞行模式后再重启模块"
        );
        assert_eq!(value["recoverable"], true);
        assert!(value.get("retryable").is_none());
        assert!(value.get("submitted").is_none());
    }

    #[test]
    fn usbnet_mode_parser_keeps_legacy_defaults_and_rejects_invalid_shapes() {
        assert_eq!(decode_usbnet_mode(b"{}").unwrap(), 0);
        assert_eq!(decode_usbnet_mode(b"null").unwrap(), 0);
        assert_eq!(decode_usbnet_mode(br#"{"mode":1}"#).unwrap(), 1);
        assert!(decode_usbnet_mode(br#"{"mode":1,"extra":true}"#).is_err());
        assert!(decode_usbnet_mode(br#"{"mode":1.5}"#).is_err());
        assert!(decode_usbnet_mode(b"[]").is_err());
    }

    #[tokio::test]
    async fn public_usbnet_mode_uses_fixed_diagnostic_command_and_releases_lease() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let task = f.begin(Job::PublicUSBNetMode {
            mode: 1,
            deadline: Instant::now() + USBNET_DEADLINE,
            expected_binding: f.service.public_mutation_binding(),
            read_wait: None,
        });
        let (action, reply) = f.next().await;
        assert!(matches!(
            action,
            Action::Diagnostic { ref command, timeout_ms: 8_000 }
                if command == r#"AT+QCFG="usbnet",1"#
        ));
        let response = "AT+QCFG=\"usbnet\",1\r\nOK";
        reply
            .send(Ok(Outcome {
                complete: true,
                submitted: true,
                verified: true,
                response: response.into(),
                ..Outcome::default()
            }))
            .unwrap();
        assert_eq!(
            task.await.unwrap().unwrap(),
            json!({"mode":1,"response":response,"needs_reboot":true})
        );
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    // Actual public HTTP/receipt + production reservation paths; the physical
    // fixture is held until the original temporary read owner really releases.
    // The two queue cases release their fake reads promptly; they do not
    // exercise the 25-second deadline boundary.
    #[tokio::test]
    async fn usbnet_disable_queues_behind_network_diagnostic_and_disables_once() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        // This is the same Diagnostic job used by GET /api/network; calling
        // the actor directly keeps host interface and ioreg reads out of the
        // fake-transport test.
        let reading = f.begin(Job::PublicNetworkDiagnostic {
            deadline: Instant::now() + Duration::from_secs(20),
        });
        let (action, first_reply) = f.next().await;
        assert!(matches!(action, Action::Diagnostic { ref command, .. }
            if command == r#"AT+QCFG="usbnet""#));
        assert_eq!(
            f.service.0.state.lock().unwrap().reservation.as_ref().unwrap().purpose,
            Purpose::Diagnostic
        );

        let service = f.service.clone();
        let body = r#"{"client_request_id":"network-read-disable","mode":0}"#;
        let disabling = tokio::spawn(async move {
            request_public_route(service, "POST", "/api/network/usbnet", body).await
        });
        until(|| f.service.0.state.lock().unwrap().jobs >= 2).await;
        assert!(!disabling.is_finished());
        f.empty();

        first_reply.send(Ok(Outcome {
            complete: true,
            submitted: true,
            verified: true,
            response: "usbnet: OK".into(),
            ..Outcome::default()
        })).unwrap();
        for (key, expected) in [
            ("usbcfg", r#"AT+QCFG="usbcfg""#),
            ("cgdcont", "AT+CGDCONT?"),
            ("cgact", "AT+CGACT?"),
            ("cgpaddr", "AT+CGPADDR=1"),
        ] {
            let (action, reply) = f.next().await;
            assert!(matches!(action, Action::Diagnostic { ref command, .. }
                if command == expected));
            reply.send(Ok(Outcome {
                complete: true,
                submitted: true,
                verified: true,
                response: format!("{key}: OK"),
                ..Outcome::default()
            })).unwrap();
        }
        let network = reading.await.unwrap().unwrap();
        assert_eq!(network["completed_commands"], 5);

        let (action, reply) = f.next().await;
        assert!(matches!(action, Action::Diagnostic { ref command, .. }
            if command == r#"AT+QCFG="usbnet",0"#));
        reply.send(Ok(Outcome {
            complete: true,
            submitted: true,
            verified: true,
            response: "OK".into(),
            ..Outcome::default()
        })).unwrap();
        let (status, receipt) = disabling.await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(receipt["mode"], 0);
        let (status, replay) =
            request_public_route(f.service.clone(), "POST", "/api/network/usbnet", body).await;
        assert_eq!(status, 200);
        assert_eq!(replay, receipt);
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn usbnet_disable_queues_behind_call_mode_readiness_and_disables_once() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let service = f.service.clone();
        let reading = tokio::spawn(async move { service.call_mode_readiness_facts().await });
        let (action, usb_reply) = f.next().await;
        assert!(matches!(action, Action::CallModeUsbCompositionRead));
        assert_eq!(
            f.service.0.state.lock().unwrap().reservation.as_ref().unwrap().purpose,
            Purpose::CallMode
        );

        let service = f.service.clone();
        let body = r#"{"client_request_id":"call-read-disable","mode":0}"#;
        let disabling = tokio::spawn(async move {
            request_public_route(service, "POST", "/api/network/usbnet", body).await
        });
        until(|| f.service.0.state.lock().unwrap().jobs >= 2).await;
        assert!(!disabling.is_finished());
        f.empty();

        let usb = UsbComposition {
            vendor_id: 0x2ca3,
            product_id: 0x4006,
            flags: [1, 1, 1, 1, 1, 1, 1],
        };
        usb_reply.send(Ok(Outcome {
            complete: true,
            verified: true,
            usb_composition: Some(usb),
            usb_composition_target: Some(usb),
            ..Outcome::default()
        })).unwrap();
        let (action, voice_reply) = f.next().await;
        assert!(matches!(action, Action::CallModeVoiceRead));
        let voice = VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        };
        voice_reply.send(Ok(Outcome {
            complete: true,
            verified: true,
            voice: Some(voice),
            ..Outcome::default()
        })).unwrap();
        assert_eq!(reading.await.unwrap().unwrap().voice, Some(voice));

        let (action, reply) = f.next().await;
        assert!(matches!(action, Action::Diagnostic { ref command, .. }
            if command == r#"AT+QCFG="usbnet",0"#));
        reply.send(Ok(Outcome {
            complete: true,
            submitted: true,
            verified: true,
            response: "OK".into(),
            ..Outcome::default()
        })).unwrap();
        let (status, receipt) = disabling.await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(receipt["mode"], 0);
        let (status, replay) =
            request_public_route(f.service.clone(), "POST", "/api/network/usbnet", body).await;
        assert_eq!(status, 200);
        assert_eq!(replay, receipt);
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn usbnet_enable_then_background_read_waits_and_disables_once_after_exact_release() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let service = f.service.clone();
        let enabling = tokio::spawn(async move {
            request_public_route(
                service,
                "POST",
                "/api/network/usbnet",
                r#"{"client_request_id":"background-read-enable","mode":1}"#,
            )
            .await
        });
        let (action, reply) = f.next().await;
        assert!(matches!(action, Action::Diagnostic { ref command, .. }
            if command == r#"AT+QCFG="usbnet",1"#));
        reply
            .send(Ok(Outcome {
                complete: true,
                submitted: true,
                verified: true,
                response: "OK".into(),
                ..Outcome::default()
            }))
            .unwrap();
        assert_eq!(enabling.await.unwrap().0, 200);

        let lease = f.service.reserve_call_mode_read().await.unwrap();
        assert!(lease.owned);
        let service = f.service.clone();
        let body = r#"{"client_request_id":"background-read-disable","mode":0}"#;
        let disabling = tokio::spawn(async move {
            request_public_route(service, "POST", "/api/network/usbnet", body).await
        });
        until(|| {
            f.service.0.usbnet_waiters.load(Ordering::SeqCst) == 1
                && f.service.0.state.lock().unwrap().jobs == 0
        })
        .await;
        assert!(!disabling.is_finished());
        f.empty();
        assert_eq!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .identity
                .token,
            lease.token
        );
        f.service.release_call_mode_operation(&lease).await.unwrap();
        let (action, reply) = f.next().await;
        assert!(matches!(action, Action::Diagnostic { ref command, .. }
            if command == r#"AT+QCFG="usbnet",0"#));
        reply
            .send(Ok(Outcome {
                complete: true,
                submitted: true,
                verified: true,
                response: "OK".into(),
                ..Outcome::default()
            }))
            .unwrap();
        let (status, receipt) = disabling.await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(receipt["needs_reboot"], true);
        let (status, replay) =
            request_public_route(f.service.clone(), "POST", "/api/network/usbnet", body).await;
        assert_eq!(status, 200);
        assert_eq!(replay, receipt);
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn eight_usbnet_http_waiters_release_all_actor_slots_for_original_cleanup() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let lease = f.service.reserve_call_mode_read().await.unwrap();
        let mut tasks = Vec::new();
        for index in 0..8 {
            let service = f.service.clone();
            tasks.push(tokio::spawn(async move {
                let body = format!(r#"{{"client_request_id":"eight-wait-{index}","mode":0}}"#);
                request_public_route(service, "POST", "/api/network/usbnet", &body).await
            }));
        }
        until(|| {
            f.service.0.usbnet_waiters.load(Ordering::SeqCst) == 8
                && f.service.0.state.lock().unwrap().jobs == 0
        })
        .await;
        assert_eq!(f.service.0.slots.available_permits(), 8);
        f.empty();
        // Real release must acquire an ordinary slot; no special test bypass.
        f.service.release_call_mode_operation(&lease).await.unwrap();
        for _ in 0..8 {
            let (action, reply) = f.next().await;
            assert!(matches!(action, Action::Diagnostic { ref command, .. }
                if command == r#"AT+QCFG="usbnet",0"#));
            reply
                .send(Ok(Outcome {
                    complete: true,
                    submitted: true,
                    verified: true,
                    response: "OK".into(),
                    ..Outcome::default()
                }))
                .unwrap();
        }
        for task in tasks {
            assert_eq!(task.await.unwrap().0, 200);
        }
        assert_eq!(f.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn usbnet_read_wait_deadline_is_absolute_and_does_not_release_owner() {
        let mut f = Fixture::new();
        f.configure().await;
        let lease = f.service.reserve_call_mode_read().await.unwrap();
        let binding = f.service.public_mutation_binding();
        let error = f
            .service
            .submit_usbnet_mode_until(0, &binding, Instant::now() + Duration::from_millis(35))
            .await
            .unwrap_err();
        assert_eq!(error.code, "module_radio_busy");
        assert!(!error.submitted && !error.uncertain);
        assert_eq!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .identity
                .token,
            lease.token
        );
        assert_eq!(f.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
        f.empty();
        f.service.release_call_mode_operation(&lease).await.unwrap();
    }

    #[tokio::test]
    async fn usbnet_wait_rechecks_binding_and_never_adopts_a_replacement_module() {
        let mut f = Fixture::new();
        f.configure().await;
        let lease = f.service.reserve_call_mode_read().await.unwrap();
        let binding = f.service.public_mutation_binding();
        let service = f.service.clone();
        let waiting = tokio::spawn(async move {
            service
                .submit_usbnet_mode_until(0, &binding, Instant::now() + Duration::from_secs(2))
                .await
        });
        until(|| {
            f.service.0.usbnet_waiters.load(Ordering::SeqCst) == 1
                && f.service.0.state.lock().unwrap().jobs == 0
        })
        .await;
        // A real original cleanup still completes; subsequent config identity
        // drift must be observed before any resumed AT reaches the transport.
        f.service.0.state.lock().unwrap().config_seq += 1;
        f.service.0.finished.notify_waiters();
        assert_eq!(
            waiting.await.unwrap().unwrap_err().code,
            "radio_stale_generation"
        );
        f.empty();
        f.service.release_call_mode_operation(&lease).await.unwrap();
    }

    #[tokio::test]
    async fn usbnet_actual_call_mode_operation_and_unknown_reads_stay_busy() {
        let mut f = Fixture::new();
        f.configure().await;
        let lease = f.service.reserve_call_mode_operation().await.unwrap();
        let binding = f.service.public_mutation_binding();
        let error = f
            .service
            .submit_usbnet_mode_bound(0, &binding)
            .await
            .unwrap_err();
        assert_eq!(error.code, "module_radio_busy");
        assert!(!error.submitted && !error.uncertain);
        assert_eq!(f.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
        f.empty();
        f.service.release_call_mode_operation(&lease).await.unwrap();
        let read = f.service.reserve_call_mode_read().await.unwrap();
        f.service
            .0
            .state
            .lock()
            .unwrap()
            .reservation
            .as_mut()
            .unwrap()
            .unknown = true;
        let error = f
            .service
            .submit_usbnet_mode_bound(0, &binding)
            .await
            .unwrap_err();
        assert_eq!(error.code, "module_radio_busy");
        assert!(!error.submitted && !error.uncertain);
        assert_eq!(f.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
        f.empty();
        f.service.release_call_mode_operation(&read).await.unwrap();

        let pin = f.service.borrow_shared_voice_operation("producer-1", false).unwrap();
        let error = f.service.submit_usbnet_mode_bound(0, &binding).await.unwrap_err();
        assert_eq!(error.code, "module_radio_busy");
        assert!(!error.submitted && !error.uncertain);
        assert_eq!(f.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
        f.empty();
        drop(pin);
    }

    #[tokio::test]
    async fn usbnet_waiter_cancel_does_not_release_original_read_or_dispatch_later() {
        let mut f = Fixture::new();
        f.configure().await;
        let lease = f.service.reserve_call_mode_read().await.unwrap();
        let binding = f.service.public_mutation_binding();
        let service = f.service.clone();
        let waiter =
            tokio::spawn(async move { service.submit_usbnet_mode_bound(0, &binding).await });
        until(|| {
            f.service.0.usbnet_waiters.load(Ordering::SeqCst) == 1
                && f.service.0.state.lock().unwrap().jobs == 0
        })
        .await;
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert_eq!(f.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
        assert_eq!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .identity
                .token,
            lease.token
        );
        f.service.release_call_mode_operation(&lease).await.unwrap();
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn usbnet_cancel_after_dispatch_retains_real_job_and_original_reservation() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let binding = f.service.public_mutation_binding();
        let service = f.service.clone();
        let waiter =
            tokio::spawn(async move { service.submit_usbnet_mode_bound(0, &binding).await });
        let (action, reply) = f.next().await;
        assert!(matches!(action, Action::Diagnostic { ref command, .. }
            if command == r#"AT+QCFG="usbnet",0"#));
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        {
            let state = f.service.0.state.lock().unwrap();
            assert_eq!(state.jobs, 1);
            assert_eq!(
                state.reservation.as_ref().unwrap().purpose,
                Purpose::Diagnostic
            );
        }
        reply
            .send(Ok(Outcome {
                complete: true,
                submitted: true,
                verified: true,
                response: "OK".into(),
                ..Outcome::default()
            }))
            .unwrap();
        until(|| f.service.0.state.lock().unwrap().jobs == 0).await;
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        assert_eq!(f.service.0.slots.available_permits(), 8);
        f.empty();
    }

    #[tokio::test]
    async fn usbnet_wait_observes_shutdown_without_consuming_original_cleanup_slot() {
        let mut f = Fixture::new();
        f.configure().await;
        let lease = f.service.reserve_call_mode_read().await.unwrap();
        let binding = f.service.public_mutation_binding();
        let service = f.service.clone();
        let waiter =
            tokio::spawn(async move { service.submit_usbnet_mode_bound(0, &binding).await });
        until(|| {
            f.service.0.usbnet_waiters.load(Ordering::SeqCst) == 1
                && f.service.0.state.lock().unwrap().jobs == 0
        })
        .await;
        f.service.begin_shutdown();
        assert_eq!(waiter.await.unwrap().unwrap_err().code, "backend_stopping");
        assert_eq!(f.service.0.slots.available_permits(), 8);
        f.service.release_call_mode_operation(&lease).await.unwrap();
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn admitted_recovery_operation_alone_does_not_exclude_usbnet_disable() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        // OperationRight is finite authority, not a global RF reservation.
        let operation = f
            .service
            .module_recovery()
            .admit_operation("producer-1")
            .unwrap();
        let task = f.begin(Job::PublicUSBNetMode {
            mode: 0,
            deadline: Instant::now() + USBNET_DEADLINE,
            expected_binding: f.service.public_mutation_binding(),
            read_wait: None,
        });
        let (action, reply) = f.next().await;
        assert!(matches!(action, Action::Diagnostic { ref command, .. }
            if command == r#"AT+QCFG="usbnet",0"#));
        reply
            .send(Ok(Outcome {
                complete: true,
                submitted: true,
                verified: true,
                response: "OK".into(),
                ..Outcome::default()
            }))
            .unwrap();
        assert_eq!(task.await.unwrap().unwrap()["mode"], 0);
        assert!(
            operation
                .right()
                .is_active_for(f.service.module_recovery(), "producer-1")
        );
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        drop(operation);
        f.empty();
    }

    #[tokio::test]
    async fn public_usbnet_mode_releases_lease_after_transport_failure() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let task = f.begin(Job::PublicUSBNetMode {
            mode: 0,
            deadline: Instant::now() + USBNET_DEADLINE,
            expected_binding: f.service.public_mutation_binding(),
            read_wait: None,
        });
        let (action, reply) = f.next().await;
        assert!(matches!(
            action,
            Action::Diagnostic { ref command, .. }
                if command == r#"AT+QCFG="usbnet",0"#
        ));
        reply
            .send(Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "fixture_transport_error",
                "fixed fixture transport failure",
            )))
            .unwrap();
        assert_eq!(
            task.await.unwrap().unwrap_err().code,
            "fixture_transport_error"
        );
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn public_network_diagnostic_runs_fixed_queries_under_one_lease() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let commands = [
            ("usbnet", r#"AT+QCFG="usbnet""#),
            ("usbcfg", r#"AT+QCFG="usbcfg""#),
            ("cgdcont", "AT+CGDCONT?"),
            ("cgact", "AT+CGACT?"),
            ("cgpaddr", "AT+CGPADDR=1"),
        ];
        let task = f.begin(Job::PublicNetworkDiagnostic {
            deadline: Instant::now() + Duration::from_secs(15),
        });
        for (key, expected_command) in commands {
            let (action, reply) = f.next().await;
            assert!(matches!(
                action,
                Action::Diagnostic { ref command, timeout_ms: 8_000 }
                    if command == expected_command
            ));
            reply
                .send(Ok(Outcome {
                    complete: true,
                    submitted: true,
                    verified: true,
                    response: format!("{key}: OK"),
                    ..Outcome::default()
                }))
                .unwrap();
        }
        let result = task.await.unwrap().unwrap();
        assert_eq!(result["completed_commands"], 5);
        assert_eq!(result["raw"]["usbnet"], "usbnet: OK");
        assert_eq!(result["raw"]["cgpaddr"], "cgpaddr: OK");
        assert_eq!(result["errors"], json!({}));
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[test]
    fn diagnostic_grammar_preserves_quoted_data_and_checks_compound_commands() {
        for text in [
            "AT+CFUN=4",
            "AT+CFUN?",
            "AT+CFUN=?",
            r#"AT+NAME="a;+CFUN=1""#,
        ] {
            assert!(!diagnostic::blocked(text).unwrap(), "{text}");
        }
        for text in [
            "AT+CFUN=1",
            "ATE0+CFUN=1",
            "AT; +CSQ",
            "AT+CFUN=4;+COPS=?",
            "AT+CGATT=1",
        ] {
            assert!(diagnostic::blocked(text).unwrap(), "{text}");
        }
        for text in [
            "AT\n+CFUN=1",
            "AT\u{200b}+CFUN=1",
            r#"AT+X="unfinished"#,
            "+CFUN=1",
        ] {
            assert!(diagnostic::blocked(text).is_err(), "{text}");
        }
    }
    #[tokio::test]
    async fn configure_rejects_nonexistent_real_usb_session_before_ack() {
        let f = Fixture::new();
        *f.service.0.physical_fixture.lock().unwrap() = None;
        let result = f
            .service
            .submit(Job::Configure(Configure {
                producer_id: "producer-1".into(),
                config_seq: 1,
                transport: "usb".into(),
                session_id: "nonexistent".into(),
            }))
            .await;
        assert!(result.is_err());
        assert!(f.service.0.state.lock().unwrap().binding.is_none());
        assert_eq!(f.service.0.state.lock().unwrap().config_seq, 0);
    }
    #[tokio::test]
    async fn action_purpose_matrix_rejects_esim_reboot_and_mode_write() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(Job::Reservation(ReservationRequest {
                producer_id: "producer-1".into(),
                action: "acquire".into(),
                purpose: Purpose::Esim,
                token: "reservation".into(),
                sequence: 1,
            }))
            .await
            .unwrap();
        for kind in [
            "set_mode",
            "reboot",
            "diagnostic",
            "call_mode_voice",
            "call_mode_voice_read",
        ] {
            let mut input = action(kind);
            input.purpose = Purpose::Esim;
            input.kind = kind.into();
            if kind != "set_mode" {
                input.target = None;
            }
            if kind == "diagnostic" {
                input.command = Some("AT+CFUN=4".into());
                input.timeout_ms = Some(1000);
            }
            if kind == "call_mode_voice" {
                input.voice_target = Some(VoiceConfiguration {
                    ims: 1,
                    volte_capability: 1,
                    volte_disabled: 0,
                });
            }
            assert_eq!(
                f.service.submit(Job::Action(input)).await.unwrap_err().code,
                "invalid_radio_request"
            );
        }
        assert!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .attempts
                .is_empty()
        );
        f.empty();
    }
    #[tokio::test]
    async fn exhausted_whole_job_budget_never_starts_later_write_or_rollback() {
        let mut f = Fixture::new();
        f.configure().await;
        let task = f.begin(Job::PublicSet(SetRequest {
            enabled: true,
            user_initiated: true,
        }));
        let (_, reply) = f.next().await;
        f.service.0.state.lock().unwrap().job_deadline = Some(Instant::now());
        reply.send(Ok(observed(1))).unwrap();
        assert_eq!(
            task.await.unwrap().unwrap_err().code,
            "flight_mode_enable_failed"
        );
        f.empty();
        assert!(
            f.service.0.store.load().unwrap().enabled,
            "fail-closed durable policy remains until a later confirmed operation"
        );
    }
    #[tokio::test]
    async fn stopping_allows_old_cleanup_but_rejects_new_admission_without_io() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation("acquire", "reservation", 1))
            .await
            .unwrap();
        f.service.begin_shutdown();
        assert_eq!(
            f.service
                .submit(reservation("acquire", "next", 2))
                .await
                .unwrap_err()
                .code,
            "backend_stopping"
        );
        f.service
            .submit(reservation("release", "reservation", 1))
            .await
            .unwrap();
        f.service.submit(Job::Query(producer())).await.unwrap();
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }
    #[tokio::test]
    async fn explicit_optout_diagnostic_still_checks_actual_external_rf_off() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        f.service
            .submit(reservation("acquire", "reservation", 1))
            .await
            .unwrap();
        let mut input = action("cellular-diagnostic");
        input.kind = "diagnostic".into();
        input.target = None;
        input.command = Some("AT+CFUN=1;+CSQ".into());
        input.timeout_ms = Some(1000);
        let task = f.begin(Job::Action(input.clone()));
        assert!(matches!(
            f.answer(4).await,
            Action::Observe { cellular: false }
        ));
        assert_eq!(
            task.await.unwrap().unwrap_err().code,
            "module_flight_mode_blocks_cellular_probe"
        );
        f.empty();
        input.operation_id = "allowed-diagnostic".into();
        let task = f.begin(Job::Action(input));
        assert!(matches!(
            f.answer(1).await,
            Action::Observe { cellular: false }
        ));
        let (command, reply) = f.next().await;
        assert!(matches!(command, Action::Diagnostic { .. }));
        reply
            .send(Ok(Outcome {
                complete: true,
                submitted: true,
                response: "OK".into(),
                ..Outcome::default()
            }))
            .unwrap();
        assert_eq!(task.await.unwrap().unwrap()["response"], "OK");
        assert!(f.service.0.state.lock().unwrap().observed.cfun.is_none());
    }
    #[tokio::test]
    async fn actual_worker_obeys_three_second_idle_schedule_never_overlaps_and_joins() {
        let mut f = Fixture::new();
        f.preferences(Preferences {
            enabled: true,
            restore_cfun: 1,
            user_configured: false,
        });
        let (timer, mut timers) = mpsc::unbounded_channel();
        *f.service.0.timer_fixture.lock().unwrap() = Some(timer);
        f.service.start();
        f.service.start();
        let (duration, initial) = timers.recv().await.unwrap();
        assert_eq!(duration, Duration::from_secs(3));
        f.configure().await;
        let (action, first) = f.next().await;
        assert!(matches!(action, Action::Observe { cellular: false }));
        assert!(
            initial.send(()).is_err(),
            "configure Notify woke the idle worker"
        );
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        f.empty();
        assert!(
            timers.try_recv().is_err(),
            "long-running tick never starts another timer or overlaps"
        );
        first.send(Ok(observed(4))).unwrap();
        let (duration, next) = timers.recv().await.unwrap();
        assert_eq!(duration, Duration::from_secs(3));
        f.empty();
        next.send(()).unwrap();
        let (_, pending) = f.next().await;
        f.service.begin_shutdown();
        let service = f.service.clone();
        let close = tokio::spawn(async move { service.finish_shutdown().await });
        tokio::task::yield_now().await;
        assert!(!close.is_finished());
        pending.send(Ok(observed(4))).unwrap();
        close.await.unwrap().unwrap();
        assert!(!f.service.0.state.lock().unwrap().running);
        f.empty();
    }
    #[tokio::test]
    async fn automatic_enable_preserves_legacy_user_configured_transition_rules() {
        for (mode, already_enabled, expected) in [
            (1, false, false),
            (1, true, false),
            (4, false, false),
            (4, true, true),
        ] {
            let mut f = Fixture::new();
            f.configure().await;
            f.preferences(Preferences {
                enabled: already_enabled,
                restore_cfun: 1,
                user_configured: true,
            });
            // Direct explicit public request is required to override a previous opt-out;
            // default automatic requests deliberately do not re-enable that preference.
            let task = f.begin(Job::PublicSet(SetRequest {
                enabled: true,
                user_initiated: false,
            }));
            if !already_enabled {
                task.await.unwrap().unwrap();
                assert!(f.service.0.store.load().unwrap().user_configured);
                continue;
            }
            f.answer(mode).await;
            if mode != 4 {
                f.answer(4).await;
                f.answer(4).await;
            } else {
                f.answer(4).await;
                f.answer(4).await;
            }
            task.await.unwrap().unwrap();
            assert_eq!(f.service.0.store.load().unwrap().user_configured, expected);
        }
    }
    async fn public_failure_vectors() -> BTreeMap<String, u16> {
        let mut results = BTreeMap::new();
        fn add(results: &mut BTreeMap<String, u16>, error: Failure) {
            results.insert(error.code.into(), error.status.as_u16());
        }
        let request = || {
            Job::PublicSet(SetRequest {
                enabled: true,
                user_initiated: true,
            })
        };
        {
            let f = Fixture::new();
            add(&mut results, f.service.submit(request()).await.unwrap_err());
        }
        {
            let f = Fixture::new();
            f.configure().await;
            f.service
                .submit(reservation("acquire", "busy", 1))
                .await
                .unwrap();
            add(&mut results, f.service.submit(request()).await.unwrap_err());
        }
        {
            let f = Fixture::new();
            f.configure().await;
            std::fs::create_dir_all(&f.root).unwrap();
            std::fs::write(f.root.join("module-flight-mode.json"), b"invalid").unwrap();
            add(&mut results, f.service.submit(request()).await.unwrap_err());
        }
        {
            let mut f = Fixture::new();
            f.configure().await;
            let task = f.begin(request());
            let (_, reply) = f.next().await;
            reply
                .send(Err(Failure::new(
                    StatusCode::CONFLICT,
                    "radio_read_unconfirmed",
                    "fixed fixture",
                )))
                .unwrap();
            add(&mut results, task.await.unwrap().unwrap_err());
        }
        {
            let mut f = Fixture::new();
            f.configure().await;
            let task = f.begin(request());
            let (_, reply) = f.next().await;
            std::fs::create_dir(f.root.join("module-flight-mode.json")).unwrap();
            reply.send(Ok(observed(1))).unwrap();
            add(&mut results, task.await.unwrap().unwrap_err());
        }
        {
            let mut f = Fixture::new();
            f.configure().await;
            let task = f.begin(request());
            f.answer(1).await;
            let (_, reply) = f.next().await;
            reply
                .send(Ok(Outcome {
                    cfun: Some(1),
                    ..Outcome::default()
                }))
                .unwrap();
            f.answer(1).await;
            add(&mut results, task.await.unwrap().unwrap_err());
        }
        {
            let mut f = Fixture::new();
            f.configure().await;
            let task = f.begin(request());
            f.answer(4).await;
            f.answer(1).await;
            add(&mut results, task.await.unwrap().unwrap_err());
        }
        {
            let mut f = Fixture::new();
            f.configure().await;
            let task = f.begin(request());
            f.answer(1).await;
            f.answer(4).await;
            f.answer(1).await;
            f.answer(1).await;
            add(&mut results, task.await.unwrap().unwrap_err());
        }
        {
            let mut f = Fixture::new();
            f.configure().await;
            f.preferences(Preferences {
                enabled: true,
                restore_cfun: 1,
                user_configured: true,
            });
            let task = f.begin(Job::PublicSet(SetRequest {
                enabled: false,
                user_initiated: true,
            }));
            f.answer(4).await;
            let (_, reply) = f.next().await;
            reply
                .send(Err(Failure::new(
                    StatusCode::CONFLICT,
                    "radio_write_rejected",
                    "fixed fixture",
                )))
                .unwrap();
            add(&mut results, task.await.unwrap().unwrap_err());
        }
        results
    }
    #[tokio::test]
    async fn legacy_go_radio_projection_and_failure_parity() {
        use crate::module_radio_transport::{Plan, Reply};
        let bytes = std::env::var("SWITCH_RADIO_PARITY_FIXTURE")
            .map(|path| std::fs::read(path).expect("read explicit radio oracle"))
            .unwrap_or_else(|_| {
                include_bytes!("../../../../testdata/retired-go-oracles/radio.json").to_vec()
            });
        let fixture: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            fixture["source_sha256"],
            "a65d889dc8ef61cf7314f07f4f64bef7085ec2a3b32ca994975ab0fa8aff80df"
        );
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 86, "complete frozen radio corpus");
        let mut names = std::collections::BTreeSet::new();
        let f = Fixture::new();
        for case in cases {
            let input = &case["input"];
            let name = case["name"].as_str().unwrap();
            assert!(names.insert(name), "duplicate radio fixture name");
            let prefs: Preferences = serde_json::from_value(input["preferences"].clone()).unwrap();
            let owned = input["owned"].as_bool().unwrap();
            let mut plan = Plan::new(Action::Observe {
                cellular: !prefs.suppressed() && !owned,
            });
            let mut commands = 0;
            while !plan.done() {
                commands += 1;
                assert!(commands <= 4, "{name}: unexpected polling");
                let step = plan.next().expect("observation has no deferred gap");
                let (raw, failed) = match step.command.as_str() {
                    "AT+CFUN?" => ("cfun_response", "cfun_error"),
                    "AT+CEREG?" => ("cereg_response", "registration_error"),
                    "AT+CREG?" => ("creg_response", "registration_error"),
                    "AT+CSQ" => ("signal_response", "signal_error"),
                    other => panic!("{name}: unexpected {other}"),
                };
                let response = if !input[failed].as_str().unwrap().is_empty() {
                    "ERROR".into()
                } else {
                    format!("{}\r\nOK", input[raw].as_str().unwrap())
                };
                plan.accept(Reply::response(response));
            }
            let observation = plan.finish();
            if input["cfun_error"] == "" {
                assert_eq!(
                    json!(observation.cfun),
                    case["mode"],
                    "{name}: actual CFUN parser"
                );
            }
            let mut state = f.service.0.state.lock().unwrap();
            state.binding = input["available"]
                .as_bool()
                .unwrap()
                .then_some(Binding::Serial { serial_session: 1 });
            state.preferences = Some(prefs);
            state.preference_error = !input["preference_error"].as_str().unwrap().is_empty();
            state.observed = observation;
            state.intent = owned.then_some(Intent {
                operation: Arc::new(
                    f.service
                        .module_recovery()
                        .admit_operation("producer-1")
                        .unwrap(),
                ),
                identity: Identity {
                    token: "fixture".into(),
                    sequence: 1,
                },
                active: true,
                restore_mode: Some(1),
            });
            let actual = status_with_block_reason(&state, input["block_reason"].as_str().unwrap());
            let mut expected = case["expected"].clone();
            // Only diagnostic text is intentionally redacted; state/error semantics remain compared.
            if expected["summary"] == "无法读取模块飞行模式偏好" {
                expected["detail"] = json!("读取模块飞行模式偏好失败");
            }
            if expected["summary"] == "无法读取模块射频状态" {
                expected["detail"] = json!("模块射频状态未通过核验");
            }
            assert_eq!(actual, expected, "{name}");
        }
        let errors = public_failure_vectors().await;
        assert_eq!(
            json!(errors),
            fixture["error_codes"],
            "actual public failure stages"
        );
    }
    #[tokio::test]
    async fn native_suppression_requires_confirmed_optout_and_tightens_before_work() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        assert_eq!(
            f.service.submit(Job::Query(producer())).await.unwrap()["suppressed"],
            true,
            "unknown physical state stays closed"
        );
        let bootstrap = f.begin(Job::Bootstrap(producer()));
        f.answer(1).await;
        bootstrap.await.unwrap().unwrap();
        assert!(!suppressed(&f.service.0.state.lock().unwrap()));
        f.service.submit(intent("prepare", 1)).await.unwrap();
        assert!(suppressed(&f.service.0.state.lock().unwrap()));
        f.service.submit(intent("restore", 1)).await.unwrap();
        assert!(!suppressed(&f.service.0.state.lock().unwrap()));
        let enabled = f.begin(Job::PublicSet(SetRequest {
            enabled: true,
            user_initiated: true,
        }));
        f.answer(1).await;
        let (_, reply) = f.next().await;
        assert!(
            suppressed(&f.service.0.state.lock().unwrap()),
            "save and physical write tighten admission before ACK"
        );
        reply.send(Ok(observed(4))).unwrap();
        f.answer(4).await;
        enabled.await.unwrap().unwrap();
        assert!(suppressed(&f.service.0.state.lock().unwrap()));
    }
    #[tokio::test]
    async fn diagnostic_unknown_survives_reservation_release_and_blocks_tick() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation("acquire", "reservation", 1))
            .await
            .unwrap();
        let mut input = action("diagnostic-unknown");
        input.kind = "diagnostic".into();
        input.target = None;
        input.command = Some("AT+CFUN=4".into());
        input.timeout_ms = Some(1000);
        let task = f.begin(Job::Action(input));
        let (_, reply) = f.next().await;
        reply
            .send(Ok(Outcome {
                submitted: true,
                uncertain: true,
                error_code: "radio_response_incomplete".into(),
                ..Outcome::default()
            }))
            .unwrap();
        task.await.unwrap().unwrap();
        f.service
            .submit(reservation("release", "reservation", 1))
            .await
            .unwrap();
        f.service.submit(Job::Tick).await.unwrap();
        f.service
            .submit(reservation("acquire", "next", 2))
            .await
            .unwrap();
        let mut next = action("new-reservation-operation");
        next.token = "next".into();
        next.sequence = 2;
        assert_eq!(
            f.service.submit(Job::Action(next)).await.unwrap_err().code,
            "radio_outcome_unknown"
        );
        f.empty();
        let state = f.service.0.state.lock().unwrap();
        assert!(state.observed.submitted && state.observed.uncertain);
        assert_eq!(state.observed.error_code, "radio_response_incomplete");
        assert!(suppressed(&state));
    }
    #[tokio::test]
    async fn bootstrap_is_a_real_barrier_while_tick_can_defer_blocked_work() {
        let mut f = Fixture::new();
        assert_eq!(
            f.service
                .submit(Job::Bootstrap(producer()))
                .await
                .unwrap_err()
                .code,
            "module_unavailable"
        );
        f.service.submit(Job::Tick).await.unwrap();
        f.empty();
        f.configure().await;
        f.preferences(Preferences {
            enabled: true,
            restore_cfun: 1,
            user_configured: false,
        });
        f.service
            .submit(reservation("acquire", "reservation", 1))
            .await
            .unwrap();
        assert_eq!(
            f.service
                .submit(Job::Bootstrap(producer()))
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        f.service.submit(Job::Tick).await.unwrap();
        f.empty();
        f.service
            .submit(reservation("release", "reservation", 1))
            .await
            .unwrap();
        let barrier = f.begin(Job::Bootstrap(producer()));
        assert!(matches!(
            f.answer(4).await,
            Action::Observe { cellular: false }
        ));
        assert_eq!(barrier.await.unwrap().unwrap()["verified"], true);
        f.service.submit(intent("prepare", 1)).await.unwrap();
        assert_eq!(
            f.service
                .submit(Job::Bootstrap(producer()))
                .await
                .unwrap_err()
                .code,
            "module_radio_busy"
        );
        f.service.submit(Job::Tick).await.unwrap();
        f.empty();
        f.service.submit(intent("restore", 1)).await.unwrap();
        {
            let mut state = f.service.0.state.lock().unwrap();
            state.observed.uncertain = true;
            state.observed.submitted = true;
        }
        let error = f
            .service
            .submit(Job::Bootstrap(producer()))
            .await
            .unwrap_err();
        assert_eq!(error.code, "radio_outcome_unknown");
        assert!(error.uncertain && error.submitted);
        f.service.submit(Job::Tick).await.unwrap();
        f.empty();
        let refresh = f.begin(Job::Refresh(producer()));
        assert!(matches!(
            f.answer(4).await,
            Action::Observe { cellular: false }
        ));
        refresh.await.unwrap().unwrap();
        let barrier = f.begin(Job::Bootstrap(producer()));
        assert!(matches!(
            f.answer(4).await,
            Action::Observe { cellular: false }
        ));
        assert_eq!(barrier.await.unwrap().unwrap()["verified"], true);
        f.empty();
    }
    #[tokio::test]
    async fn final_radio_receipt_waits_for_restore_readback_and_releases_exact_intent() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        f.service.submit(intent("prepare", 1)).await.unwrap();
        let enter = f.begin(intent("enter", 1));
        f.answer(1).await;
        f.answer(4).await;
        enter.await.unwrap().unwrap();
        let service = f.service.clone();
        let shutdown = tokio::spawn(async move { service.finish_shutdown_receipt().await });
        let (action, reply) = f.next().await;
        assert!(matches!(action, Action::Observe { cellular: false }));
        assert!(!shutdown.is_finished());
        assert!(f.service.0.state.lock().unwrap().intent.is_some());
        reply.send(Ok(observed(1))).unwrap();
        assert!(shutdown.await.unwrap().unwrap().is_empty());
        let state = f.service.0.state.lock().unwrap();
        assert!(state.intent.is_none());
        assert!(state.intent_water.released);
        assert_eq!(state.intent_water.token, "intent-1");
        drop(state);
        assert!(
            f.service
                .finish_shutdown_receipt()
                .await
                .unwrap()
                .is_empty()
        );
        f.empty();
    }

    #[tokio::test]
    async fn final_radio_unknown_restore_retains_fence_and_poll_does_not_replay() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        f.service.submit(intent("prepare", 1)).await.unwrap();
        let enter = f.begin(intent("enter", 1));
        f.answer(1).await;
        f.answer(4).await;
        enter.await.unwrap().unwrap();
        let service = f.service.clone();
        let shutdown = tokio::spawn(async move { service.finish_shutdown_receipt().await });
        assert!(matches!(
            f.answer(4).await,
            Action::Observe { cellular: false }
        ));
        let (action, reply) = f.next().await;
        assert!(matches!(action, Action::SetVerify { target: 1 }));
        reply
            .send(Ok(Outcome {
                submitted: true,
                uncertain: true,
                ..Outcome::default()
            }))
            .unwrap();
        let error = shutdown.await.unwrap().unwrap_err();
        {
            let state = f.service.0.state.lock().unwrap();
            assert_eq!(state.intent.as_ref().unwrap().identity.token, "intent-1");
            assert!(!state.intent_water.released);
            assert!(state.observed.uncertain);
        }
        assert_eq!(
            f.service.finish_shutdown_receipt().await.unwrap_err(),
            error
        );
        f.empty();
    }

    #[tokio::test]
    async fn final_radio_idle_with_unknown_mutation_is_not_a_release_receipt() {
        let f = Fixture::new();
        f.service.0.state.lock().unwrap().observed.uncertain = true;
        assert!(f.service.finish_shutdown_receipt().await.is_err());
        assert!(f.service.0.state.lock().unwrap().observed.uncertain);
    }
    #[tokio::test]
    async fn live_revoke_rejects_new_radio_jobs_and_captured_unprepared_intent() {
        let mut f = Fixture::new();
        f.configure().await;
        let captured = f.service.native_vowifi_intent().unwrap();
        f.service.module_recovery().revoke_for_tests();
        for job in [
            Job::Query(producer()),
            Job::CallModeOpen(producer()),
            Job::Bootstrap(producer()),
            Job::Refresh(producer()),
            Job::Tick,
            Job::CallPoll,
            Job::PublicReboot,
            reservation("acquire", "new-reservation", 1),
            reservation("release", "unproven-release", 1),
            intent("prepare", 1),
            intent("restore", 1),
        ] {
            assert_eq!(
                f.service.submit(job).await.unwrap_err().code,
                crate::module_recovery::CODE
            );
        }
        assert_eq!(
            captured.prepare().await.unwrap_err().code,
            crate::module_recovery::CODE
        );
        assert!(f.service.native_vowifi_intent().is_err());
        assert_eq!(
            f.service.reserve_call_mode_read().await.err().unwrap().code,
            crate::module_recovery::CODE
        );
        assert!(f.service.0.state.lock().unwrap().intent.is_none());
        assert!(f.service.0.state.lock().unwrap().reservation.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn actor_job_accepted_before_revoke_finishes_after_waiting_for_gate() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        let service = f.service.clone();
        let gate = service.0.gate.lock().await;
        let queued = f.begin(Job::Refresh(producer()));
        until(|| f.service.0.state.lock().unwrap().jobs == 1).await;
        f.service.module_recovery().revoke_for_tests();
        drop(gate);
        assert!(matches!(
            f.answer(1).await,
            Action::Observe { cellular: true }
        ));
        queued.await.unwrap().unwrap();
        assert_eq!(
            f.service
                .submit(Job::Refresh(producer()))
                .await
                .unwrap_err()
                .code,
            crate::module_recovery::CODE
        );
        assert_eq!(f.service.0.state.lock().unwrap().observed.cfun, Some(1));
        f.empty();
    }

    #[tokio::test]
    async fn live_revoke_preserves_accepted_intent_enter_and_final_rf_restore() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        });
        f.service.submit(intent("prepare", 1)).await.unwrap();
        let right = f
            .service
            .0
            .state
            .lock()
            .unwrap()
            .intent
            .as_ref()
            .unwrap()
            .operation
            .right();
        let enter = f.begin(intent("enter", 1));
        let (initial, reply) = f.next().await;
        assert!(matches!(initial, Action::Observe { cellular: false }));
        f.service.module_recovery().revoke_for_tests();
        reply.send(Ok(observed(1))).unwrap();
        assert!(matches!(f.answer(4).await, Action::SetVerify { target: 4 }));
        enter.await.unwrap().unwrap();
        assert!(right.is_active_for(f.service.module_recovery(), "producer-1"));
        assert!(f.service.vowifi_owns_sim_channel());
        assert_eq!(
            f.service
                .submit(intent("restore", 2))
                .await
                .unwrap_err()
                .code,
            crate::module_recovery::CODE
        );
        let service = f.service.clone();
        let shutdown = tokio::spawn(async move { service.finish_shutdown_receipt().await });
        assert!(matches!(
            f.answer(4).await,
            Action::Observe { cellular: false }
        ));
        assert!(matches!(f.answer(1).await, Action::SetVerify { target: 1 }));
        shutdown.await.unwrap().unwrap();
        assert!(!right.is_active_for(f.service.module_recovery(), "producer-1"));
        assert!(!f.service.vowifi_owns_sim_channel());
        assert_eq!(
            f.service.submit(intent("restore", 1)).await.unwrap()["restored"],
            true
        );
        f.empty();
    }

    #[tokio::test]
    async fn live_revoke_retains_unknown_action_and_exact_release_receipt() {
        let mut f = Fixture::new();
        f.configure().await;
        f.service
            .submit(reservation("acquire", "reservation", 1))
            .await
            .unwrap();
        let right = f
            .service
            .0
            .state
            .lock()
            .unwrap()
            .reservation
            .as_ref()
            .unwrap()
            .operation
            .right();
        let accepted = action("accepted-operation");
        let task = f.begin(Job::Action(accepted.clone()));
        let (_, reply) = f.next().await;
        f.service.module_recovery().revoke_for_tests();
        reply
            .send(Ok(Outcome {
                submitted: true,
                uncertain: true,
                error_code: "radio_timeout".into(),
                ..Outcome::default()
            }))
            .unwrap();
        let outcome = task.await.unwrap().unwrap();
        assert_eq!(outcome["submitted"], true);
        assert_eq!(outcome["uncertain"], true);
        assert_eq!(
            f.service.submit(Job::Action(accepted)).await.unwrap(),
            outcome
        );
        assert!(f.service.0.state.lock().unwrap().observed.uncertain);
        assert_eq!(
            f.service
                .submit(reservation("release", "forged", 1))
                .await
                .unwrap_err()
                .code,
            crate::module_recovery::CODE
        );
        f.service
            .submit(reservation("release", "reservation", 1))
            .await
            .unwrap();
        until(|| f.service.0.state.lock().unwrap().jobs == 0).await;
        assert!(!right.is_active_for(f.service.module_recovery(), "producer-1"));
        f.service
            .submit(reservation("release", "reservation", 1))
            .await
            .unwrap();
        assert!(f.service.0.state.lock().unwrap().observed.uncertain);
        assert_eq!(
            f.service
                .submit(reservation("acquire", "replacement", 2))
                .await
                .unwrap_err()
                .code,
            crate::module_recovery::CODE
        );
        f.empty();
    }

    #[tokio::test]
    async fn call_mode_replaced_lock_or_root_blocks_writes_but_retains_exact_settlement() {
        use std::os::unix::fs::PermissionsExt;
        for replace_root in [false, true] {
            let mut f = Fixture::new();
            f.configure().await;
            let lease = f.service.reserve_call_mode_operation().await.unwrap();
            f.service.validate_call_mode_operation_lock(&lease).unwrap();
            let (held, right) = {
                let state = f.service.0.state.lock().unwrap();
                let reservation = state.reservation.as_ref().unwrap();
                (
                    reservation.operation_lock.as_ref().unwrap().clone(),
                    reservation.operation.right(),
                )
            };
            let retained_root = f.root.with_extension("retained");
            let old_lock = if replace_root {
                std::fs::rename(&f.root, &retained_root).unwrap();
                std::fs::create_dir(&f.root).unwrap();
                std::fs::set_permissions(&f.root, std::fs::Permissions::from_mode(0o700)).unwrap();
                retained_root.join("call-mode-operation.lock")
            } else {
                let old = f.root.join("retained-operation.lock");
                std::fs::rename(f.root.join("call-mode-operation.lock"), &old).unwrap();
                std::fs::write(
                    f.root.join("call-mode-operation.lock"),
                    b"replacement namespace",
                )
                .unwrap();
                std::fs::set_permissions(
                    f.root.join("call-mode-operation.lock"),
                    std::fs::Permissions::from_mode(0o600),
                )
                .unwrap();
                old
            };
            assert_eq!(
                f.service
                    .validate_call_mode_operation_lock(&lease)
                    .unwrap_err()
                    .code,
                "call_mode_operation_lock_unavailable"
            );
            let error = f
                .service
                .call_mode_action(&lease, Action::Reboot)
                .await
                .unwrap_err();
            assert_eq!(error.code, "call_mode_operation_lock_unavailable");
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(f.service.0.state.lock().unwrap().jobs, 0);
            f.empty();

            // A new shared kernel lock on the renamed original inode still
            // contends with the real exclusive guard, despite its stale name.
            let contender = std::fs::File::open(&old_lock).unwrap();
            assert!(matches!(
                contender.try_lock_shared(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            let pin = f
                .service
                .borrow_call_mode_operation_voice_settlement(&lease, &right)
                .unwrap();
            assert!(Arc::ptr_eq(&held, &pin.operation_lock));
            assert!(f.service.release_call_mode_operation(&lease).await.is_err());
            drop(pin);
            f.service.release_call_mode_operation(&lease).await.unwrap();
            // The retained physical owner, rather than namespace validity,
            // controls the actual unlock. No reacquire substitutes a guard.
            assert!(matches!(
                contender.try_lock_shared(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            assert!(held.validate().is_err());
            drop(held);
            contender.try_lock_shared().unwrap();
            contender.unlock().unwrap();
            f.empty();
            if replace_root {
                std::fs::remove_dir_all(&f.root).unwrap();
                std::fs::rename(&retained_root, &f.root).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn call_mode_write_validation_preserves_real_pin_busy_and_read_only_ownership() {
        let mut f = Fixture::new();
        f.configure().await;
        let lease = f.service.reserve_call_mode_operation().await.unwrap();
        let read = f.service.reserve_call_mode_read().await.unwrap();
        assert!(!read.owned);
        assert!(
            f.service
                .call_mode_action(&read, Action::Reboot)
                .await
                .is_err()
        );
        f.empty();
        let pin = f.service.borrow_call_mode_operation_voice(&lease).unwrap();
        let error = f
            .service
            .call_mode_action(&lease, Action::Reboot)
            .await
            .unwrap_err();
        assert_eq!(error.code, "module_radio_busy");
        f.empty();
        drop(pin);
        f.service.validate_call_mode_operation_lock(&lease).unwrap();
        f.service.release_call_mode_operation(&lease).await.unwrap();
        assert!(f.service.validate_call_mode_operation_lock(&lease).is_err());
        f.empty();
    }

    #[tokio::test]
    async fn live_revoke_exclusive_call_mode_continues_and_release_loop_terminates() {
        let mut f = Fixture::new();
        f.configure().await;
        let lease = f.service.reserve_call_mode_operation().await.unwrap();
        let right = f
            .service
            .0
            .state
            .lock()
            .unwrap()
            .reservation
            .as_ref()
            .unwrap()
            .operation
            .right();
        f.service.module_recovery().revoke_for_tests();
        assert!(f.service.reserve_call_mode_read().await.is_err());
        let service = f.service.clone();
        let worker_lease = lease.clone();
        let read = tokio::spawn(async move { service.call_mode_ensure_idle(&worker_lease).await });
        assert!(matches!(f.answer(1).await, Action::CallModeIdleRead));
        read.await.unwrap().unwrap();
        f.service.begin_shutdown();
        let pin = f.service.borrow_call_mode_operation_voice(&lease).unwrap();
        assert!(right.is_active_for(f.service.module_recovery(), "producer-1"));
        drop(pin);
        tokio::time::timeout(
            Duration::from_secs(3),
            f.service.release_call_mode_operation(&lease),
        )
        .await
        .unwrap()
        .unwrap();
        until(|| f.service.0.state.lock().unwrap().jobs == 0).await;
        assert!(!right.is_active_for(f.service.module_recovery(), "producer-1"));
        f.service.release_call_mode_operation(&lease).await.unwrap();
        let mut fake = lease;
        fake.token = "forged".into();
        assert!(f.service.release_call_mode_operation(&fake).await.is_err());
        f.empty();
    }

    #[tokio::test]
    async fn live_revoke_shared_voice_settlement_requires_retained_authority() {
        let f = Fixture::new();
        let operation = f
            .service
            .module_recovery()
            .admit_operation("producer-1")
            .unwrap();
        let right = operation.right();
        let foreign_gate = crate::module_recovery::Gate::default();
        let foreign = foreign_gate.admit_operation("producer-1").unwrap();
        f.service.module_recovery().revoke_for_tests();
        f.service.begin_shutdown();
        assert!(
            f.service
                .borrow_shared_voice_operation("producer-1", true)
                .is_err()
        );
        assert!(
            f.service
                .borrow_shared_voice_settlement("producer-1", &foreign.right())
                .is_err()
        );
        let pin = f
            .service
            .borrow_shared_voice_settlement("producer-1", &right)
            .unwrap();
        let service = f.service.clone();
        let shutdown_right = right.clone();
        let shutdown = tokio::spawn(async move {
            service
                .borrow_shared_voice_shutdown_settlement("producer-1", &shutdown_right)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        drop(pin);
        let final_pin = tokio::time::timeout(Duration::from_secs(3), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(final_pin);
        drop(operation);
        assert!(
            f.service
                .borrow_shared_voice_settlement("producer-1", &right)
                .is_err()
        );
        assert_eq!(f.service.0.state.lock().unwrap().jobs, 0);
    }
    #[tokio::test]
    async fn uncertain_exclusive_voice_stop_requires_exact_retained_obligation() {
        for reservation_unknown in [true, false] {
            let mut f = Fixture::new();
            f.configure().await;
            let lease = f.service.reserve_call_mode_operation().await.unwrap();
            let operation = f
                .service
                .module_recovery()
                .admit_operation("producer-1")
                .unwrap();
            let wrong_producer = f
                .service
                .module_recovery()
                .admit_operation("other-producer")
                .unwrap();
            let foreign_gate = crate::module_recovery::Gate::default();
            let foreign = foreign_gate.admit_operation("producer-1").unwrap();
            let ended = f
                .service
                .module_recovery()
                .admit_operation("producer-1")
                .unwrap();
            let ended_right = ended.right();
            drop(ended);
            {
                let mut state = f.service.0.state.lock().unwrap();
                state.reservation.as_mut().unwrap().unknown = reservation_unknown;
                state.observed.uncertain = !reservation_unknown;
            }
            f.service.module_recovery().revoke_for_tests();
            assert_eq!(
                f.service
                    .borrow_call_mode_operation_voice(&lease)
                    .err()
                    .unwrap()
                    .code,
                "radio_outcome_unknown"
            );
            for invalid in [foreign.right(), wrong_producer.right(), ended_right] {
                assert!(
                    f.service
                        .borrow_call_mode_operation_voice_settlement(&lease, &invalid)
                        .is_err()
                );
            }
            for invalid in [
                CallModeReservation {
                    epoch: lease.epoch + 1,
                    ..lease.clone()
                },
                CallModeReservation {
                    token: "forged-token".into(),
                    ..lease.clone()
                },
                CallModeReservation {
                    owned: false,
                    ..lease.clone()
                },
            ] {
                assert!(
                    f.service
                        .borrow_call_mode_operation_voice_settlement(&invalid, &operation.right())
                        .is_err()
                );
            }
            f.service.begin_shutdown();
            let pin = f
                .service
                .borrow_call_mode_operation_voice_settlement(&lease, &operation.right())
                .unwrap();
            assert_eq!(
                f.service
                    .0
                    .state
                    .lock()
                    .unwrap()
                    .reservation
                    .as_ref()
                    .unwrap()
                    .active_voice_pins,
                1
            );
            assert!(
                f.service
                    .borrow_call_mode_operation_voice_settlement(&lease, &operation.right())
                    .is_err()
            );
            assert!(f.service.release_call_mode_operation(&lease).await.is_err());
            drop(pin);
            {
                let mut state = f.service.0.state.lock().unwrap();
                assert_eq!(
                    state.reservation.as_ref().unwrap().unknown,
                    reservation_unknown
                );
                assert_eq!(state.observed.uncertain, !reservation_unknown);
                let mut pending = action("pending-radio-action");
                pending.purpose = Purpose::CallMode;
                pending.token = lease.token.clone();
                pending.sequence = lease.sequence;
                state.reservation.as_mut().unwrap().attempts.insert(
                    "pending-radio-action".into(),
                    Attempt {
                        input: pending,
                        result: None,
                    },
                );
            }
            assert_eq!(
                f.service
                    .borrow_call_mode_operation_voice_settlement(&lease, &operation.right())
                    .err()
                    .unwrap()
                    .code,
                "module_radio_busy"
            );
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_mut()
                .unwrap()
                .attempts
                .clear();
            tokio::time::timeout(
                Duration::from_secs(3),
                f.service.release_call_mode_operation(&lease),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(
                f.service
                    .borrow_call_mode_operation_voice_settlement(&lease, &operation.right())
                    .is_err()
            );
            f.empty();
        }
    }
    #[tokio::test]
    async fn native_intent_right_requires_actual_prepare_and_ends_after_exact_restore() {
        let mut f = Fixture::new();
        f.configure().await;
        let captured = f.service.native_vowifi_intent().unwrap();
        assert!(captured.retained_operation_right().is_err());
        captured.prepare().await.unwrap();
        let right = captured.retained_operation_right().unwrap();
        f.service.module_recovery().revoke_for_tests();
        f.service.begin_shutdown();
        assert!(
            captured
                .retained_operation_right()
                .unwrap()
                .is_active_for(f.service.module_recovery(), "producer-1")
        );
        captured.restore().await.unwrap();
        until(|| f.service.0.state.lock().unwrap().jobs == 0).await;
        assert!(!right.is_active_for(f.service.module_recovery(), "producer-1"));
        assert!(captured.retained_operation_right().is_err());
        f.empty();
    }
    #[tokio::test]
    async fn not_admitted_native_prepare_permanently_closes_all_captured_clones() {
        let mut f = Fixture::new();
        f.configure().await;
        let captured = f.service.native_vowifi_intent().unwrap();
        let clone = captured.clone();
        let capacity = f
            .service
            .0
            .slots
            .clone()
            .acquire_many_owned(8)
            .await
            .unwrap();
        let failure = captured.prepare_with_receipt().await.unwrap_err();
        assert!(
            matches!(failure, NativePrepareFailure::NotAdmitted(error) if error.code == "module_radio_busy")
        );
        drop(capacity);
        assert!(
            matches!(clone.prepare_with_receipt().await, Err(NativePrepareFailure::AcceptedOrUnknown(error)) if error.code == "radio_stale_generation")
        );
        assert!(captured.prepare().await.is_err());
        assert!(captured.retained_operation_right().is_err());
        assert_eq!(f.service.0.state.lock().unwrap().jobs, 0);
        assert!(f.service.0.state.lock().unwrap().intent.is_none());
        f.empty();
    }

    #[tokio::test]
    async fn cancelled_inflight_native_prepare_clone_cannot_claim_not_admitted() {
        let mut f = Fixture::new();
        f.configure().await;
        let captured = f.service.native_vowifi_intent().unwrap();
        let gate_owner = f.service.clone();
        let actor_gate = gate_owner.0.gate.lock().await;
        let first_capability = captured.clone();
        let first = tokio::spawn(async move { first_capability.prepare_with_receipt().await });
        until(|| f.service.0.state.lock().unwrap().jobs == 1).await;
        let second_capability = captured.clone();
        let second = tokio::spawn(async move { second_capability.prepare_with_receipt().await });
        tokio::task::yield_now().await;
        assert_eq!(
            f.service.0.state.lock().unwrap().jobs,
            1,
            "clones serialize before another prepare can queue"
        );
        f.service.module_recovery().revoke_for_tests();
        first.abort();
        let _ = first.await;
        assert!(
            matches!(second.await.unwrap(), Err(NativePrepareFailure::AcceptedOrUnknown(error)) if error.code == crate::module_recovery::CODE)
        );
        drop(actor_gate);
        until(|| f.service.0.state.lock().unwrap().intent.is_some()).await;
        assert!(captured.retained_operation_right().is_ok());
        // An already admitted idempotent prepare remains available after revoke.
        captured.prepare().await.unwrap();
        captured.restore().await.unwrap();
        until(|| f.service.0.state.lock().unwrap().jobs == 0).await;
        assert!(f.service.0.state.lock().unwrap().intent.is_none());
        f.empty();
    }
    async fn retained_status_child_contract(initial_retry: bool, drop_observer: bool) {
        let mut f = Fixture::new();
        f.configure().await;
        let inbox = crate::message_archive::InboxState::for_go_producer("producer-1".into());
        let lifecycle = crate::call_mode_lifecycle::Service::new(
            &f.root,
            f.service.clone(),
            inbox,
            f.service.0.usb.clone(),
        );
        let mut probe = crate::call_mode_adb::child_tests::HeldProbe::new("status-refresh");
        std::fs::remove_dir_all(&probe.root).unwrap();
        probe.root = f.root.clone();
        probe.timeout = Duration::from_millis(250);
        lifecycle.install_refresh_probe_for_tests(probe.clone());
        let owner = lifecycle.clone();
        let observer = tokio::spawn(async move {
            if initial_retry {
                let reply = owner.retry().await;
                serde_json::from_slice::<Value>(
                    &reply.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap()
            } else {
                serde_json::to_value(owner.public_status().await).unwrap()
            }
        });
        until(|| probe.root.join("child-ready").exists()).await;
        assert_eq!(lifecycle.active_status_reads_for_tests(), 1);
        let original = f
            .service
            .0
            .state
            .lock()
            .unwrap()
            .reservation
            .as_ref()
            .unwrap()
            .identity
            .token
            .clone();
        // A is accepted and owns a real read reservation, but has not reached
        // its probe deadline. Repeated status/retry must not spawn/queue B.
        for _ in 0..4 {
            assert_eq!(lifecycle.public_status().await.state, "checking");
            let reply = lifecycle.retry().await;
            let value: Value =
                serde_json::from_slice(&reply.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(value["can_retry"], false);
            assert_eq!(lifecycle.active_status_reads_for_tests(), 1);
            assert_eq!(probe.spawned.load(std::sync::atomic::Ordering::Acquire), 1);
        }
        if drop_observer {
            observer.abort();
            assert!(observer.await.unwrap_err().is_cancelled());
            until(|| crate::call_mode_adb::pending_probe(&probe.root, "producer-1").is_some())
                .await;
        } else {
            let observed = tokio::time::timeout(Duration::from_secs(3), observer)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(observed["can_retry"], false);
            assert!(observed["detail"].as_str().unwrap().contains("尚未确认"));
        }
        assert!(probe.alive() && probe.lock_busy());
        assert_eq!(lifecycle.active_status_reads_for_tests(), 1);
        assert_eq!(
            f.service
                .0
                .state
                .lock()
                .unwrap()
                .reservation
                .as_ref()
                .unwrap()
                .identity
                .token,
            original
        );
        assert!(f.service.reserve_call_mode_operation().await.is_err());
        for _ in 0..4 {
            let status =
                tokio::time::timeout(Duration::from_millis(300), lifecycle.public_status())
                    .await
                    .unwrap();
            assert!(!status.can_retry && status.detail.contains("尚未确认"));
        }
        f.empty();
        probe.release();
        until(|| lifecycle.active_status_reads_for_tests() == 0).await;
        until(|| f.service.0.state.lock().unwrap().reservation.is_none()).await;
        assert!(!probe.alive() && !probe.lock_busy());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(probe.spawned.load(std::sync::atomic::Ordering::Acquire), 1);
        assert_eq!(lifecycle.public_status().await.state, "failed");
        assert_eq!(probe.spawned.load(std::sync::atomic::Ordering::Acquire), 1);
        f.empty();
    }
    #[tokio::test]
    async fn status_read_registers_original_identity_before_pending_and_reaps_without_second_probe()
    {
        retained_status_child_contract(false, false).await;
    }
    #[tokio::test]
    async fn disconnected_status_observer_retains_exact_real_child_read_and_radio_reservation() {
        retained_status_child_contract(false, true).await;
    }
    #[tokio::test]
    async fn retry_read_observer_returns_pending_without_releasing_real_radio_owner() {
        retained_status_child_contract(true, false).await;
    }

    #[tokio::test]
    async fn recovery_reboot_binding_rejects_usb_replacement_before_physical_dispatch() {
        let mut fixture = Fixture::new();
        if let Some(task) = fixture.service.0.task.lock().unwrap().take() { task.abort(); }
        fixture.configure().await;
        let binding = fixture.service.public_mutation_binding();
        crate::usb_at::reconnect_tests::install_default(&fixture.service.0.usb, "replacement");
        for direct in [false, true] {
            let failure = if direct {
                fixture.service.reboot_module(&binding).await
            } else {
                fixture.service.recover_network_reboot(&binding).await
            }.unwrap_err();
            assert_eq!(failure.code, "radio_stale_generation");
            assert!(!failure.command_submitted());
            fixture.empty();
        }
        assert_eq!(fixture.service.0.state.lock().unwrap().jobs, 0);
    }

    // The oneshot can be lost after the actor owns a physical reboot. The
    // receipt must remain unknown for the durable network recovery ledger.
    pub(crate) fn real_lost_reboot_receipt() -> Failure {
        use std::future::{Future, poll_fn};
        use std::task::Poll;
        let first = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2).enable_all().build().unwrap();
        let mut fixture = { let _entered = first.enter(); Fixture::new() };
        if let Some(task) = fixture.service.0.task.lock().unwrap().take() { task.abort(); }
        first.block_on(fixture.configure());
        fixture.preferences(Preferences { enabled: false, restore_cfun: 1, user_configured: true });
        fixture.service.0.state.lock().unwrap().observed = observed(1);
        let service = fixture.service.clone();
        let binding = service.public_mutation_binding();
        let mut observer = Box::pin(async move { service.reboot_module(&binding).await });
        let physical_reply = first.block_on(async {
            assert!(poll_fn(|cx| Poll::Ready(observer.as_mut().poll(cx).is_pending())).await);
            assert!(matches!(fixture.answer(1).await, Action::Observe { cellular: false }));
            let (action, reply) = fixture.next().await;
            assert!(matches!(action, Action::Reboot));
            reply
        });
        drop(first);
        assert!(physical_reply.is_closed());
        let second = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2).enable_all().build().unwrap();
        let failure = second.block_on(observer).unwrap_err();
        assert_eq!(failure.code, "backend_stopping");
        assert!(fixture.service.0.state.lock().unwrap().reservation.is_some());
        failure
    }

    #[test]
    fn reboot_actor_loss_keeps_unknown_submission_receipt() {
        let failure = real_lost_reboot_receipt();
        assert!(failure.command_submitted(), "admitted reboot reply loss must retain unknown submission");
        assert!(failure.uncertain);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn retired_physical_reboot_keeps_submission_receipt() {
        let mut f = Fixture::new();
        if let Some(task) = f.service.0.task.lock().unwrap().take() { task.abort(); }
        f.configure().await;
        f.preferences(Preferences { enabled: false, restore_cfun: 1, user_configured: true });
        f.service.0.state.lock().unwrap().observed = observed(1);
        let first = f.begin(Job::PublicReboot);
        assert!(matches!(f.answer(1).await, Action::Observe { cellular: false }));
        let (action, reply) = f.next().await;
        assert!(matches!(action, Action::Reboot));
        let service = f.service.clone();
        let retirement = tokio::spawn(async move { service.retire_producer("producer-2".into()).await });
        until(|| f.service.0.state.lock().unwrap().retiring).await;
        reply.send(Ok(Outcome {
            complete: true, submitted: true, verified: true,
            response: "OK".into(), ..Outcome::default()
        })).unwrap();
        let failure = first.await.unwrap().unwrap_err();
        retirement.await.unwrap().unwrap();
        assert_eq!(failure.code, "radio_stale_generation");
        assert!(failure.command_submitted(), "physical reboot receipt must survive action fence");
        assert!(failure.uncertain);
    }

    #[tokio::test]
    async fn final_radio_terminal_receipt_rejects_late_shared_voice_cleanup_admission() {
        let mut f = Fixture::new();
        let original = f.service.module_recovery().admit_operation("producer-1").unwrap();
        let right = original.right();
        assert!(f.service.finish_shutdown_receipt().await.unwrap().is_empty());
        // The compatibility voice-status path calls this exact cleanup=true
        // admission before USB capture or ADB selection. No device is opened.
        let late = f.service.borrow_shared_voice_operation("producer-1", true);
        assert!(matches!(late, Err(ref error) if error.code == "backend_stopping"),
            "a final physical receipt must not reopen shared voice jobs/pins");
        assert!(matches!(f.service.borrow_shared_voice_settlement("producer-1", &right),
            Err(ref error) if error.code == "backend_stopping"));
        assert!(matches!(f.service.borrow_shared_voice_shutdown("producer-1").await,
            Err(ref error) if error.code == "backend_stopping"));
        assert!(matches!(f.service.borrow_shared_voice_shutdown_settlement("producer-1", &right).await,
            Err(ref error) if error.code == "backend_stopping"));
        assert!(matches!(f.service.submit(Job::Query(producer())).await,
            Err(ref error) if error.code == "backend_stopping"));
        let state = f.service.0.state.lock().unwrap();
        assert_eq!((state.jobs, state.active_shared_voice_pins), (0, 0));
        drop(state);
        f.empty();
    }

    #[tokio::test]
    async fn final_radio_terminal_seal_waits_for_admitted_cleanup_across_observer_join() {
        let mut f = Fixture::new();
        let original = f.service.module_recovery().admit_operation("producer-1").unwrap();
        let (release_observer, observer) = oneshot::channel::<()>();
        *f.service.0.call_observation_task.lock().unwrap() = Some(tokio::spawn(async move {
            observer.await.unwrap();
        }));
        let service = f.service.clone();
        let mut close = tokio::spawn(async move { service.finish_shutdown_receipt().await });
        until(|| f.service.0.shutdown_receipt.lock().unwrap().is_some()).await;
        // The first wait_idle is zero, but observer join is still pending. A
        // genuine retained settlement may arrive here and must remain owned.
        let pin = f.service.borrow_shared_voice_settlement("producer-1", &original.right()).unwrap();
        assert_eq!(f.service.0.state.lock().unwrap().jobs, 1);
        release_observer.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(30), &mut close).await.is_err());
        assert!(!f.service.0.state.lock().unwrap().finalized);
        drop(pin);
        assert!(close.await.unwrap().unwrap().is_empty());
        assert!(f.service.0.state.lock().unwrap().finalized);
        assert!(matches!(f.service.borrow_shared_voice_settlement("producer-1", &original.right()),
            Err(ref error) if error.code == "backend_stopping"));
        f.empty();
    }

    #[tokio::test]
    async fn cancelled_final_radio_observer_keeps_original_pin_until_real_retirement_and_seal() {
        let mut f = Fixture::new();
        let pin = f.service.borrow_shared_voice_operation("producer-1", false).unwrap();
        let service = f.service.clone();
        let close = tokio::spawn(async move { service.finish_shutdown_receipt().await });
        until(|| f.service.0.shutdown_receipt.lock().unwrap().is_some()).await;
        close.abort();
        assert!(close.await.unwrap_err().is_cancelled());
        assert_eq!(f.service.0.state.lock().unwrap().jobs, 1);
        assert!(!f.service.0.state.lock().unwrap().finalized);
        assert!(matches!(f.service.borrow_shared_voice_operation("producer-1", false),
            Err(ref error) if error.code == "backend_stopping"));
        drop(pin);
        // finish_shutdown_receipt retains its actual finalizer despite caller
        // cancellation; a later observer gets the same eventual terminal seal.
        assert!(f.service.finish_shutdown_receipt().await.unwrap().is_empty());
        assert!(f.service.0.state.lock().unwrap().finalized);
        assert_eq!(f.service.0.state.lock().unwrap().jobs, 0);
        f.empty();
    }

    #[tokio::test]
    async fn terminal_completed_cleanup_ack_is_exact_memory_only_even_when_slots_are_full() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences { enabled: false, restore_cfun: 1, user_configured: true });
        f.service.submit(intent("prepare", 1)).await.unwrap();
        let original = f.service.submit(intent("restore", 1)).await.unwrap();
        f.service.submit(reservation("acquire", "completed-reservation", 1)).await.unwrap();
        let released = f.service.submit(reservation("release", "completed-reservation", 1)).await.unwrap();
        assert!(f.service.finish_shutdown_receipt().await.unwrap().is_empty());
        let slots = f.service.0.slots.clone().acquire_many_owned(8).await.unwrap();
        // A generic new job cannot get a permit. Corrupt preferences also make
        // any accidental disk refresh observable; cached ACKs need neither.
        std::fs::write(f.root.join("module-flight-mode.json"), b"invalid-json").unwrap();
        assert_eq!(f.service.submit(intent("restore", 1)).await.unwrap(), original);
        assert_eq!(f.service.submit(reservation("release", "completed-reservation", 1)).await.unwrap(), released);
        for wrong in [intent("restore", 2), intent("observe", 1), intent("enter", 1), Job::Query(producer()),
            reservation("release", "completed-reservation", 2), reservation("release", "other-token", 1)] {
            assert_eq!(f.service.submit(wrong).await.unwrap_err().code, "backend_stopping");
        }
        let state = f.service.0.state.lock().unwrap();
        assert!(state.finalized && !state.preference_error);
        assert_eq!((state.jobs, state.active_shared_voice_pins), (0, 0));
        assert_eq!((state.intent_water.sequence, state.intent_water.token.as_str()), (1, "intent-1"));
        let water = state.waters.get(&Purpose::Diagnostic).unwrap();
        assert_eq!((water.sequence, water.token.as_str()), (1, "completed-reservation"));
        drop(state);
        drop(slots);
        f.empty();
    }

    #[tokio::test]
    async fn terminal_native_completed_ack_still_checks_original_epoch_and_binding() {
        let mut f = Fixture::new();
        f.configure().await;
        f.preferences(Preferences { enabled: false, restore_cfun: 1, user_configured: true });
        let captured = f.service.native_vowifi_intent().unwrap();
        captured.prepare().await.unwrap();
        captured.restore().await.unwrap();
        assert!(f.service.finish_shutdown_receipt().await.unwrap().is_empty());
        let slots = f.service.0.slots.clone().acquire_many_owned(8).await.unwrap();
        captured.restore().await.unwrap();
        assert_eq!(f.service.0.state.lock().unwrap().jobs, 0);
        let binding = f.service.0.state.lock().unwrap().binding.take();
        assert_eq!(captured.restore().await.unwrap_err().code, "radio_stale_generation");
        f.service.0.state.lock().unwrap().binding = binding;
        f.service.0.state.lock().unwrap().epoch += 1;
        assert_eq!(captured.restore().await.unwrap_err().code, "radio_stale_generation");
        assert_eq!(f.service.0.state.lock().unwrap().jobs, 0);
        drop(slots);
        f.empty();
    }
}
