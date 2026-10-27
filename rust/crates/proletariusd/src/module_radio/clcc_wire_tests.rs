//! Byte-shaped observations through real USB ownership, radio reservations,
//! NativeDriver and public call DTO handlers. Only transport I/O is synthetic.
use super::*;
use crate::call_control;
use crate::usb_at::reconnect_tests::{self, Trace};
use http_body_util::BodyExt;
use std::sync::atomic::AtomicUsize;

const PRODUCER: &str = "clcc-wire-fixture";
const SESSION: &str = "clcc-wire-session";
// Same standard response as the fragmented NativeTransport fixture.
const DATA_ONLY: &str = "AT+CLCC\r\n+CLCC: 1,0,0,1,0,\"\",128\r\nOK\r\n";
const VOICE: &str = "+CLCC: 1,0,0,0,0,\"\",128\r\nOK\r\n";

struct Fixture {
    radio: Service,
    calls: crate::module_call::Service,
    trace: Arc<Trace>,
    factory_calls: Arc<AtomicUsize>,
    root: PathBuf,
    lifetime: Option<crate::daemon_lifetime::Lease>,
}
impl Fixture {
    async fn new(response: &str) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/clcc-wire-tests")
            .join(crate::snapshot::new_instance_id());
        std::fs::create_dir_all(&root).unwrap();
        let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let opened = factory_calls.clone();
        let usb = crate::usb_at::Owner::with_factory(
            PRODUCER.into(),
            Arc::new(move || {
                opened.fetch_add(1, Ordering::AcqRel);
                Err(crate::usb_at_transport::TransportError {
                    code: "usb_at_not_found",
                    message: "fixture forbids physical device discovery".into(),
                    response: String::new(),
                    uncertain: false,
                })
            }),
        )
        .with_recovery_gate_for_tests(gate.clone());
        let trace = reconnect_tests::install_default(&usb, SESSION);
        *trace.clcc_response.lock().unwrap() = response.into();
        let modem = crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap();
        let radio = Service::new(
            root.clone(),
            usb.clone(),
            modem,
            BackendAvailability::with_module_recovery(gate),
            PRODUCER.into(),
        );
        radio
            .0
            .store
            .save(Preferences {
                enabled: false,
                restore_cfun: 1,
                user_configured: true,
            })
            .unwrap();
        assert!(radio.0.physical_fixture.lock().unwrap().is_none());
        radio
            .submit(Job::Configure(Configure {
                producer_id: PRODUCER.into(),
                config_seq: 1,
                transport: "usb".into(),
                session_id: SESSION.into(),
            }))
            .await
            .unwrap();
        // Earn RF readiness through the production plan and the USB owner.
        radio
            .submit(Job::Bootstrap(Producer {
                producer_id: PRODUCER.into(),
            }))
            .await
            .unwrap();
        let inbox = crate::message_archive::InboxState::for_go_producer(PRODUCER.into());
        let mode = crate::call_mode_lifecycle::Service::new(
            &root,
            radio.clone(),
            inbox.clone(),
            usb.clone(),
        );
        let facts = crate::call_facts::CallFacts::new(
            proletarius_store::ArchiveStore::new(root.join("calls.json")),
            PRODUCER.into(),
        );
        let calls =
            crate::module_call::Service::new(root.clone(), radio.clone(), inbox, usb, mode, facts)
                .unwrap();
        Self {
            radio,
            calls,
            trace,
            factory_calls,
            root,
            lifetime: Some(lifetime),
        }
    }

    async fn status(&self, expected: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let reply = call_control::handle_status(self.calls.clone()).await;
                assert_eq!(reply.status(), StatusCode::OK);
                let value = body(reply).await;
                if value["state"] == expected {
                    return value;
                }
                // Status may return the previous snapshot while acquisition or
                // release owns the call lane; never replace that lane in tests.
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("native call status did not reach the expected observation")
    }

    async fn mode_idle(&self) {
        let lease = self.radio.reserve_call_mode_operation().await.unwrap();
        {
            let state = self.radio.0.state.lock().unwrap();
            let owner = state.reservation.as_ref().unwrap();
            assert_eq!(owner.purpose, Purpose::CallMode);
            assert!(owner.operation_lock.is_some());
        }
        self.radio.call_mode_ensure_idle(&lease).await.unwrap();
        self.radio
            .release_call_mode_operation(&lease)
            .await
            .unwrap();
        assert!(self.radio.0.state.lock().unwrap().reservation.is_none());
    }

    fn only_read_commands(&self) {
        assert_eq!(self.factory_calls.load(Ordering::Acquire), 0);
        let commands = self.trace.commands.lock().unwrap();
        assert!(commands.iter().any(|command| command == "AT+CFUN?"));
        assert!(commands.iter().any(|command| command == "AT+CLCC"));
        assert!(
            commands
                .iter()
                .all(|command| matches!(command.as_str(), "AT+CFUN?" | "AT+CLCC")),
            "{commands:?}"
        );
    }

    async fn finish(mut self) {
        self.calls.finish_shutdown().await.unwrap();
        self.radio.finish_shutdown().await.unwrap();
        self.radio.0.usb.shutdown().await.unwrap();
        self.radio.0.modem.shutdown().unwrap();
        assert_eq!(self.factory_calls.load(Ordering::Acquire), 0);
        drop(self.lifetime.take());
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.calls.begin_shutdown();
        self.radio.begin_shutdown();
        let _ = self.radio.0.modem.shutdown();
        // Failed tests retain their isolated directory for runner diagnostics.
    }
}

async fn body(reply: call_control::Reply) -> Value {
    serde_json::from_slice(&reply.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native radio ownership did not settle");
}

#[tokio::test]
async fn usb_packet_data_http_polling_and_idle_hangup_never_acquire_call_owner() {
    let f = Fixture::new(DATA_ONLY).await;
    for _ in 0..3 {
        let value = f.status("idle").await;
        assert!(
            !value["number"]
                .as_str()
                .is_some_and(|number| !number.is_empty())
        );
        let state = f.radio.0.state.lock().unwrap();
        assert!(state.reservation.is_none());
        assert!(!state.waters.contains_key(&Purpose::Call));
    }
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/call/hangup")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let reply = call_control::handle_hangup(request, f.calls.clone()).await;
    assert_eq!(reply.status(), StatusCode::OK);
    let value = body(reply).await;
    assert_eq!(value["accepted"], true);
    assert_eq!(value["already_idle"], true);
    assert!(
        !f.radio
            .0
            .state
            .lock()
            .unwrap()
            .waters
            .contains_key(&Purpose::Call)
    );
    f.mode_idle().await;
    assert!(
        !f.radio
            .0
            .state
            .lock()
            .unwrap()
            .waters
            .contains_key(&Purpose::Call)
    );
    f.only_read_commands();
    f.finish().await;
}

#[tokio::test]
async fn usb_anonymous_voice_owner_blocks_mode_until_data_only_releases_exact_lease() {
    let f = Fixture::new(VOICE).await;
    let value = f.status("active").await;
    assert!(
        !value["number"]
            .as_str()
            .is_some_and(|number| !number.is_empty())
    );
    until(|| {
        f.radio
            .0
            .state
            .lock()
            .unwrap()
            .reservation
            .as_ref()
            .is_some_and(|owner| owner.purpose == Purpose::Call)
    })
    .await;
    let proof = {
        let state = f.radio.0.state.lock().unwrap();
        let owner = state.reservation.as_ref().unwrap();
        (
            state.epoch,
            state.producer.clone(),
            owner.identity.token.clone(),
            owner.identity.sequence,
        )
    };
    let before = f.trace.commands.lock().unwrap().len();
    let failure = f.radio.reserve_call_mode_operation().await.unwrap_err();
    assert_eq!(failure.public_status(), StatusCode::CONFLICT);
    assert_eq!(failure.public_code(), "module_radio_busy");
    assert_eq!(f.trace.commands.lock().unwrap().len(), before);

    *f.trace.clcc_response.lock().unwrap() = DATA_ONLY.into();
    f.status("idle").await;
    until(|| {
        f.radio
            .0
            .state
            .lock()
            .unwrap()
            .confirmed_call_releases
            .contains(&proof)
    })
    .await;
    {
        let state = f.radio.0.state.lock().unwrap();
        assert!(state.reservation.is_none());
        let water = state.waters.get(&Purpose::Call).unwrap();
        assert_eq!(water.token, proof.2);
        assert_eq!(water.sequence, proof.3);
        assert!(water.released);
    }
    f.mode_idle().await;
    f.status("idle").await;
    {
        let state = f.radio.0.state.lock().unwrap();
        let water = state.waters.get(&Purpose::Call).unwrap();
        assert_eq!(water.token, proof.2);
        assert_eq!(water.sequence, proof.3);
        assert!(water.released);
        assert!(state.confirmed_call_releases.contains(&proof));
        assert!(state.reservation.is_none());
    }
    f.only_read_commands();
    f.finish().await;
}
