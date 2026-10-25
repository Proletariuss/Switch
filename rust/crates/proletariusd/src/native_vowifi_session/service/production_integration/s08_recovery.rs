//! S08 composition: real HTTP/SIP unknown, durable reopen, production successor
//! epoch, historical-ID replay fence, then one explicit fresh SMS with RP ACK.
//! Only remote protocol bytes and the physical card/USB factory are mocked.
use super::{
    renewal_peer::{Command, Event},
    sms_exchange::sms_http,
    *,
};
use proletarius_sip::Message;
use std::sync::atomic::AtomicBool;
use tokio::io::{AsyncWrite, AsyncWriteExt};

const EXACT: &str = "native_vowifi_session::service::production_integration::s08_recovery::wire_unknown_reopen_successor_never_replays_old_id_then_fresh_sms";
const CHILD: &str = "SWITCH_S08_705_CHILD";
const ROLE: &str = "SWITCH_S08_705_ROLE";
const OLD: &str = "s08-705-old";
const FRESH: &str = "s08-705-fresh";
const RP_ACK: &str = "s08-705-fresh-rp-ack";
const DOMAIN: &str = "ims.mnc033.mcc234.3gppnetwork.org";

pub(super) struct WireFacts {
    root: PathBuf,
    module: bool,
    multi: Option<(crate::multistandby_store::Store, String)>,
    reject_initial_register: bool,
    allow_fresh: AtomicBool,
    old_submits: AtomicUsize,
    fresh_submits: AtomicUsize,
    fresh_rp_ack: AtomicUsize,
    barriers: AtomicUsize,
}
impl WireFacts {
    fn new(root: PathBuf, module: bool) -> Self {
        Self {
            root,
            module,
            multi: None,
            reject_initial_register: false,
            allow_fresh: AtomicBool::new(false),
            old_submits: AtomicUsize::new(0),
            fresh_submits: AtomicUsize::new(0),
            fresh_rp_ack: AtomicUsize::new(0),
            barriers: AtomicUsize::new(0),
        }
    }
    pub(super) fn reject_register(&self, association: usize) -> bool {
        self.reject_initial_register && association == 1
    }
    pub(super) async fn observe<W: AsyncWrite + Unpin>(
        &self,
        association: usize,
        message: Message,
        write: &mut W,
        events: &tokio::sync::mpsc::UnboundedSender<Event>,
    ) {
        match message {
            Message::Response(response) => {
                assert_eq!(response.status, 200);
                let id = response.data.headers.get("Call-ID").unwrap().value();
                if id == format!("s08-barrier-{association}") {
                    assert!(
                        response
                            .data
                            .headers
                            .get("CSeq")
                            .unwrap()
                            .value()
                            .ends_with(" OPTIONS")
                    );
                    self.barriers.fetch_add(1, Ordering::SeqCst);
                    events.send(Event::RecoveryBarrier(association)).unwrap();
                } else {
                    assert_eq!(id, RP_ACK);
                    assert_eq!(association, 2);
                    assert_eq!(self.fresh_rp_ack.fetch_add(1, Ordering::SeqCst), 0);
                    events.send(Event::RecoveryRpAck(association)).unwrap();
                }
            }
            Message::Request(request) => {
                assert_eq!(request.method.as_str(), "MESSAGE");
                assert_eq!(
                    request.data.body.first(),
                    Some(&0),
                    "only one-part outgoing RP-DATA is expected"
                );
                assert!(
                    request
                        .data
                        .headers
                        .get("Security-Verify")
                        .unwrap()
                        .value()
                        .contains("aes-cbc")
                );
                assert!(
                    request
                        .data
                        .headers
                        .get("Route")
                        .unwrap()
                        .value()
                        .contains("10.20.30.9:6060")
                );
                let target = if self.module {
                    "tel:+447802002606".into()
                } else {
                    format!("sip:smsc@{DOMAIN}")
                };
                assert_eq!(request.uri.to_string(), target);
                let reference = request.data.body[1];
                let mut tpdu = proletarius_sms::submit::prepare(
                    sms_peer::RECIPIENT,
                    sms_peer::OUTGOING,
                    reference,
                )
                .unwrap()
                .remove(0);
                tpdu[1] = reference;
                assert_eq!(
                    request.data.body,
                    proletarius_sms::build_mobile_originated_data(
                        reference,
                        "+447802002606",
                        &tpdu
                    )
                    .unwrap()
                );
                let id = match association {
                    1 => {
                        assert!(!self.allow_fresh.load(Ordering::SeqCst));
                        assert_eq!(
                            self.old_submits.fetch_add(1, Ordering::SeqCst),
                            0,
                            "unknown old request replayed on original association"
                        );
                        OLD
                    }
                    2 => {
                        assert!(
                            self.allow_fresh.load(Ordering::SeqCst),
                            "historical ID dispatched on successor before explicit fresh intent"
                        );
                        assert_eq!(
                            self.fresh_submits.fetch_add(1, Ordering::SeqCst),
                            0,
                            "more than one successor RP-DATA"
                        );
                        FRESH
                    }
                    _ => panic!("unexpected association {association}"),
                };
                // Inspect real persisted claim before emitting any acceptance.
                if let Some((store, profile)) = &self.multi {
                    let rows = multi_messages(store);
                    assert_eq!(rows.len(), 1);
                    assert_eq!(rows[0]["profile_id"], *profile);
                    assert_eq!(rows[0]["client_request_id"], id);
                    assert_eq!(rows[0]["state"], "submitting");
                    assert!(!rows[0]["attempt_token"].as_str().unwrap().is_empty());
                } else {
                    let stored = crate::device_send_store::Store::open(&self.root)
                        .unwrap()
                        .apply("get", json!({"client_request_id":id}))
                        .unwrap();
                    assert_eq!(stored["state"], "submitting");
                    assert!(!stored["attempt"].as_str().unwrap().is_empty());
                }
                events
                    .send(Event::RecoverySubmitted(association, reference))
                    .unwrap();
                if association == 1 {
                    // No SIP final response or RP ACK: the test next closes the
                    // real protected TCP association, yielding a real unknown.
                    return;
                }
                let mut wire = ims_peer::response(&request, 202, "", "");
                wire.extend(rp_ack(reference));
                write.write_all(&wire).await.unwrap();
                write.flush().await.unwrap();
            }
        }
    }
}
fn rp_ack(reference: u8) -> Vec<u8> {
    let mut wire = format!("MESSAGE sip:+447700900123@{DOMAIN} SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:6060;branch=z9hG4bK-{RP_ACK}\r\nFrom: <sip:smsc@{DOMAIN}>;tag=carrier\r\nP-Asserted-Identity: <sip:smsc@{DOMAIN}>\r\nTo: <sip:+447700900123@{DOMAIN}>\r\nCall-ID: {RP_ACK}\r\nCSeq: 93 MESSAGE\r\nContent-Type: application/vnd.3gpp.sms\r\nContent-Length: 2\r\n\r\n").into_bytes();
    wire.extend([3, reference]);
    wire
}

struct RoleFixture {
    fixture: Fixture,
    server: crate::ServerState,
    module: Option<(
        crate::usb_at::Owner,
        crate::module_radio::Service,
        Arc<crate::module_vowifi_card::tests::Trace>,
    )>,
}
impl RoleFixture {
    async fn new(root: PathBuf, module: bool) -> Self {
        std::fs::create_dir_all(&root).unwrap();
        // The same real production open/recover hook as daemon startup. This
        // is deliberately not recover_go_owned(), a cloned Arc or seeded JSON.
        let (store, sends, forwarding) = crate::initialize_sms_stores(&root).unwrap();
        let fixture = Fixture::new_in(
            root,
            "+447802002606",
            "sip:smsc@ims.mnc033.mcc234.3gppnetwork.org",
        );
        let mut server = fixture.sms_state();
        server.multistandby_store = store;
        server.device_send_store = sends;
        server.forwarding_handoff_store = forwarding;
        let module = if module {
            let (usb, radio, trace) =
                crate::module_vowifi_card::tests::service_fixture(fixture.root.clone()).await;
            server.usb_at = usb.clone();
            server.module_radio = radio.clone();
            confirm_idle_module_call_observer(&server, radio.clone()).await;
            Some((usb, radio, trace))
        } else {
            install_synthetic_idle_module_call_observer(&fixture).await;
            None
        };
        Self {
            fixture,
            server,
            module,
        }
    }
    async fn start(&self, proxy: std::net::SocketAddr) -> NativeSmsBinding {
        let (path, body) = if self.module.is_some() {
            (
                "/api/vowifi",
                json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true}),
            )
        } else {
            (
                "/api/reader/vowifi",
                json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":true,"proxy":proxy.to_string(),"epdg":"epdg.invalid","epdg_port":500}),
            )
        };
        let (code, value) = sms_http(&self.server, "POST", path, body).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{value}");
        if self.module.is_some() {
            module_exchange::active(&self.server).await;
            self.server
                .native_vowifi_lifecycle
                .retained_module_service()
                .unwrap()
                .sms_binding()
                .unwrap()
        } else {
            self.fixture.registered().await
        }
    }
    fn request(&self, id: &str) -> serde_json::Value {
        json!({"client_request_id":id,"phone":sms_peer::RECIPIENT,"message":sms_peer::OUTGOING,"device":if self.module.is_some(){"dji"}else{"reader"},"expected_subscription_id":native_device_sms_subscription_id(ICCID,IMSI).unwrap()})
    }
    fn record(&self, id: &str) -> serde_json::Value {
        self.server
            .device_send_store
            .apply("get", json!({"client_request_id":id}))
            .unwrap()
    }
    async fn finish(self) {
        if let Some((_, _, trace)) = self.module.as_ref() {
            let (code, value) = sms_http(&self.server, "POST", "/api/vowifi", json!({"observe_operation":true,"client_request_id":crate::snapshot::new_instance_id(),"enable":false})).await;
            assert_eq!(code, StatusCode::ACCEPTED, "{value}");
            tokio::time::timeout(Duration::from_secs(12), async {
                loop {
                    let (_, result) =
                        sms_http(&self.server, "GET", "/api/vowifi/result", json!({})).await;
                    if result["done"] == true {
                        assert!(result.get("error").is_none(), "{result}");
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("actual Module join and RF restoration");
            assert_eq!(trace.mode.load(Ordering::Acquire), 1);
            assert!(!self.server.module_radio.vowifi_owns_sim_channel());
            assert!(
                !self
                    .server
                    .native_vowifi_lifecycle
                    .retained_module_service()
                    .unwrap()
                    .owns_session()
            );
        } else {
            gateway::stop(&self.fixture).await;
        }
        let Self {
            fixture,
            server,
            module,
        } = self;
        if let Some((usb, radio, _)) = module {
            radio.finish_shutdown().await.unwrap();
            usb.shutdown().await.unwrap();
            drop(server);
            fixture.finish_idle_reader().await;
        } else {
            drop(server);
            fixture.finish().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_unknown_reopen_successor_never_replays_old_id_then_fresh_sms() {
    if std::env::var_os(CHILD).is_none() {
        for role in ["reader", "module"] {
            let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
                "runtime/s08-705-{role}-{}",
                crate::multistandby_store::random_token().unwrap()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let config = root.join("carrier.yaml");
            std::fs::write(
                &config,
                "carrier_overrides:\n  '234/33': {ip_stack: ipv4, custom_epdg: epdg.invalid}\n",
            )
            .unwrap();
            let role_name = role.to_owned();
            let evidence = root.join("child-execution");
            let result = tokio::task::spawn_blocking(move || {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                sanitize_child(&mut command);
                command
                    .args(["--exact", EXACT, "--test-threads=1", "--nocapture"])
                    .env(CHILD, &root)
                    .env(ROLE, &role_name)
                    .env("SWITCH_DATA_DIR", &root)
                    .env("PROLETARIUS_VOWIFI_CONFIG", &config)
                    .output()
                    .unwrap()
            })
            .await
            .unwrap();
            // Failure evidence survives assertion, including a child timeout or
            // panic. Runtime/ledger roots are intentionally never deleted here.
            std::fs::create_dir_all(&evidence).unwrap();
            std::fs::write(evidence.join("stdout.log"), &result.stdout).unwrap();
            std::fs::write(evidence.join("stderr.log"), &result.stderr).unwrap();
            std::fs::write(evidence.join("status.txt"), result.status.to_string()).unwrap();
            assert!(
                result.status.success(),
                "role={role}\n{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            println!(
                "S08-705 role={role}: {}",
                String::from_utf8_lossy(&result.stdout)
            );
        }
        return;
    }
    let role = std::env::var(ROLE).unwrap();
    let root = PathBuf::from(std::env::var_os(CHILD).unwrap()).join("same-durable-root");
    tokio::time::timeout(Duration::from_secs(75), scenario(root, role == "module"))
        .await
        .expect("S08 wire/reopen composition deadline");
}
async fn scenario(root: PathBuf, module: bool) {
    let subscription = native_device_sms_subscription_id(ICCID, IMSI).unwrap();
    let facts = Arc::new(WireFacts::new(root.clone(), module));
    let (carrier, mut client) =
        renewal_peer::new_recovery_sms(root.clone(), subscription, facts.clone());
    let peer = ike_peer::Peer::with_renewal(carrier).await;
    if module {
        crate::system_proxy::TEST_SCUTIL_OUTPUT
            .set(format!(
                "<dictionary> {{\n SOCKSEnable : 1\n SOCKSProxy : 127.0.0.1\n SOCKSPort : {}\n}}\n",
                peer.proxy.port()
            ))
            .unwrap();
    }
    let first = RoleFixture::new(root.clone(), module).await;
    let old_binding = first.start(peer.proxy).await;
    assert_eq!(client.next().await, Event::Registered(1));
    let old_body = first.request(OLD);
    let (code, prepared) = sms_http(
        &first.server,
        "POST",
        "/api/sms/send-intents",
        old_body.clone(),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{prepared}");
    assert_eq!(prepared["can_submit"], true);
    let server = first.server.clone();
    let submit =
        tokio::spawn(async move { sms_http(&server, "POST", "/api/sms/send", old_body).await });
    assert!(matches!(
        client.next().await,
        Event::RecoverySubmitted(1, _)
    ));
    assert_eq!(first.record(OLD)["state"], "submitting");
    client.command(1, Command::CloseSignaling).await;
    assert_eq!(client.next().await, Event::SignalingClosed(1));
    let (code, unknown) = submit.await.unwrap();
    assert_eq!(code, StatusCode::BAD_GATEWAY, "{unknown}");
    assert_eq!(unknown["code"], "sms_send_outcome_unknown");
    let original = first.record(OLD);
    assert_eq!(original["state"], "unknown");
    assert_eq!(original["archive_confirmed"], false);
    first.finish().await;
    assert!(
        old_binding.controls.is_closed(),
        "old retained dispatcher is still alive"
    );
    assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 1);
    assert_eq!(
        peer.trace.registers.load(Ordering::SeqCst),
        1,
        "old fixture restarted before full retirement"
    );

    let second = RoleFixture::new(root.clone(), module).await;
    assert_eq!(
        second.record(OLD),
        original,
        "real reopen/recover must retain original unknown receipt"
    );
    let successor = second.start(peer.proxy).await;
    assert_eq!(client.next().await, Event::Registered(2));
    assert_ne!(old_binding.binding, successor.binding);
    assert_ne!(
        old_binding.binding.split(':').next(),
        successor.binding.split(':').next(),
        "a fresh production service epoch must be generated"
    );
    assert_eq!(old_binding.subscription_id, successor.subscription_id);
    if module {
        assert!(
            second
                .server
                .native_vowifi_lifecycle
                .retained_module_service()
                .unwrap()
                .resolve_sms_binding(&old_binding.binding)
                .is_err()
        );
    } else {
        assert!(
            second
                .fixture
                .service
                .resolve_sms_binding(&old_binding.binding)
                .is_err()
        );
    }
    let (code, historical) = sms_http(
        &second.server,
        "GET",
        &format!("/api/sms/send-intents?client_request_id={OLD}"),
        json!({}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{historical}");
    assert_eq!(historical["state"], "unknown");
    assert_eq!(historical["can_submit"], false);
    assert!(historical.get("binding").is_none() && historical.get("attempt").is_none());
    let (code, replay) =
        sms_http(&second.server, "POST", "/api/sms/send", second.request(OLD)).await;
    assert_eq!(code, StatusCode::BAD_GATEWAY, "{replay}");
    assert_eq!(replay["code"], "sms_send_outcome_unknown");
    let mut changed = second.request(OLD);
    changed["message"] = json!("changed payload");
    let (code, conflict) = sms_http(&second.server, "POST", "/api/sms/send", changed).await;
    assert_eq!(code, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["code"], "sms_intent_conflict");
    let (code, reprepared) = sms_http(
        &second.server,
        "POST",
        "/api/sms/send-intents",
        second.request(OLD),
    )
    .await;
    if code == StatusCode::OK {
        assert_eq!(reprepared["can_submit"], false);
        assert_eq!(reprepared["state"], "unknown");
    } else {
        assert_eq!(code, StatusCode::CONFLICT, "{reprepared}");
        assert_eq!(reprepared["code"], "sms_intent_conflict");
    }
    client.command(2, Command::RecoveryBarrier).await;
    assert_eq!(client.next().await, Event::RecoveryBarrier(2));
    assert_eq!(facts.old_submits.load(Ordering::SeqCst), 1);
    assert_eq!(facts.fresh_submits.load(Ordering::SeqCst), 0);
    assert_eq!(second.record(OLD), original);

    facts.allow_fresh.store(true, Ordering::SeqCst);
    let fresh = second.request(FRESH);
    let (code, prepared) = sms_http(
        &second.server,
        "POST",
        "/api/sms/send-intents",
        fresh.clone(),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{prepared}");
    assert_eq!(prepared["can_submit"], true);
    let (code, sent) = sms_http(&second.server, "POST", "/api/sms/send", fresh.clone()).await;
    assert_eq!(code, StatusCode::OK, "{sent}");
    assert_eq!(sent["sent"], true);
    assert!(matches!(
        client.next().await,
        Event::RecoverySubmitted(2, _)
    ));
    assert_eq!(client.next().await, Event::RecoveryRpAck(2));
    let accepted = second.record(FRESH);
    assert_eq!(accepted["state"], "accepted");
    assert_eq!(accepted["archive_confirmed"], true);
    assert_eq!(accepted["segments"], 1);
    assert_ne!(accepted["attempt"], original["attempt"]);
    let rows = proletarius_store::ArchiveStore::new(root.join("sms-archive.json"))
        .load()
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "unknown old intent must not be archived as a new accepted SMS"
    );
    assert_eq!(rows[0]["send_request_id"], FRESH);
    assert_eq!(rows[0]["subscription_id"], successor.subscription_id);
    assert_eq!(rows[0]["content"], sms_peer::OUTGOING);
    let (code, repeated) = sms_http(&second.server, "POST", "/api/sms/send", fresh).await;
    assert_eq!(code, StatusCode::OK, "{repeated}");
    assert_eq!(repeated["sent"], true);
    client.command(2, Command::RecoveryBarrier).await;
    assert_eq!(client.next().await, Event::RecoveryBarrier(2));
    assert_eq!(second.record(OLD), original);
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    second.finish().await;
    assert_eq!(client.next().await, Event::SignalingClosed(2));
    assert!(successor.controls.is_closed());
    assert_eq!(peer.trace.registers.load(Ordering::SeqCst), 2);
    assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 2);
    peer.finish().await;
    // Final peer join observes every outstanding frame; no empty-channel or
    // timing-only assertion substitutes for the absence of duplicate dispatch.
    assert_eq!(facts.old_submits.load(Ordering::SeqCst), 1);
    assert_eq!(facts.fresh_submits.load(Ordering::SeqCst), 1);
    assert_eq!(facts.fresh_rp_ack.load(Ordering::SeqCst), 1);
    assert_eq!(facts.barriers.load(Ordering::SeqCst), 2);
    println!(
        "same-root reopen/full-recover; fresh production epoch; old GET/POST fenced; wire RP-DATA old=1 fresh=1; fresh RP-ACK=1 archive=1; all owners joined"
    );
}

// Each composed test owns its configuration and daemon data in a child process.
// Opt-ins for external SIM/operator delivery and inherited test child markers
// are removed before starting it. The runtime roots and failed logs survive.
async fn isolated_child(exact: &'static str, marker: &'static str) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os(marker) {
        return Some(root.into());
    }
    let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
        "runtime/s08-705-{}-{}",
        marker.to_ascii_lowercase(),
        crate::multistandby_store::random_token().unwrap()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let config = root.join("carrier.yaml");
    std::fs::write(
        &config,
        "carrier_overrides:\n  '234/33': {ip_stack: ipv4, custom_epdg: epdg.invalid}\n",
    )
    .unwrap();
    let evidence = root.join("child-execution");
    let result = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        sanitize_child(&mut command);
        command
            .args(["--exact", exact, "--test-threads=1", "--nocapture"])
            .env(marker, &root)
            .env("SWITCH_DATA_DIR", &root)
            .env("PROLETARIUS_VOWIFI_CONFIG", config)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    std::fs::create_dir_all(&evidence).unwrap();
    std::fs::write(evidence.join("stdout.log"), &result.stdout).unwrap();
    std::fs::write(evidence.join("stderr.log"), &result.stderr).unwrap();
    std::fs::write(evidence.join("status.txt"), result.status.to_string()).unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    println!("{}", String::from_utf8_lossy(&result.stdout));
    None
}
fn sanitize_child(command: &mut std::process::Command) {
    for (name, _) in std::env::vars_os() {
        let text = name.to_string_lossy();
        if text.starts_with("SWITCH_TEST_REAL_")
            || text.starts_with("SWITCH_REAL_")
            || text.ends_with("_CHILD")
            || text == "SWITCH_EXPECT_PROXY"
            || text == ROLE
        {
            command.env_remove(name);
        }
    }
}
fn multi_messages(store: &crate::multistandby_store::Store) -> Vec<serde_json::Value> {
    let value: serde_json::Value =
        serde_json::from_reader(store.snapshot_file().unwrap().unwrap()).unwrap();
    let snapshots = value.as_array().expect("ArchiveStore snapshot array");
    assert_eq!(
        snapshots.len(),
        1,
        "Nwait store contains one atomic snapshot"
    );
    snapshots[0]["messages"]
        .as_object()
        .unwrap()
        .values()
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn module_remote_eof_auto_recovers_then_first_public_sms() {
    let Some(root) = isolated_child(
        "native_vowifi_session::service::production_integration::s08_recovery::module_remote_eof_auto_recovers_then_first_public_sms",
        "SWITCH_S08_705_MODULE_CHILD",
    ).await else { return };
    tokio::time::timeout(Duration::from_secs(60), module_eof(root))
        .await
        .expect("Module automatic recovery composition deadline");
}
async fn module_eof(root: PathBuf) {
    let facts = Arc::new(WireFacts::new(root.clone(), true));
    let (carrier, mut client) = renewal_peer::new_recovery_sms(
        root.clone(),
        native_device_sms_subscription_id(ICCID, IMSI).unwrap(),
        facts.clone(),
    );
    let peer = ike_peer::Peer::with_renewal(carrier).await;
    crate::system_proxy::TEST_SCUTIL_OUTPUT
        .set(format!(
            "<dictionary> {{\n SOCKSEnable : 1\n SOCKSProxy : 127.0.0.1\n SOCKSPort : {}\n}}\n",
            peer.proxy.port()
        ))
        .unwrap();
    let role = RoleFixture::new(root.clone(), true).await;
    let first = role.start(peer.proxy).await;
    assert_eq!(client.next().await, Event::Registered(1));
    let trace = role.module.as_ref().unwrap().2.clone();
    let restore = Arc::new(crate::module_vowifi_card::tests::Block::default());
    *trace.blocked.lock().unwrap() = Some(("AT+CFUN=1".into(), restore.clone()));
    client.command(1, Command::CloseSignaling).await;
    assert_eq!(client.next().await, Event::SignalingClosed(1));
    tokio::time::timeout(Duration::from_secs(12), restore.entered.notified())
        .await
        .expect("real EOF leads to actual RF restore");
    assert_eq!(
        peer.trace.deletes.load(Ordering::SeqCst),
        1,
        "real network retirement precedes RF restoration"
    );
    assert!(
        first.controls.is_closed(),
        "retired dispatcher must join before RF restore"
    );
    assert_eq!(peer.trace.registers.load(Ordering::SeqCst), 1);
    assert_eq!(trace.mode.load(Ordering::SeqCst), 4);
    assert!(role.server.module_radio.vowifi_owns_sim_channel());
    assert!(
        role.server
            .native_vowifi_lifecycle
            .retained_module_service()
            .unwrap()
            .resolve_sms_binding(&first.binding)
            .is_err()
    );
    restore.release();
    // No public enable/retry request is sent: the retained Module owner performs
    // its own release-gated retry after the old generation's joined cleanup.
    assert_eq!(client.next().await, Event::Registered(2));
    let online = module_exchange::active(&role.server).await;
    assert_eq!(online["sim_iccid"], ICCID);
    let successor = role
        .server
        .native_vowifi_lifecycle
        .retained_module_service()
        .unwrap()
        .sms_binding()
        .unwrap();
    assert!(successor.generation.value() > first.generation.value());
    assert_eq!(successor.subscription_id, first.subscription_id);
    assert_eq!(trace.auth.load(Ordering::SeqCst), 4);
    client.command(2, Command::RecoveryBarrier).await;
    assert_eq!(client.next().await, Event::RecoveryBarrier(2));
    assert_eq!(facts.old_submits.load(Ordering::SeqCst), 0);
    assert_eq!(facts.fresh_submits.load(Ordering::SeqCst), 0);
    facts.allow_fresh.store(true, Ordering::SeqCst);
    let body = role.request(FRESH);
    let (status, prepared) =
        sms_http(&role.server, "POST", "/api/sms/send-intents", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    assert_eq!(prepared["can_submit"], true);
    let (status, sent) = sms_http(&role.server, "POST", "/api/sms/send", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert_eq!(sent["sent"], true);
    assert!(matches!(
        client.next().await,
        Event::RecoverySubmitted(2, _)
    ));
    assert_eq!(client.next().await, Event::RecoveryRpAck(2));
    let accepted = role.record(FRESH);
    assert_eq!(accepted["state"], "accepted");
    assert_eq!(accepted["archive_confirmed"], true);
    let rows = proletarius_store::ArchiveStore::new(root.join("sms-archive.json"))
        .load()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["send_request_id"], FRESH);
    assert_eq!(rows[0]["subscription_id"], successor.subscription_id);
    assert_eq!(rows[0]["device"], "dji");
    let (status, repeat) = sms_http(&role.server, "POST", "/api/sms/send", body).await;
    assert_eq!(status, StatusCode::OK, "{repeat}");
    client.command(2, Command::RecoveryBarrier).await;
    assert_eq!(client.next().await, Event::RecoveryBarrier(2));
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    role.finish().await;
    assert_eq!(client.next().await, Event::SignalingClosed(2));
    assert!(successor.controls.is_closed());
    assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 2);
    assert_eq!(peer.trace.registers.load(Ordering::SeqCst), 2);
    peer.finish().await;
    assert_eq!(facts.old_submits.load(Ordering::SeqCst), 0);
    assert_eq!(facts.fresh_submits.load(Ordering::SeqCst), 1);
    assert_eq!(facts.fresh_rp_ack.load(Ordering::SeqCst), 1);
    assert_eq!(facts.barriers.load(Ordering::SeqCst), 2);
    println!(
        "Module actual TCP EOF -> DELETE/join -> blocked RF restore -> automatic generation2 -> first public SMS/RP-ACK/archive; RP-DATA=1; all owners joined"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nwait_transient_register_failure_cleanup_due_retry_then_first_public_sms() {
    let Some(root) = isolated_child(
        "native_vowifi_session::service::production_integration::s08_recovery::nwait_transient_register_failure_cleanup_due_retry_then_first_public_sms",
        "SWITCH_S08_705_NWAIT_CHILD",
    ).await else { return };
    tokio::time::timeout(Duration::from_secs(60), nwait_503(root))
        .await
        .expect("Nwait REGISTER503 due retry composition deadline");
}
async fn nwait_503(root: PathBuf) {
    use crate::multistandby_native_runtime::{Action, ConnectionOptions, Resources};
    use crate::multistandby_native_service::Service as Facade;
    use crate::multistandby_runtime_report::Journal;
    let physical = crate::multistandby_card::production::NativeFixture::new();
    physical.pool.shutdown().await.unwrap();
    let store = crate::multistandby_store::Store::open(&root).unwrap();
    let journal_path = root.join("runtime-report.json");
    let journal = Arc::new(Mutex::new(Journal::open(&journal_path, epoch_ms())));
    // Build the real discovery/manager facade with only the physical card factory
    // and remote carrier replaced. No registration/readiness flags are injected.
    let (owner, gate, card_service) = physical.inventory_resources();
    let mut resources = Resources {
        owner,
        gate,
        card_service,
        data_root: root.clone(),
        store: store.clone(),
        journal: journal.clone(),
        transactions: Default::default(),
        connection: ConnectionOptions::default(),
    };
    let discovery = Facade::new(Resources {
        owner: resources.owner.clone(),
        gate: resources.gate.clone(),
        card_service: resources.card_service.clone(),
        data_root: resources.data_root.clone(),
        store: resources.store.clone(),
        journal: resources.journal.clone(),
        transactions: resources.transactions.clone(),
        connection: ConnectionOptions::default(),
    })
    .unwrap();
    assert_eq!(
        discovery.snapshot("", true, false).await.unwrap()["available"],
        true
    );
    let snapshot = discovery.snapshot("", false, false).await.unwrap();
    let rows = snapshot["profiles"].as_array().unwrap();
    let profile = rows
        .iter()
        .find(|row| row["iccid"] == physical.profile(true).iccid)
        .unwrap()["profile_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let card = rows[0]["card_id"].as_str().unwrap().to_owned();
    discovery.shutdown().await.unwrap();
    let mut wire = WireFacts::new(root.clone(), false);
    wire.multi = Some((store.clone(), profile.clone()));
    wire.reject_initial_register = true;
    let facts = Arc::new(wire);
    let (carrier, mut client) =
        renewal_peer::new_recovery_sms(root.clone(), String::new(), facts.clone());
    let peer = ike_peer::Peer::with_renewal(carrier).await;
    resources.connection = ConnectionOptions {
        proxy: peer.proxy.to_string(),
        epdg: "epdg.invalid".into(),
        epdg_port: Some(500),
    };
    let facade = Facade::new(resources).unwrap();
    assert_eq!(
        facade.snapshot("", true, false).await.unwrap()["available"],
        true
    );
    let fixture = Fixture::new_in(
        root.clone(),
        "+447802002606",
        "sip:smsc@ims.mnc033.mcc234.3gppnetwork.org",
    );
    let mut state = fixture.sms_state();
    state.multistandby_service = Arc::new(std::sync::OnceLock::from(Ok(facade.clone())));
    state.multistandby_store = store.clone();
    state.multi_runtime_report = journal.clone();
    let (status, start) = sms_http(
        &state,
        "POST",
        "/api/multistandby/start",
        json!({"card_id":card,"profile_ids":[profile],"client_request_id":"s08-nwait-start"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{start}");
    assert_eq!(client.next().await, Event::RegisterRejected(1));
    assert_eq!(client.next().await, Event::SignalingClosed(1));
    let due = tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let snapshot = facade.snapshot("", false, false).await.unwrap();
            let row = snapshot["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["profile_id"] == profile)
                .unwrap();
            if row["auth_state"] == "backoff" {
                if let Some(due) = journal.lock().unwrap().next_due_retry(i64::MAX) {
                    break due;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("real REGISTER503 cleanup yields durable automatic retry");
    assert_eq!(due.profile_id, profile);
    assert!(!due.attempt_id.is_empty());
    assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 1);
    assert_eq!(physical.transaction_count(), 0);
    assert!(!facade.ready(&profile).channel_ready);
    assert_eq!(peer.trace.registers.load(Ordering::SeqCst), 1);
    let reopened = Journal::open(&journal_path, epoch_ms());
    assert_eq!(
        reopened.next_due_retry(i64::MAX),
        Some(due.clone()),
        "retry notification must survive a real journal open"
    );
    // Use the exact production 250ms durable due scheduler. No manual Retry,
    // direct dispatch_retry_due or edited due time can cause generation2.
    let (stop, stopping) = watch::channel(false);
    let scheduler = tokio::spawn(crate::multistandby_retry::run(
        journal.clone(),
        facade.clone(),
        stopping,
    ));
    assert_eq!(client.next().await, Event::Registered(2));
    tokio::time::timeout(Duration::from_secs(12), async {
        while !facade.ready(&profile).channel_ready {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(physical.auth_count(), 4);
    assert!(journal.lock().unwrap().next_due_retry(i64::MAX).is_none());
    assert!(
        Journal::open(&journal_path, epoch_ms())
            .next_due_retry(i64::MAX)
            .is_none()
    );
    client.command(2, Command::RecoveryBarrier).await;
    assert_eq!(client.next().await, Event::RecoveryBarrier(2));
    assert!(multi_messages(&store).is_empty());
    facts.allow_fresh.store(true, Ordering::SeqCst);
    let body = json!({"profile_id":profile,"client_request_id":FRESH,"phone":sms_peer::RECIPIENT,"body":sms_peer::OUTGOING});
    let (status, queued) = sms_http(
        &state,
        "POST",
        "/api/multistandby/messages/send",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    assert!(facade.maintenance());
    assert!(matches!(
        client.next().await,
        Event::RecoverySubmitted(2, _)
    ));
    assert_eq!(client.next().await, Event::RecoveryRpAck(2));
    tokio::time::timeout(Duration::from_secs(5), async {
        while multi_messages(&store)[0]["state"] != "accepted" {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let rows = multi_messages(&store);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["profile_id"], profile);
    assert_eq!(rows[0]["message_id"], queued["message_id"]);
    assert_eq!(rows[0]["client_request_id"], FRESH);
    assert_eq!(rows[0]["body"], sms_peer::OUTGOING);
    assert_eq!(rows[0]["state"], "accepted");
    let (status, repeated) =
        sms_http(&state, "POST", "/api/multistandby/messages/send", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{repeated}");
    assert_eq!(repeated["message_id"], queued["message_id"]);
    assert!(facade.maintenance());
    client.command(2, Command::RecoveryBarrier).await;
    assert_eq!(client.next().await, Event::RecoveryBarrier(2));
    stop.send_replace(true);
    scheduler.await.unwrap();
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    facade
        .execute(Action::Stop, card, vec![profile])
        .await
        .unwrap();
    facade.shutdown().await.unwrap();
    assert_eq!(client.next().await, Event::SignalingClosed(2));
    assert_eq!(peer.trace.registers.load(Ordering::SeqCst), 2);
    assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 2);
    assert_eq!(physical.transaction_count(), 0);
    drop(state);
    fixture.finish_idle_reader().await;
    peer.finish().await;
    physical.finish().await;
    assert_eq!(multi_messages(&store).len(), 1);
    assert_eq!(facts.old_submits.load(Ordering::SeqCst), 0);
    assert_eq!(facts.fresh_submits.load(Ordering::SeqCst), 1);
    assert_eq!(facts.fresh_rp_ack.load(Ordering::SeqCst), 1);
    assert_eq!(facts.barriers.load(Ordering::SeqCst), 2);
    println!(
        "Nwait actual REGISTER503 -> DELETE/join/card release -> durable due reopen -> production due scheduler -> same-profile REGISTER200 -> first public queue/SIP/RP-ACK/accepted archive; RP-DATA=1; all owners joined"
    );
}
fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
