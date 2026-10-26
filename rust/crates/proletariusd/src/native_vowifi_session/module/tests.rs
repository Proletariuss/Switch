use super::*;

async fn production_preparation_caller_case(public_stop_during_reap: bool) {
    use std::sync::atomic::Ordering;
    let root = std::env::temp_dir().join(format!(
        "module-preparation-caller-{}",
        crate::snapshot::new_instance_id()
    ));
    let (usb, radio, trace) = crate::module_vowifi_card::tests::service_fixture(root.clone()).await;
    let block = Arc::new(crate::module_vowifi_card::tests::Block::default());
    *trace.blocked.lock().unwrap() = Some(("AT+CNUM".into(), block.clone()));
    let mut server = crate::reader_pcsc_gateway_tests::state(&root);
    server.usb_at = usb.clone();
    server.module_radio = radio.clone();
    let service = server
        .native_vowifi_lifecycle
        .module_service(&server)
        .unwrap();
    let claim = service
        .0
        .admission
        .acquire_native_module(&service.0.epoch, 1)
        .unwrap();
    let record = Arc::new(Record {
        control: Default::default(),
        id: 1,
        claim,
        connection: request::automatic(),
        stop: watch::channel(false).0,
        state: Mutex::new(RecordState {
            attempt: 1,
            ..Default::default()
        }),
        completed: Notify::new(),
        join: Default::default(),
    });
    let card =
        ModuleSessionCard::begin_prepare(usb.clone(), radio.clone(), record.stop.subscribe());
    let cli = Arc::new(super::super::startup_transport::tests::HeldCli::new());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let owned_record = record.clone();
    let owned_card = card.clone();
    let owned_cli = cli.clone();
    let owner = tokio::spawn(async move {
        let result = service
            .run_prepared_with(
                &owned_record,
                owned_card,
                &mut None,
                deadline,
                |_, card, stop| async move {
                    let error = owned_cli.select(stop).await.unwrap_err();
                    Err((error, card))
                },
            )
            .await;
        result
    });
    tokio::time::timeout(Duration::from_secs(3), block.entered.notified())
        .await
        .unwrap();
    // This is the real card-read worker, inside run_prepared's original
    // absolute attempt budget, rather than a delay before invoking a helper.
    tokio::time::sleep_until(deadline - Duration::from_secs(3)).await;
    block.release();
    let pid = cli.started_pid().await;
    cli.waiting_reap().await;
    assert!(tokio::time::Instant::now() >= deadline);
    assert!(
        !owner.is_finished(),
        "run_prepared must retain the active CLI join"
    );
    assert!(
        !*record.stop.borrow(),
        "attempt expiry must preserve retry intent"
    );
    assert_eq!(record.state.lock().unwrap().attempt, 1);
    assert_eq!(trace.mode.load(Ordering::Acquire), 4);
    assert!(
        !trace
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "AT+CFUN=1")
    );
    assert!(record.claim.is_current());
    if public_stop_during_reap {
        record.stop.send_replace(true);
        tokio::task::yield_now().await;
        assert!(
            !owner.is_finished(),
            "a second cancellation cannot drop the active reap"
        );
        assert_eq!(trace.mode.load(Ordering::Acquire), 4);
        assert!(
            !trace
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|c| c == "AT+CFUN=1")
        );
    }
    cli.release_reap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), owner)
            .await
            .unwrap()
            .unwrap(),
        Ok(!public_stop_during_reap)
    );
    cli.assert_reaped(&pid).await;
    assert_eq!(
        record.state.lock().unwrap().last_retry_error.as_deref(),
        if public_stop_during_reap {
            None
        } else {
            Some("module_vowifi_prepare_timeout")
        }
    );
    // The enclosing production run releases this retained card only after
    // run_prepared returns. Exercise the same real release, without claiming
    // this direct-caller fixture runs the complete ordinary-session loop.
    ModuleSessionCard::release_with_confirmation(&card)
        .await
        .unwrap();
    assert_eq!(trace.mode.load(Ordering::Acquire), 1);
    assert!(
        trace
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "AT+CFUN=1")
    );
    record.claim.release_confirmed().unwrap();
    radio.begin_shutdown();
    radio.finish_shutdown().await.unwrap();
    usb.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_preparation_keeps_original_card_deadline_and_cli_reap_ownership() {
    production_preparation_caller_case(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_preparation_public_stop_retains_cli_join_and_real_rf_hold() {
    production_preparation_caller_case(true).await;
}

#[tokio::test]
async fn preparation_deadline_waits_for_cleanup_ack_without_stopping_retained_retry() {
    let (record_stop, stop) = watch::channel(false);
    let (entered, observed_entered) = tokio::sync::oneshot::channel();
    let (confirm, confirmed) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(prepare_until(
        tokio::time::sleep(Duration::ZERO),
        stop,
        |mut attempt_stop| async move {
            while !*attempt_stop.borrow_and_update() {
                attempt_stop.changed().await.unwrap();
            }
            entered.send(()).unwrap();
            confirmed.await.unwrap()
        },
    ));
    observed_entered.await.unwrap();
    assert!(
        !*record_stop.borrow(),
        "attempt expiry cannot stop the public intent"
    );
    assert!(
        !task.is_finished(),
        "expiry cannot release the unjoined preparation"
    );
    confirm.send("cleanup-joined").unwrap();
    let (end, result) = task.await.unwrap();
    assert_eq!(end, PreparationEnd::Deadline);
    assert_eq!(result, "cleanup-joined");
    assert!(initial_retry_allowed(false, 0));
    assert!(!initial_retry_allowed(false, START_RETRY_LIMIT));
}

#[test]
fn registered_publication_cannot_overwrite_an_admitted_disable() {
    let gate = Arc::new(crate::ordinary_vowifi_admission::Gate::default());
    let claim = gate.acquire_native_module("register-stop-race", 1).unwrap();
    let (stop, observed_stop) = watch::channel(false);
    let record = Record {
        control: Default::default(),
        id: 1,
        claim: claim.clone(),
        connection: request::automatic(),
        stop,
        state: Mutex::new(RecordState {
            operation_done: false,
            stopping: true,
            error: Some("cleanup pending".into()),
            ..RecordState::default()
        }),
        completed: Notify::new(),
        join: Default::default(),
    };

    record.stop.send_replace(true);
    publish_registered_operation(&record, &observed_stop);

    let state = record.state.lock().unwrap();
    assert!(!state.operation_done, "disable must remain non-terminal");
    assert_eq!(state.error.as_deref(), Some("cleanup pending"));
    drop(state);

    {
        let mut state = record.state.lock().unwrap();
        state.stopping = false;
        state.error = Some("shutdown pending".into());
    }
    publish_registered_operation(&record, &observed_stop);
    let state = record.state.lock().unwrap();
    assert!(
        !state.operation_done,
        "process stop must remain non-terminal"
    );
    assert_eq!(state.error.as_deref(), Some("shutdown pending"));
    drop(state);
    claim.release_confirmed().unwrap();
}

#[test]
fn registered_publication_completes_a_live_start() {
    let gate = Arc::new(crate::ordinary_vowifi_admission::Gate::default());
    let claim = gate.acquire_native_module("register-live", 1).unwrap();
    let (stop, observed_stop) = watch::channel(false);
    let record = Record {
        control: Default::default(),
        id: 1,
        claim: claim.clone(),
        connection: request::automatic(),
        stop,
        state: Mutex::new(RecordState {
            error: Some("stale startup error".into()),
            ..RecordState::default()
        }),
        completed: Notify::new(),
        join: Default::default(),
    };

    publish_registered_operation(&record, &observed_stop);

    let state = record.state.lock().unwrap();
    assert!(state.operation_done);
    assert!(state.error.is_none());
    drop(state);
    claim.release_confirmed().unwrap();
}

fn failed_projection_fixture() -> (PathBuf, crate::ServerState, Service, Arc<Record>) {
    let root = std::env::temp_dir().join(format!(
        "module-stop-projection-{}",
        crate::snapshot::new_instance_id()
    ));
    let mut server = crate::reader_pcsc_gateway_tests::state(&root);
    server.usb_at = crate::usb_at::Owner::unavailable_for_tests("stop-no-device".into());
    let service = server
        .native_vowifi_lifecycle
        .module_service(&server)
        .unwrap();
    let claim = service
        .0
        .admission
        .acquire_native_module(&service.0.epoch, 1)
        .unwrap();
    let record = Arc::new(Record {
        control: Default::default(),
        id: 1,
        claim,
        connection: request::automatic(),
        stop: watch::channel(false).0,
        state: Mutex::new(RecordState {
            operation: Some(super::super::operation::Clock::start(
                tokio::time::Instant::now(),
                START_OPERATION_BUDGET,
            )),
            operation_enable: true,
            operation_done: true,
            ..Default::default()
        }),
        completed: Notify::new(),
        join: Default::default(),
    });
    service.0.state.lock().unwrap().record = Some(record.clone());
    (root, server, service, record)
}

fn inject_late_reader_failure(record: &Record) {
    let diagnostic = super::super::startup_diagnostics::startup(
        &NativeRegisteredStartupFailure::Tunnel(NativeTunnelStartError::LeaseLost),
    );
    assert_eq!(diagnostic.code, "reader_identity_changed");
    let mut state = record.state.lock().unwrap();
    state.startup_failure = Some(diagnostic);
    state.error = Some(format!("模块 Wi-Fi Calling: {}", diagnostic.message));
    state.last_retry_error = Some("native_reader_changed".into());
    state.retrying = true;
}

async fn observed_result_for(service: &Service, request_id: &str) -> Value {
    let response = service
        .handle(
            Request::builder()
                .uri(format!(
                    "/api/vowifi/result?observe_operation=true&client_request_id={request_id}"
                ))
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn confirmed_disable_and_cancel_discard_late_reader_failure_only_after_cleanup() {
    for cancelling in [false, true] {
        let (root, server, service, record) = failed_projection_fixture();
        record.state.lock().unwrap().operation_done = !cancelling;
        let (admitted_cancel, _, _) = service
            .request_with_observation(false, request::automatic(), None, "stop-fixture")
            .unwrap();
        assert_eq!(admitted_cancel, cancelling);
        assert!(*record.stop.borrow());
        // The card's stop watcher can report lease loss before the ordinary
        // owner observes request_stop. Reproduce that late diagnostic exactly.
        inject_late_reader_failure(&record);
        record.state.lock().unwrap().cleanup_error = Some("RF restore pending".into());
        let observed_stop = record.stop.subscribe();
        publish_registered_operation(&record, &observed_stop);
        let draining = service.operation_projection();
        assert_eq!(draining.phase, "draining");
        assert!(!draining.terminal);
        assert!(!draining.cleanup_confirmed);
        assert!(record.claim.try_is_current().unwrap());
        assert_eq!(service.status()["cleanup_error"], "RF restore pending");

        // Match the worker's order: RF restoration returns, the global claim
        // releases, and only then may the retained operation complete.
        record.state.lock().unwrap().cleanup_error = None;
        record.claim.release_confirmed().unwrap();
        record.state.lock().unwrap().complete_cleanup();
        let response = service
            .handle(
                Request::builder()
                    .uri("/api/vowifi/result?observe_operation=true")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await;
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["done"], true);
        assert_eq!(value["operation"]["phase"], "disabled");
        assert_eq!(
            value["operation"]["outcome"],
            if cancelling { "cancelled" } else { "stopped" }
        );
        assert_eq!(value["operation"]["succeeded"], true);
        assert_eq!(value["operation"]["cleanup_confirmed"], true);
        assert_eq!(value["status"]["state"], "disabled");
        assert_eq!(value["status"]["retrying"], false);
        assert!(value["status"].get("startup_failure").is_none());
        assert!(value["status"].get("detail").is_none());
        assert!(service.operation().get("error").is_none());
        drop(record);
        drop(service);
        drop(server);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn operation_facts_share_the_status_guard_across_cleanup_publication() {
    let (root, server, service, record) = failed_projection_fixture();
    record.state.lock().unwrap().operation_done = false;
    let observed_stop = record.stop.subscribe();
    publish_registered_operation(&record, &observed_stop);
    // A startup completion marker alone is not card/network readiness. Both
    // public status and operation facts must use the actual readiness sample.
    let preparing = service.operation_projection();
    assert_eq!(preparing.phase, "registering");
    assert!(!preparing.terminal);
    assert!(!preparing.succeeded);
    assert_eq!(service.status()["ims_registered"], false);
    assert_eq!(service.status()["vowifi_active"], false);

    service
        .request_with_observation(false, request::automatic(), None, "atomic-stop")
        .unwrap();
    inject_late_reader_failure(&record);
    let status = record.state.lock().unwrap();
    let before = Service::status_for_locked(&record, &status, None);
    assert_eq!(before["state"], "failed");

    let retained = record.clone();
    let (attempted, observed_attempt) = std::sync::mpsc::channel();
    let cleanup = std::thread::spawn(move || {
        // This is the exact publication boundary that used to fit between
        // status_for() and the operation's second record-lock acquisition.
        retained.claim.release_confirmed().unwrap();
        attempted
            .send(matches!(
                retained.state.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ))
            .unwrap();
        retained.state.lock().unwrap().complete_cleanup();
    });
    assert!(
        observed_attempt
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
    );
    let held_status = Service::status_for_locked(&record, &status, None);
    assert_eq!(held_status["state"], before["state"]);
    assert_eq!(held_status["ims_registered"], false);
    let (draining, paired_status) = service.operation_observation_for_locked(
        &record,
        &status,
        true,
        None,
        tokio::time::Instant::now(),
    );
    assert_eq!(draining.phase, "draining");
    assert!(!draining.cleanup_confirmed);
    assert!(!draining.terminal);
    assert!(!draining.succeeded);
    assert_eq!(paired_status, held_status);
    assert_eq!(
        Service::status_for_locked(&record, &status, None),
        held_status
    );
    drop(status);
    cleanup.join().unwrap();

    let (stopped, after) = service.operation_observation_for(Some(record.clone()));
    assert_eq!(stopped.phase, "disabled");
    assert_eq!(stopped.outcome.as_deref(), Some("stopped"));
    assert!(stopped.succeeded);
    assert!(stopped.cleanup_confirmed);
    assert_eq!(after["state"], stopped.phase);
    assert_eq!(after["ims_registered"], false);
    assert_eq!(after["vowifi_active"], false);
    assert_eq!(after["sms_ready"], false);
    assert_eq!(after["call_ready"], false);
    assert!(after.get("startup_failure").is_none());
    {
        let mut state = service.0.state.lock().unwrap();
        state.record = None;
        state.idle_mode = Some(4);
    }
    let idle = service.status();
    let (idle_operation, idle_status) = service.operation_observation_for(None);
    assert_eq!(idle_status, idle);
    assert_eq!(idle_status["cfun_mode"], 4);
    assert_eq!(idle_status["airplane_mode"], true);
    assert_eq!(idle_operation.outcome.as_deref(), Some("stopped"));
    assert!(idle_operation.cleanup_confirmed);
    drop(record);
    drop(service);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn receipt_observation_fences_successor_admission_and_resamples_disable_sequence() {
    use super::super::operation::{Clock, Receipt, request_fingerprint};
    let (root, server, service, first) = failed_projection_fixture();
    first.state.lock().unwrap().operation_done = false;
    let fingerprint = request_fingerprint(br#"{"enable":true}"#);
    let mut state = service.0.state.lock().unwrap();
    let (old_projection, _) = service.operation_observation_in_state(&state, state.record.as_ref());
    state
        .requests
        .insert(Receipt::capture("start-a", &fingerprint, &old_projection))
        .unwrap();

    let publisher = service.clone();
    let fingerprint_b = fingerprint.clone();
    let (attempted, observed_attempt) = std::sync::mpsc::channel();
    let admission = std::thread::spawn(move || {
        attempted
            .send(matches!(
                publisher.0.state.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ))
            .unwrap();
        let mut state = publisher.0.state.lock().unwrap();
        state
            .record
            .as_ref()
            .unwrap()
            .claim
            .release_confirmed()
            .unwrap();
        let successor = Arc::new(Record {
            control: Default::default(),
            id: 2,
            claim: publisher
                .0
                .admission
                .acquire_native_module(&publisher.0.epoch, 2)
                .unwrap(),
            connection: request::automatic(),
            stop: watch::channel(false).0,
            state: Mutex::new(RecordState {
                operation: Some(Clock::start(
                    tokio::time::Instant::now(),
                    START_OPERATION_BUDGET,
                )),
                operation_enable: true,
                ..Default::default()
            }),
            completed: Notify::new(),
            join: Default::default(),
        });
        state.next = 2;
        state.record = Some(successor.clone());
        let (projection, _) = publisher.operation_observation_in_state(&state, Some(&successor));
        state
            .requests
            .insert(Receipt::capture("start-b", &fingerprint_b, &projection))
            .unwrap();
        successor
    });
    assert!(
        observed_attempt
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
    );
    // A's projection is applied while admission/receipt insertion is still
    // excluded. B cannot appear between sampling A and observing the ledger.
    let (receipt, current, _) = service
        .lookup_request_locked(&mut state, "start-a", None)
        .unwrap();
    assert_eq!(current.operation_id, old_projection.operation_id);
    assert!(!receipt.unwrap().resolve(Some(current)).terminal);
    drop(state);
    let successor = admission.join().unwrap();
    let (receipt_b, current_b, _) = service
        .lookup_request_with_observation("start-b", None)
        .unwrap();
    let start_b = receipt_b.unwrap().resolve(Some(current_b));
    assert_eq!(start_b.generation, 2);
    assert!(!start_b.terminal);
    assert_ne!(start_b.outcome.as_deref(), Some("superseded"));
    let old = service
        .lookup_request("start-a", None)
        .unwrap()
        .unwrap()
        .resolve(Some(service.operation_projection()));
    assert_eq!(old.outcome.as_deref(), Some("superseded"));
    let historical = observed_result_for(&service, "start-a").await;
    assert_eq!(historical["operation"]["outcome"], "superseded");
    assert!(
        historical.get("status").is_none(),
        "old record cannot borrow B status"
    );
    assert!(
        observed_result_for(&service, "start-b")
            .await
            .get("status")
            .is_some()
    );

    // A new Clock sequence on the same record needs the same transaction
    // protection as a successor record. A prior start must not retire the new
    // stop receipt, and the prior start must retain its own identity.
    let mut state = service.0.state.lock().unwrap();
    let stop_receipt = {
        let mut status = successor.state.lock().unwrap();
        let clock = status.operation.as_mut().unwrap();
        clock
            .stop(tokio::time::Instant::now(), Duration::from_secs(30), true)
            .unwrap();
        let receipt = clock.receipt(
            "stop-b",
            &request_fingerprint(br#"{"enable":false}"#),
            &service.0.epoch,
            2,
            2,
        );
        status.operation_enable = true;
        status.cancel_requested = true;
        status.stopping = true;
        receipt
    };
    state.requests.insert(stop_receipt).unwrap();
    successor.stop.send_replace(true);
    let (receipt, stop, status) = service
        .lookup_request_locked(&mut state, "stop-b", None)
        .unwrap();
    let stop = receipt.unwrap().resolve(Some(stop));
    assert_ne!(stop.operation_id, start_b.operation_id);
    assert_eq!(stop.generation, start_b.generation);
    assert!(!stop.requested_enable);
    assert!(!stop.terminal);
    assert_eq!(stop.phase, "draining");
    assert_eq!(status["ims_registered"], false);
    let previous = state
        .requests
        .lookup("start-b", None)
        .unwrap()
        .unwrap()
        .resolve(Some(stop));
    assert_eq!(previous.operation_id, start_b.operation_id);
    assert_eq!(previous.outcome.as_deref(), Some("superseded"));
    drop(state);
    let historical = observed_result_for(&service, "start-b").await;
    assert_eq!(
        historical["operation"]["operation_id"],
        start_b.operation_id
    );
    assert!(
        historical.get("status").is_none(),
        "old Clock sequence cannot borrow stop status"
    );
    let observed_stop = observed_result_for(&service, "stop-b").await;
    assert_eq!(observed_stop["operation"]["phase"], "draining");
    assert!(observed_stop.get("status").is_some());
    successor.claim.release_confirmed().unwrap();
    successor.state.lock().unwrap().complete_cleanup();
    drop(successor);
    drop(first);
    drop(service);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retired_enable_failure_survives_until_disable_and_its_receipt_stays_failed() {
    let (root, server, service, record) = failed_projection_fixture();
    inject_late_reader_failure(&record);
    record.claim.release_confirmed().unwrap();
    record.state.lock().unwrap().complete_cleanup();
    let failed = service.operation_projection();
    assert_eq!(failed.phase, "failed");
    assert_eq!(failed.outcome.as_deref(), Some("failed"));
    assert!(!failed.succeeded);
    assert!(failed.cleanup_confirmed);
    assert_eq!(
        service.status()["startup_failure"]["code"],
        "reader_identity_changed"
    );
    service
        .0
        .state
        .lock()
        .unwrap()
        .requests
        .insert(super::super::operation::Receipt::capture(
            "original-enable",
            &super::super::operation::request_fingerprint(br#"{"enable":true}"#),
            &failed,
        ))
        .unwrap();

    service
        .request_with_observation(false, request::automatic(), None, "retired-disable")
        .unwrap();
    let stopped = service.operation_projection();
    assert_eq!(stopped.phase, "disabled");
    assert_eq!(stopped.outcome.as_deref(), Some("stopped"));
    assert!(stopped.succeeded);
    assert!(stopped.cleanup_confirmed);
    assert!(service.status().get("startup_failure").is_none());
    let original = service
        .lookup_request("original-enable", None)
        .unwrap()
        .unwrap()
        .resolve(Some(stopped));
    assert_eq!(original.operation_id, failed.operation_id);
    assert_eq!(original.phase, "failed");
    assert_eq!(original.outcome.as_deref(), Some("failed"));
    assert!(!original.succeeded);
    drop(record);
    drop(service);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn transport_pause_fences_first_start_and_does_not_revive_stopped_attempt_or_shutdown() {
    let mut state = State::default();
    state.pause_transport();
    assert_eq!(
        state.check_request(true),
        Err("module_vowifi_transport_recovering")
    );
    assert_eq!(state.check_request(false), Ok(()));
    let gate = Arc::new(crate::ordinary_vowifi_admission::Gate::default());
    let claim = gate.acquire_native_module("pause-fixture", 1).unwrap();
    let record = Arc::new(Record {
        control: Default::default(),
        id: 1,
        claim,
        connection: request::automatic(),
        stop: watch::channel(false).0,
        state: Mutex::new(RecordState::default()),
        completed: Notify::new(),
        join: Default::default(),
    });
    state.record = Some(record.clone());
    state.pause_transport();
    assert!(*record.stop.borrow());
    assert!(
        record.claim.try_is_current().unwrap(),
        "pause does not certify cleanup"
    );
    state.transport_recovering = false;
    assert_eq!(state.check_request(true), Ok(()));
    assert!(
        *record.stop.borrow(),
        "resume never revives the old attempt"
    );
    state.stopping = true;
    assert_eq!(state.check_request(true), Err("module_vowifi_stopping"));
    assert_eq!(state.check_request(false), Err("module_vowifi_stopping"));
    record.claim.release_confirmed().unwrap();
}
#[test]
fn request_decoder_preserves_legacy_shape_and_freezes_reader_connection_fields() {
    assert!(request::decode(br#"{"ENABLE":false,"enable":true,"wifi_calling_mode":2,"epdg_auto_detect":false,"unknown":[]} {}"#).unwrap().enable);
    assert!(
        !request::decode(
            br#"{"enable":false,"epdg_address":"ignored","proxy_host":"bad","proxy_port":"0"}"#
        )
        .unwrap()
        .enable
    );
    assert!(
        request::decode(br#"{"enable":null,"Enable":true}"#)
            .unwrap()
            .enable
    );
    assert!(request::decode(br#"{"enable":true,"enable":null}"#).is_err());
    let decoded = request::decode(br#"{"enable":true,"proxy_host":"2001:db8::1","proxy_port":"1080","epdg_address":" wlan.example.test ","epdg_address":null,"epdg_port_text":"4500"}"#).unwrap();
    assert_eq!(decoded.connection.proxy, "socks5://[2001:db8::1]:1080");
    assert_eq!(decoded.connection.epdg, "wlan.example.test");
    assert_eq!(decoded.connection.epdg_port, 4500);
    assert_eq!(
        request::decode(br#"{"enable":true,"epdg_ip_address":"198.51.100.1"}"#).err(),
        Some("module_epdg_ip_unsupported")
    );
    for body in [
        br#"null"#.as_slice(),
        br#"{}"#,
        br#"{"enable":1}"#,
        br#"{"enable":true,"wifi_calling_mode":"1"}"#,
        br#"{"enable":true,"epdg_auto_detect":1}"#,
        br#"{"enable":true,"epdg_address":7}"#,
    ] {
        assert!(request::decode(body).is_err());
    }
    for body in [
        br#"{"enable":true,"proxy_host":"proxy.example","proxy_port":"0"}"#.as_slice(),
        br#"{"enable":true,"epdg":"https://wlan.example.test"}"#,
        br#"{"enable":true,"epdg_port_text":"65536"}"#,
    ] {
        assert!(request::decode(body).is_err());
    }
}

#[test]
fn initial_retry_budget_is_bounded_but_registered_reconnects_remain_allowed() {
    assert!(initial_retry_allowed(false, 0));
    assert!(!initial_retry_allowed(false, START_RETRY_LIMIT));
    assert!(initial_retry_allowed(true, START_RETRY_LIMIT));

    let now = tokio::time::Instant::now();
    let mut state = RecordState {
        startup_deadline: Some(now + START_ATTEMPT_BUDGET + START_WATCHDOG_MARGIN),
        ..Default::default()
    };
    assert_eq!(
        next_attempt_deadline(&state, now),
        Some(now + START_ATTEMPT_BUDGET)
    );
    state.startup_deadline =
        Some(now + START_ATTEMPT_BUDGET + START_WATCHDOG_MARGIN - Duration::from_millis(1));
    assert_eq!(next_attempt_deadline(&state, now), None);
    state.operation_done = true;
    assert_eq!(
        next_attempt_deadline(&state, now),
        Some(now + START_ATTEMPT_BUDGET)
    );
}

#[test]
fn internal_retry_keeps_operation_identity_and_never_projects_superseded() {
    let root = std::env::temp_dir().join(format!(
        "module-retry-projection-{}",
        crate::snapshot::new_instance_id()
    ));
    let mut server = crate::reader_pcsc_gateway_tests::state(&root);
    server.usb_at = crate::usb_at::Owner::unavailable_for_tests("retry-no-device".into());
    let service = server
        .native_vowifi_lifecycle
        .module_service(&server)
        .unwrap();
    let claim = service
        .0
        .admission
        .acquire_native_module(&service.0.epoch, 1)
        .unwrap();
    let record = Arc::new(Record {
        control: Default::default(),
        id: 1,
        claim: claim.clone(),
        connection: request::automatic(),
        stop: watch::channel(false).0,
        state: Mutex::new(RecordState {
            operation: Some(super::super::operation::Clock::start(
                tokio::time::Instant::now(),
                START_OPERATION_BUDGET,
            )),
            operation_enable: true,
            attempt: 2,
            retrying: true,
            ..Default::default()
        }),
        completed: Notify::new(),
        join: Default::default(),
    });
    service.0.state.lock().unwrap().record = Some(record.clone());

    let first = service.operation_projection_for(Some(record.clone()));
    assert_eq!(first.generation, 1);
    assert_ne!(first.phase, "superseded");
    assert_ne!(first.outcome.as_deref(), Some("superseded"));
    let second = service.operation_projection_for(Some(record.clone()));
    assert_eq!(second.operation_id, first.operation_id);
    assert_eq!(second.generation, first.generation);

    service.0.state.lock().unwrap().record = None;
    claim.release_confirmed().unwrap();
    drop(record);
    drop(service);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn module_request_receipt_keeps_old_identity_when_current_owner_is_a_ready_successor() {
    let now = tokio::time::Instant::now();
    let old = super::super::operation::Clock::start(now, Duration::from_secs(90)).project(
        now,
        "module",
        1,
        super::super::operation::Facts {
            generation: 1,
            phase: "starting".into(),
            ready: false,
            sms_ready: false,
            call_ready: false,
            cleanup_confirmed: false,
            superseded: false,
            error: None,
        },
    );
    let receipt = super::super::operation::Receipt::capture("module-request-1", "body-a", &old);
    let mut state = State::default();
    state.requests.insert(receipt).unwrap();

    let successor = super::super::operation::Clock::start(now, Duration::from_secs(90)).project(
        now,
        "module",
        2,
        super::super::operation::Facts {
            generation: 2,
            phase: "active".into(),
            ready: true,
            sms_ready: true,
            call_ready: true,
            cleanup_confirmed: false,
            superseded: false,
            error: None,
        },
    );
    let resolved = state
        .requests
        .lookup("module-request-1", Some("body-a"))
        .unwrap()
        .unwrap()
        .resolve(Some(successor));
    assert_eq!(resolved.operation_id, old.operation_id);
    assert_eq!(resolved.generation, 1);
    assert_eq!(resolved.phase, "superseded");
    assert_eq!(resolved.outcome.as_deref(), Some("superseded"));
    assert!(resolved.terminal);
    assert!(!resolved.succeeded);
    assert!(matches!(
        state.requests.lookup("module-request-1", Some("body-b")),
        Err("vowifi_request_id_conflict")
    ));
}

#[tokio::test]
async fn durable_module_service_reopen_replays_crashed_owner_before_worker_dispatch() {
    use super::super::operation::{Clock, Facts, Receipt, RequestLedger, request_fingerprint};
    for phase in ["admitted", "submitted"] {
        let root = std::env::temp_dir().join(format!(
            "module-vowifi-reopen-{}",
            crate::snapshot::new_instance_id()
        ));
        let mut server = crate::reader_pcsc_gateway_tests::state(&root);
        server.usb_at =
            crate::usb_at::Owner::unavailable_for_tests("durable-fixture-no-device".into());
        let data = server.multistandby_store.data_root();
        let bytes =
            br#"{"enable":true,"observe_operation":true,"client_request_id":"module-crash-1"}"#;
        let now = tokio::time::Instant::now();
        let mut old = Clock::start(now, Duration::from_secs(90)).project(
            now,
            "old-module",
            41,
            Facts {
                generation: 41,
                phase: phase.into(),
                ready: false,
                sms_ready: false,
                call_ready: false,
                cleanup_confirmed: false,
                superseded: false,
                error: None,
            },
        );
        old.phase = phase.into();
        let mut ledger = RequestLedger::open(data, "module").unwrap();
        ledger
            .insert(Receipt::capture(
                "module-crash-1",
                &request_fingerprint(bytes),
                &old,
            ))
            .unwrap();
        drop(ledger);
        let service = server
            .native_vowifi_lifecycle
            .module_service(&server)
            .unwrap();
        let response = service
            .handle(
                Request::builder()
                    .method("POST")
                    .uri("/api/vowifi")
                    .body(http_body_util::Full::new(bytes::Bytes::copy_from_slice(
                        bytes,
                    )))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["operation"]["operation_id"], old.operation_id);
        assert_eq!(value["operation"]["phase"], "unknown");
        assert_eq!(value["operation"]["terminal"], false);
        let altered =
            br#"{"enable":false,"observe_operation":true,"client_request_id":"module-crash-1"}"#;
        let response = service
            .handle(
                Request::builder()
                    .method("POST")
                    .uri("/api/vowifi")
                    .body(http_body_util::Full::new(bytes::Bytes::copy_from_slice(
                        altered,
                    )))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let state = service.0.state.lock().unwrap();
        assert_eq!(
            state.next, 0,
            "replay/conflict never reaches the worker dispatcher"
        );
        assert!(state.record.is_none());
        drop(state);
        drop(service);
        drop(server);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn public_vowifi_routes_reject_identity_bypasses_before_any_dispatch() {
    let root = std::env::temp_dir().join(format!(
        "vowifi-route-identity-{}",
        crate::snapshot::new_instance_id()
    ));
    let mut server = crate::reader_pcsc_gateway_tests::state(&root);
    server.usb_at =
        crate::usb_at::Owner::unavailable_for_tests("identity-fixture-no-device".into());
    server.native_vowifi_lifecycle.activate().unwrap();
    let module = server
        .native_vowifi_lifecycle
        .module_service(&server)
        .unwrap();
    let reader = server.native_vowifi_lifecycle.service(&server).unwrap();
    for path in ["/api/reader/vowifi", "/api/vowifi"] {
        for enable in [true, false] {
            for observe in [
                None,
                Some(json!(false)),
                Some(json!(null)),
                Some(json!(true)),
                Some(json!(1)),
                Some(json!("true")),
            ] {
                for identity in [
                    None,
                    Some(json!(null)),
                    Some(json!("")),
                    Some(json!("bad id")),
                    Some(json!(7)),
                    Some(json!("x".repeat(129))),
                    Some(json!("swift-route-1")),
                ] {
                    if observe == Some(json!(true)) && identity == Some(json!("swift-route-1")) {
                        continue;
                    }
                    let mut body = json!({"enable":enable});
                    if let Some(observe) = &observe {
                        body["observe_operation"] = observe.clone();
                    }
                    if let Some(identity) = identity {
                        body["client_request_id"] = identity;
                    }
                    let (status, value) = crate::reader_pcsc_gateway_tests::request_with_body(
                        server.clone(),
                        path,
                        true,
                        false,
                        Some(("x-proletarius-app-token", "app-fixture")),
                        body.to_string().as_bytes(),
                    )
                    .await;
                    assert_eq!(status, StatusCode::BAD_REQUEST, "{path} {body}: {value}");
                    assert_eq!(value["code"], "invalid_operation_observation");
                    let state = module.0.state.lock().unwrap();
                    assert_eq!(state.next, 0, "dispatch_count must remain zero");
                    assert!(state.record.is_none());
                    drop(state);
                    assert!(!module.owns_session());
                    assert!(!reader.has_intent());
                    assert!(!server.reader_gate.native_reader_session_active());
                }
            }
        }
    }
    // A rejected disable must not signal an already retained owner either.
    let claim = module
        .0
        .admission
        .acquire_native_module("identity-fixture", 42)
        .unwrap();
    let record = Arc::new(Record {
        control: Default::default(),
        id: 42,
        claim: claim.clone(),
        connection: request::automatic(),
        stop: watch::channel(false).0,
        state: Mutex::new(RecordState::default()),
        completed: Notify::new(),
        join: Default::default(),
    });
    module.0.state.lock().unwrap().record = Some(record.clone());
    for body in [
        br#"{"enable":false}"#.as_slice(),
        br#"{"enable":false,"client_request_id":"stop-without-observe"}"#,
        br#"{"enable":false,"observe_operation":false,"client_request_id":"stop-opt-out"}"#,
        br#"{"enable":false,"observe_operation":null,"client_request_id":"stop-null"}"#,
        br#"{"enable":false,"observe_operation":true}"#,
    ] {
        let (status, _) = crate::reader_pcsc_gateway_tests::request_with_body(
            server.clone(),
            "/api/vowifi",
            true,
            false,
            Some(("x-proletarius-app-token", "app-fixture")),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            !*record.stop.borrow(),
            "invalid POST must never signal retained owner stop"
        );
        assert!(!record.state.lock().unwrap().stopping);
        assert!(claim.try_is_current().unwrap());
        assert_eq!(module.0.state.lock().unwrap().next, 0);
    }
    module.0.state.lock().unwrap().record = None;
    claim.release_confirmed().unwrap();
    for path in ["/api/vowifi", "/api/vowifi/result"] {
        let response = module
            .handle(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "GET has no identity requirement"
        );
    }
    // This is the normal Swift disable shape, accepted and durably replayable.
    let bytes = br#"{"enable":false,"observe_operation":true,"client_request_id":"swift-module-disable-1"}"#;
    let post = |bytes: &'static [u8]| {
        crate::reader_pcsc_gateway_tests::request_with_body(
            server.clone(),
            "/api/vowifi",
            true,
            false,
            Some(("x-proletarius-app-token", "app-fixture")),
            bytes,
        )
    };
    let (status, first) = post(bytes).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        first["operation"]["client_request_id"],
        "swift-module-disable-1"
    );
    let (status, replay) = post(bytes).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        first["operation"]["operation_id"],
        replay["operation"]["operation_id"]
    );
    assert_eq!(post(br#"{"enable":true,"observe_operation":true,"client_request_id":"swift-module-disable-1"}"#).await.0, StatusCode::CONFLICT);
    assert_eq!(module.0.state.lock().unwrap().next, 0);
    drop(reader);
    drop(module);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn epdg_alias_reordering_conflicts_before_dispatch_and_legacy_receipts_fail_closed() {
    use super::super::operation::{Clock, Facts, Receipt, RequestLedger, request_fingerprint};
    let first = br#"{"enable":true,"epdg":"a.example","epdg_address":"b.example","observe_operation":true,"client_request_id":"alias-order"}"#;
    let reversed = br#"{"enable":true,"epdg_address":"b.example","epdg":"a.example","observe_operation":true,"client_request_id":"alias-order"}"#;
    assert_eq!(request::decode(first).unwrap().connection.epdg, "b.example");
    assert_eq!(
        request::decode(reversed).unwrap().connection.epdg,
        "a.example"
    );
    for legacy in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "module-alias-order-{}",
            crate::snapshot::new_instance_id()
        ));
        let mut server = crate::reader_pcsc_gateway_tests::state(&root);
        server.usb_at =
            crate::usb_at::Owner::unavailable_for_tests("alias-fixture-no-device".into());
        let now = tokio::time::Instant::now();
        let old = Clock::start(now, Duration::from_secs(90)).project(
            now,
            "old-module",
            41,
            Facts {
                generation: 41,
                phase: "submitted".into(),
                ready: false,
                sms_ready: false,
                call_ready: false,
                cleanup_confirmed: false,
                superseded: false,
                error: None,
            },
        );
        let fingerprint = if legacy {
            // Reproduce the old order-insensitive encoding already on disk.
            let sorted: std::collections::BTreeMap<String, Value> =
                serde_json::from_slice(first).unwrap();
            proletarius_sms::stable_digest_hex(&serde_json::to_vec(&sorted).unwrap())
        } else {
            request_fingerprint(first)
        };
        let mut ledger =
            RequestLedger::open(server.multistandby_store.data_root(), "module").unwrap();
        ledger
            .insert(Receipt::capture("alias-order", &fingerprint, &old))
            .unwrap();
        drop(ledger);
        let service = server
            .native_vowifi_lifecycle
            .module_service(&server)
            .unwrap();
        for (body, same) in [(first.as_slice(), true), (reversed.as_slice(), false)] {
            let response = service
                .handle(
                    Request::builder()
                        .method("POST")
                        .uri("/api/vowifi")
                        .body(http_body_util::Full::new(bytes::Bytes::copy_from_slice(
                            body,
                        )))
                        .unwrap(),
                )
                .await;
            assert_eq!(
                response.status(),
                if same && !legacy {
                    StatusCode::ACCEPTED
                } else {
                    StatusCode::CONFLICT
                }
            );
            if !same || legacy {
                let value: Value = serde_json::from_slice(
                    &response.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                assert_eq!(value["code"], "vowifi_request_id_conflict");
            }
            let state = service.0.state.lock().unwrap();
            assert_eq!(
                state.next, 0,
                "persisted request must not dispatch a second worker"
            );
            assert!(state.record.is_none());
        }
        drop(service);
        drop(server);
        std::fs::remove_dir_all(root).unwrap();
    }
}
