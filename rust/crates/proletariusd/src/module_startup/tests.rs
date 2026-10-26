use super::*;
use crate::availability::BackendAvailability;
use crate::usb_at_transport::{Transport, TransportError};
use std::sync::{
    Condvar,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const PRODUCER: &str = "native-startup-test";

#[derive(Default)]
struct Gate {
    entered: Notify,
    released: Mutex<bool>,
    resume: Condvar,
}
impl Gate {
    fn wait(&self) {
        self.entered.notify_one();
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.resume.wait(released).unwrap();
        }
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.resume.notify_all();
    }
}
#[derive(Default)]
struct Physical {
    commands: Mutex<Vec<String>>,
    open_gate: Mutex<Option<Arc<Gate>>>,
    open_error: Mutex<Option<TransportError>>,
    available: AtomicBool,
    bootstrap_gate: Mutex<Option<Arc<Gate>>>,
    warm_close_gate: Mutex<Option<Arc<Gate>>>,
    warm_channel_available: AtomicBool,
    channels_opened: AtomicUsize,
    channels_closed: AtomicUsize,
    opens: AtomicUsize,
    drops: AtomicUsize,
}
struct Port(Arc<Physical>);
impl Transport for Port {
    fn checked_close(
        self: Box<Self>,
    ) -> Result<crate::usb_at_transport::CloseDisposition, crate::usb_at_transport::TransportError>
    {
        // Explicit fixture release; Drop barriers must finish before success.
        drop(self);
        Ok(crate::usb_at_transport::CloseDisposition::ReleasedAndHandleDropped)
    }

    fn description(&self) -> String {
        "isolated startup USB fixture".into()
    }
    fn command(
        &mut self,
        command: &str,
        _: Option<&[u8]>,
        _: Duration,
        _: usize,
    ) -> Result<String, TransportError> {
        self.0.commands.lock().unwrap().push(command.into());
        if command == "AT+CFUN?" {
            let gate = self.0.bootstrap_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.wait();
            }
            return Ok("+CFUN: 4\r\nOK\r\n".into());
        }
        // An ordinary SIM rejecting a logical eUICC channel is an expected,
        // best-effort warmup failure, not permission to skip RF bootstrap.
        if command.starts_with("AT+CCHO=") {
            if self.0.warm_channel_available.load(Ordering::SeqCst) {
                self.0.channels_opened.fetch_add(1, Ordering::SeqCst);
                return Ok("+CCHO: 1\r\nOK\r\n".into());
            }
            return Ok("ERROR\r\n".into());
        }
        if command.starts_with("AT+CGLA=") {
            return Ok("+CGLA: 4,\"6A82\"\r\nOK\r\n".into());
        }
        if command == "AT+CCHC=1" {
            let gate = self.0.warm_close_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.wait();
            }
            self.0.channels_closed.fetch_add(1, Ordering::SeqCst);
            return Ok("OK\r\n".into());
        }
        if command == "AT+CPMS?" {
            return Ok("+CPMS: \"SM\",0,50,\"SM\",0,50,\"SM\",0,50\r\nOK\r\n".into());
        }
        if command == "ATI" {
            return Ok("Startup fixture firmware\r\nOK\r\n".into());
        }
        if command == "AT+CPIN?" {
            return Ok("+CPIN: READY\r\nOK\r\n".into());
        }
        Ok("OK\r\n".into())
    }
}
impl Drop for Port {
    fn drop(&mut self) {
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}
struct Fixture {
    root: std::path::PathBuf,
    modem: ModemActor,
    usb: usb_at::Owner,
    radio: module_radio::Service,
    inbox: InboxState,
    physical: Arc<Physical>,
    gates: Mutex<Vec<Arc<Gate>>>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/module-startup-tests")
            .join(crate::snapshot::new_instance_id());
        std::fs::create_dir_all(&root).unwrap();
        let availability = BackendAvailability::new();
        let module_gate = availability.module_recovery().clone();
        let physical = Arc::new(Physical::default());
        physical.available.store(true, Ordering::SeqCst);
        let factory = physical.clone();
        let usb = usb_at::Owner::with_factory(
            PRODUCER.into(),
            Arc::new(move || {
                factory.opens.fetch_add(1, Ordering::SeqCst);
                let gate = factory.open_gate.lock().unwrap().take();
                if let Some(gate) = gate {
                    gate.wait();
                }
                if let Some(error) = factory.open_error.lock().unwrap().take() {
                    return Err(error);
                }
                if !factory.available.load(Ordering::SeqCst) {
                    return Err(crate::usb_at_transport::no_device_for_tests());
                }
                Ok(Box::new(Port(factory.clone())))
            }),
        )
        .with_recovery_gate_for_tests(module_gate.clone());
        let modem = ModemActor::with_test_port_opener_and_module_recovery(
            |_, _| {
                Err(ModemActor::discovery_error_for_test(
                    "serial_fixture_no_port",
                ))
            },
            module_gate,
        )
        .unwrap();
        let radio = module_radio::Service::new(
            root.clone(),
            usb.clone(),
            modem.clone(),
            availability,
            PRODUCER.into(),
        );
        Self {
            root,
            modem,
            usb,
            radio,
            inbox: InboxState::for_go_producer("compatibility".into()),
            physical,
            gates: Mutex::new(Vec::new()),
        }
    }
    fn start(&self, probe: Probe) -> Service {
        Service::start_inner(
            probe,
            self.modem.clone(),
            self.usb.clone(),
            self.radio.clone(),
            self.inbox.clone(),
            PRODUCER.into(),
            ArchiveSource::Fixture(proletarius_store::ArchiveStore::new(
                self.root.join("sms.json"),
            )),
        )
    }
    fn missing(&self) -> Service {
        self.start(error_probe(
            "at_port_not_found",
            Arc::new(AtomicUsize::new(0)),
        ))
    }
    fn gate(&self) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        *self.physical.bootstrap_gate.lock().unwrap() = Some(gate.clone());
        self.gates.lock().unwrap().push(gate.clone());
        gate
    }
    fn open_gate(&self) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        *self.physical.open_gate.lock().unwrap() = Some(gate.clone());
        self.gates.lock().unwrap().push(gate.clone());
        gate
    }
    fn warm_close_gate(&self) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        *self.physical.warm_close_gate.lock().unwrap() = Some(gate.clone());
        self.physical
            .warm_channel_available
            .store(true, Ordering::SeqCst);
        self.gates.lock().unwrap().push(gate.clone());
        gate
    }
    async fn close(&self, service: &Service) {
        service.shutdown().await.unwrap();
        self.radio.begin_shutdown();
        self.radio.finish_shutdown().await.unwrap();
        self.usb.shutdown().await.unwrap();
        self.modem
            .shutdown()
            .expect("final modem close acknowledgement");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for gate in self.gates.lock().unwrap().iter() {
            gate.release();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn error_probe(code: &'static str, count: Arc<AtomicUsize>) -> Probe {
    Probe::Fixture(Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Err(ModemActor::discovery_error_for_test(code)) })
    }))
}
async fn until(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !check() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn entered(gate: &Gate) {
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn serial_selection_retains_open_watch_and_never_claims_usb() {
    let fixture = Fixture::new();
    let service = fixture.start(Probe::Fixture(Arc::new(|| {
        Box::pin(async {
            Ok(crate::modem_actor::DiscoveredATPort {
                port: "/dev/cu.fake-startup".into(),
            })
        })
    })));
    let mut selected = service.selection();
    until(|| matches!(*selected.borrow(), Selection::Serial { .. })).await;
    assert_eq!(
        *selected.borrow_and_update(),
        Selection::Serial {
            port: "/dev/cu.fake-startup".into()
        }
    );
    service.shutdown().await.unwrap();
    assert!(
        matches!(selected.has_changed(), Ok(false)),
        "live service retains watch sender after worker exits"
    );
    assert!(!fixture.usb.native_managed());
    assert_eq!(fixture.physical.opens.load(Ordering::SeqCst), 0);
    fixture.close(&service).await;
}

#[tokio::test]
async fn non_not_found_failures_publish_error_and_retry_without_usb() {
    for code in [
        "serial_actor_busy",
        "serial_actor_stopped",
        "serial_not_open",
    ] {
        let fixture = Fixture::new();
        let count = Arc::new(AtomicUsize::new(0));
        let service = fixture.start(error_probe(code, count.clone()));
        let mut selected = service.selection();
        selected.changed().await.unwrap();
        assert_eq!(*selected.borrow(), Selection::Failed { code });
        assert_eq!(service.last_error(), Some(code));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "failure must not busy-loop"
        );
        assert!(!fixture.usb.native_managed());
        assert_eq!(fixture.physical.opens.load(Ordering::SeqCst), 0);
        if code == "serial_actor_busy" {
            until(|| count.load(Ordering::SeqCst) == 2).await;
            assert!(!fixture.usb.native_managed());
        }
        fixture.close(&service).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_usb_does_not_trap_later_serial_hotplug_behind_native_usb_selection() {
    let fixture = Fixture::new();
    fixture.physical.available.store(false, Ordering::SeqCst);
    let probes = Arc::new(AtomicUsize::new(0));
    let service = fixture.start(Probe::Fixture(Arc::new({
        let probes = probes.clone();
        move || {
            let attempt = probes.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if attempt == 0 {
                    Err(ModemActor::discovery_error_for_test("at_port_not_found"))
                } else {
                    Ok(crate::modem_actor::DiscoveredATPort {
                        port: "/dev/cu.late-dji".into(),
                    })
                }
            })
        }
    })));
    until(|| service.last_error() == Some("usb_at_not_found")).await;
    assert!(!fixture.usb.native_managed());
    assert_eq!(fixture.physical.opens.load(Ordering::SeqCst), 1);
    until(|| matches!(*service.selection().borrow(), Selection::Serial { .. })).await;
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    assert_eq!(
        *service.selection().borrow(),
        Selection::Serial {
            port: "/dev/cu.late-dji".into(),
        }
    );
    assert!(!fixture.usb.native_managed());
    fixture.close(&service).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_precommit_usb_open_never_publishes_new_selection() {
    let fixture = Fixture::new();
    let gate = fixture.open_gate();
    let service = fixture.missing();
    entered(&gate).await;
    service.begin_shutdown();
    let shutting_down = {
        let service = service.clone();
        tokio::spawn(async move { service.shutdown().await })
    };
    assert!(!shutting_down.is_finished());
    gate.release();
    shutting_down.await.unwrap().unwrap();
    assert_eq!(*service.selection().borrow(), Selection::Probing);
    assert!(fixture.usb.native_managed());
    assert_eq!(fixture.physical.opens.load(Ordering::SeqCst), 1);
    fixture.close(&service).await;
    assert_eq!(fixture.physical.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unconfirmed_precommit_usb_cleanup_stops_without_serial_rescan_or_reopen() {
    let fixture = Fixture::new();
    *fixture.physical.open_error.lock().unwrap() =
        Some(crate::usb_at_transport::close_unconfirmed());
    let probes = Arc::new(AtomicUsize::new(0));
    let service = fixture.start(error_probe("at_port_not_found", probes.clone()));
    until(|| matches!(*service.selection().borrow(), Selection::Failed { .. })).await;
    assert_eq!(
        *service.selection().borrow(),
        Selection::Failed {
            code: "usb_at_close_unconfirmed"
        }
    );
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.physical.opens.load(Ordering::SeqCst), 1);
    assert!(!fixture.usb.native_managed());
    assert_eq!(
        service.shutdown().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    fixture.usb.claim_native_management().unwrap();
    assert_eq!(
        fixture.usb.open_native_session().await.unwrap_err().code,
        "usb_at_close_unconfirmed"
    );
    assert_eq!(fixture.physical.opens.load(Ordering::SeqCst), 1);
    assert!(fixture.usb.shutdown().await.is_err());
    fixture.radio.begin_shutdown();
    fixture.radio.finish_shutdown().await.unwrap();
    fixture.modem.shutdown().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sms_ready_waits_for_actual_bootstrap_reply_and_warmup_cleanup() {
    let fixture = Fixture::new();
    let gate = fixture.gate();
    let receiver = crate::usb_sms_receive::Service::new(
        proletarius_store::ArchiveStore::new(fixture.root.join("sms.json")),
        fixture.inbox.clone(),
        fixture.usb.clone(),
        BackendAvailability::with_module_recovery(fixture.radio.module_recovery().clone()),
        PRODUCER.into(),
    );
    receiver.start();
    let service = fixture.missing();
    entered(&gate).await;
    assert_eq!(*service.selection().borrow(), Selection::NativeUsb);
    assert!(fixture.usb.native_managed());
    assert_eq!(
        fixture.inbox.current_module_producer_id().as_deref(),
        Some(PRODUCER)
    );
    assert!(fixture.radio.uses_usb_transport());
    assert!(!fixture.radio.usb_esim_route_available());
    assert!(!fixture.usb.native_sms_ready());
    assert!(fixture.usb.sms_poll_binding(PRODUCER).unwrap().is_none());
    assert!(
        !fixture
            .physical
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("AT+CMGL"))
    );
    gate.release();
    until(|| fixture.usb.native_sms_ready()).await;
    assert!(fixture.radio.usb_esim_route_available());
    assert!(fixture.usb.sms_poll_binding(PRODUCER).unwrap().is_some());
    assert_eq!(service.last_error(), None);
    let bootstrap_reads = fixture
        .physical
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.as_str() == "AT+CFUN?")
        .count();
    tokio::time::sleep(RETRY_DELAY + Duration::from_millis(30)).await;
    assert_eq!(
        fixture
            .physical
            .commands
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.as_str() == "AT+CFUN?")
            .count(),
        bootstrap_reads,
        "ready startup must not repeat bootstrap AT work"
    );
    receiver.shutdown().await.unwrap();
    fixture.close(&service).await;
    assert_eq!(fixture.physical.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_shutdown_waiter_retains_actual_job_and_join_receipt() {
    let fixture = Fixture::new();
    let gate = fixture.gate();
    let service = fixture.missing();
    entered(&gate).await;
    let waiter = service.clone();
    let task = tokio::spawn(async move { waiter.shutdown().await });
    until(|| *service.0.stop.borrow()).await;
    assert!(!task.is_finished());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(service.0.completion.lock().unwrap().is_none());
    assert_eq!(fixture.physical.drops.load(Ordering::SeqCst), 0);
    assert!(!fixture.usb.native_sms_ready());
    gate.release();
    service.shutdown().await.unwrap();
    assert!(
        !fixture.usb.native_sms_ready(),
        "stop must prevent subsequent warmup/SMS admission"
    );
    assert!(
        !fixture
            .physical
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("AT+CCHO="))
    );
    assert_eq!(
        fixture.physical.drops.load(Ordering::SeqCst),
        0,
        "workflow completion does not forge physical close"
    );
    fixture.close(&service).await;
    assert_eq!(fixture.physical.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn physical_reopen_requires_new_bootstrap_without_reselecting_transport() {
    let fixture = Fixture::new();
    let probes = Arc::new(AtomicUsize::new(0));
    let service = fixture.start(error_probe("at_port_not_found", probes.clone()));
    until(|| fixture.usb.native_sms_ready()).await;
    let old_session = fixture.usb.open_native_session().await.unwrap();
    let gate = fixture.gate();
    let reconnect = fixture
        .usb
        .call_mode_radio_reconnect(
            PRODUCER,
            &old_session,
            std::time::Instant::now() + Duration::from_secs(5),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await;
    if let Err(error) = &reconnect {
        assert_eq!(error.public_code(), "usb_at_busy");
    }
    assert_eq!(fixture.physical.drops.load(Ordering::SeqCst), 1);
    assert!(!fixture.usb.native_sms_ready());
    assert!(fixture.usb.sms_poll_binding(PRODUCER).unwrap().is_none());
    entered(&gate).await;
    assert_eq!(fixture.physical.opens.load(Ordering::SeqCst), 2);
    assert_eq!(*service.selection().borrow(), Selection::NativeUsb);
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    assert!(!fixture.usb.native_sms_ready());
    assert!(fixture.usb.commit_native_sms_ready(&old_session).is_err());
    gate.release();
    until(|| fixture.usb.native_sms_ready()).await;
    let new_session = fixture.usb.open_native_session().await.unwrap();
    assert_ne!(new_session, old_session);
    if let Ok(reopened_session) = reconnect {
        assert_eq!(new_session, reopened_session);
    }
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    fixture.close(&service).await;
    assert_eq!(fixture.physical.drops.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn shutdown_joins_pending_probe_and_prevents_late_usb_fallback() {
    let fixture = Fixture::new();
    let probing = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let service = fixture.start(Probe::Fixture(Arc::new({
        let probing = probing.clone();
        let release = release.clone();
        move || {
            let probing = probing.clone();
            let release = release.clone();
            Box::pin(async move {
                probing.notify_one();
                release.notified().await;
                Err(ModemActor::discovery_error_for_test("at_port_not_found"))
            })
        }
    })));
    probing.notified().await;
    let waiter = service.clone();
    let task = tokio::spawn(async move { waiter.shutdown().await });
    until(|| *service.0.stop.borrow()).await;
    assert!(!task.is_finished());
    assert!(service.0.completion.lock().unwrap().is_none());
    release.notify_one();
    task.await.unwrap().unwrap();
    assert_eq!(*service.selection().borrow(), Selection::Probing);
    assert!(!fixture.usb.native_managed());
    assert_eq!(fixture.physical.opens.load(Ordering::SeqCst), 0);
    fixture.close(&service).await;
}

#[tokio::test]
async fn inbox_binding_retry_cannot_reselect_serial_after_confirmed_usb_choice() {
    let fixture = Fixture::new();
    fixture
        .inbox
        .bind_native_module_producer("already-bound-native-owner".into())
        .unwrap();
    let probes = Arc::new(AtomicUsize::new(0));
    let service = fixture.start(Probe::Fixture(Arc::new({
        let probes = probes.clone();
        move || {
            let attempt = probes.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if attempt == 0 {
                    Err(ModemActor::discovery_error_for_test("at_port_not_found"))
                } else {
                    Ok(crate::modem_actor::DiscoveredATPort {
                        port: "/dev/cu.late-serial".into(),
                    })
                }
            })
        }
    })));
    until(|| service.last_error() == Some("module_startup_inbox_unavailable")).await;
    assert!(fixture.usb.native_managed());
    tokio::time::sleep(RETRY_DELAY + Duration::from_millis(50)).await;
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    assert_eq!(
        *service.selection().borrow(),
        Selection::Failed {
            code: "module_startup_inbox_unavailable"
        }
    );
    assert_eq!(fixture.physical.opens.load(Ordering::SeqCst), 1);
    assert!(!fixture.usb.native_sms_ready());
    fixture.close(&service).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warm_cleanup_contention_recovers_sms_metadata_and_usbnet_http_after_actual_channel_close()
{
    let fixture = Fixture::new();
    let mut evidence = fixture.radio.module_diagnostics_for_test();
    let gate = fixture.warm_close_gate();
    fixture.usb.claim_native_management().unwrap();
    let (_stop, stopped) = watch::channel(false);
    let usb = fixture.usb.clone();
    let radio = fixture.radio.clone();
    let bootstrap = tokio::spawn(async move { bootstrap_usb(&usb, &radio, &stopped).await });
    entered(&gate).await;
    assert!(!bootstrap.is_finished());
    assert_eq!(fixture.physical.channels_opened.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.physical.channels_closed.load(Ordering::SeqCst), 0);
    assert!(fixture.radio.sim_channel_busy());
    assert!(!fixture.usb.native_sms_ready());

    // Ordinary Status cannot overlap the eSIM owner; a mandatory retained
    // cleanup still can. Neither path writes internal reservation/epoch state.
    assert_eq!(
        fixture
            .radio
            .borrow_shared_voice_operation(PRODUCER, false)
            .err()
            .unwrap()
            .code,
        "module_radio_busy"
    );
    let shared_cleanup = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(guard) = fixture.radio.borrow_shared_voice_operation(PRODUCER, true) {
                break guard;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    gate.release();
    until(|| fixture.radio.esim_release_pending_for_test()).await;
    assert!(!bootstrap.is_finished());
    let opened = fixture.physical.channels_opened.load(Ordering::SeqCst);
    assert!(opened > 0);
    assert_eq!(
        fixture.physical.channels_closed.load(Ordering::SeqCst),
        opened
    );
    assert!(!fixture.usb.native_sms_ready());
    assert!(fixture.usb.sms_poll_binding(PRODUCER).unwrap().is_none());
    assert!(fixture.radio.sim_channel_busy());
    let (status, body) = crate::module_radio::tests::tests::request_public_route(
        fixture.radio.clone(),
        "POST",
        "/api/network/usbnet",
        r#"{"client_request_id":"startup-cleanup-pending","mode":1}"#,
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["code"], "module_radio_busy");
    assert!(
        !fixture
            .physical
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command == r#"AT+QCFG="usbnet",1"#)
    );
    drop(shared_cleanup);
    tokio::time::timeout(Duration::from_secs(5), bootstrap)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(fixture.usb.native_sms_ready());
    assert!(fixture.usb.sms_poll_binding(PRODUCER).unwrap().is_some());
    assert!(!fixture.radio.sim_channel_busy());
    // Use the real metadata plan after the same startup has recovered.
    let session = fixture.radio.usb_transport_session().unwrap();
    let reservation = fixture.radio.reserve_esim().await.unwrap();
    let observation = fixture
        .usb
        .native_device_observation(&session)
        .await
        .unwrap();
    fixture.radio.release_esim(reservation).await.unwrap();
    assert_eq!(observation.status["firmware"], "Startup fixture firmware");
    assert_eq!(observation.status["sim_inserted"], true);
    assert!(
        fixture
            .physical
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command == "ATI")
    );
    let (status, body) = crate::module_radio::tests::tests::request_public_route(
        fixture.radio.clone(),
        "POST",
        "/api/network/usbnet",
        r#"{"client_request_id":"startup-cleanup-recovered","mode":1}"#,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["mode"], 1);
    assert_eq!(body["needs_reboot"], true);
    assert_eq!(
        fixture
            .physical
            .commands
            .lock()
            .unwrap()
            .iter()
            .filter(|command| command.as_str() == r#"AT+QCFG="usbnet",1"#)
            .count(),
        1
    );
    // The receipt proves the command, not a reboot or a usable network link.
    let mut events = Vec::new();
    while let Ok(event) = evidence.try_recv() {
        events.push(event);
    }
    use crate::support_diagnostics::module::{Code, Owner, ResultKind, Stage};
    assert!(
        events
            .iter()
            .any(|event| event.stage == Stage::ReservationRelease
                && event.result == ResultKind::Succeeded
                && event.owner.owner == Owner::None)
    );
    assert!(events.iter().any(|event| event.stage == Stage::Admission
        && event.result == ResultKind::Failed
        && event.code == Code::Busy
        && event.owner.owner == Owner::Esim
        && event.owner.shared_voice_pins == 1));
    assert!(
        events
            .iter()
            .any(|event| event.stage == Stage::Physical && event.result == ResultKind::Succeeded)
    );
    fixture.radio.finish_shutdown().await.unwrap();
    fixture.usb.shutdown().await.unwrap();
    fixture
        .modem
        .shutdown()
        .expect("final modem close acknowledgement");
    assert_eq!(fixture.physical.drops.load(Ordering::SeqCst), 1);
}
