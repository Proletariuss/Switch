//! Multi-generation carrier oracle: only real SIP input/output changes daemon state.
use super::sms_peer::{OUTGOING, RECIPIENT};
use proletarius_sip::{Message, Request};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::mpsc,
};
const DOMAIN: &str = "ims.mnc033.mcc234.3gppnetwork.org";
const DELIVERY: &str = "production-492-renewal-delivery";
const SUBMIT_ACK: &str = "production-492-recovered-ack";
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Event {
    Registered(usize),
    RegisterRejected(usize),
    Refreshed(usize),
    SignalingClosed(usize),
    IncomingAccepted,
    ReportHeld,
    ReportReleased,
    Submitted(u8),
    SubmitRpAckAccepted,
    RecoverySubmitted(usize, u8),
    RecoveryRpAck(usize),
    RecoveryBarrier(usize),
}
pub(super) enum Command {
    DeliverReport,
    ReleaseReport,
    CloseSignaling,
    RecoveryBarrier,
}
struct Shared {
    commands: BTreeMap<usize, mpsc::Sender<Command>>,
    root: PathBuf,
    subscription: String,
    lifecycle: bool,
    recovery: Option<Arc<super::s08_recovery::WireFacts>>,
}
#[derive(Clone)]
pub(super) struct Server {
    shared: Arc<Mutex<Shared>>,
    events: mpsc::UnboundedSender<Event>,
}
pub(super) struct Client {
    shared: Arc<Mutex<Shared>>,
    events: mpsc::UnboundedReceiver<Event>,
}
pub(super) fn new(root: PathBuf, subscription: String) -> (Server, Client) {
    let shared = Arc::new(Mutex::new(Shared {
        commands: BTreeMap::new(),
        root,
        subscription,
        lifecycle: false,
        recovery: None,
    }));
    let (events, output) = mpsc::unbounded_channel();
    (
        Server {
            shared: shared.clone(),
            events,
        },
        Client {
            shared,
            events: output,
        },
    )
}
pub(super) fn new_recovery_sms(
    root: PathBuf,
    subscription: String,
    facts: Arc<super::s08_recovery::WireFacts>,
) -> (Server, Client) {
    let (server, client) = new(root, subscription);
    server.shared.lock().unwrap().recovery = Some(facts);
    (server, client)
}
pub(super) fn new_lifecycle(root: PathBuf) -> (Server, Client) {
    let (server, client) = new(root, String::new());
    server.shared.lock().unwrap().lifecycle = true;
    (server, client)
}
impl Client {
    pub(super) async fn next(&mut self) -> Event {
        tokio::time::timeout(std::time::Duration::from_secs(12), self.events.recv())
            .await
            .expect("renewal carrier event deadline")
            .expect("renewal carrier ended")
    }
    pub(super) async fn command(&self, generation: usize, command: Command) {
        let sender = self.shared.lock().unwrap().commands[&generation].clone();
        sender.send(command).await.unwrap();
    }
}
pub(super) struct Association {
    pub(super) generation: usize,
    pub(super) expiry: u32,
    pub(super) commands: mpsc::Receiver<Command>,
    pub(super) events: mpsc::UnboundedSender<Event>,
    root: PathBuf,
    subscription: String,
    held: Option<Vec<u8>>,
    delivered: bool,
    submitted: bool,
    lifecycle: bool,
    recovery: Option<Arc<super::s08_recovery::WireFacts>>,
}
impl Server {
    pub(super) fn association(&self) -> Association {
        let mut shared = self.shared.lock().unwrap();
        let generation = shared.commands.len() + 1;
        assert!(generation <= 3, "unexpected fourth production generation");
        let (commands, input) = mpsc::channel(8);
        shared.commands.insert(generation, commands);
        Association {
            generation,
            expiry: if shared.recovery.is_some() {
                3600
            } else if shared.lifecycle {
                3
            } else if generation == 1 {
                90
            } else {
                3600
            },
            lifecycle: shared.lifecycle,
            recovery: shared.recovery.clone(),
            commands: input,
            events: self.events.clone(),
            root: shared.root.clone(),
            subscription: shared.subscription.clone(),
            held: None,
            delivered: false,
            submitted: false,
        }
    }
}
fn request(id: &str, body: &[u8]) -> Vec<u8> {
    let mut wire = format!("MESSAGE sip:+447700900123@{DOMAIN} SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:6060;branch=z9hG4bK-{id}\r\nFrom: <sip:smsc@{DOMAIN}>;tag=carrier\r\nP-Asserted-Identity: <sip:smsc@{DOMAIN}>\r\nTo: <sip:+447700900123@{DOMAIN}>\r\nCall-ID: {id}\r\nCSeq: 91 MESSAGE\r\nContent-Type: application/vnd.3gpp.sms\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    wire.extend(body);
    wire
}
fn reply(request: &Request) -> Vec<u8> {
    response(request, 202)
}
fn response(request: &Request, status: u16) -> Vec<u8> {
    let headers = ["Via", "From", "To", "Call-ID", "CSeq"]
        .iter()
        .map(|name| {
            format!(
                "{name}: {}\r\n",
                request.data.headers.get(name).unwrap().value()
            )
        })
        .collect::<String>();
    format!("SIP/2.0 {status} Carrier\r\n{headers}Content-Length: 0\r\n\r\n").into_bytes()
}
impl Association {
    pub(super) fn reject_register(&self) -> bool {
        self.recovery
            .as_ref()
            .is_some_and(|facts| facts.reject_register(self.generation))
    }
    pub(super) async fn command<W: AsyncWrite + Unpin>(
        &mut self,
        command: Command,
        write: &mut W,
    ) -> bool {
        match command {
            Command::RecoveryBarrier => {
                assert!(self.recovery.is_some());
                let id = format!("s08-barrier-{}", self.generation);
                let wire = format!(
                    "OPTIONS sip:+447700900123@{DOMAIN} SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:6060;branch=z9hG4bK-{id}\r\nFrom: <sip:smsc@{DOMAIN}>;tag=carrier\r\nTo: <sip:+447700900123@{DOMAIN}>\r\nCall-ID: {id}\r\nCSeq: 99 OPTIONS\r\nContent-Length: 0\r\n\r\n"
                );
                write.write_all(wire.as_bytes()).await.unwrap();
                write.flush().await.unwrap();
            }
            Command::CloseSignaling => {
                assert!((1..=3).contains(&self.generation));
                return true;
            }
            Command::DeliverReport => {
                assert_eq!(self.generation, 1);
                assert!(!self.delivered);
                self.delivered = true;
                let tpdu = [
                    0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x42,
                ];
                let mut body = vec![1, 43, 0, 0, tpdu.len() as u8];
                body.extend(tpdu);
                write.write_all(&request(DELIVERY, &body)).await.unwrap();
            }
            Command::ReleaseReport => {
                assert_eq!(self.generation, 1);
                write
                    .write_all(
                        &self
                            .held
                            .take()
                            .expect("actual RP-ACK must arrive before release"),
                    )
                    .await
                    .unwrap();
                self.events.send(Event::ReportReleased).unwrap();
            }
        }
        write.flush().await.unwrap();
        false
    }
    pub(super) async fn observe<W: AsyncWrite + Unpin>(&mut self, message: Message, write: &mut W) {
        if let Some(facts) = self.recovery.as_ref() {
            facts
                .observe(self.generation, message, write, &self.events)
                .await;
            return;
        }
        match message {
            Message::Response(response) => {
                assert_eq!(response.status, 200);
                match response.data.headers.get("Call-ID").unwrap().value() {
                    DELIVERY => {
                        let rows = proletarius_store::ArchiveStore::new(
                            self.root.join("sms-archive.json"),
                        )
                        .load()
                        .unwrap();
                        let incoming: Vec<_> =
                            rows.iter().filter(|v| v["direction"] == "in").collect();
                        assert_eq!(incoming.len(), 1);
                        assert_eq!(incoming[0]["content"], "B");
                        assert_eq!(incoming[0]["subscription_id"], self.subscription);
                        self.events.send(Event::IncomingAccepted).unwrap();
                    }
                    SUBMIT_ACK => {
                        self.events.send(Event::SubmitRpAckAccepted).unwrap();
                    }
                    id => panic!("unexpected SIP response {id}"),
                }
            }
            Message::Request(r) => {
                if r.method.as_str() == "REGISTER" {
                    if !self.lifecycle {
                        assert_eq!(self.generation, 1);
                    }
                    assert!(
                        self.lifecycle || self.held.is_some(),
                        "RP report must remain in flight across real refresh"
                    );
                    assert!(r.data.headers.get("Security-Verify").is_some());
                    write
                        .write_all(&super::ims_peer::response(
                            &r,
                            200,
                            if self.lifecycle {
                                "Expires: 3\r\n"
                            } else {
                                "Expires: 3600\r\n"
                            },
                            "",
                        ))
                        .await
                        .unwrap();
                    write.flush().await.unwrap();
                    self.events.send(Event::Refreshed(self.generation)).unwrap();
                    return;
                }
                if r.method.as_str() == "OPTIONS" {
                    // The real production heartbeat remains active while the
                    // registration timer waits and the report is draining.
                    write.write_all(&response(&r, 200)).await.unwrap();
                    write.flush().await.unwrap();
                    return;
                }
                assert_eq!(r.method.as_str(), "MESSAGE");
                assert!(
                    r.data
                        .headers
                        .get("Security-Verify")
                        .unwrap()
                        .value()
                        .contains("aes-cbc")
                );
                assert!(
                    r.data
                        .headers
                        .get("Route")
                        .unwrap()
                        .value()
                        .contains("10.20.30.9:6060")
                );
                assert_eq!(r.uri.to_string(), format!("sip:smsc@{DOMAIN}"));
                match r.data.body[0] {
                    2 => {
                        assert_eq!(self.generation, 1);
                        assert!(self.held.is_none());
                        assert_eq!(r.data.body, proletarius_sms::build_ack(43));
                        assert_eq!(r.data.headers.get("In-Reply-To").unwrap().value(), DELIVERY);
                        self.held = Some(reply(&r));
                        self.events.send(Event::ReportHeld).unwrap();
                    }
                    0 => {
                        assert_eq!(self.generation, 3);
                        assert!(!self.submitted);
                        self.submitted = true;
                        let reference = r.data.body[1];
                        let mut tpdu =
                            proletarius_sms::submit::prepare(RECIPIENT, OUTGOING, reference)
                                .unwrap()
                                .remove(0);
                        tpdu[1] = reference;
                        assert_eq!(
                            r.data.body,
                            proletarius_sms::build_mobile_originated_data(
                                reference,
                                "+447802002606",
                                &tpdu
                            )
                            .unwrap()
                        );
                        let mut wire = reply(&r);
                        wire.extend(request(SUBMIT_ACK, &[3, reference]));
                        write.write_all(&wire).await.unwrap();
                        write.flush().await.unwrap();
                        self.events.send(Event::Submitted(reference)).unwrap();
                    }
                    kind => panic!("unexpected RP message {kind}"),
                }
            }
        }
    }
}
