use super::*;
use http_body_util::BodyExt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

struct Fake {
    binding: ActiveBinding,
    current: AtomicU64,
    session_current: AtomicBool,
    resolve_calls: AtomicU64,
}
impl Service for Fake {
    fn resolve(
        &self,
        generation: u64,
        _: bool,
    ) -> BoxFuture<'_, Result<ActiveBinding, ApiFailure>> {
        Box::pin(async move {
            self.resolve_calls.fetch_add(1, Ordering::SeqCst);
            if !self.session_current.load(Ordering::SeqCst)
                || self.current.load(Ordering::SeqCst) == 0
            {
                return Err(ApiFailure::new("读卡器 VoWiFi 未连接"));
            }
            if generation != 0 && generation != self.current.load(Ordering::SeqCst) {
                return Err(ApiFailure::new("eSIM Profile 已切换，请刷新后重试"));
            }
            Ok(self.binding.clone())
        })
    }
    fn revalidate<'a>(
        &'a self,
        binding: &'a ActiveBinding,
    ) -> BoxFuture<'a, Result<(), ApiFailure>> {
        Box::pin(async move {
            if self.current.load(Ordering::SeqCst) != binding.reader_generation
                || !self.session_current.load(Ordering::SeqCst)
            {
                Err(ApiFailure::new("old binding"))
            } else {
                Ok(())
            }
        })
    }
    fn history_error(&self) -> Option<String> {
        Some("archive pending".into())
    }
}
fn fixture() -> (Arc<Fake>, mpsc::Receiver<registered::Control>) {
    let (controls, input) = registered::controls();
    (
        Arc::new(Fake {
            binding: ActiveBinding {
                reader_generation: 91,
                session_generation: SessionGeneration::from_value(7).unwrap(),
                source: "reader".into(),
                controls,
            },
            current: AtomicU64::new(91),
            session_current: AtomicBool::new(true),
            resolve_calls: AtomicU64::new(0),
        }),
        input,
    )
}

struct CountedBody {
    bytes: Option<Bytes>,
    reads: Arc<AtomicU64>,
}

struct StalledBody;
impl hyper::body::Body for StalledBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        std::task::Poll::Pending
    }
}

#[tokio::test]
async fn stalled_call_ingress_expires_before_resolving_or_dispatching_controls() {
    let (service, mut controls) = fixture();
    let request = Request::builder()
        .method(hyper::Method::POST)
        .uri("/api/reader/vowifi/calls/dial")
        .body(StalledBody)
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(7), handle(request, service.as_ref()))
        .await
        .expect("ingress must expire without a completed body");
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(service.resolve_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        controls.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn call_status_does_not_consume_a_stalled_get_body() {
    let (service, mut controls) = fixture();
    let worker = tokio::spawn(async move {
        let registered::Control::Call { command, reply, .. } = controls.recv().await.unwrap()
        else {
            panic!("expected status control");
        };
        assert!(matches!(command, registered::CallCommand::Snapshot));
        assert!(
            reply
                .send(Ok(native_ims_calls::Snapshot::default()))
                .is_ok()
        );
    });
    let request = Request::builder()
        .uri("/api/reader/vowifi/calls/status")
        .body(StalledBody)
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(1), handle(request, service.as_ref()))
        .await
        .expect("status must not collect a GET body");
    assert_eq!(response.status(), StatusCode::OK);
    worker.await.unwrap();
}
impl hyper::body::Body for CountedBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        std::task::Poll::Ready(self.bytes.take().map(|bytes| {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(hyper::body::Frame::data(bytes))
        }))
    }
}

#[test]
fn exact_call_route_ownership_is_identical_for_both_active_session_aliases() {
    for prefix in ["/api/reader/vowifi/calls/", "/api/vowifi/calls/"] {
        for action in ["status", "dial", "answer", "reject", "hangup", "dtmf"] {
            let method = if action == "status" {
                hyper::Method::GET
            } else {
                hyper::Method::POST
            };
            let path = format!("{prefix}{action}");
            assert_eq!(
                call_route_owner(&method, &path, false, false),
                Some(CallRouteOwner::LegacyActiveSession)
            );
            assert_eq!(
                call_route_owner(&method, &path, true, false),
                Some(CallRouteOwner::NativeIntent)
            );
            assert_eq!(
                call_route_owner(&method, &path, true, true),
                Some(CallRouteOwner::AmbiguousOwners)
            );
            assert_eq!(
                call_route_owner(&method, &path, false, true),
                Some(CallRouteOwner::LegacyActiveSession)
            );
            let wrong = if action == "status" {
                hyper::Method::POST
            } else {
                hyper::Method::GET
            };
            assert_eq!(call_route_owner(&wrong, &path, true, false), None);
        }
        for suffix in ["dial/", "dial/extra", "STATUS", "status/", "", "unknown"] {
            assert_eq!(
                call_route_owner(
                    &hyper::Method::POST,
                    &format!("{prefix}{suffix}"),
                    true,
                    false
                ),
                None
            );
        }
    }
    for path in [
        "/api/call/dial",
        "/api/call/status",
        "/api/module/vowifi",
        "/api/reader/vowifi/ussd",
    ] {
        assert_eq!(
            call_route_owner(&hyper::Method::POST, path, true, false),
            None
        );
    }
}

#[tokio::test]
async fn no_native_intent_returns_module_capable_legacy_request_without_reading_it() {
    let (service, mut input) = fixture();
    for prefix in ["/api/reader/vowifi/calls/", "/api/vowifi/calls/"] {
        let reads = Arc::new(AtomicU64::new(0));
        let body = Bytes::from_static(br#"{"peer":"123","generation":91,"source":"module","device":"module-vowifi"} {"peer":"456"}"#);
        let request = Request::builder()
            .method("POST")
            .uri(format!("{prefix}dial?device=reader"))
            .header("X-Test", "original")
            .body(CountedBody {
                bytes: Some(body.clone()),
                reads: reads.clone(),
            })
            .unwrap();
        let request = match handle_call_if_owned(request, service.as_ref(), false, true).await {
            Err(request) => request,
            Ok(_) => panic!("native service stole the legacy module-capable active session"),
        };
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(request.uri().query(), Some("device=reader"));
        assert_eq!(request.headers()["X-Test"], "original");
        assert_eq!(
            request.into_body().collect().await.unwrap().to_bytes(),
            body
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }
    assert_eq!(service.resolve_calls.load(Ordering::SeqCst), 0);
    assert!(input.try_recv().is_err());
}

#[tokio::test]
async fn conflicting_module_claim_rejects_both_aliases_without_body_or_control_consumption() {
    let (service, mut input) = fixture();
    for prefix in ["/api/reader/vowifi/calls/", "/api/vowifi/calls/"] {
        for (method, action) in [("GET", "status"), ("POST", "dial"), ("POST", "hangup")] {
            let reads = Arc::new(AtomicU64::new(0));
            let request = Request::builder()
                .method(method)
                .uri(format!("{prefix}{action}"))
                .body(CountedBody {
                    bytes: Some(Bytes::from_static(b"must not read even malformed input")),
                    reads: reads.clone(),
                })
                .unwrap();
            let response = match handle_call_if_owned(request, service.as_ref(), true, true).await {
                Ok(response) => response,
                Err(_) => panic!("ambiguous ownership forwarded a request to Go"),
            };
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                value(response).await["error"],
                "VoWiFi 通话会话归属冲突，请等待资源释放"
            );
            assert_eq!(reads.load(Ordering::SeqCst), 0);
        }
    }
    assert_eq!(service.resolve_calls.load(Ordering::SeqCst), 0);
    assert!(input.try_recv().is_err());
}
async fn value(response: Response<Full<Bytes>>) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
fn ussd_result() -> native_ims_calls::ussd::UssdResult {
    native_ims_calls::ussd::UssdResult {
        session_id: "dialog-1".into(),
        text: "menu".into(),
        language: String::new(),
        needs_input: true,
        complete: false,
    }
}

#[test]
fn legacy_decoder_preserves_first_object_case_duplicates_null_and_partial_type_errors() {
    let call=decode(br#"{"Generation":90,"GENERATION":91,"generation":null,"Peer":"first","peer":"last","unknown":{},"peer":null} []"#,false);
    assert!(!call.invalid);
    assert_eq!(call.generation, 91);
    assert_eq!(call.peer, "last");
    let call = decode(br#"{"generation":91,"peer":42,"digit":"1"}"#, false);
    assert!(call.invalid);
    assert_eq!(call.generation, 91);
    assert_eq!(call.digit, "1");
    assert!(decode(br#"{"generation":1.0}"#, false).invalid);
    assert!(decode(br#"{"generation":-1}"#, false).invalid);
    assert!(decode(br#"{"generation":18446744073709551616}"#, false).invalid);
    assert!(!decode(b"null", true).invalid);
    assert!(decode(br#"{"command":"*1#","extra":0}"#, true).invalid);
    assert!(
        !decode(
            br#"{"Command":"*1#","cancel":false,"CANCEL":true,"cancel":null} {}"#,
            true
        )
        .invalid
    );
    assert_eq!(
        decode(
            br#"{"command":"*1#","cancel":false,"CANCEL":true,"cancel":null}"#,
            true
        )
        .cancel,
        true
    );
    assert_eq!(
        decode(br#"{"\u017fe\u017f\u017fion_id":"dialog"}"#, true).session_id,
        "dialog"
    );
}

#[tokio::test]
async fn call_aliases_keep_physical_generation_status_and_legacy_projection() {
    for prefix in ["/api/reader/vowifi/calls/", "/api/vowifi/calls/"] {
        for (action, expected) in [
            ("status", 200),
            ("dial", 202),
            ("answer", 200),
            ("reject", 200),
            ("hangup", 200),
            ("dtmf", 200),
        ] {
            let (service, mut input) = fixture();
            let peer = tokio::spawn(async move {
                let registered::Control::Call {
                    generation,
                    command,
                    reply,
                } = input.recv().await.unwrap()
                else {
                    panic!("typed call")
                };
                assert_eq!(generation.value(), 7);
                match (action, command) {
                    ("dial", registered::CallCommand::Dial(peer)) => assert_eq!(peer, "123"),
                    ("dtmf", registered::CallCommand::Dtmf(digit)) => assert_eq!(digit, '#'),
                    ("status", registered::CallCommand::Snapshot)
                    | ("answer", registered::CallCommand::Answer)
                    | ("reject", registered::CallCommand::Reject)
                    | ("hangup", registered::CallCommand::Hangup) => {}
                    _ => panic!("wrong command"),
                }
                reply
                    .send(Ok(native_ims_calls::Snapshot {
                        state: "active".into(),
                        peer: "123".into(),
                        ..Default::default()
                    }))
                    .ok()
                    .unwrap();
            });
            let response = handle_bytes(
                &format!("{prefix}{action}"),
                br##"{"peer":"123","digit":"#","generation":91}"##,
                service.as_ref(),
            )
            .await;
            assert_eq!(response.status().as_u16(), expected);
            let body = value(response).await;
            assert_eq!(body["generation"], 91);
            assert_eq!(body["source"], "reader");
            assert_eq!(body["history_error"], "archive pending");
            assert_eq!(body["call"]["peer"], "123");
            assert!(body["call"].get("uncertain").is_none());
            assert!(body["call"].get("audio_token").is_none());
            assert!(body["call"].get("audio_port").is_none());
            assert!(body["call"].get("direction").is_none());
            assert_eq!(body["call"]["rtp_packets_sent"], 0);
            peer.await.unwrap();
        }
    }
}

#[tokio::test]
async fn vowifi_dial_uses_the_shared_rust_number_canonicalizer() {
    let (service, mut input) = fixture();
    let peer = tokio::spawn(async move {
        let registered::Control::Call { command, reply, .. } = input.recv().await.unwrap() else {
            panic!("typed call")
        };
        let registered::CallCommand::Dial(peer) = command else {
            panic!("dial command")
        };
        assert_eq!(peer, "+442079460000");
        assert!(
            reply
                .send(Ok(native_ims_calls::Snapshot::default()))
                .is_ok()
        );
    });
    let response = handle_bytes(
        "/api/vowifi/calls/dial",
        br#"{"peer":" 00 44 (20) 7946-0000 ","generation":91}"#,
        service.as_ref(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    peer.await.unwrap();
}

#[tokio::test]
async fn validation_codes_and_unavailable_status_do_not_queue_any_control() {
    let (service, mut input) = fixture();
    for (path, body, message) in [
        ("/api/vowifi/calls/dial", &b"{}"[..], "请输入有效的被叫号码"),
        (
            "/api/vowifi/calls/dtmf",
            &br#"{"digit":"A"}"#[..],
            "按键只能是 0-9、* 或 #",
        ),
        (
            "/api/reader/vowifi/ussd",
            &br#"{"extra":1}"#[..],
            "无效 USSD 请求",
        ),
    ] {
        let response = handle_bytes(path, body, service.as_ref()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(value(response).await, json!({"error":message}));
    }
    assert_eq!(service.resolve_calls.load(Ordering::SeqCst), 0);
    let response = handle_bytes(
        "/api/vowifi/calls/dial",
        br#"{"peer":"123","generation":7}"#,
        service.as_ref(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    service.current.store(0, Ordering::SeqCst);
    let response = handle_bytes("/api/vowifi/calls/status", b"", service.as_ref()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = value(response).await;
    assert_eq!(body["generation"], 0);
    assert_eq!(body["call"]["state"], "unavailable");
    assert_eq!(body["call"]["signaling_ready"], false);
    assert!(body.get("source").is_none());
    assert!(input.try_recv().is_err());
}

#[tokio::test]
async fn action_failures_keep_go_status_mapping_and_never_retry_a_write() {
    for (action, status) in [
        ("dial", 502),
        ("answer", 409),
        ("reject", 409),
        ("hangup", 502),
        ("dtmf", 502),
    ] {
        let (service, mut input) = fixture();
        let worker = tokio::spawn(async move {
            let registered::Control::Call { reply, .. } = input.recv().await.unwrap() else {
                panic!("call")
            };
            reply
                .send(Err(registered::Failure {
                    code: "ims_call_write_unconfirmed",
                    uncertain: true,
                    sip_status: None,
                }))
                .ok()
                .unwrap();
            input
        });
        let response = handle_bytes(
            &format!("/api/vowifi/calls/{action}"),
            br##"{"peer":"123","digit":"#","generation":91}"##,
            service.as_ref(),
        )
        .await;
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(value(response).await["error"], "ims_call_write_unconfirmed");
        assert!(worker.await.unwrap().try_recv().is_err());
    }
}

#[tokio::test]
async fn call_success_from_a_replaced_binding_does_not_expose_old_peer_or_audio_token() {
    for physical_change in [true, false] {
        let (service, mut input) = fixture();
        let worker_service = service.clone();
        let worker = tokio::spawn(async move {
            let registered::Control::Call { reply, .. } = input.recv().await.unwrap() else {
                panic!("call")
            };
            if physical_change {
                worker_service.current.store(92, Ordering::SeqCst);
            } else {
                worker_service
                    .session_current
                    .store(false, Ordering::SeqCst);
            }
            reply
                .send(Ok(native_ims_calls::Snapshot {
                    peer: "old-secret".into(),
                    audio_token: "old-token".into(),
                    ..Default::default()
                }))
                .ok()
                .unwrap();
        });
        let response = handle_bytes(
            "/api/vowifi/calls/answer",
            br#"{"generation":91,"peer":42}"#,
            service.as_ref(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = value(response).await.to_string();
        assert!(!body.contains("old-secret"));
        assert!(!body.contains("old-token"));
        worker.await.unwrap();
    }
}

#[tokio::test]
async fn ussd_pending_does_not_block_owner_call_status_and_checks_exact_binding_before_reply() {
    for stale in [false, true] {
        let (service, mut input) = fixture();
        let api_service = service.clone();
        let request = tokio::spawn(async move {
            handle_bytes(
                "/api/reader/vowifi/ussd",
                br#"{"command":"*123#","generation":91}"#,
                api_service.as_ref(),
            )
            .await
        });
        let registered::Control::Ussd {
            generation,
            command,
            reply,
        } = input.recv().await.unwrap()
        else {
            panic!("USSD")
        };
        assert_eq!(generation.value(), 7);
        assert!(matches!(command,registered::UssdCommand::Start(ref s) if s=="*123#"));
        let (done, receiver) = oneshot::channel();
        reply
            .send(Ok(registered::UssdReply::Pending(
                native_ims_calls::ussd::Pending {
                    session_id: "dialog-1".into(),
                    receiver,
                },
            )))
            .ok()
            .unwrap();
        let call_service = service.clone();
        let status = tokio::spawn(async move {
            handle_bytes("/api/vowifi/calls/status", b"", call_service.as_ref()).await
        });
        let registered::Control::Call { reply, .. } = input.recv().await.unwrap() else {
            panic!("owner is free while Pending waits")
        };
        reply.send(Ok(Default::default())).ok().unwrap();
        assert_eq!(status.await.unwrap().status(), StatusCode::OK);
        assert!(!request.is_finished());
        if stale {
            service.current.store(92, Ordering::SeqCst);
        }
        done.send(Ok(ussd_result())).ok().unwrap();
        let response = request.await.unwrap();
        if stale {
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(
                value(response).await,
                json!({"error":"号卡已变更，已丢弃旧 USSD 响应"})
            );
        } else {
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                value(response).await,
                json!({"generation":91,"result":{"session_id":"dialog-1","text":"menu","needs_input":true,"complete":false}})
            );
        }
    }
}

#[tokio::test]
async fn ussd_continue_cancel_and_invalid_combination_preserve_legacy_contract() {
    for cancelling in [false, true] {
        let (service, mut input) = fixture();
        let worker = tokio::spawn(async move {
            let registered::Control::Ussd { command, reply, .. } = input.recv().await.unwrap()
            else {
                panic!("USSD")
            };
            match command {
                registered::UssdCommand::Cancel { session_id } if cancelling => {
                    assert_eq!(session_id, "dialog-1")
                }
                registered::UssdCommand::Continue { session_id, input } if !cancelling => {
                    assert_eq!(session_id, "dialog-1");
                    assert_eq!(input, "1");
                }
                _ => panic!("wrong USSD command"),
            }
            reply
                .send(Ok(registered::UssdReply::Complete(ussd_result())))
                .ok()
                .unwrap();
        });
        let response=handle_bytes("/api/reader/vowifi/ussd",format!("{{\"session_id\":\"dialog-1\",\"input\":\"1\",\"cancel\":{cancelling},\"generation\":91}}").as_bytes(),service.as_ref()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = value(response).await;
        assert_eq!(body["generation"], 91);
        if cancelling {
            assert_eq!(
                body["result"],
                json!({"session_id":"dialog-1","text":"","needs_input":false,"complete":true})
            );
        }
        worker.await.unwrap();
    }
    let (service, mut input) = fixture();
    for (body, message) in [
        (br#"{"cancel":true}"#.as_slice(), "缺少 USSD 会话"),
        (
            br#"{"session_id":"s","command":"*1#"}"#.as_slice(),
            "交互请求不能重复提交短码",
        ),
    ] {
        let response = handle_bytes("/api/reader/vowifi/ussd", body, service.as_ref()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(value(response).await["error"], message);
    }
    assert!(input.try_recv().is_err());
}

#[tokio::test]
async fn cancelling_http_pending_wait_does_not_cancel_or_replay_accepted_ussd() {
    let (service, mut input) = fixture();
    let api_service = service.clone();
    let request = tokio::spawn(async move {
        handle_bytes(
            "/api/reader/vowifi/ussd",
            br#"{"command":"*1#"}"#,
            api_service.as_ref(),
        )
        .await
    });
    let registered::Control::Ussd { reply, .. } = input.recv().await.unwrap() else {
        panic!("USSD")
    };
    let (done, receiver) = oneshot::channel();
    reply
        .send(Ok(registered::UssdReply::Pending(
            native_ims_calls::ussd::Pending {
                session_id: "dialog-1".into(),
                receiver,
            },
        )))
        .ok()
        .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert!(done.send(Ok(ussd_result())).is_err());
    assert!(input.try_recv().is_err());
    assert!(!service.binding.controls.is_closed());
}

#[tokio::test]
async fn http_body_limit_precedes_mutation_but_status_does_not_consume_a_body() {
    let (service, mut input) = fixture();
    for path in ["/api/vowifi/calls/dial", "/api/reader/vowifi/ussd"] {
        let request = Request::builder()
            .uri(path)
            .body(Full::new(Bytes::from(vec![b' '; 4097])))
            .unwrap();
        assert_eq!(
            handle(request, service.as_ref()).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(service.resolve_calls.load(Ordering::SeqCst), 0);
    assert!(input.try_recv().is_err());
    service.current.store(0, Ordering::SeqCst);
    let request = Request::builder()
        .uri("/api/vowifi/calls/status")
        .body(Full::new(Bytes::from(vec![b' '; 8192])))
        .unwrap();
    assert_eq!(
        handle(request, service.as_ref()).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn cancelling_a_request_blocked_on_the_control_queue_never_admits_its_command() {
    let (service, mut input) = fixture();
    for _ in 0..service.binding.controls.capacity() {
        let (reply, _) = oneshot::channel();
        service
            .binding
            .controls
            .try_send(registered::Control::Call {
                generation: service.binding.session_generation,
                command: registered::CallCommand::Snapshot,
                reply,
            })
            .ok()
            .unwrap();
    }
    let count = input.len();
    let waiter_service = service.clone();
    let request = tokio::spawn(async move {
        handle_bytes(
            "/api/vowifi/calls/dial",
            br#"{"peer":"123","generation":91}"#,
            waiter_service.as_ref(),
        )
        .await
    });
    while service.resolve_calls.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    assert!(!request.is_finished());
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    for _ in 0..count {
        let registered::Control::Call {
            command: registered::CallCommand::Snapshot,
            ..
        } = input.recv().await.unwrap()
        else {
            panic!("cancelled dial was admitted")
        };
    }
    assert!(input.try_recv().is_err());
}

#[tokio::test]
async fn cancelled_accepted_call_waiter_keeps_dispatcher_alive_without_replay_or_stop() {
    let (service, mut input) = fixture();
    let waiter_service = service.clone();
    let request = tokio::spawn(async move {
        handle_bytes(
            "/api/vowifi/calls/dial",
            br#"{"peer":"123","generation":91}"#,
            waiter_service.as_ref(),
        )
        .await
    });
    let registered::Control::Call { reply, .. } = input.recv().await.unwrap() else {
        panic!("typed dial")
    };
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert!(reply.send(Ok(Default::default())).is_err());
    assert!(input.try_recv().is_err());
    assert!(!service.binding.controls.is_closed());
}

#[tokio::test]
async fn closed_dispatcher_is_unavailable_and_ussd_failure_is_not_retried() {
    let (service, input) = fixture();
    drop(input);
    let response = handle_bytes("/api/vowifi/calls/status", b"", service.as_ref()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(value(response).await["call"]["state"], "unavailable");
    let response = handle_bytes(
        "/api/reader/vowifi/ussd",
        br#"{"command":"*1#"}"#,
        service.as_ref(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(value(response).await["error"], "native_session_stopped");
}

/// USSD must remain signaling-only even when HTTP and the same actor also own
/// call controls. Every unexpected RTP/PCM allocation is an immediate failure.
struct NoMedia;
impl native_ims_calls::Media for NoMedia {
    fn allocate(
        self: Arc<Self>,
        _: native_ims_calls::CallBinding,
    ) -> native_ims_calls::MediaFuture<native_ims_calls::LocalMedia> {
        panic!("USSD allocated voice media")
    }
    fn configure(
        self: Arc<Self>,
        _: native_ims_calls::CallBinding,
        _: proletarius_call::sdp::AudioNegotiation,
    ) -> native_ims_calls::MediaFuture<()> {
        panic!("USSD configured voice media")
    }
    fn start(
        self: Arc<Self>,
        _: native_ims_calls::CallBinding,
        _: bool,
    ) -> native_ims_calls::MediaFuture<()> {
        panic!("USSD started voice media")
    }
    fn stop(
        self: Arc<Self>,
        _: native_ims_calls::CallBinding,
    ) -> native_ims_calls::MediaFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn dtmf(
        self: Arc<Self>,
        _: native_ims_calls::CallBinding,
        _: char,
    ) -> native_ims_calls::MediaFuture<()> {
        panic!("USSD sent DTMF")
    }
}

#[tokio::test]
async fn actual_registered_single_actor_services_http_ussd_pending_and_call_status_then_drains() {
    real_registered_ussd_exchange(None, None).await;
}

#[tokio::test]
async fn actual_registered_ussd_invite_rejection_preserves_sip_403_in_http_failure() {
    real_registered_ussd_exchange(Some(403), None).await;
}

#[tokio::test]
async fn actual_registered_ussd_info_rejection_preserves_sip_481_in_http_failure() {
    real_registered_ussd_exchange(None, Some(481)).await;
}

async fn real_registered_ussd_exchange(invite_rejected: Option<u16>, input_rejected: Option<u16>) {
    real_registered_exchange(invite_rejected, input_rejected, false).await;
}

#[tokio::test]
async fn actual_registered_call_alias_selection_decodes_once_and_stale_intent_never_falls_back() {
    real_registered_exchange(None, None, true).await;
}

async fn real_registered_exchange(
    invite_rejected: Option<u16>,
    input_rejected: Option<u16>,
    routed: bool,
) {
    tokio::time::timeout(Duration::from_secs(8), async {
        use crate::{
            native_ims_actor, native_sms_consumer, native_sms_originating,
            native_vowifi_supervisor::{NativeVoWiFiSupervisor, ReaderSessionKey},
        };
        use proletarius_sip::{Header, Headers, Message, MessageData, Method, Parser, Uri};
        use tokio::sync::watch;
        use zeroize::Zeroizing;
        let mut owner = NativeVoWiFiSupervisor::default();
        let key = ReaderSessionKey::new("api-real-reader", "8901234567890123456").unwrap();
        let permit = owner.reserve(key.clone()).unwrap();
        owner.mark_running(&permit).unwrap();
        let generation = permit.generation();
        let stop = permit.cancellation_sender();
        let mut registered =
            crate::native_sms_originating::tests::registered_fixture(permit.stop_signal()).await;
        let (commands, command_input, events, event_input) = native_ims_actor::channels();
        let (_lease, lost) = watch::channel(false);
        let reports = Arc::new(
            crate::ims_sms_transactions::NativeReportOwner::new(
                Default::default(),
                "native-api-real-registered-session".into(),
            )
            .unwrap(),
        );
        let outgoing = native_sms_originating::Originating::new(
            reports.clone(),
            native_sms_originating::SessionConfig {
                generation,
                snapshot: &registered.snapshot,
                minimum_cseq: 74,
                smsc: "+447802002606",
                target: "sip:smsc@ims.example",
                access_network_info: None,
            },
            commands.clone(),
            permit.stop_signal(),
            lost.clone(),
        )
        .unwrap();
        let incoming = native_sms_consumer::RegisteredConsumer::new(
            native_sms_consumer::Consumer::new(
                std::path::PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap())
                    .join("tmp/iteration-484-api-real"),
                format!("sub1_{}", "a".repeat(64)),
                generation,
                commands.clone(),
                permit.stop_signal(),
                lost.clone(),
                reports,
            ),
            registered
                .session
                .as_ref()
                .unwrap()
                .message_context("sip:configured@ims.example", &[])
                .unwrap(),
            None,
            74,
        );
        let calls = native_ims_calls::Service::new(
            generation,
            native_ims_calls::Config {
                local_ip: registered.snapshot.local_ip(),
                port_c: registered.snapshot.local_port(),
                contact: "<sip:authorized@10.0.0.2:5064>".into(),
                from_uri: registered.snapshot.routing().identity().into(),
                preferred_identity: registered.snapshot.routing().identity().into(),
                route: registered.snapshot.routing().routes().join(", "),
                service_routes: registered.snapshot.routing().routes().to_vec(),
                encoded_icsi: String::new(),
                phone_context: String::new(),
                home_domain: "ims.example".into(),
                realm: "ims.example".into(),
                user_agent: "api-real-test".into(),
                security_verify: String::new(),
                pani: String::new(),
                early_media_header: false,
                evs_only: false,
                sms_only: false,
                ussd_service_selector: true,
            },
            commands.clone(),
            Arc::new(NoMedia),
        )
        .unwrap();
        let consumers = registered::Consumers::new(incoming, outgoing, calls).unwrap();
        let (controls, control_input) = registered::controls();
        let service = Arc::new(Fake {
            binding: ActiveBinding {
                reader_generation: 91,
                session_generation: generation,
                source: "reader".into(),
                controls,
            },
            current: AtomicU64::new(91),
            session_current: AtomicBool::new(true),
            resolve_calls: AtomicU64::new(0),
        });
        let session = registered.session.take().unwrap();
        let actor = tokio::spawn(async move {
            native_ims_actor::run(
                proletarius_vowifi::sip_session_runtime::RegisteredSipRuntime::new(session),
                &key,
                permit,
                lost,
                command_input,
                events,
            )
            .await
        });
        let (notices, _notice_input) = mpsc::channel(32);
        let consumer = tokio::spawn(consumers.run(event_input, commands, control_input, notices));
        let request_service = service.clone();
        let query = tokio::spawn(async move {
            handle(
                Request::builder()
                    .uri("/api/reader/vowifi/ussd")
                    .body(Full::new(Bytes::from_static(
                        br#"{"command":"*123#","generation":91}"#,
                    )))
                    .unwrap(),
                request_service.as_ref(),
            )
            .await
        });
        let invite = registered.frames.recv().await.unwrap();
        let (Message::Request(invite), _) = Parser::default().parse(&invite, false).unwrap() else {
            panic!("real INVITE")
        };
        assert_eq!(invite.method, Method::INVITE);
        assert_eq!(
            invite.data.headers.get("Route").unwrap().value(),
            "<sip:registered-route@ims.example;lr>"
        );
        assert!(
            invite
                .data
                .headers
                .get("From")
                .unwrap()
                .value()
                .contains("sip:authorized@ims.example")
        );
        assert!(!query.is_finished());
        let status = handle(
            Request::builder()
                .uri("/api/vowifi/calls/status")
                .body(Full::new(Bytes::new()))
                .unwrap(),
            service.as_ref(),
        )
        .await;
        assert_eq!(status.status(), StatusCode::OK);
        assert_eq!(value(status).await["generation"], 91);
        if routed {
            for prefix in ["/api/reader/vowifi/calls/", "/api/vowifi/calls/"] {
                let resolved = service.resolve_calls.load(Ordering::SeqCst);
                let reads = Arc::new(AtomicU64::new(0));
                let request = Request::builder().method("POST").uri(format!("{prefix}dial")).body(CountedBody { bytes: Some(Bytes::from_static(br#"{"peer":"123","generation":91}"#)), reads: reads.clone() }).unwrap();
                let response = match handle_call_if_owned(request, service.as_ref(), true, true).await {
                    Ok(response) => response,
                    Err(_) => panic!("module/native conflict escaped to Go"),
                };
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(reads.load(Ordering::SeqCst), 0);
                assert_eq!(service.resolve_calls.load(Ordering::SeqCst), resolved);
                let reads = Arc::new(AtomicU64::new(0));
                let request = Request::builder().method("GET").uri(format!("{prefix}status")).body(CountedBody { bytes: Some(Bytes::from_static(b"unread status body")), reads: reads.clone() }).unwrap();
                let response = match handle_call_if_owned(request, service.as_ref(), true, false).await {
                    Ok(response) => response,
                    Err(_) => panic!("registered native owner fell through to legacy"),
                };
                assert_eq!(response.status(), StatusCode::OK);
                let response = value(response).await;
                assert_eq!(response["generation"], 91);
                assert_eq!(response["source"], "reader");
                assert_eq!(response["call"]["state"], "idle");
                assert_eq!(reads.load(Ordering::SeqCst), 0);

                // Go ignores source/device: they must not turn this reader
                // binding into a module command or override physical generation.
                let reads = Arc::new(AtomicU64::new(0));
                let request = Request::builder().method("POST").uri(format!("{prefix}answer?source=module")).body(CountedBody { bytes: Some(Bytes::from_static(br#"{"generation":91,"source":"module","device":"module"} {"generation":7}"#)), reads: reads.clone() }).unwrap();
                let response = match handle_call_if_owned(request, service.as_ref(), true, false).await {
                    Ok(response) => response,
                    Err(_) => panic!("known native rejection fell through"),
                };
                assert_eq!(response.status(), StatusCode::CONFLICT);
                assert_eq!(value(response).await["error"], "imscore: no incoming reader call");
                assert_eq!(reads.load(Ordering::SeqCst), 1);
                // Supplying the actual actor generation is still a stale
                // physical reader generation at the HTTP boundary.
                let request = Request::builder().method("POST").uri(format!("{prefix}dial")).body(Full::new(Bytes::from(json!({"peer":"123", "generation":generation.value()}).to_string()))).unwrap();
                let response = match handle_call_if_owned(request, service.as_ref(), true, false).await {
                    Ok(response) => response,
                    Err(_) => panic!("stale physical generation fell through"),
                };
                assert_eq!(response.status(), StatusCode::CONFLICT);

                service.session_current.store(false, Ordering::SeqCst);
                let request = Request::builder().method("GET").uri(format!("{prefix}status")).body(Full::new(Bytes::new())).unwrap();
                let response = match handle_call_if_owned(request, service.as_ref(), true, false).await {
                    Ok(response) => response,
                    Err(_) => panic!("retired native intent exposed replacement Go state"),
                };
                assert_eq!(response.status(), StatusCode::OK);
                let response = value(response).await;
                assert_eq!(response["generation"], 0);
                assert_eq!(response["call"]["state"], "unavailable");
                assert!(response.get("source").is_none());
                service.session_current.store(true, Ordering::SeqCst);
            }
            assert!(registered.frames.try_recv().is_err(), "call route selection emitted unexpected SIP while USSD awaited its response");
        }
        assert!(
            !query.is_finished(),
            "call status must not complete the USSD network waiter"
        );
        let mut headers = Headers::default();
        for name in ["Via", "From", "Call-ID", "CSeq"] {
            headers.push(invite.data.headers.get(name).unwrap().clone());
        }
        headers.push(
            Header::new(
                "To",
                format!(
                    "{};tag=peer",
                    invite.data.headers.get("To").unwrap().value()
                ),
            )
            .unwrap(),
        );
        headers.push(Header::new("Contact", "<sip:network@ims.example>").unwrap());
        registered
            .replies
            .as_ref()
            .unwrap()
            .send(Zeroizing::new(
                Message::Response(proletarius_sip::Response {
                    status: invite_rejected.unwrap_or(200),
                    reason: "OK".into(),
                    data: MessageData::new("SIP/2.0", headers, Vec::new()),
                })
                .encode(),
            ))
            .await
            .unwrap();
        if let Some(status) = invite_rejected {
            let response = query.await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(
                value(response).await,
                json!({"error":format!("运营商拒绝 USSD：SIP {status}")})
            );
            let ack = registered.frames.recv().await.unwrap();
            let (Message::Request(ack), _) = Parser::default().parse(&ack, false).unwrap() else {
                panic!("rejected INVITE ACK")
            };
            assert_eq!(ack.method, Method::ACK);
            assert_eq!(
                ack.data.headers.get("Call-ID").unwrap().value(),
                invite.data.headers.get("Call-ID").unwrap().value()
            );
            stop.send_replace(true);
            assert!(actor.await.unwrap().cleanup.is_ok());
            assert!(consumer.await.unwrap().media_cleanup.is_ok());
            registered.finish().await;
            return;
        }
        let mut headers = Headers::default();
        for (name, value) in [
            ("Via", "SIP/2.0/TCP network;branch=z9hG4bKapi-info".into()),
            (
                "From",
                format!(
                    "{};tag=peer",
                    invite.data.headers.get("To").unwrap().value()
                ),
            ),
            (
                "To",
                invite.data.headers.get("From").unwrap().value().into(),
            ),
            (
                "Call-ID",
                invite.data.headers.get("Call-ID").unwrap().value().into(),
            ),
            ("CSeq", "2 INFO".into()),
            ("Info-Package", "g.3gpp.ussd".into()),
            ("Content-Type", "application/vnd.3gpp.ussd+xml".into()),
        ] {
            headers.push(Header::new(name, value).unwrap());
        }
        registered
            .replies
            .as_ref()
            .unwrap()
            .send(Zeroizing::new(
                Message::Request(proletarius_sip::Request {
                    method: Method::INFO,
                    uri: Uri::parse("sip:authorized@ims.example").unwrap(),
                    data: MessageData::new(
                        "SIP/2.0",
                        headers,
                        proletarius_ims::ussd_xml::encode("Balance?").unwrap(),
                    ),
                })
                .encode(),
            ))
            .await
            .unwrap();
        let response = query.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result = value(response).await;
        assert_eq!(result["generation"], 91);
        assert_eq!(result["result"]["text"], "Balance?");
        assert_eq!(result["result"]["needs_input"], true);
        let id = result["result"]["session_id"].as_str().unwrap();
        assert_eq!(id, invite.data.headers.get("Call-ID").unwrap().value());
        for expected in ["ACK", "200"] {
            let frame = registered.frames.recv().await.unwrap();
            let (message, _) = Parser::default().parse(&frame, false).unwrap();
            match (expected, message) {
                ("ACK", Message::Request(r)) => assert_eq!(r.method, Method::ACK),
                ("200", Message::Response(r)) => assert_eq!(r.status, 200),
                _ => panic!("wrong wire reply"),
            }
        }
        if let Some(status) = input_rejected {
            let request_service = service.clone();
            let request_body =
                Bytes::from(json!({"session_id":id,"input":"1","generation":91}).to_string());
            let continuation = tokio::spawn(async move {
                handle(
                    Request::builder()
                        .uri("/api/reader/vowifi/ussd")
                        .body(Full::new(request_body))
                        .unwrap(),
                    request_service.as_ref(),
                )
                .await
            });
            let info = registered.frames.recv().await.unwrap();
            let (Message::Request(info), _) = Parser::default().parse(&info, false).unwrap() else {
                panic!("actual outgoing INFO")
            };
            assert_eq!(info.method, Method::INFO);
            assert_eq!(info.data.headers.get("Call-ID").unwrap().value(), id);
            let mut headers = Headers::default();
            for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
                headers.push(info.data.headers.get(name).unwrap().clone());
            }
            registered
                .replies
                .as_ref()
                .unwrap()
                .send(Zeroizing::new(
                    Message::Response(proletarius_sip::Response {
                        status,
                        reason: "Fixture".into(),
                        data: MessageData::new("SIP/2.0", headers, Vec::new()),
                    })
                    .encode(),
                ))
                .await
                .unwrap();
            let response = continuation.await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(
                value(response).await,
                json!({"error":format!("USSD 交互被拒绝：SIP {status}")})
            );
        } else {
            let response = handle(
                Request::builder()
                    .uri("/api/reader/vowifi/ussd")
                    .body(Full::new(Bytes::from(
                        json!({"session_id":id,"cancel":true,"generation":91}).to_string(),
                    )))
                    .unwrap(),
                service.as_ref(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(value(response).await["result"]["complete"], true);
        }
        let bye = registered.frames.recv().await.unwrap();
        let (Message::Request(bye), _) = Parser::default().parse(&bye, false).unwrap() else {
            panic!("real BYE")
        };
        assert_eq!(bye.method, Method::BYE);
        assert_eq!(bye.data.headers.get("Call-ID").unwrap().value(), id);
        stop.send_replace(true);
        assert!(actor.await.unwrap().cleanup.is_ok());
        assert!(consumer.await.unwrap().media_cleanup.is_ok());
        registered.finish().await;
    })
    .await
    .unwrap();
}
