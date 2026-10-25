use super::*;
use crate::reader_pcsc::{Failure as PcscFailure, Physical};
use proletarius_sim::ApduTransport;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Trace {
    opened: AtomicUsize,
    closed: AtomicUsize,
    calls: AtomicUsize,
    retire_reject: std::sync::atomic::AtomicBool,
    stop_reject: std::sync::atomic::AtomicBool,
    busy: std::sync::atomic::AtomicBool,
    stops: AtomicUsize,
    queries: AtomicUsize,
    retirements: AtomicUsize,
    fail_apdu: std::sync::atomic::AtomicBool,
    close_block: Mutex<
        Option<(
            std::sync::mpsc::SyncSender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
    block: Mutex<
        Option<(
            std::sync::mpsc::SyncSender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
}
struct Fake(Arc<Trace>);
impl Physical for Fake {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn ApduTransport<Error = PcscFailure>),
    ) -> Result<(), PcscFailure> {
        body(self);
        Ok(())
    }
    fn close(&mut self) -> Result<(), PcscFailure> {
        if let Some((started, wait)) = self.0.close_block.lock().unwrap().take() {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        self.0.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
impl ApduTransport for Fake {
    type Error = PcscFailure;
    fn transmit(&mut self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((started, wait)) = self.0.block.lock().unwrap().take() {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        if self.0.fail_apdu.swap(false, Ordering::SeqCst) {
            return Err(PcscFailure {
                status: StatusCode::BAD_GATEWAY,
                code: "reader_io_unconfirmed",
                message: "physical read result unknown".into(),
                submitted: true,
                uncertain: true,
            });
        }
        Ok(vec![0x6a, 0x82])
    }
}
struct Fixture {
    service: Service,
    ordinary: Arc<crate::native_vowifi_session::NativeLifecycle>,
    trace: Arc<Trace>,
    inbox: crate::message_archive::InboxState,
    server: tokio::task::JoinHandle<()>,
    socket: PathBuf,
}

fn automatic_inventory() -> Arc<Mutex<proletarius_reader::ReaderDiscovery>> {
    let mut discovery = proletarius_reader::ReaderDiscovery::default();
    discovery.reconcile(
        vec![proletarius_reader::ReaderObservation {
            name: "Reader".into(),
            card: proletarius_reader::CardPresence::Present,
            in_use: false,
            muted: false,
            atr: vec![],
        }],
        None,
    );
    Arc::new(Mutex::new(discovery))
}

#[tokio::test]
async fn automatic_selection_rechecks_inventory_and_generation_in_the_retained_owner() {
    for drift in [
        "revision",
        "generation",
        "instance",
        "duplicate",
        "retained",
    ] {
        let f = Fixture::new();
        let inventory = automatic_inventory();
        let mut proposal = crate::reader_inventory::propose(
            &inventory.lock().unwrap().snapshot(),
            &f.service.snapshot().selection,
            "instance",
        )
        .unwrap();
        match drift {
            "revision" => {
                proposal.inventory_revision += 1;
            }
            "generation" => {
                proposal.reader_generation += 1;
            }
            "instance" => {
                proposal.instance_id = "old-instance".into();
            }
            "duplicate" => {
                let rows = inventory.lock().unwrap().snapshot().readers;
                inventory
                    .lock()
                    .unwrap()
                    .reconcile(vec![rows[0].clone(), rows[0].clone()], None);
            }
            "retained" => {
                f.service.connect("Other").await.unwrap();
            }
            _ => unreachable!(),
        }
        let opened = f.trace.opened.load(Ordering::SeqCst);
        let result = f
            .service
            .connect_automatic(proposal, inventory, "instance".into())
            .await;
        assert_eq!(
            result.unwrap_err().code,
            "reader_auto_connect_stale",
            "{drift}"
        );
        assert_eq!(
            f.trace.opened.load(Ordering::SeqCst),
            opened,
            "stale proposal must not open physical I/O: {drift}"
        );
        if drift == "retained" {
            assert_eq!(f.service.snapshot().selection.reader, "Other");
        }
    }
}

#[tokio::test]
async fn automatic_selection_queued_before_new_inventory_cannot_connect_after_it() {
    let f = Fixture::new();
    let inventory = automatic_inventory();
    let proposal = crate::reader_inventory::propose(
        &inventory.lock().unwrap().snapshot(),
        &f.service.snapshot().selection,
        "instance",
    )
    .unwrap();
    let serial = f.service.0.serial.lock().await;
    let service = f.service.clone();
    let queued_inventory = inventory.clone();
    let queued = tokio::spawn(async move {
        service
            .connect_automatic(proposal, queued_inventory, "instance".into())
            .await
    });
    tokio::task::yield_now().await;
    inventory.lock().unwrap().reconcile(vec![], None);
    drop(serial);
    assert_eq!(
        queued.await.unwrap().unwrap_err().code,
        "reader_auto_connect_stale"
    );
    assert_eq!(f.trace.opened.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn automatic_selection_uses_the_existing_connect_owner_on_a_matching_proposal() {
    let f = Fixture::new();
    let inventory = automatic_inventory();
    let proposal = crate::reader_inventory::propose(
        &inventory.lock().unwrap().snapshot(),
        &f.service.snapshot().selection,
        "instance",
    )
    .unwrap();
    let result = f
        .service
        .connect_automatic(proposal, inventory, "instance".into())
        .await
        .unwrap();
    assert_eq!(result.selection.reader, "Reader");
    assert_eq!(f.trace.opened.load(Ordering::SeqCst), 1);
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}
impl Fixture {
    fn new() -> Self {
        let gate = Arc::new(Gate::default());
        gate.initialize_native_reader_producer("native");
        gate.initialize_business_producer("go-current");
        let trace = Arc::new(Trace::default());
        let opening = trace.clone();
        let resetting = trace.clone();
        let owner = Owner::with_factory(
            "native".into(),
            Arc::new(move |_| {
                opening.opened.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Fake(opening.clone())))
            }),
            Arc::new(|| Ok(vec!["Reader".into(), "Other".into()])),
            Arc::new(move |_, _| Ok(Box::new(Fake(resetting.clone())))),
        );
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap()
            .join("build");
        std::fs::create_dir_all(&root).unwrap();
        // The source checkout can live under an arbitrarily long audit-capsule
        // path. Keep the AF_UNIX fixture independent of CARGO_MANIFEST_DIR so
        // Darwin's fixed sun_path field cannot turn an otherwise valid cold
        // build into a test failure.
        let socket = std::env::temp_dir().join(format!(
            "sw-rl-{}-{}.sock",
            std::process::id(),
            &crate::multistandby_store::random_token().unwrap()[..12]
        ));
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let bridge_trace = trace.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let bridge_trace = bridge_trace.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(
                        move |request: hyper::Request<hyper::body::Incoming>| {
                            let bridge_trace = bridge_trace.clone();
                            async move {
                                use http_body_util::BodyExt;
                                let bytes = request.into_body().collect().await.unwrap().to_bytes();
                                let input: Value = serde_json::from_slice(&bytes).unwrap();
                                if input["action"] == "query_busy" {
                                    bridge_trace.queries.fetch_add(1, Ordering::SeqCst);
                                }
                                if input["action"] == "retire_generation" {
                                    bridge_trace.retirements.fetch_add(1, Ordering::SeqCst);
                                }
                                let status = if (input["action"] == "retire_generation"
                                    && bridge_trace.retire_reject.load(Ordering::SeqCst))
                                    || (input["action"] == "stop_mutation"
                                        && bridge_trace.stop_reject.load(Ordering::SeqCst))
                                {
                                    StatusCode::BAD_GATEWAY
                                } else {
                                    StatusCode::OK
                                };
                                if input["action"] == "stop_mutation" {
                                    bridge_trace.stops.fetch_add(1, Ordering::SeqCst);
                                }
                                Ok::<_, std::convert::Infallible>(crate::json_response(
                                    status,
                                    &json!({"ok":true,"producer_id":input["producer_id"],"identity_busy":bridge_trace.busy.load(Ordering::SeqCst),"selection_busy":bridge_trace.busy.load(Ordering::SeqCst)}),
                                ))
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        let inbox = crate::message_archive::InboxState::for_go_producer("go-current".into());
        let availability = crate::availability::BackendAvailability::new();
        availability.mark_ready();
        let service = Service::new(
            root.join("runtime/reader-lifecycle-tests"),
            gate,
            owner,
            None,
            None,
            "native".into(),
        )
        .unwrap();
        let ordinary = Arc::new(crate::native_vowifi_session::NativeLifecycle::default());
        ordinary
            .ordinary_admission()
            .set_go_producer("go-current")
            .unwrap();
        service.set_ordinary_lifecycle(Arc::downgrade(&ordinary));
        service.set_business_bridge(crate::reader_business_runtime::Bridge::new(inbox.clone()));
        Self {
            service,
            ordinary,
            trace,
            inbox,
            server,
            socket,
        }
    }
    async fn settled(&self) {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if self.service.0.life.lock().unwrap().jobs == 0 {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
fn physical() -> LifecycleBinding {
    LifecycleBinding {
        producer: "native".into(),
        reader: "Reader".into(),
        session_id: "session".into(),
        session_sequence: 1,
    }
}
fn facts(iccid: &str) -> NativeReaderFacts {
    NativeReaderFacts {
        metadata: Some(
            json!({"iccid":iccid,"imsi":"454030000000000","mcc":"454","mnc":"03","smsc_psi":"sip:smsc@ims.test","smsc_target":"sip:smsc@ims.test","phone_number":""}),
        ),
        has_esim: true,
        notification_aids: vec!["a0000005591010ffffffff8900000100".into()],
        profile_name: "Card profile".into(),
        ble_connection: None,
    }
}
fn selected() -> Arc<Gate> {
    let gate = Arc::new(Gate::default());
    gate.initialize_native_reader_producer("native");
    gate.initialize_business_producer("go-current");
    gate.initialize_native_lifecycle("native").unwrap();
    let transition = gate
        .prepare_native_transition(
            "native",
            &SelectionSnapshot::default(),
            Kind::Connect,
            "Reader",
            NativeAuthority::Idle,
        )
        .unwrap();
    gate.commit_native_transition(&transition, Some(&physical()), facts("8986000000000000000"))
        .unwrap();
    gate
}

#[tokio::test]
async fn connect_without_ready_sim_has_native_session_and_card_only_supplier() {
    let fixture = Fixture::new();
    let snapshot = fixture.service.connect("Reader").await.unwrap();
    assert_eq!(snapshot.selection.generation, 1);
    assert!(snapshot.usim_info.is_none());
    assert!(snapshot.pcsc_session.is_some());
    assert_eq!(snapshot.status["has_card"], false);
    assert_eq!(snapshot.status["card_state"], "unknown");
    let observed = fixture
        .service
        .observed_snapshot_with(|_| async {
            presence_sample(proletarius_reader::CardPresence::Present)
        })
        .await
        .unwrap();
    assert_eq!(observed.status["has_card"], true);
    assert!(observed.usim_info.is_none());
    assert_eq!(observed.status["metadata_ready"], false);
    assert_eq!(observed.status["session_ready"], true);
    let binding = fixture
        .service
        .0
        .gate
        .begin_native_pin("Reader", NativePinPolicy::CardOnly)
        .unwrap();
    let supplier = NativeSessionSupplier::snapshot(&fixture.service, &binding).unwrap();
    assert_eq!(supplier["identity"]["iccid"], "");
    assert!(supplier["session"].is_object());
    assert_eq!(fixture.trace.opened.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.service.connect("Reader").await.unwrap_err().code,
        "card_busy"
    );
    fixture.service.0.gate.drain_native_pin(&binding).unwrap();
    fixture.service.0.gate.finish_native_pin(&binding).unwrap();
    fixture.service.disconnect().await.unwrap();
}
#[tokio::test]
async fn missing_new_reader_after_confirmed_old_close_commits_disconnected_generation() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    assert!(fixture.service.connect("Missing").await.is_err());
    let snapshot = fixture.service.snapshot();
    assert!(!snapshot.selection.pending);
    assert!(snapshot.selection.reader.is_empty());
    assert_eq!(snapshot.selection.generation, 2);
    assert!(snapshot.usim_info.is_none());
    assert!(snapshot.pcsc_session.is_none());
    assert!(fixture.trace.closed.load(Ordering::SeqCst) > 0);
}
#[tokio::test]
async fn cancelled_connect_retains_real_worker_and_shutdown_waits_for_completion() {
    let fixture = Fixture::new();
    let (started, ready) = std::sync::mpsc::sync_channel(1);
    let (release, wait) = std::sync::mpsc::channel();
    *fixture.trace.block.lock().unwrap() = Some((started, wait));
    let service = fixture.service.clone();
    let waiter = tokio::spawn(async move { service.connect("Reader").await });
    tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(3)).unwrap())
        .await
        .unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(fixture.service.snapshot().selection.pending);
    assert!(
        fixture
            .service
            .0
            .gate
            .begin_native_pin("Reader", NativePinPolicy::CardOnly)
            .is_err()
    );
    fixture.service.begin_shutdown();
    let service = fixture.service.clone();
    let finish = tokio::spawn(async move { service.finish_shutdown().await });
    tokio::task::yield_now().await;
    assert!(!finish.is_finished());
    release.send(()).unwrap();
    finish.await.unwrap().unwrap();
    assert_eq!(fixture.service.snapshot().selection.reader, "Reader");
}
#[tokio::test]
async fn failed_refresh_preserves_confirmed_old_metadata_and_generation() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let old = fixture.service.snapshot();
    let transition = fixture
        .service
        .0
        .gate
        .prepare_native_transition(
            "native",
            &old.selection,
            Kind::Identity,
            "Reader",
            NativeAuthority::Idle,
        )
        .unwrap();
    fixture
        .service
        .commit(
            &transition,
            old.pcsc_session.as_ref(),
            facts("8986000000000000000"),
        )
        .unwrap();
    let before = fixture.service.snapshot();
    assert!(fixture.service.refresh().await.is_err());
    let after = fixture.service.snapshot();
    assert_eq!(after.selection, before.selection);
    assert_eq!(after.usim_info, before.usim_info);
    assert!(after.esim_manager_available);
}
#[test]
fn identity_commit_atomically_advances_generation_and_full_routing_facts() {
    let gate = selected();
    let before = gate.selection_snapshot();
    let transition = gate
        .prepare_native_transition(
            "native",
            &before,
            Kind::Identity,
            "Reader",
            NativeAuthority::Idle,
        )
        .unwrap();
    assert!(gate.reader_metadata("Reader", before.generation).is_none());
    let next = facts("8986000000000000001");
    let after = gate
        .commit_native_transition(&transition, Some(&physical()), next.clone())
        .unwrap();
    assert_eq!(after.generation, before.generation + 1);
    assert!(gate.reader_metadata("Reader", before.generation).is_none());
    assert_eq!(
        gate.reader_metadata("Reader", after.generation).unwrap()["smsc_target"],
        "sip:smsc@ims.test"
    );
    assert_eq!(
        gate.commit_native_transition(&transition, Some(&physical()), next.clone())
            .unwrap(),
        after
    );
    let mut conflict = next;
    conflict.profile_name = "different".into();
    assert_eq!(
        gate.commit_native_transition(&transition, Some(&physical()), conflict)
            .unwrap_err(),
        "native_reader_receipt_conflict"
    );
    assert!(
        gate.prepare_native_transition(
            "native",
            &before,
            Kind::Identity,
            "Reader",
            NativeAuthority::Idle
        )
        .is_err()
    );
}
#[test]
fn native_transition_cannot_abort_or_publish_before_actual_worker_drain() {
    let gate = selected();
    let before = gate.selection_snapshot();
    let transition = gate
        .prepare_native_transition(
            "native",
            &before,
            Kind::Identity,
            "Reader",
            NativeAuthority::Idle,
        )
        .unwrap();
    let worker = gate.track_native_lifecycle_worker(&transition).unwrap();
    assert_eq!(
        gate.abort_native_transition(&transition).unwrap_err(),
        "reader_physical_work_pending"
    );
    assert_eq!(
        gate.commit_native_transition(&transition, Some(&physical()), facts("8986000000000000000"))
            .unwrap_err(),
        "reader_physical_work_pending"
    );
    drop(worker);
    gate.abort_native_transition(&transition).unwrap();
    assert_eq!(gate.selection_snapshot(), before);
}
#[test]
fn stale_phone_update_and_go_replacement_cannot_mutate_native_reader_facts() {
    let gate = selected();
    let generation = gate.selection_snapshot().generation;
    assert!(gate.native_phone_update("Reader", generation, "+85212345678"));
    assert!(!gate.native_phone_update("Reader", generation + 1, "wrong"));
    gate.test_confirm_go_exit("go-replacement");
    let (selection, facts, session) = gate.native_reader_snapshot();
    assert_eq!(selection.reader, "Reader");
    assert_eq!(
        facts.unwrap().metadata.unwrap()["phone_number"],
        "+85212345678"
    );
    assert_eq!(session.unwrap(), physical());
}
#[tokio::test]
async fn invalid_aid_or_target_is_rejected_before_any_physical_reset() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let calls = fixture.trace.calls.load(Ordering::SeqCst);
    for input in [
        json!({"action":"rebuild","operation_id":"unique","aid":"zz"}),
        json!({"action":"rebuild","operation_id":"unique","target_iccid":"abc"}),
    ] {
        assert!(fixture.service.handle_service(input).await.is_err());
    }
    assert_eq!(fixture.trace.calls.load(Ordering::SeqCst), calls);
    assert!(!fixture.service.snapshot().selection.pending);
}
#[test]
fn native_finalizer_does_not_reenter_supplier_worker_and_advances_original_pin() {
    let gate = selected();
    let pin = gate
        .begin_native_pin("Reader", NativePinPolicy::Mutation)
        .unwrap();
    gate.activate_native_pin(&pin).unwrap();
    gate.begin_native_finalize(&pin).unwrap();
    let supplier_worker = gate.track_native_supplier_worker(&pin, true).unwrap();
    let transaction = CardTransaction::Native {
        binding: pin.clone(),
        purpose: "finalize".into(),
        operation_sequence: Some(1),
    };
    let transition = gate
        .prepare_native_transition(
            "native",
            &gate.selection_snapshot(),
            Kind::Rebuild,
            "Reader",
            NativeAuthority::Transaction(transaction),
        )
        .unwrap();
    let physical_worker = gate.track_native_lifecycle_worker(&transition).unwrap();
    assert!(gate.retire_native_finalizer_selection(&pin).is_err());
    drop(physical_worker);
    let committed = gate
        .commit_native_transition(&transition, Some(&physical()), facts("8986000000000000001"))
        .unwrap();
    gate.advance_native_pin_generation(&pin, committed.generation)
        .unwrap();
    drop(supplier_worker);
    assert!(!gate.retire_native_finalizer_selection(&pin).unwrap());
    gate.finish_native_finalize(&pin).unwrap();
    assert_eq!(gate.selection_snapshot().reader, "Reader");
    assert_eq!(
        gate.native_reader_snapshot().1.unwrap().metadata.unwrap()["iccid"],
        "8986000000000000001"
    );
}

#[tokio::test]
async fn selection_retirement_uses_held_all_ordinary_fence_and_actual_close_without_go_ack() {
    let fixture = Fixture::new();
    // A legacy bridge rejecting every retirement cannot override Rust's exact
    // empty ordinary fence and physically completed transition.
    fixture.trace.retire_reject.store(true, Ordering::SeqCst);
    fixture.service.connect("Reader").await.unwrap();
    fixture.service.connect("Other").await.unwrap();
    assert_eq!(fixture.trace.closed.load(Ordering::SeqCst), 1);
    fixture.service.disconnect().await.unwrap();
    assert_eq!(fixture.trace.closed.load(Ordering::SeqCst), 2);
    assert!(fixture.service.snapshot().selection.reader.is_empty());
    assert_eq!(fixture.trace.queries.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.trace.retirements.load(Ordering::SeqCst), 0);
    fixture.settled().await;
}

#[tokio::test]
async fn borrowed_rebuild_during_shutdown_freezes_receipt_and_never_replays_reset() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let before = fixture.service.snapshot();
    let operation = json!({"producer_id":"go-current","operation_token":"operation-token-000000000000000000000000000","operation_sequence":1,"reader":"Reader","reader_generation":1,"purpose":"card","authority":{"kind":"idle"}});
    fixture
        .service
        .0
        .gate
        .test_operation_command("acquire", operation.clone())
        .unwrap();
    let pcsc = Binding {
        producer_id: "native".into(),
        reader: "Reader".into(),
        session_id: before.pcsc_session.unwrap().session_id,
        lease_id: "native-physical-operation-000000000000000000".into(),
        lease_seq: 71,
    };
    fixture
        .service
        .0
        .owner
        .native_acquire_binding(&pcsc, Duration::from_secs(120), Arc::new(|| true))
        .unwrap();
    fixture.service.begin_shutdown();
    assert_eq!(
        fixture.service.refresh().await.unwrap_err().code,
        "backend_stopping"
    );
    let request = json!({"action":"rebuild","operation_id":"once-000000000000000000000000000000000000","reader":"Reader","expected_generation":1,"transaction":{"kind":"operation","binding":operation},"pcsc":pcsc,"reset":false});
    let first = fixture
        .service
        .handle_service(request.clone())
        .await
        .unwrap();
    assert_eq!(first["snapshot"]["selection"]["generation"], 2);
    let calls = fixture.trace.calls.load(Ordering::SeqCst);
    let opens = fixture.trace.opened.load(Ordering::SeqCst);
    assert_eq!(
        fixture
            .service
            .handle_service(request.clone())
            .await
            .unwrap(),
        first
    );
    assert_eq!(fixture.trace.calls.load(Ordering::SeqCst), calls);
    assert_eq!(fixture.trace.opened.load(Ordering::SeqCst), opens);
    let mut conflict = request;
    conflict["reset"] = json!(true);
    assert_eq!(
        fixture
            .service
            .handle_service(conflict)
            .await
            .unwrap_err()
            .code,
        "reader_receipt_conflict"
    );
    fixture.service.0.owner.release(&pcsc).unwrap();
    fixture.service.finish_shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_card_binding_aborts_prepared_transition_without_io_or_identity_loss() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let before = fixture.service.snapshot();
    let operation = json!({"producer_id":"go-current","operation_token":"operation-token-000000000000000000000000000","operation_sequence":1,"reader":"Reader","reader_generation":1,"purpose":"card","authority":{"kind":"idle"}});
    fixture
        .service
        .0
        .gate
        .test_operation_command("acquire", operation.clone())
        .unwrap();
    let bad = Binding {
        producer_id: "native".into(),
        reader: "Reader".into(),
        session_id: "replaced-session".into(),
        lease_id: "operation-physical-000000000000000000000000".into(),
        lease_seq: 71,
    };
    let calls = fixture.trace.calls.load(Ordering::SeqCst);
    assert!(fixture.service.handle_service(json!({"action":"refresh","reader":"Reader","expected_generation":1,"transaction":{"kind":"operation","binding":operation},"pcsc":bad})).await.is_err());
    assert_eq!(fixture.service.snapshot().selection, before.selection);
    assert!(!fixture.service.snapshot().selection.pending);
    assert_eq!(fixture.trace.calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn service_native_finalizer_borrows_exact_worker_and_projects_real_identity_warning() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let snapshot = fixture.service.snapshot();
    let pin = fixture
        .service
        .0
        .gate
        .begin_native_pin("Reader", NativePinPolicy::Mutation)
        .unwrap();
    fixture.service.0.gate.activate_native_pin(&pin).unwrap();
    let pcsc = fixture
        .service
        .0
        .owner
        .native_pin_binding(snapshot.pcsc_session.as_ref().unwrap(), &pin)
        .unwrap();
    fixture
        .service
        .0
        .owner
        .native_acquire_binding(&pcsc, Duration::from_secs(120), Arc::new(|| true))
        .unwrap();
    fixture.service.0.gate.begin_native_finalize(&pin).unwrap();
    let actual_supplier = fixture
        .service
        .0
        .gate
        .track_native_supplier_worker(&pin, true)
        .unwrap();
    let tx = CardTransaction::Native {
        binding: pin.clone(),
        purpose: "finalize".into(),
        operation_sequence: Some(1),
    };
    let (snapshot, finalized) = fixture
        .service
        .observe_inner(
            Some(tx),
            Some(pcsc.clone()),
            true,
            "8986000000000000000",
            "",
            None,
        )
        .await
        .unwrap();
    let finalized = finalized.unwrap();
    assert_eq!(snapshot.selection.generation, 2);
    assert!(!finalized.target_identity_confirmed);
    assert!(
        finalized
            .reader_refresh_warning_code
            .contains("switch_reader_identity_unconfirmed")
    );
    assert!(
        finalized
            .reader_refresh_warning
            .contains("应用没有对未经验证的读卡器强制复位")
    );
    assert!(fixture.service.0.gate.native_pin_producer_current(&pin));
    drop(actual_supplier);
    assert!(
        !fixture
            .service
            .0
            .gate
            .retire_native_finalizer_selection(&pin)
            .unwrap()
    );
    fixture.service.0.gate.finish_native_finalize(&pin).unwrap();
    fixture.service.0.owner.release(&pcsc).unwrap();
    fixture.service.0.gate.drain_native_pin(&pin).unwrap();
    fixture.service.0.gate.finish_native_pin(&pin).unwrap();
    fixture.service.disconnect().await.unwrap();
}

#[tokio::test]
async fn bounded_fresh_admission_reserves_cleanup_and_drains_cancelled_waiters() {
    let fixture = Fixture::new();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let mut waiters = Vec::new();
    for _ in 0..16 {
        let service = fixture.service.clone();
        let release = release.clone();
        waiters.push(tokio::spawn(async move {
            service
                .retained(false, move |_| async move {
                    release.acquire_owned().await.unwrap().forget();
                    Ok(())
                })
                .await
        }));
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if fixture.service.0.life.lock().unwrap().fresh_jobs == 16 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fixture
            .service
            .retained(false, |_| async { Ok(()) })
            .await
            .unwrap_err()
            .code,
        "reader_busy"
    );
    let service = fixture.service.clone();
    let cleanup = tokio::spawn(async move { service.retained(true, |_| async { Ok(7u8) }).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if fixture.service.0.life.lock().unwrap().cleanup_jobs == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for waiter in waiters {
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
    }
    fixture.service.begin_shutdown();
    assert_eq!(
        fixture
            .service
            .retained(false, |_| async { Ok(()) })
            .await
            .unwrap_err()
            .code,
        "backend_stopping"
    );
    let service = fixture.service.clone();
    let shutdown = tokio::spawn(async move { service.finish_shutdown().await });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    release.add_permits(16);
    assert_eq!(cleanup.await.unwrap().unwrap(), 7);
    shutdown.await.unwrap().unwrap();
    assert_eq!(fixture.service.0.life.lock().unwrap().jobs, 0);
}

#[tokio::test]
async fn mutation_requires_exact_drained_native_maintenance_without_go_socket() {
    let fixture = Fixture::new();
    let snapshot = fixture.service.connect("Reader").await.unwrap();
    fixture.server.abort();
    let service = Arc::new(fixture.service.clone());
    assert!(
        service
            .clone()
            .pre_admit("Reader".into(), NativePinPolicy::Mutation, None, None)
            .await
            .is_err()
    );
    let gate = fixture.ordinary.ordinary_admission();
    let wrong = gate
        .begin_reader_maintenance(
            "Other",
            snapshot.selection.generation,
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain,
        )
        .unwrap();
    assert!(
        service
            .clone()
            .pre_admit(
                "Reader".into(),
                NativePinPolicy::Mutation,
                None,
                Some(wrong.clone())
            )
            .await
            .is_err()
    );
    wrong.release_confirmed().unwrap();
    for (generation, policy) in [
        (
            snapshot.selection.generation + 1,
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain,
        ),
        (
            snapshot.selection.generation,
            crate::ordinary_vowifi_admission::MaintenancePolicy::RejectAllOrdinary,
        ),
    ] {
        let wrong = gate
            .begin_reader_maintenance("Reader", generation, policy)
            .unwrap();
        assert!(
            service
                .clone()
                .pre_admit(
                    "Reader".into(),
                    NativePinPolicy::Mutation,
                    None,
                    Some(wrong.clone())
                )
                .await
                .is_err()
        );
        wrong.release_confirmed().unwrap();
    }
    let old = gate
        .acquire_native_reader("still-running", 1, "Reader", snapshot.selection.generation)
        .unwrap();
    let pending = gate
        .begin_reader_maintenance(
            "Reader",
            snapshot.selection.generation,
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain,
        )
        .unwrap();
    assert!(!pending.ordinary_drained());
    assert!(
        service
            .clone()
            .pre_admit(
                "Reader".into(),
                NativePinPolicy::Mutation,
                None,
                Some(pending.clone())
            )
            .await
            .is_err()
    );
    assert!(old.is_current());
    assert!(pending.release_confirmed().is_err());
    old.release_confirmed().unwrap();
    pending.release_confirmed().unwrap();
    let maintenance = gate
        .begin_reader_maintenance(
            "Reader",
            snapshot.selection.generation,
            crate::ordinary_vowifi_admission::MaintenancePolicy::StopAndDrain,
        )
        .unwrap();
    service
        .clone()
        .drain_reader_maintenance(maintenance.clone())
        .await
        .unwrap();
    assert_eq!(
        service
            .pre_admit(
                "Reader".into(),
                NativePinPolicy::Mutation,
                None,
                Some(maintenance.clone())
            )
            .await
            .unwrap(),
        snapshot.selection.generation
    );
    assert_eq!(fixture.trace.stops.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.trace.queries.load(Ordering::SeqCst), 0);
    maintenance.release_confirmed().unwrap();
    fixture.service.disconnect().await.unwrap();
}

#[tokio::test]
async fn shutdown_waits_for_pending_pre_even_between_panic_and_reconcile_jobs() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let pre = fixture
        .service
        .0
        .gate
        .begin_native_pre_admission("Reader", NativePinPolicy::Mutation)
        .unwrap();
    assert_eq!(fixture.service.0.life.lock().unwrap().jobs, 0);
    let service = fixture.service.clone();
    let shutdown = tokio::spawn(async move { service.finish_shutdown().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(25), async {
            loop {
                if shutdown.is_finished() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err()
    );
    fixture
        .service
        .0
        .gate
        .finish_native_pre_admission(&pre)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn maintenance_rejects_retained_native_intent_without_a_physical_pin() {
    let fixture = Fixture::new();
    let before = fixture.service.connect("Reader").await.unwrap();
    let admission = fixture.ordinary.ordinary_admission();
    let intent = admission
        .acquire_native_reader("backoff", 1, "Reader", before.selection.generation)
        .unwrap();
    let calls = fixture.trace.calls.load(Ordering::SeqCst);
    let closed = fixture.trace.closed.load(Ordering::SeqCst);
    for failure in [
        fixture.service.refresh().await.unwrap_err(),
        fixture.service.disconnect().await.unwrap_err(),
        fixture.service.connect("Other").await.unwrap_err(),
    ] {
        assert_eq!(failure.code, "reader_operation_active");
    }
    assert_eq!(fixture.trace.calls.load(Ordering::SeqCst), calls);
    assert_eq!(fixture.trace.closed.load(Ordering::SeqCst), closed);
    assert!(intent.is_current());
    assert_eq!(fixture.trace.stops.load(Ordering::SeqCst), 0);
    intent.release_confirmed().unwrap();
    assert_eq!(
        fixture.service.refresh().await.unwrap_err().code,
        "reader_metadata_failed"
    );
    assert!(fixture.trace.calls.load(Ordering::SeqCst) > calls);
}

#[tokio::test]
async fn cancelled_refresh_keeps_shared_maintenance_until_actual_apdu_exit() {
    let fixture = Fixture::new();
    let before = fixture.service.connect("Reader").await.unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    *fixture.trace.block.lock().unwrap() = Some((entered_tx, finish_rx));
    let service = fixture.service.clone();
    let waiter = tokio::spawn(async move { service.refresh().await });
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(3)).unwrap())
        .await
        .unwrap();
    let admission = fixture.ordinary.ordinary_admission();
    assert!(
        admission
            .acquire_native_reader("replacement", 1, "Reader", before.selection.generation)
            .is_err()
    );
    assert!(
        admission
            .acquire_go("go-current", "module", "late-module", 1)
            .is_err()
    );
    assert!(
        admission
            .acquire_go("go-current", "reader", "late-reader", 1)
            .is_err()
    );
    waiter.abort();
    let _ = waiter.await;
    assert!(fixture.service.snapshot().selection.pending);
    assert!(
        admission
            .acquire_native_reader("replacement", 1, "Reader", before.selection.generation)
            .is_err()
    );
    assert!(
        admission
            .acquire_go("go-current", "module", "late-module", 1)
            .is_err()
    );
    assert_eq!(fixture.trace.queries.load(Ordering::SeqCst), 0);
    finish_tx.send(()).unwrap();
    fixture.settled().await;
    let snapshot = fixture.service.snapshot();
    assert!(!snapshot.selection.pending);
    let next = admission
        .acquire_native_reader("replacement", 1, "Reader", snapshot.selection.generation)
        .unwrap();
    next.release_confirmed().unwrap();
    let module = admission
        .acquire_go("go-current", "module", "late-module", 1)
        .unwrap();
    module.release_confirmed().unwrap();
}

#[tokio::test]
async fn module_busy_is_rejected_without_stopping_and_preserves_shared_owner() {
    let fixture = Fixture::new();
    let before = fixture.service.connect("Reader").await.unwrap();
    let admission = fixture.ordinary.ordinary_admission();
    let module = admission
        .acquire_go("go-current", "module", "module-fixed", 1)
        .unwrap();
    fixture.trace.busy.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture.service.refresh().await.unwrap_err().code,
        "reader_operation_active"
    );
    assert_eq!(
        fixture.service.disconnect().await.unwrap_err().code,
        "reader_operation_active"
    );
    assert_eq!(fixture.trace.stops.load(Ordering::SeqCst), 0);
    assert!(module.is_current());
    module.release_confirmed().unwrap();
    fixture.trace.busy.store(false, Ordering::SeqCst);
    assert_eq!(
        fixture.service.refresh().await.unwrap_err().code,
        "reader_metadata_failed"
    );
    assert_eq!(
        fixture.service.snapshot().selection.generation,
        before.selection.generation
    );
}

#[tokio::test]
async fn uncertain_physical_read_keeps_fence_through_actual_close_confirmation() {
    let fixture = Fixture::new();
    let before = fixture.service.connect("Reader").await.unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    *fixture.trace.close_block.lock().unwrap() = Some((entered_tx, finish_rx));
    fixture.trace.fail_apdu.store(true, Ordering::SeqCst);
    let service = fixture.service.clone();
    let task = tokio::spawn(async move { service.refresh().await });
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(3)).unwrap())
        .await
        .unwrap();
    assert!(fixture.service.snapshot().selection.pending);
    assert!(!task.is_finished());
    assert_eq!(fixture.trace.closed.load(Ordering::SeqCst), 0);
    assert!(
        fixture
            .ordinary
            .ordinary_admission()
            .acquire_native_reader("new", 1, "Reader", before.selection.generation)
            .is_err()
    );
    finish_tx.send(()).unwrap();
    assert_eq!(
        task.await.unwrap().unwrap_err().code,
        "reader_pcsc_stale_session"
    );
    assert_eq!(fixture.trace.closed.load(Ordering::SeqCst), 1);
    assert!(!fixture.service.snapshot().selection.pending);
    let next = fixture
        .ordinary
        .ordinary_admission()
        .begin_reader_maintenance(
            "Other",
            0,
            crate::ordinary_vowifi_admission::MaintenancePolicy::Reject,
        )
        .unwrap();
    next.release_confirmed().unwrap();
}

#[tokio::test]
async fn unknown_transition_does_not_release_from_a_later_clear_snapshot() {
    let fixture = Fixture::new();
    let before = fixture.service.connect("Reader").await.unwrap();
    let admission = fixture.ordinary.ordinary_admission();
    let maintenance = admission
        .begin_reader_maintenance(
            "Reader",
            before.selection.generation,
            crate::ordinary_vowifi_admission::MaintenancePolicy::Reject,
        )
        .unwrap();
    let transition = fixture
        .service
        .0
        .gate
        .prepare_native_transition(
            "native",
            &before.selection,
            Kind::Identity,
            "Reader",
            NativeAuthority::Idle,
        )
        .unwrap();
    fixture
        .service
        .0
        .gate
        .quarantine_native_transition(&transition)
        .unwrap();
    fixture.service.finish_maintenance(maintenance.clone());
    assert!(maintenance.is_current());
    // A separate cleanup owner can clear its exact physical transition. That
    // later snapshot alone does not retroactively confirm this fence owner.
    fixture
        .service
        .0
        .owner
        .native_close(before.pcsc_session.as_ref().unwrap())
        .unwrap();
    fixture
        .service
        .0
        .gate
        .complete_native_failed_connect(&transition)
        .unwrap();
    assert!(!fixture.service.snapshot().selection.pending);
    tokio::task::yield_now().await;
    assert!(maintenance.is_current());
    assert!(
        admission
            .acquire_native_reader("new", 1, "Reader", before.selection.generation)
            .is_err()
    );
    maintenance.release_confirmed().unwrap();
}

#[tokio::test]
async fn ordinary_supplier_requires_claim_from_the_same_shared_gate() {
    let fixture = Fixture::new();
    let before = fixture.service.connect("Reader").await.unwrap();
    let foreign_gate = crate::ordinary_vowifi_admission::Gate::default();
    let foreign = foreign_gate
        .acquire_native_reader("foreign", 1, "Reader", before.selection.generation)
        .unwrap();
    assert!(
        foreign
            .validate_reader_start("Reader", before.selection.generation)
            .is_ok()
    );
    let result = fixture.service.capture_reader_maintenance(
        "Reader",
        before.selection.generation,
        NativePinPolicy::Identity,
        Some(&foreign),
    );
    assert!(matches!(
        result,
        Err(
            proletarius_domain::BackendFailure::ReaderAdmissionRejected {
                code: "ordinary_vowifi_claim_stale"
            }
        )
    ));
    let own = fixture
        .ordinary
        .ordinary_admission()
        .acquire_native_reader("own", 1, "Reader", before.selection.generation)
        .unwrap();
    assert!(matches!(
        fixture.service.capture_reader_maintenance(
            "Reader",
            before.selection.generation,
            NativePinPolicy::Identity,
            Some(&own)
        ),
        Ok(None)
    ));
    own.release_confirmed().unwrap();
}

#[tokio::test]
async fn card_only_supplier_preserves_module_and_fences_native_until_real_lease_release() {
    let fixture = Fixture::new();
    let before = fixture.service.connect("Reader").await.unwrap();
    let supplier: Arc<dyn NativeSessionSupplier> = Arc::new(fixture.service.clone());
    fixture
        .service
        .0
        .gate
        .set_native_session_supplier(Arc::downgrade(&supplier));
    let admission = fixture.ordinary.ordinary_admission();
    let module = admission
        .acquire_go("go-current", "module", "fixed-module", 1)
        .unwrap();
    let lease = crate::reader_session_lease::NativeReaderSessionLease::acquire_for_card_operation(
        &fixture.socket,
        None,
        "Reader",
        fixture.service.0.gate.clone(),
        fixture.service.0.owner.clone(),
    )
    .await
    .unwrap();
    assert!(module.is_current());
    assert_eq!(fixture.trace.stops.load(Ordering::SeqCst), 0);
    module.release_confirmed().unwrap();
    assert!(
        admission
            .acquire_native_reader("next", 1, "Reader", before.selection.generation)
            .is_err()
    );
    // Module uses another physical device and remains admitted under a
    // reader-only card fence, matching the existing ordinary module policy.
    let module = admission
        .acquire_go("go-current", "module", "next-module", 2)
        .unwrap();
    module.release_confirmed().unwrap();
    lease.release().await.unwrap();
    assert!(lease.release_confirmed().await.unwrap());
    let next = admission
        .acquire_native_reader("next", 1, "Reader", before.selection.generation)
        .unwrap();
    next.release_confirmed().unwrap();
}

#[tokio::test]
async fn mutation_without_go_rpc_keeps_maintenance_until_cancelled_apdu_really_finishes() {
    let fixture = Fixture::new();
    let before = fixture.service.connect("Reader").await.unwrap();
    let supplier: Arc<dyn NativeSessionSupplier> = Arc::new(fixture.service.clone());
    fixture
        .service
        .0
        .gate
        .set_native_session_supplier(Arc::downgrade(&supplier));
    let lease = Arc::new(
        crate::reader_session_lease::NativeReaderSessionLease::acquire_for_esim_mutation(
            &fixture.socket,
            None,
            "Reader",
            fixture.service.0.gate.clone(),
            fixture.service.0.owner.clone(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(fixture.trace.stops.load(Ordering::SeqCst), 0);
    let context = lease.pcsc_context(fixture.service.0.owner.clone()).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    *fixture.trace.block.lock().unwrap() = Some((entered_tx, finish_rx));
    let work_lease = lease.clone();
    let worker = tokio::spawn(async move {
        work_lease
            .run_card_worker(&crate::blocking_pool::BlockingPool::new(1), move || {
                context.with_transaction(|transport| transport.transmit(&[0, 0xa4, 0, 0]))
            })
            .await
    });
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(3)).unwrap())
        .await
        .unwrap();
    worker.abort();
    let _ = worker.await;
    let draining = lease.clone();
    let release = tokio::spawn(async move { draining.release().await });
    tokio::task::yield_now().await;
    assert!(!release.is_finished());
    assert!(
        fixture
            .ordinary
            .ordinary_admission()
            .acquire_native_reader("next", 1, "Reader", before.selection.generation)
            .is_err()
    );
    assert_eq!(fixture.trace.stops.load(Ordering::SeqCst), 0);
    finish_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(4), release)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(lease.release_confirmed().await.unwrap());
    let next = fixture
        .ordinary
        .ordinary_admission()
        .acquire_native_reader("next", 1, "Reader", before.selection.generation)
        .unwrap();
    next.release_confirmed().unwrap();
}

mod native_ordinary;

#[tokio::test]
async fn native_selection_refresh_and_disconnect_need_no_go_bridge_or_producer_status() {
    let fixture = Fixture::new();
    *fixture.service.0.bridge.lock().unwrap() = None;
    fixture.server.abort();
    let before = fixture.service.connect("Reader").await.unwrap();
    assert!(before.pcsc_session.is_some());
    assert_eq!(before.selection.reader, "Reader");
    assert_eq!(
        fixture.service.refresh().await.unwrap_err().code,
        "reader_metadata_failed"
    );
    assert_eq!(fixture.service.snapshot().selection, before.selection);
    let after = fixture.service.disconnect().await.unwrap();
    assert!(after.selection.reader.is_empty());
    assert!(after.selection.generation > before.selection.generation);
    assert_eq!(fixture.trace.opened.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.trace.closed.load(Ordering::SeqCst), 1);
    assert!(fixture.trace.calls.load(Ordering::SeqCst) > 0);
    assert_eq!(fixture.trace.queries.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.trace.retirements.load(Ordering::SeqCst), 0);
    fixture.settled().await;
}

mod ble_loss;

// These observations inject only scanner facts; all physical lifecycle jobs
// still use the existing retained fake owner, never a system reader.
fn presence_sample(card: proletarius_reader::CardPresence) -> presence::Sample {
    presence::Sample::pcsc(
        "Reader",
        &[proletarius_reader::ReaderCardStatus {
            reader: proletarius_reader::ReaderObservation {
                name: "Reader".into(),
                card,
                in_use: false,
                muted: false,
                atr: vec![],
            },
            unpowered: false,
            event_count: 0,
        }],
    )
}

#[tokio::test]
async fn information_observation_rejects_late_old_selection_without_touching_new_session() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let service = fixture.service.clone();
    let observation = tokio::spawn(async move {
        service
            .observed_snapshot_with(|_| async move {
                started.send(()).unwrap();
                wait.await.unwrap();
                presence_sample(proletarius_reader::CardPresence::Absent)
            })
            .await
    });
    ready.await.unwrap();
    fixture.service.connect("Other").await.unwrap();
    let current = fixture.service.snapshot();
    release.send(()).unwrap();
    assert_eq!(
        observation.await.unwrap().unwrap_err().code,
        "reader_observation_stale"
    );
    assert_eq!(fixture.service.snapshot().selection, current.selection);
    assert_eq!(
        fixture.service.snapshot().pcsc_session,
        current.pcsc_session
    );
    assert_eq!(fixture.trace.opened.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.trace.closed.load(Ordering::SeqCst), 1);
    fixture.service.disconnect().await.unwrap();
    fixture.settled().await;
}

#[tokio::test]
async fn information_observation_rejects_same_generation_metadata_publication() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let publish = || {
        let before = fixture.service.snapshot();
        let transition = fixture
            .service
            .0
            .gate
            .prepare_native_transition(
                "native",
                &before.selection,
                Kind::Identity,
                "Reader",
                NativeAuthority::Idle,
            )
            .unwrap();
        fixture
            .service
            .commit(
                &transition,
                before.pcsc_session.as_ref(),
                facts("8986000000000000000"),
            )
            .unwrap()
    };
    publish();
    let before = fixture.service.snapshot();
    let revision = fixture.service.snapshot_with_epoch().1;
    let calls = fixture.trace.calls.load(Ordering::SeqCst);
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let service = fixture.service.clone();
    let observation = tokio::spawn(async move {
        service
            .observed_snapshot_with(|_| async move {
                started.send(()).unwrap();
                wait.await.unwrap();
                presence_sample(proletarius_reader::CardPresence::Absent)
            })
            .await
    });
    ready.await.unwrap();
    let published = publish();
    assert_eq!(published, before.selection);
    assert!(fixture.service.snapshot_with_epoch().1 > revision);
    release.send(()).unwrap();
    assert_eq!(
        observation.await.unwrap().unwrap_err().code,
        "reader_observation_stale"
    );
    assert_eq!(fixture.service.snapshot().usim_info, before.usim_info);
    assert_eq!(fixture.trace.calls.load(Ordering::SeqCst), calls);
    assert_eq!(fixture.trace.opened.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.trace.closed.load(Ordering::SeqCst), 0);
    fixture.service.disconnect().await.unwrap();
    fixture.settled().await;
}

#[tokio::test]
async fn cancelling_information_leader_releases_singleflight_without_card_actions() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let calls = fixture.trace.calls.load(Ordering::SeqCst);
    let (started, ready) = tokio::sync::oneshot::channel();
    let service = fixture.service.clone();
    let leader = tokio::spawn(async move {
        service
            .observed_snapshot_with(|_| async move {
                started.send(()).unwrap();
                std::future::pending::<presence::Sample>().await
            })
            .await
    });
    ready.await.unwrap();
    leader.abort();
    assert!(leader.await.unwrap_err().is_cancelled());
    let observed = tokio::time::timeout(
        Duration::from_secs(2),
        fixture.service.observed_snapshot_with(|_| async {
            presence_sample(proletarius_reader::CardPresence::Present)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(observed.status["has_card"], true);
    assert_eq!(fixture.trace.calls.load(Ordering::SeqCst), calls);
    fixture.service.disconnect().await.unwrap();
    fixture.settled().await;
}

#[tokio::test]
async fn information_waiter_deadline_includes_singleflight_queue_without_dispatch_or_mutation() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let before = fixture.service.snapshot();
    let lock = fixture.service.0.presence.lock().await;
    let dispatched = std::sync::atomic::AtomicBool::new(false);
    let error = fixture
        .service
        .observed_snapshot_until(
            |_| async {
                dispatched.store(true, Ordering::SeqCst);
                presence_sample(proletarius_reader::CardPresence::Absent)
            },
            tokio::time::Instant::now() + Duration::from_millis(30),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "reader_observation_timeout");
    assert!(!dispatched.load(Ordering::SeqCst));
    assert_eq!(fixture.service.snapshot().selection, before.selection);
    assert_eq!(fixture.service.snapshot().usim_info, before.usim_info);
    drop(lock);
    let observed = fixture
        .service
        .observed_snapshot_with(|_| async {
            presence_sample(proletarius_reader::CardPresence::Present)
        })
        .await
        .unwrap();
    assert_eq!(observed.status["has_card"], true);
    fixture.service.disconnect().await.unwrap();
    fixture.settled().await;
}

#[tokio::test]
async fn information_scan_timeout_keeps_running_worker_capacity_until_actual_completion() {
    let fixture = Fixture::new();
    fixture.service.connect("Reader").await.unwrap();
    let pool = crate::blocking_pool::BlockingPool::new(1);
    let worker_pool = pool.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let service = fixture.service.clone();
    let observer = tokio::spawn(async move {
        service
            .observed_snapshot_with(|_| async move {
                worker_pool
                    .run(move || {
                        started.send(()).unwrap();
                        wait.recv_timeout(Duration::from_secs(6)).unwrap();
                    })
                    .await
                    .unwrap();
                presence_sample(proletarius_reader::CardPresence::Present)
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), ready)
        .await
        .unwrap()
        .unwrap();
    let unknown = observer.await.unwrap().unwrap();
    assert_eq!(unknown.status["card_state"], "unknown");
    assert_eq!(unknown.status["presence_error"], "scan_timeout");
    assert_eq!(unknown.status["has_card"], false);
    let executed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let called = executed.clone();
    let mut queued = Box::pin(pool.run(move || called.store(true, Ordering::SeqCst)));
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    assert!(poll_fn(|cx| Poll::Ready(queued.as_mut().poll(cx).is_pending())).await);
    assert!(!executed.load(Ordering::SeqCst));
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .unwrap()
        .unwrap();
    assert!(executed.load(Ordering::SeqCst));
    fixture.service.disconnect().await.unwrap();
    fixture.settled().await;
}
