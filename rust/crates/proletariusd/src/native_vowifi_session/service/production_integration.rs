//! Production Service/LiveDriver -> native PCSC -> real SOCKS IKE -> inner-ESP
//! REGISTER -> stop/DELETE. Only the card and carrier peers are simulated.
use super::*;
use crate::{reader_pcsc, reader_reservation, reader_session_lease};
use std::sync::atomic::{AtomicUsize, Ordering};

mod call_exchange;
mod call_peer;
mod degraded_exchange;
mod facade_exchange;
mod gateway;
mod ike_peer;
mod ims_peer;
mod manager_exchange;
mod manager_multi_profile;
mod module_exchange;
mod module_exchange_serial;
mod native_multi_exchange;
mod nwait_heartbeat_policy;
mod refresh_exchange;
mod rekey_exchange;
mod renewal_exchange;
mod renewal_peer;
mod s08_recovery;
mod sessions_exchange;
mod sms_exchange;
mod sms_peer;
const APP_TOKEN: &str = "production-integration-app-token";
const ICCID: &str = "8901234567890123456";
const IMSI: &str = "234330123456789";

fn public_call_body(method: &str, path: &str, mut body: serde_json::Value) -> serde_json::Value {
    if method == "POST" && crate::unified_call_control::is_mutation(&hyper::Method::POST, path) {
        let owner = if path.starts_with("/api/vowifi/calls/") {
            "module-vowifi"
        } else {
            "reader"
        };
        let fields = body
            .as_object_mut()
            .expect("production public call body must be an object");
        fields
            .entry("client_request_id")
            .or_insert_with(|| json!(crate::snapshot::new_instance_id()));
        if path.ends_with("/dial") {
            fields
                .entry("preferred_owner")
                .or_insert_with(|| json!(owner));
        } else if path != "/api/reader/vowifi/ussd" {
            fields
                .entry("observed_owner")
                .or_insert_with(|| json!(owner));
        }
    }
    body
}

async fn confirm_idle_module_call_observer(
    state: &crate::ServerState,
    radio: crate::module_radio::Service,
) {
    let mut observer_state = state.clone();
    observer_state.module_radio = radio;
    let service = observer_state
        .module_call()
        .expect("production module call observer");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let response = crate::call_control::handle_status(service.clone()).await;
            assert_eq!(response.status(), StatusCode::OK);
            let value: serde_json::Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            match value["state"].as_str() {
                Some("idle") => break,
                Some("unknown") => tokio::time::sleep(Duration::from_millis(10)).await,
                _ => panic!("unexpected module call observer state: {value}"),
            }
        }
    })
    .await
    .expect("production module call observer idle deadline");
    service.retire_transport().await.unwrap();
}

async fn install_synthetic_idle_module_call_observer(fixture: &Fixture) {
    let state = fixture.sms_state();
    let (_, radio, _) = crate::module_vowifi_card::tests::service_fixture(
        fixture.root.join("idle-module-call-observer"),
    )
    .await;
    confirm_idle_module_call_observer(&state, radio).await;
}

fn reader_disconnect_body(fixture: &Fixture, client_request_id: &str) -> serde_json::Value {
    let state = fixture.sms_state();
    let selection = state.reader_gate.selection_snapshot();
    let inventory_revision = state.reader_discovery.lock().unwrap().snapshot().revision;
    json!({
        "client_request_id": client_request_id,
        "reader": selection.reader,
        "reader_generation": selection.generation,
        "inventory_revision": inventory_revision,
        "instance_id": state.snapshot_instance_id,
    })
}
#[derive(Default)]
struct CardTrace {
    commands: Mutex<Vec<Vec<u8>>>,
    auth: AtomicUsize,
    auth_delay_ms: AtomicUsize,
    auth_finished: AtomicUsize,
    auth_hold: Mutex<Option<Arc<AuthBarrier>>>,
    closed: AtomicUsize,
}
// Test-only proof of actual APDU entry, independent of SIP scheduling.
#[derive(Default)]
struct AuthBarrier {
    entered: tokio::sync::Notify,
    released: Mutex<bool>,
    wake: std::sync::Condvar,
}
struct HeldAuth(Arc<AuthBarrier>);
impl HeldAuth {
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(3), self.0.entered.notified())
            .await
            .expect("actual card authenticate entry deadline");
    }
    fn release(&self) {
        *self.0.released.lock().unwrap() = true;
        self.0.wake.notify_all();
    }
}
impl Drop for HeldAuth {
    fn drop(&mut self) {
        // An assertion failure/cancelled scenario must not strand the worker.
        self.release();
    }
}
impl CardTrace {
    fn hold_next_auth(&self) -> HeldAuth {
        let barrier = Arc::new(AuthBarrier::default());
        assert!(
            self.auth_hold
                .lock()
                .unwrap()
                .replace(barrier.clone())
                .is_none()
        );
        HeldAuth(barrier)
    }
}
struct Card {
    trace: Arc<CardTrace>,
    selected: [u8; 2],
}
impl proletarius_sim::ApduTransport for Card {
    type Error = reader_pcsc::Failure;
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
        self.trace.commands.lock().unwrap().push(command.to_vec());
        let mut reply = match command[1] {
            0xa4 => {
                if command.len() >= 7 && command[4] == 2 {
                    self.selected = command[5..7].try_into().unwrap();
                }
                vec![]
            }
            0xb2 => vec![
                0x61, 11, 0x4f, 9, 0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff, 1, 0x61, 11, 0x4f, 9, 0xa0,
                0, 0, 0, 0x87, 0x10, 4, 0xff, 1,
            ],
            0xb0 => match self.selected {
                [0x6f, 0x07] if command[4] == 1 => vec![8],
                [0x6f, 0x07] => vec![8, 0x29, 0x43, 0x33, 0x10, 0x32, 0x54, 0x76, 0x98],
                [0x2f, 0xe2] => vec![0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6],
                _ => return Ok(vec![0x6a, 0x82]),
            },
            0x88 => {
                self.trace.auth.fetch_add(1, Ordering::SeqCst);
                let hold = self.trace.auth_hold.lock().unwrap().take();
                if let Some(hold) = hold {
                    hold.entered.notify_one();
                    let mut released = hold.released.lock().unwrap();
                    while !*released {
                        released = hold.wake.wait(released).unwrap();
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(
                        self.trace.auth_delay_ms.load(Ordering::SeqCst) as u64,
                    ));
                }
                self.trace.auth_finished.fetch_add(1, Ordering::SeqCst);
                let ike = command.windows(16).any(|v| v == [0x11; 16]);
                let mut value = vec![0xdb, 8];
                value.extend(if ike {
                    vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
                } else {
                    vec![1; 8]
                });
                value.push(16);
                value.extend([if ike { 0x31 } else { 2 }; 16]);
                value.push(16);
                value.extend([if ike { 0x42 } else { 3 }; 16]);
                value
            }
            _ => return Ok(vec![0x6a, 0x82]),
        };
        reply.extend([0x90, 0]);
        Ok(reply)
    }
}
impl reader_pcsc::Physical for Card {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn proletarius_sim::ApduTransport<Error = reader_pcsc::Failure>),
    ) -> Result<(), reader_pcsc::Failure> {
        body(self);
        Ok(())
    }
    fn close(&mut self) -> Result<(), reader_pcsc::Failure> {
        self.trace.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Fixture {
    gateway: std::sync::OnceLock<crate::ServerState>,
    service: Service,
    lifecycle: Arc<NativeLifecycle>,
    reader: Arc<crate::reader_lifecycle::Service>,
    card: Arc<CardTrace>,
    session: reader_pcsc::LifecycleBinding,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        Self::new_with_routes(
            "+447802002606",
            "sip:smsc@ims.mnc033.mcc234.3gppnetwork.org",
        )
    }
    fn new_with_routes(smsc: &str, target: &str) -> Self {
        let id = crate::multistandby_store::random_token().unwrap();
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap())
            .join(format!("runtime/production-490-{id}"));
        Self::new_in(root, smsc, target)
    }
    // Recreate real owners against the same durable root; no directory deletion
    // or ledger seeding is part of this test-only constructor.
    fn new_in(root: PathBuf, smsc: &str, target: &str) -> Self {
        std::fs::create_dir_all(&root).unwrap();
        let gate = Arc::new(reader_reservation::Gate::default());
        let card = Arc::new(CardTrace::default());
        let factory = card.clone();
        let physical = reader_pcsc::Owner::with_factory(
            "native-production-490".into(),
            Arc::new(move |_| {
                Ok(Box::new(Card {
                    trace: factory.clone(),
                    selected: [0; 2],
                }))
            }),
            Arc::new(|| Ok(vec!["Production Reader".into()])),
            Arc::new(|_, _| panic!("no physical reset is part of ordinary IMS")),
        );
        let reader = Arc::new(
            crate::reader_lifecycle::Service::new(
                root.clone(),
                gate.clone(),
                physical.clone(),
                None,
                None,
                "native-production-490".into(),
            )
            .unwrap(),
        );
        let lifecycle = Arc::new(NativeLifecycle::default());
        lifecycle.activate().unwrap();
        gate.set_ordinary_admission(lifecycle.ordinary_admission())
            .unwrap();
        reader.set_ordinary_lifecycle(Arc::downgrade(&lifecycle));
        let session = physical.native_open("Production Reader").unwrap();
        let prepared = gate
            .prepare_native_transition(
                "native-production-490",
                &Default::default(),
                reader_reservation::NativeTransitionKind::Connect,
                "Production Reader",
                reader_reservation::NativeAuthority::Idle,
            )
            .unwrap();
        gate.commit_native_transition(&prepared,Some(&session),reader_reservation::NativeReaderFacts {
            metadata:Some(json!({"iccid":ICCID,"imsi":IMSI,"mcc":"234","mnc":"33","private_identity":format!("{IMSI}@ims.mnc033.mcc234.3gppnetwork.org"),"public_identity":format!("sip:{IMSI}@ims.mnc033.mcc234.3gppnetwork.org"),"home_domain":"ims.mnc033.mcc234.3gppnetwork.org","smsc":smsc,"smsc_psi":target,"smsc_target":target})),..Default::default()
        }).unwrap();
        let erased: Arc<dyn reader_session_lease::NativeSessionSupplier> = reader.clone();
        gate.set_native_session_supplier(Arc::downgrade(&erased));
        let service = Service(Arc::new(Inner {
            sms_epoch: sms::new_epoch().unwrap(),
            resources: Resources {
                legacy_module_claim: Arc::new(|| false),
                ordinary_admission: lifecycle.ordinary_admission(),
                identity: reader.clone(),
                lifecycle: Arc::downgrade(&lifecycle),
                gate,
                pcsc: physical,
                socket: root.join("never-created-go.sock"),
                token: None,
                data_root: root.clone(),
                transactions: Default::default(),
                calls: crate::call_facts::CallFacts::new(
                    proletarius_store::ArchiveStore::new(root.join("calls.json")),
                    "production-490".into(),
                ),
            },
            state: Mutex::new(State::default()),
            mutation: tokio::sync::Mutex::new(()),
        }));
        Self {
            gateway: Default::default(),
            service,
            lifecycle,
            reader,
            card,
            session,
            root,
        }
    }
    async fn registered(&self) -> NativeSmsBinding {
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                if let Ok(binding) = self.service.sms_binding() {
                    return binding;
                }
                let status = self.service.status();
                assert_ne!(
                    status["tunnel_state"],
                    "error",
                    "production startup failed: {status}; detail={:?}",
                    self.service
                        .record()
                        .unwrap()
                        .and_then(|record| record.driver.handle())
                        .and_then(|handle| handle.snapshot().termination)
                        .and_then(|termination| termination.detail().map(str::to_owned))
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|error| panic!("production REGISTER deadline: {error:?}; status={}; binding_error={:?}; ordinary={:?}", self.service.status(),self.service.sms_binding().err(),self.service.record().unwrap().and_then(|record|record.driver.handle()).map(|handle|handle.snapshot())))
    }
    async fn finish(self) {
        self.finish_observed_reader(true).await;
    }
    async fn finish_idle_reader(self) {
        assert!(
            self.card.commands.lock().unwrap().is_empty(),
            "module work must not touch the selected PCSC reader"
        );
        assert_eq!(self.card.auth.load(Ordering::SeqCst), 0);
        self.finish_observed_reader(false).await;
    }
    async fn finish_observed_reader(self, reader_io: bool) {
        self.service.begin_shutdown();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        self.service.finish_before(deadline).await.unwrap();
        self.lifecycle.finish_shutdown(deadline).await.unwrap();
        assert!(!self.service.0.resources.gate.native_reader_session_active());
        assert!(!self.lifecycle.ordinary_admission().has_ordinary_claim());
        if !self.reader.snapshot().selection.reader.is_empty() {
            self.service
                .0
                .resources
                .pcsc
                .native_close(&self.session)
                .unwrap();
        }
        assert_eq!(
            self.card.closed.load(Ordering::SeqCst),
            usize::from(reader_io)
        );
        assert!(!self.root.join("never-created-go.sock").exists());
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_start_drives_native_reader_ike_cp_protected_register_and_actual_stop() {
    // Isolate the supported YAML override in a child test process. Full gates run
    // tests concurrently, so changing the parent's process environment is unsafe.
    if std::env::var_os("SWITCH_PRODUCTION_490_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/production-490-config-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier-overrides.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "native_vowifi_session::service::production_integration::public_start_drives_native_reader_ike_cp_protected_register_and_actual_stop", "--nocapture"])
                .env("SWITCH_PRODUCTION_490_CHILD", "1")
                .env("PROLETARIUS_VOWIFI_CONFIG", config)
                .output().unwrap()
        }).await.unwrap();
        assert!(
            output.status.success(),
            "child production fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(30),async {
        let peer=ike_peer::Peer::new().await;let fixture=Fixture::new();
        install_synthetic_idle_module_call_observer(&fixture).await;
        let response=gateway::request(&fixture, Request::builder().method("POST").uri("/api/reader/vowifi").body(Full::new(Bytes::from(serde_json::to_vec(&json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true,"proxy":peer.proxy.to_string(),"epdg":"epdg.invalid","epdg_port":500})).unwrap()))).unwrap()).await;
        let status=response.status();let body=response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(status,StatusCode::ACCEPTED,"{}",String::from_utf8_lossy(&body));
        let binding=fixture.registered().await;
        assert_eq!(binding.generation.value(), 1, "first production generation must satisfy the real transaction-owner validator");
        assert_eq!(peer.trace.ike.load(Ordering::SeqCst),1);assert_eq!(peer.trace.registers.load(Ordering::SeqCst),1);
        assert_eq!(fixture.card.auth.load(Ordering::SeqCst),2);
        assert!(fixture.service.reader_observation().unwrap().sms_ready);
        let snapshot=crate::snapshot::capture_reader_vowifi(&fixture.sms_state()).await.unwrap();
        assert_eq!(snapshot.ims_state,"registered");assert_eq!(snapshot.sms_ready,Some(true));
        for path in ["/api/reader/vowifi", "/api/reader/vowifi/sessions"] {
            let response=gateway::request(&fixture,Request::builder().method("GET").uri(path).body(Full::new(Bytes::new())).unwrap()).await;
            assert_eq!(response.status(),StatusCode::OK);
            let value:serde_json::Value=serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
            let row=if path.ends_with("/sessions"){assert!(value.get("legacy_sessions_unavailable").is_none(), "native-only session list must not depend on retired Go observation");assert_eq!(value["sessions"].as_array().unwrap().len(),1);&value["sessions"][0]}else{&value};
            assert_eq!(row["ims_state"],"registered");assert_eq!(row["sms_ready"],true);assert_eq!(row["reader_generation"],fixture.reader.snapshot().selection.generation);
        }
        peer.trace.stop_requested.store(true, Ordering::SeqCst);
        gateway::stop(&fixture).await;
        assert!(fixture.service.resolve_sms_binding(&binding.binding).is_err());
        let response=gateway::request(&fixture,Request::builder().method("POST").uri("/api/reader/vowifi").body(Full::new(Bytes::from(serde_json::to_vec(&json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true,"proxy":peer.proxy.to_string(),"epdg":"epdg.invalid","epdg_port":500})).unwrap()))).unwrap()).await;
        assert_eq!(response.status(),StatusCode::ACCEPTED);
        let second=fixture.registered().await;
        assert_ne!(second.generation,binding.generation);
        let old_reader_generation=fixture.reader.snapshot().selection.generation;
        let busy_body=reader_disconnect_body(&fixture,"123e4567-e89b-42d3-a456-426614174001");
        let busy=gateway::request(&fixture,Request::builder().method("POST").uri("/api/reader/disconnect").body(Full::new(Bytes::from(serde_json::to_vec(&busy_body).unwrap()))).unwrap()).await;
        assert_eq!(busy.status(),StatusCode::CONFLICT,"active ordinary must keep its reader pinned");
        assert_eq!(fixture.card.closed.load(Ordering::SeqCst),0);
        assert_eq!(fixture.reader.snapshot().selection.generation,old_reader_generation);
        assert!(fixture.service.resolve_sms_binding(&second.binding).is_ok());
        gateway::stop(&fixture).await;
        let disconnected_body=reader_disconnect_body(&fixture,"123e4567-e89b-42d3-a456-426614174002");
        let disconnected=gateway::request(&fixture,Request::builder().method("POST").uri("/api/reader/disconnect").body(Full::new(Bytes::from(serde_json::to_vec(&disconnected_body).unwrap()))).unwrap()).await;
        let status=disconnected.status();let body=disconnected.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(status,StatusCode::OK,"{}",String::from_utf8_lossy(&body));
        let selection=fixture.reader.snapshot().selection;
        assert!(selection.reader.is_empty());assert!(selection.generation>old_reader_generation);
        assert!(fixture.service.resolve_sms_binding(&second.binding).is_err());
        assert!(!fixture.lifecycle.ordinary_admission().has_ordinary_claim());
        assert_eq!(fixture.card.closed.load(Ordering::SeqCst),1);
        let stale=gateway::request(&fixture,Request::builder().method("POST").uri("/api/reader/vowifi/calls/dial").body(Full::new(Bytes::from(serde_json::to_vec(&json!({"client_request_id":crate::snapshot::new_instance_id(),"preferred_owner":"reader","generation":old_reader_generation,"peer":"+447700900555"})).unwrap()))).unwrap()).await;
        assert_eq!(stale.status(),StatusCode::CONFLICT);
        let trace = peer.trace.clone();
        fixture.finish().await;peer.finish().await;
        assert_eq!(trace.deletes.load(Ordering::SeqCst),2);
    }).await.unwrap();
}
