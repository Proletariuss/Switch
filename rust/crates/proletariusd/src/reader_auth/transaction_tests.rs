//! Exercise the public auth/EAP handlers over real Unix HTTP connections while
//! borrowing a production Rust workflow and an injected PC/SC transaction.
use crate::{
    eap_identity::EapExchangeStore,
    reader_pcsc::{Binding, Failure, Owner, Physical},
    reader_reservation::{CardTransaction, Gate},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode, service::service_fn};
use hyper_util::rt::TokioIo;
use proletarius_sim::ApduTransport;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use tokio::net::UnixStream;

const READER: &str = "transaction-auth-fixture";
const PRODUCER: &str = "auth-transaction-producer";
const AID: [u8; 8] = [0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff];
const AUTH: &str = "/fixture/auth";
const EAP: &str = "/fixture/eap";
const IDENTITY: &str = "/api/v2/vowifi/eap-identity";

#[derive(Default)]
struct Trace {
    commands: Mutex<Vec<Vec<u8>>>,
    block: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
}
struct Card(Arc<Trace>);
impl Physical for Card {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn ApduTransport<Error = Failure>),
    ) -> Result<(), Failure> {
        body(self);
        Ok(())
    }
    fn close(&mut self) -> Result<(), Failure> {
        Ok(())
    }
}
impl ApduTransport for Card {
    type Error = Failure;
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Failure> {
        self.0.commands.lock().unwrap().push(command.to_vec());
        let block = self.0.block.lock().unwrap().take();
        if let Some((started, finish)) = block {
            started.send(()).unwrap();
            finish.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        Ok(match command[1] {
            0xb2 => [vec![0x61, 10, 0x4f, 8], AID.to_vec(), vec![0x90, 0]].concat(),
            // A synchronization failure exercises the complete SIM request and
            // response path without fabricating an EAP MAC or network keys.
            0x88 => [vec![0xdc, 14], vec![0x5a; 14], vec![0x90, 0]].concat(),
            _ => vec![0x90, 0],
        })
    }
}
struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Fixture {
    gate: Arc<Gate>,
    owner: Owner,
    trace: Arc<Trace>,
    exchanges: EapExchangeStore,
    physical: Binding,
    binding: Value,
}
impl Fixture {
    async fn new() -> Arc<Self> {
        let gate = Arc::new(Gate::default());
        gate.initialize_business_producer(PRODUCER);
        gate.test_seed_selection(READER, 4);
        let trace = Arc::new(Trace::default());
        let open = trace.clone();
        let reset = trace.clone();
        let owner = Owner::with_factory(
            PRODUCER.into(),
            Arc::new(move |_| Ok(Box::new(Card(open.clone())))),
            Arc::new(|| Ok(vec![READER.into()])),
            Arc::new(move |_, _| Ok(Box::new(Card(reset.clone())))),
        );
        let physical = Binding {
            producer_id: PRODUCER.into(),
            reader: READER.into(),
            session_id: "auth-session".into(),
            lease_id: "auth-physical-lease".into(),
            lease_seq: 1,
        };
        owner.test_open_acquire(&physical, 1).unwrap();
        let fixture = Arc::new(Self {
            gate,
            owner,
            trace,
            exchanges: EapExchangeStore::default(),
            physical,
            binding: json!({"producer_id":PRODUCER,"operation_token":"auth-operation-token-000000000000000001",
                "operation_sequence":1,"reader":READER,"reader_generation":4,"purpose":"card","authority":{"kind":"idle"}}),
        });
        let (status, body) = fixture.operation("acquire").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["phase"], "active");
        // The actual Go acquire gateway registers this same binding before
        // dispatch. Retain that proof even in tests rejecting subsequent AKA.
        fixture
            .gate
            .register_card_pcsc(&fixture.card_transaction(), &fixture.physical)
            .unwrap();
        fixture
    }
    fn transaction(&self) -> Value {
        json!({"kind":"operation","binding":self.binding})
    }
    fn card_transaction(&self) -> CardTransaction {
        serde_json::from_value(self.transaction()).unwrap()
    }
    async fn input(self: &Arc<Self>, endpoint: &str) -> Value {
        if endpoint == AUTH {
            return json!({"reader":READER,"pcsc":self.physical,"transaction":self.transaction(),
                "aid":AID,"rand":vec![0x11;16],"autn":vec![0x22;16]});
        }
        let (status, identity) = self
            .call(
                IDENTITY,
                json!({"identity":b"nai@example","request":[1,9,0,5,1]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{identity}");
        json!({"reader":READER,"pcsc":self.physical,"transaction":self.transaction(),
            "application":"usim","exchange_id":identity["exchange_id"],"challenge":challenge(),"checkcode_mode":"minimal"})
    }
    async fn operation(self: &Arc<Self>, action: &str) -> (StatusCode, Value) {
        self.call(
            &format!("{}operation/{action}", crate::reader_reservation::PREFIX),
            json!({"binding":self.binding}),
        )
        .await
    }
    async fn call(self: &Arc<Self>, path: &str, value: Value) -> (StatusCode, Value) {
        let (client, server) = UnixStream::pair().unwrap();
        let fixture = self.clone();
        let service = tokio::spawn(async move {
            let result = hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    TokioIo::new(server),
                    service_fn(move |request: Request<hyper::body::Incoming>| {
                        let fixture = fixture.clone();
                        async move {
                            let response = match request.uri().path() {
                                AUTH => {
                                    super::handle(
                                        request,
                                        fixture.owner.clone(),
                                        fixture.gate.clone(),
                                    )
                                    .await
                                }
                                EAP => {
                                    crate::reader_eap_aka::handle(
                                        request,
                                        &fixture.exchanges,
                                        fixture.owner.clone(),
                                        fixture.gate.clone(),
                                    )
                                    .await
                                }
                                IDENTITY => {
                                    crate::eap_identity::handle(request, &fixture.exchanges).await
                                }
                                _ => {
                                    crate::reader_reservation::handle_legacy_operation_wire(
                                        request,
                                        fixture.owner.clone(),
                                        fixture.gate.clone(),
                                    )
                                    .await
                                }
                            };
                            Ok::<_, Infallible>(response)
                        }
                    }),
                )
                .await;
            if let Err(error) = result {
                assert!(
                    crate::benign_connection_error_message(&error.to_string()),
                    "{error}"
                );
            }
        });
        let _server_guard = AbortOnDrop(service.abort_handle());
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        let _connection_guard = AbortOnDrop(connection.abort_handle());
        let response = sender
            .send_request(
                Request::post(path)
                    .body(Full::new(Bytes::from(serde_json::to_vec(&value).unwrap())))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        drop(sender);
        if let Err(error) = connection.await.unwrap() {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
        service.await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn finish(self: &Arc<Self>) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let (status, body) = self.operation("release").await;
                assert_eq!(status, StatusCode::OK, "{body}");
                if body["phase"] == "released" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        self.owner.shutdown().await.unwrap();
    }
    fn block(&self) -> (mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (started, entered) = mpsc::sync_channel(1);
        let (finish, wait) = mpsc::sync_channel(1);
        *self.trace.block.lock().unwrap() = Some((started, wait));
        (entered, finish)
    }
}
fn challenge() -> Vec<u8> {
    let mut attributes = Vec::new();
    for (kind, data) in [(1, vec![0x11; 16]), (2, vec![0x22; 16]), (11, vec![0; 16])] {
        attributes.extend([kind, 5, 0, 0]);
        attributes.extend(data);
    }
    let length = 8 + attributes.len();
    [
        vec![1, 77, (length >> 8) as u8, length as u8, 23, 1, 0, 0],
        attributes,
    ]
    .concat()
}

#[tokio::test]
async fn auth_and_eap_borrow_explicit_scope_without_second_admission() {
    for endpoint in [AUTH, EAP] {
        let fixture = Fixture::new().await;
        assert!(fixture.gate.admit(READER).is_err());
        let input = fixture.input(endpoint).await;
        let (status, body) =
            tokio::time::timeout(Duration::from_secs(2), fixture.call(endpoint, input))
                .await
                .unwrap();
        assert_eq!(status, StatusCode::OK, "{endpoint}: {body}");
        assert_eq!(body["outcome"], "synchronization_failure");
        let commands = fixture.trace.commands.lock().unwrap().clone();
        assert_eq!(commands.iter().filter(|c| c[1] == 0x88).count(), 1);
        assert_eq!(commands.len(), if endpoint == AUTH { 2 } else { 5 });
        fixture.finish().await;
    }
}

#[tokio::test]
async fn auth_and_eap_missing_mismatched_and_released_scopes_never_touch_card() {
    for endpoint in [AUTH, EAP] {
        let fixture = Fixture::new().await;
        for bad in 0..4 {
            let mut input = fixture.input(endpoint).await;
            match bad {
                0 => {
                    input.as_object_mut().unwrap().remove("transaction");
                }
                1 => input["transaction"]["binding"]["reader_generation"] = json!(3),
                2 => input["transaction"]["binding"]["producer_id"] = json!("other-producer"),
                _ => {
                    input.as_object_mut().unwrap().remove("pcsc");
                }
            }
            let (status, body) = fixture.call(endpoint, input).await;
            assert_eq!(status, StatusCode::CONFLICT, "{endpoint}: {body}");
            assert_eq!(body["code"], "card_busy");
            assert!(fixture.trace.commands.lock().unwrap().is_empty());
        }
        fixture.finish().await;
        let input = fixture.input(endpoint).await;
        let (status, body) = fixture.call(endpoint, input).await;
        assert_eq!(status, StatusCode::CONFLICT, "{endpoint}: {body}");
        assert!(fixture.trace.commands.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn auth_and_eap_http_cancellation_keeps_real_scope_until_physical_completion() {
    for endpoint in [AUTH, EAP] {
        let fixture = Fixture::new().await;
        let (entered, finish) = fixture.block();
        let input = fixture.input(endpoint).await;
        let caller = fixture.clone();
        let task = tokio::spawn(async move { caller.call(endpoint, input).await });
        tokio::task::spawn_blocking(move || entered.recv_timeout(Duration::from_secs(3)).unwrap())
            .await
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(
            fixture
                .gate
                .track_card_io(&fixture.card_transaction(), PRODUCER, READER)
                .is_err()
        );
        let (status, body) = fixture.operation("release").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["phase"], "draining");
        assert_eq!(
            fixture.owner.status(&fixture.physical).unwrap()["terminal"],
            false
        );
        assert_eq!(fixture.trace.commands.lock().unwrap().len(), 1);
        finish.send(()).unwrap();
        fixture.finish().await;
        // The original synchronous protocol completes once; cancellation never
        // admits another child or replays AUTHENTICATE to the physical card.
        assert_eq!(
            fixture
                .trace
                .commands
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c[1] == 0x88)
                .count(),
            1
        );
        assert!(fixture.gate.admit(READER).is_ok());
    }
}
