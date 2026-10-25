//! Actual task retirement and same-generation admission ownership. These tests
//! use the real Service configuration/QueueStore; no successful join is forged.
use super::*;
use std::future::{Future, poll_fn};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

pub(super) struct AdmissionGate {
    entered: Notify,
    release: Notify,
}
impl AdmissionGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Notify::new(),
        })
    }
    pub(super) async fn wait(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

fn root() -> PathBuf {
    let base = PathBuf::from(std::env::var_os("SWITCH_BUILD_DIR").expect("build directory"))
        .join("runtime/iteration-597-forwarding-retirement");
    fs::create_dir_all(&base).unwrap();
    let mut random = [0; 16];
    getrandom::fill(&mut random).unwrap();
    let path = base.join(hex(&random));
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn configuration(key: u8) -> serde_json::Value {
    serde_json::json!({
        "telegram":{"enabled":false,"bot_token":"fixture-token","chat_id":"fixture-chat","proxy":{"mode":"system","url":""}},
        "bark":{"enabled":false,"server_url":"https://api.day.app","device_key":"fixture-device","icon_url":"","group":"fixture","encrypted":false,"encryption_algorithm":"AES-128-CBC","encryption_key":""},
        "state_key":STANDARD.encode([key;32])
    })
}
fn request(mut value: serde_json::Value) -> Request<Full<Bytes>> {
    static CONFIGURATION_SEQUENCE: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(1);
    let sequence = CONFIGURATION_SEQUENCE.fetch_add(1, Ordering::Relaxed) as u64;
    let object = value.as_object_mut().expect("configuration object");
    object.insert(
        "client_request_id".to_owned(),
        serde_json::json!(format!("retirement-test-{sequence}")),
    );
    object.insert("client_sequence".to_owned(), serde_json::json!(sequence));
    Request::builder()
        .method(Method::POST)
        .uri("/api/sms/forwarding/configure")
        .body(Full::new(Bytes::from(serde_json::to_vec(&value).unwrap())))
        .unwrap()
}
async fn response(response: Response<Full<Bytes>>) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn configured(service: &Service, config: serde_json::Value) -> String {
    let (status, value) = response(service.handle(request(config)).await).await;
    assert_eq!(status, StatusCode::OK);
    value["configuration_id"].as_str().unwrap().to_owned()
}
async fn pending<F: Future>(future: std::pin::Pin<&mut F>) {
    let mut future = future;
    assert!(poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}
async fn replace_runner<F, Fut>(service: &Service, make: F)
where
    F: FnOnce(watch::Receiver<bool>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut state = service.0.state.lock().await;
    let active = state.active.as_mut().unwrap();
    active.stop().await.unwrap();
    let (shutdown, receiver) = watch::channel(false);
    active.shutdown = shutdown;
    active.runner = Some(tokio::spawn(make(receiver)));
    active.retiring = false;
    active.runner_result = None;
    active.stop_result = None;
    active.admitted = true;
}
async fn wait_runner_finished(service: &Service) {
    loop {
        if service
            .0
            .state
            .lock()
            .await
            .active
            .as_ref()
            .unwrap()
            .runner
            .as_ref()
            .unwrap()
            .is_finished()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
}
async fn blocked_runner(service: &Service) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (entered, stopped) = oneshot::channel();
    let (release, wait) = oneshot::channel();
    replace_runner(service, move |mut shutdown| async move {
        shutdown.changed().await.unwrap();
        assert!(*shutdown.borrow());
        entered.send(()).unwrap();
        wait.await.unwrap();
    })
    .await;
    (stopped, release)
}

#[tokio::test]
async fn actual_runner_panic_and_abort_are_sticky_and_reach_shutdown_report() {
    for abort in [false, true] {
        let path = root();
        let service = Service::new(&path);
        let config = configuration(7);
        let id = configured(&service, config.clone()).await;
        if abort {
            replace_runner(&service, |_| std::future::pending()).await;
            service
                .0
                .state
                .lock()
                .await
                .active
                .as_ref()
                .unwrap()
                .runner
                .as_ref()
                .unwrap()
                .abort();
        } else {
            replace_runner(&service, |_| async {
                panic!("intentional runner retirement fixture");
            })
            .await;
        }
        wait_runner_finished(&service).await;
        // Same configuration must observe the actual finished task, not return
        // the old configuration ID as a successful no-op.
        let (status, body) = response(service.handle(request(config.clone())).await).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let expected = if abort {
            "sms_forwarding_runner_aborted"
        } else {
            "sms_forwarding_runner_panicked"
        };
        assert_eq!(body["code"], expected);
        assert_eq!(service.0.state.lock().await.active.as_ref().unwrap().id, id);
        assert_eq!(service.shutdown().await, Err(expected));
        assert_eq!(service.shutdown().await, Err(expected));
        assert_eq!(service.admission_enabled().await, Err(expected));
        let mut report = crate::shutdown::Report::default();
        report.check("SMS forwarding", service.shutdown().await);
        let failure = report.finish().unwrap_err().to_string();
        assert!(failure.contains(expected));
        assert!(!failure.contains("fixture-token"));
        assert!(!failure.contains("fixture-chat"));
        assert!(!failure.contains("intentional runner"));
        let (status, body) = response(service.handle(request(configuration(8))).await).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], expected);
        assert_eq!(service.0.state.lock().await.active.as_ref().unwrap().id, id);
        drop(service);
        fs::remove_dir_all(path).unwrap();
    }
}

#[tokio::test]
async fn cancelled_replacement_retains_old_runner_until_real_join_before_next_generation() {
    let path = root();
    let service = Service::new(&path);
    let id = configured(&service, configuration(7)).await;
    let (stopped, release) = blocked_runner(&service).await;
    // A replacement changes provider settings, not the queue's bound state key.
    let mut replacement = configuration(7);
    replacement["telegram"]["chat_id"] = serde_json::json!("fixture-replacement-chat");
    let task_service = service.clone();
    let task_config = replacement.clone();
    let task = tokio::spawn(async move { task_service.handle(request(task_config)).await });
    stopped.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        assert_eq!(active.id, id);
        assert!(active.retiring);
        assert!(
            active
                .runner
                .as_ref()
                .is_some_and(|runner| !runner.is_finished())
        );
        assert!(active.stop_result.is_none());
    }
    let mut shutdown = Box::pin(service.shutdown());
    pending(shutdown.as_mut()).await;
    release.send(()).unwrap();
    shutdown.await.unwrap();
    assert!(service.0.state.lock().await.active.is_none());
    // Even an empty retired queue retains its durable key identity. A wrong
    // key must fail before a new runner is installed or the witness changes.
    assert!(!service.0.state_path.exists());
    let witness_path = path.join("sms-forwarding/state.enc.keyid");
    let witness = fs::read(&witness_path).unwrap();
    let (status, body) = response(service.handle(request(configuration(8))).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "sms_forwarding_queue_unavailable");
    assert!(service.0.state.lock().await.active.is_none());
    assert_eq!(fs::read(&witness_path).unwrap(), witness);
    let next = configured(&service, replacement).await;
    assert_ne!(next, id);
    service.shutdown().await.unwrap();
    drop(service);
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn cancelled_shutdown_waiter_does_not_consume_join_or_report_early_clean() {
    let path = root();
    let service = Service::new(&path);
    configured(&service, configuration(7)).await;
    let (stopped, release) = blocked_runner(&service).await;
    let owner = service.clone();
    let first = tokio::spawn(async move { owner.shutdown().await });
    stopped.await.unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let mut second = Box::pin(service.shutdown());
    pending(second.as_mut()).await;
    release.send(()).unwrap();
    second.await.unwrap();
    assert!(service.0.state.lock().await.active.is_none());
    service.shutdown().await.unwrap();
    drop(service);
    fs::remove_dir_all(path).unwrap();
}

async fn local_bark() -> (String, Arc<AtomicUsize>, Arc<Notify>, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Notify::new());
    let counter = count.clone();
    let signal = observed.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let (header_end, length) = loop {
                let mut buffer = [0; 1024];
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                assert!(bytes.len() <= 64 * 1024);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    assert!(headers.starts_with("POST "));
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while bytes.len() < header_end + length {
                let mut buffer = [0; 1024];
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
            assert_eq!(body["title"], "Switch 短信");
            assert!(
                body["body"]
                    .as_str()
                    .unwrap()
                    .ends_with("\nfixture pending message")
            );
            assert_eq!(body["device_key"], "fixture-device");
            counter.fetch_add(1, Ordering::SeqCst);
            let response = b"{\"code\":200,\"message\":\"success\"}";
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).as_bytes()).await.unwrap();
            socket.write_all(response).await.unwrap();
            signal.notify_one();
        }
    });
    (url, count, observed, task)
}

#[tokio::test]
async fn cancelled_new_admission_keeps_writer_and_runner_and_sends_only_after_real_mode_ack() {
    let path = root();
    let handoff = crate::forwarding_handoff_store::Store::open(&path).unwrap();
    let admission = crate::device_forwarding_store::Store::open(&path).unwrap();
    let service = Service::new_with_admission(&path, handoff, admission.clone());
    configured(&service, configuration(7)).await;
    service.shutdown().await.unwrap();
    let (url, count, observed, server) = local_bark().await;
    let mut config = configuration(7);
    config["bark"]["enabled"] = serde_json::json!(true);
    config["bark"]["server_url"] = serde_json::json!(url);
    {
        // Seed a real encrypted pending delivery while no configured owner is
        // alive. Its target is the same concrete local provider used below.
        let key = StateKey::from_slice(&[7; 32]).unwrap();
        let telegram = DestinationBinding::telegram(&key, "fixture-token", "fixture-chat");
        let bark = DestinationBinding::bark(
            &key,
            config["bark"]["server_url"].as_str().unwrap(),
            "fixture-device",
        )
        .unwrap();
        let mut queue = QueueStore::open(
            path.join("sms-forwarding/state.enc"),
            key,
            StateLimits::default(),
        )
        .unwrap();
        let message = ForwardingMessage::new(
            "pending-fixture",
            "in",
            "fixture sender",
            "fixture pending message",
            "0001-01-01T00:00:00Z",
            "reader",
        )
        .unwrap();
        queue
            .enqueue_and_persist(
                &[message],
                "0001-01-01T00:00:00Z",
                &EnqueueOptions::new(false, &telegram, true, &bark),
            )
            .unwrap();
    }
    service.0.runner_entries.store(0, Ordering::SeqCst);
    let gate = AdmissionGate::new();
    *service.0.admission_gate.lock().unwrap() = Some(gate.clone());
    let owner = service.clone();
    let candidate = config.clone();
    let task = tokio::spawn(async move { owner.handle(request(candidate)).await });
    gate.entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        assert!(!active.admitted);
        assert!(!*active.start.borrow());
        assert!(
            active
                .runner
                .as_ref()
                .is_some_and(|runner| !runner.is_finished())
        );
        let mode = state.admission.as_ref().unwrap();
        assert_eq!(mode.configuration_id.as_ref(), Some(&active.id));
        assert!(mode.outcome.is_none());
        assert!(mode.task.as_ref().is_some_and(|task| !task.is_finished()));
    }
    assert_eq!(
        service.admission_enabled().await,
        Err("sms_forwarding_configuration_pending")
    );
    assert_eq!(service.0.runner_entries.load(Ordering::SeqCst), 0);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let mut shutdown = Box::pin(service.shutdown());
    pending(shutdown.as_mut()).await;
    // Shutdown cannot return while the original owned mode task is pending.
    gate.release.notify_one();
    shutdown.await.unwrap();
    assert!(service.0.state.lock().await.active.is_none());
    assert_eq!(service.0.runner_entries.load(Ordering::SeqCst), 0);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    configured(&service, config).await;
    observed.notified().await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(service.0.runner_entries.load(Ordering::SeqCst) > 0);
    loop {
        let queue = service
            .0
            .state
            .lock()
            .await
            .active
            .as_ref()
            .unwrap()
            .worker
            .queue()
            .clone();
        if queue
            .lock()
            .await
            .inspect()
            .unwrap()
            .tasks
            .iter()
            .all(|task| task.bark.completed)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(count.load(Ordering::SeqCst), 1);
    service.shutdown().await.unwrap();
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    drop(service);
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn cancelled_disabled_mode_without_runner_cannot_late_overwrite_next_configuration() {
    let path = root();
    let handoff = crate::forwarding_handoff_store::Store::open(&path).unwrap();
    let admission = crate::device_forwarding_store::Store::open(&path).unwrap();
    let service = Service::new_with_admission(&path, handoff, admission.clone());
    let gate = AdmissionGate::new();
    *service.0.admission_gate.lock().unwrap() = Some(gate.clone());
    let mut disabled = configuration(7);
    disabled["state_key"] = serde_json::json!("");
    let owner = service.clone();
    let first = tokio::spawn(async move { owner.handle(request(disabled)).await });
    gate.entered.notified().await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    {
        let state = service.0.state.lock().await;
        assert!(state.active.is_none());
        let mode = state.admission.as_ref().unwrap();
        assert!(mode.configuration_id.is_none());
        assert!(mode.task.as_ref().is_some_and(|task| !task.is_finished()));
    }
    let mut enabled = configuration(7);
    enabled["bark"]["enabled"] = serde_json::json!(true);
    let mut next = Box::pin(service.handle(request(enabled)));
    pending(next.as_mut()).await;
    assert!(admission.current().unwrap().is_none());
    gate.release.notify_one();
    let (status, _) = response(next.await).await;
    assert_eq!(status, StatusCode::OK);
    let progress = admission.current().unwrap().unwrap();
    assert_eq!(progress.was_enabled, Some(false));
    assert!(progress.skip_next_snapshot);
    service.shutdown().await.unwrap();
    drop(service);
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn real_build_failure_restores_old_key_only_after_old_runner_retires() {
    let path = root();
    let service = Service::new(&path);
    let old = configured(&service, configuration(7)).await;
    {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        let options =
            EnqueueOptions::new(false, &active.telegram_binding, false, &active.bark_binding);
        let message = ForwardingMessage::new(
            "seen-fixture",
            "in",
            "sender",
            "body",
            "2026-10-20T00:00:00Z",
            "reader",
        )
        .unwrap();
        active
            .worker
            .queue()
            .lock()
            .await
            .enqueue_and_persist(&[message], "2026-10-20T00:00:01Z", &options)
            .unwrap();
    }
    let (status, body) = response(service.handle(request(configuration(8))).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "sms_forwarding_queue_unavailable");
    {
        let state = service.0.state.lock().await;
        let restored = state.active.as_ref().unwrap();
        assert_ne!(restored.id, old);
        assert!(restored.is_running());
        assert!(
            restored.key.as_slice() == &[7; 32],
            "original queue key must be retained"
        );
        assert_eq!(state.admission.as_ref().unwrap().outcome, Some(Ok(())));
    }
    service.shutdown().await.unwrap();
    drop(service);
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn admission_task_panic_is_sticky_but_runner_still_actually_joins() {
    let path = root();
    let service = Service::new(&path);
    let id = configured(&service, configuration(7)).await;
    {
        let mut state = service.0.state.lock().await;
        state.admission = Some(AdmissionCommit {
            configuration_id: Some(id.clone()),
            task: Some(tokio::spawn(async {
                panic!("intentional admission retirement fixture");
            })),
            outcome: None,
        });
    }
    let result = service.shutdown().await;
    assert_eq!(result, Err("sms_forwarding_admission_panicked"));
    {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        assert_eq!(active.id, id);
        assert_eq!(active.stop_result, Some(Ok(())));
        assert!(active.runner.is_none());
        assert!(state.admission.as_ref().unwrap().task.is_none());
    }
    let mut report = crate::shutdown::Report::default();
    report.check("SMS forwarding", service.shutdown().await);
    assert!(report.finish().is_err());
    let (status, body) = response(service.handle(request(configuration(8))).await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "sms_forwarding_admission_panicked");
    drop(service);
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn failed_real_mode_commit_and_failed_rollback_cannot_hide_error_or_start_runner() {
    let path = root();
    let handoff = crate::forwarding_handoff_store::Store::open(&path).unwrap();
    let admission = crate::device_forwarding_store::Store::open(&path).unwrap();
    let service = Service::new_with_admission(&path, handoff, admission);
    configured(&service, configuration(7)).await;
    let original_corruption = b"{intentionally invalid admission JSON";
    fs::write(
        path.join("device-sms-forwarding-admission.json"),
        original_corruption,
    )
    .unwrap();
    let mut next = configuration(7);
    next["bark"]["group"] = serde_json::json!("replacement");
    let (status, body) = response(service.handle(request(next)).await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "device_forwarding_admission_unavailable");
    {
        let state = service.0.state.lock().await;
        assert!(state.active.is_none());
        assert!(state.admission.is_none());
    }
    assert_eq!(
        fs::read(path.join("device-sms-forwarding-admission.json")).unwrap(),
        original_corruption
    );
    assert_eq!(
        service.admission_enabled().await,
        Err("device_forwarding_admission_unavailable")
    );
    assert_eq!(
        service.shutdown().await,
        Err("device_forwarding_admission_unavailable")
    );
    assert_eq!(
        service.shutdown().await,
        Err("device_forwarding_admission_unavailable")
    );
    drop(service);
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn naturally_completed_runner_is_joined_before_explicit_same_config_starts_new_generation() {
    let path = root();
    let service = Service::new(&path);
    let id = configured(&service, configuration(7)).await;
    replace_runner(&service, |_| async {}).await;
    wait_runner_finished(&service).await;
    let next = configured(&service, configuration(7)).await;
    assert_ne!(next, id);
    assert!(
        service
            .0
            .state
            .lock()
            .await
            .active
            .as_ref()
            .unwrap()
            .is_running()
    );
    service.shutdown().await.unwrap();
    drop(service);
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn typed_admission_separates_startup_unconfirmed_disabled_and_retiring_states() {
    let path = root();
    let service = Service::new(&path);
    assert_eq!(
        service.admission_enabled().await,
        Err("sms_forwarding_configuration_unconfirmed")
    );
    configured(&service, configuration(7)).await;
    assert_eq!(service.admission_enabled().await, Ok(false));
    let mut enabled = configuration(7);
    enabled["bark"]["enabled"] = serde_json::json!(true);
    configured(&service, enabled).await;
    assert_eq!(service.admission_enabled().await, Ok(true));
    let (stopped, release) = blocked_runner(&service).await;
    let owner = service.clone();
    let waiter = tokio::spawn(async move { owner.shutdown().await });
    stopped.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(
        service.admission_enabled().await,
        Err("sms_forwarding_stopping")
    );
    release.send(()).unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(
        service.admission_enabled().await,
        Err("sms_forwarding_configuration_unconfirmed")
    );
    drop(service);
    fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn real_corrupt_swf1_failed_replacement_and_rollback_remain_unavailable_until_explicit_disable()
 {
    let path = root();
    let service = Service::new(&path);
    configured(&service, configuration(7)).await;
    {
        let state = service.0.state.lock().await;
        let active = state.active.as_ref().unwrap();
        let options =
            EnqueueOptions::new(false, &active.telegram_binding, false, &active.bark_binding);
        let message = ForwardingMessage::new(
            "corrupt-fixture",
            "in",
            "sender",
            "body",
            "2026-10-20T00:00:00Z",
            "reader",
        )
        .unwrap();
        active
            .worker
            .queue()
            .lock()
            .await
            .enqueue_and_persist(&[message], "2026-10-20T00:00:01Z", &options)
            .unwrap();
    }
    let state_path = path.join("sms-forwarding/state.enc");
    let corrupt = b"SWF1truncated-authenticated-state";
    fs::write(&state_path, corrupt).unwrap();
    let (status, body) = response(service.handle(request(configuration(8))).await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "sms_forwarding_unavailable");
    {
        let state = service.0.state.lock().await;
        assert!(state.active.is_none());
        assert_eq!(
            state.configuration_failure,
            Some("sms_forwarding_unavailable")
        );
    }
    assert_eq!(
        service.admission_enabled().await,
        Err("sms_forwarding_unavailable")
    );
    assert_eq!(service.shutdown().await, Err("sms_forwarding_unavailable"));
    assert_eq!(fs::read(&state_path).unwrap(), corrupt);
    let mut disabled = configuration(7);
    disabled["state_key"] = serde_json::json!("");
    let (status, _) = response(service.handle(request(disabled)).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(service.admission_enabled().await, Ok(false));
    assert_eq!(fs::read(&state_path).unwrap(), corrupt);
    service.shutdown().await.unwrap();
    drop(service);
    fs::remove_dir_all(path).unwrap();
}
