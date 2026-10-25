use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_connect_captures_ble_epoch_and_shutdown_retains_late_proof_without_go() {
    ble_loss(true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_ble_shutdown_late_loss_does_not_wait_for_restarting_module_go() {
    ble_loss(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_ble_observer_joins_real_retained_cleanup_before_final_lifecycle_drain() {
    ble_loss_with_observer(false, true).await;
    ble_loss_with_observer(true, true).await;
}
async fn ble_loss(with_pool: bool) {
    ble_loss_with_observer(with_pool, false).await;
}
async fn ble_loss_with_observer(with_pool: bool, observed: bool) {
    let reader = "BLE:12345678-1234-1234-1234-123456789abc";
    let root = std::env::temp_dir().join(format!(
        "ble-life-{}",
        &crate::multistandby_store::random_token().unwrap()[..8]
    ));
    std::fs::create_dir_all(&root).unwrap();
    let socket = root.join("bridge.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();
    let delayed = Arc::new(Mutex::new(Some((entered_tx, complete_rx))));
    let mode = Arc::new(AtomicUsize::new(0));
    let bridge_mode = mode.clone();
    let apdus = Arc::new(AtomicUsize::new(0));
    let bridge_apdus = apdus.clone();
    let peer = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mode = bridge_mode.clone();
            let delayed = delayed.clone();
            let apdus = bridge_apdus.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                tokio::io::BufReader::new(read)
                    .read_line(&mut line)
                    .await
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["token"], "fixture-authenticated-app-token-32");
                let mut response = json!({"ok":true,"reader":reader,"bridge_id":"bridge-current",
                    "connection_id":"physical-current","close_operation_id":"close-current"});
                match request["action"].as_str().unwrap() {
                    "status" => {
                        response["state"] = json!("connected");
                        response["operation_tracking"] = json!(true);
                    }
                    "apdu" => {
                        assert_eq!(request["connection_id"], "physical-current");
                        apdus.fetch_add(1, Ordering::SeqCst);
                        response["terminal"] = json!(true);
                        response["operation_id"] = request["operation_id"].clone();
                        response["hex"] = json!("6a82");
                    }
                    "operation-status" => {
                        assert_eq!(request["operation_id"], "close-current");
                        assert_eq!(request["connection_id"], "physical-current");
                        response["operation_id"] = request["operation_id"].clone();
                        let closed = mode.load(Ordering::SeqCst) == 1;
                        if closed {
                            let block = delayed.lock().unwrap().take();
                            if let Some((entered, complete)) = block {
                                entered.send(()).unwrap();
                                complete.await.unwrap();
                            }
                        }
                        response["terminal"] = json!(closed);
                        response["closed"] = json!(closed);
                    }
                    action => panic!("unexpected BLE request {action}"),
                }
                write
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
            });
        }
    });
    let gate = Arc::new(Gate::default());
    let owner = Owner::with_factory(
        "ble-native".into(),
        Arc::new(|_| panic!("no PCSC")),
        Arc::new(|| Ok(vec![])),
        Arc::new(|_, _| panic!("no PCSC reset")),
    );
    let service = Service::new(
        root.clone(),
        gate.clone(),
        owner.clone(),
        Some(socket.clone()),
        Some("fixture-authenticated-app-token-32".into()),
        "ble-native".into(),
    )
    .unwrap();
    let ordinary = Arc::new(crate::native_vowifi_session::NativeLifecycle::default());
    gate.set_ordinary_admission(ordinary.ordinary_admission())
        .unwrap();
    service.set_ordinary_lifecycle(Arc::downgrade(&ordinary));
    let selected = service.connect(reader).await.unwrap();
    assert!(
        apdus.load(Ordering::SeqCst) > 0,
        "metadata was read through real BLE wire"
    );
    let captured = gate.ble_connection_binding(reader).unwrap();
    assert_eq!(captured.connection_id, "physical-current");
    let cards = crate::multistandby_card::Service::new(
        owner.clone(),
        gate.clone(),
        Some(socket),
        Some("fixture-authenticated-app-token-32".into()),
    );
    let pool = if with_pool {
        Some(
            crate::multistandby_native_card::Pool::acquire(
                owner.clone(),
                gate.clone(),
                cards,
                reader.into(),
                selected.selection.generation,
            )
            .unwrap(),
        )
    } else {
        None
    };
    let _module = if !with_pool {
        ordinary
            .ordinary_admission()
            .set_go_producer("go-current")
            .unwrap();
        service.set_business_bridge(crate::reader_business_runtime::Bridge::new(
            crate::message_archive::InboxState::for_go_producer("go-current".into()),
        ));
        Some(
            ordinary
                .ordinary_admission()
                .acquire_go("go-current", "module", "module-live", 1)
                .unwrap(),
        )
    } else {
        None
    };
    assert!(!service.reconcile_ble_presence().await.unwrap());
    assert_eq!(
        service.snapshot().selection.generation,
        selected.selection.generation
    );
    service.begin_shutdown();
    mode.store(1, Ordering::SeqCst);
    let polling = service.clone();
    let (observer_stop, observer_stopping) = tokio::sync::watch::channel(false);
    let waiter = if observed {
        tokio::spawn(async move {
            run_ble_presence_observer(polling, observer_stopping).await;
        })
    } else {
        tokio::spawn(async move {
            let _ = polling.reconcile_ble_presence().await;
        })
    };
    entered_rx.await.unwrap();
    if observed {
        observer_stop.send_replace(true);
        let finalizing = async {
            // Exact production ordering: unknown join pins this branch; only a
            // joined producer permits the real retained-job/transport drain.
            let _report = crate::shutdown::once(
                "BLE loss observer join",
                waiter.await.map_err(|error| error.to_string()),
            )
            .await;
            service.finish_shutdown().await
        };
        tokio::pin!(finalizing);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut finalizing)
                .await
                .is_err()
        );
        assert!(
            service.0.life.lock().unwrap().jobs > 0,
            "real cleanup lost its owner while observer stopped"
        );
        complete_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), &mut finalizing)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(service.0.life.lock().unwrap().jobs, 0);
    } else {
        waiter.abort();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), service.finish_shutdown())
                .await
                .is_err(),
            "cancelled caller cannot abandon pending physical proof"
        );
        assert_eq!(service.snapshot().selection.reader, reader);
        complete_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), service.finish_shutdown())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(service.snapshot().selection.reader.is_empty());
    if let Some(pool) = pool {
        assert_eq!(
            pool.shutdown_receipt().await.unwrap(),
            crate::multistandby_native_card::CleanupReceipt::ConfirmedReaderLost
        );
    }
    owner.shutdown().await.unwrap();
    peer.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pcsc_native_loss_without_go_and_real_legacy_owner_waits_for_exact_retirement() {
    for legacy in [false, true] {
        let gate = Arc::new(Gate::default());
        let trace = Arc::new(Trace::default());
        let factory = trace.clone();
        let present = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let listing = present.clone();
        let owner = Owner::with_factory(
            "loss-native".into(),
            Arc::new(move |_| Ok(Box::new(Fake(factory.clone())))),
            Arc::new(move || {
                Ok(if listing.load(Ordering::SeqCst) {
                    vec!["Reader".into()]
                } else {
                    vec![]
                })
            }),
            Arc::new(|_, _| panic!("no reset")),
        );
        let service = Service::new(
            std::env::temp_dir(),
            gate.clone(),
            owner.clone(),
            None,
            None,
            "loss-native".into(),
        )
        .unwrap();
        let ordinary = Arc::new(crate::native_vowifi_session::NativeLifecycle::default());
        gate.set_ordinary_admission(ordinary.ordinary_admission())
            .unwrap();
        service.set_ordinary_lifecycle(Arc::downgrade(&ordinary));
        let selected = service.connect("Reader").await.unwrap();
        let physical = selected.pcsc_session.unwrap();
        let inbox = crate::message_archive::InboxState::for_go_producer("go-current".into());
        let claim = if legacy {
            ordinary
                .ordinary_admission()
                .set_go_producer("go-current")
                .unwrap();
            service.set_business_bridge(crate::reader_business_runtime::Bridge::new(inbox.clone()));
            Some(
                ordinary
                    .ordinary_admission()
                    .acquire_go("go-current", "reader", "real-legacy-owner", 1)
                    .unwrap(),
            )
        } else {
            None
        };
        present.store(false, Ordering::SeqCst);
        assert!(owner.test_close_for_presence(&physical).unwrap());
        assert_eq!(trace.closed.load(Ordering::SeqCst), 1);
        if !legacy {
            ordinary.ordinary_admission().poison_for_test();
            assert!(
                service
                    .confirmed_loss(
                        "Reader",
                        selected.selection.generation,
                        Some(physical.clone())
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                service.snapshot().selection.reader,
                "Reader",
                "unreadable ownership must not commit logical loss"
            );
            ordinary.ordinary_admission().clear_test_poison();
        }
        let operation = service.clone();
        let binding = physical.clone();
        let mut wait = tokio::spawn(async move {
            operation
                .confirmed_loss("Reader", selected.selection.generation, Some(binding))
                .await
        });
        if legacy {
            assert!(
                tokio::time::timeout(Duration::from_millis(80), &mut wait)
                    .await
                    .is_err(),
                "real legacy claim cannot be waived because Go is absent"
            );
            assert!(
                service.snapshot().selection.reader.is_empty(),
                "physical loss committed before retained business retirement"
            );
            assert!(claim.as_ref().unwrap().go_binding().is_some());
            claim.as_ref().unwrap().release_confirmed().unwrap();
            assert!(!ordinary.ordinary_admission().has_go_ordinary_claim());
            assert!(
                tokio::time::timeout(Duration::from_millis(80), &mut wait)
                    .await
                    .is_err(),
                "a later empty claim cannot replace the captured producer receipt"
            );
            // Exactly the already-captured producer is retired by confirmed exit;
            // a fresh empty status or a replaced reader is not used as evidence.
            ordinary
                .ordinary_admission()
                .retire_go_producer("go-current", "go-next")
                .unwrap();
            inbox.replace_go_producer("go-next".into()).unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(2), wait)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        );
        owner.test_ack_presence_loss(&physical);
        service.finish_shutdown().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn unknown_ble_observer_join_never_allows_dependent_close() {
    let observer = tokio::spawn(async {
        panic!("controlled BLE observer panic");
    });
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let token = closed.clone();
    let shutdown = async {
        let _report = crate::shutdown::once(
            "BLE loss observer join",
            observer.await.map_err(|error| error.to_string()),
        )
        .await;
        token.store(true, Ordering::SeqCst);
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(20), shutdown)
            .await
            .is_err()
    );
    assert!(!closed.load(Ordering::SeqCst));
}
