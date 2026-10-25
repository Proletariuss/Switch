//! Carrier-side SMS wire oracle. All application outcomes come from the real
//! daemon consumers; this peer only emits and verifies local SIP/RP packets.
use proletarius_sip::{Message, Request};
use std::path::PathBuf;
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::mpsc,
};

pub(super) const RECIPIENT: &str = "+447700900456";
pub(super) const OUTGOING: &str = "production SMS fixture";
pub(super) const INCOMING_ID: &str = "production-491-incoming";
const SUBMIT_ACK_ID: &str = "production-491-submit-rp-ack";
const DOMAIN: &str = "ims.mnc033.mcc234.3gppnetwork.org";
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Event {
    IncomingAccepted,
    IncomingRpAck,
    Submitted(u8),
    SubmitRpAckAccepted,
}
pub(super) enum Command {
    Duplicate,
}
pub(super) struct Client {
    pub(super) commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<Event>,
}
impl Client {
    pub(super) async fn next(&mut self) -> Event {
        tokio::time::timeout(std::time::Duration::from_secs(5), self.events.recv())
            .await
            .expect("SMS peer event deadline")
            .expect("SMS peer ended")
    }
}
pub(super) struct Server {
    pub(super) commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
    root: PathBuf,
    subscription: String,
    pub(super) target: String,
    pub(super) multi: Option<(crate::multistandby_store::Store, String)>,
    pub(super) recover_go_exit: bool,
    outgoing: usize,
    incoming: usize,
    reports: usize,
    report_cseqs: Vec<String>,
}
pub(super) fn new(root: PathBuf, subscription: String) -> (Server, Client) {
    let (commands, input) = mpsc::channel(8);
    let (events, output) = mpsc::channel(16);
    (
        Server {
            commands: input,
            events,
            root,
            subscription,
            target: format!("sip:smsc@{DOMAIN}"),
            multi: None,
            recover_go_exit: false,
            outgoing: 0,
            incoming: 0,
            reports: 0,
            report_cseqs: Vec::new(),
        },
        Client {
            commands,
            events: output,
        },
    )
}
fn request(call_id: &str, cseq: u32, body: &[u8]) -> Vec<u8> {
    let mut wire = format!("MESSAGE sip:+447700900123@{DOMAIN} SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:6060;branch=z9hG4bK-{call_id}\r\nFrom: <sip:smsc@{DOMAIN}>;tag=carrier\r\nP-Asserted-Identity: <sip:smsc@{DOMAIN}>\r\nTo: <sip:+447700900123@{DOMAIN}>\r\nCall-ID: {call_id}\r\nCSeq: {cseq} MESSAGE\r\nContent-Type: application/vnd.3gpp.sms\r\nContent-Length: {}\r\n\r\n",body.len()).into_bytes();
    wire.extend(body);
    wire
}
fn reply(request: &Request, status: u16) -> Vec<u8> {
    let headers = ["Via", "From", "To", "Call-ID", "CSeq"]
        .iter()
        .map(|name| {
            format!(
                "{name}: {}\r\n",
                request.data.headers.get(name).unwrap().value()
            )
        })
        .collect::<String>();
    format!("SIP/2.0 {status} Fixture\r\n{headers}Content-Length: 0\r\n\r\n").into_bytes()
}
impl Server {
    pub(super) fn delivery(&self) -> Vec<u8> {
        // SMS-DELIVER: sender 888, UCS2 "A", fixed valid timestamp. Repeating
        // this exact RP-DATA must acknowledge again without a second archive row.
        let tpdu = [
            0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
        ];
        let mut rpdu = vec![1, 42, 0, 0, tpdu.len() as u8];
        rpdu.extend(tpdu);
        request(INCOMING_ID, 73, &rpdu)
    }
    pub(super) async fn observe<W: AsyncWrite + Unpin>(&mut self, message: Message, write: &mut W) {
        match message {
            Message::Response(response) => {
                assert_eq!(
                    response.status, 200,
                    "client must acknowledge actual local RP packet"
                );
                match response.data.headers.get("Call-ID").unwrap().value() {
                    INCOMING_ID => {
                        if let Some((store, profile)) = &self.multi {
                            let (messages, _) = store.forwarding_page(0).unwrap();
                            let incoming: Vec<_> = messages
                                .iter()
                                .filter(|m| m.id().starts_with(&format!("nwait:{profile}:")))
                                .collect();
                            assert_eq!(
                                incoming.len(),
                                1,
                                "SIP 200 requires profile-scoped durable commit"
                            );
                            assert_eq!(incoming[0].content(), "A");
                            assert_eq!(incoming[0].device(), "N待");
                        } else {
                            let rows = proletarius_store::ArchiveStore::new(
                                self.root.join("sms-archive.json"),
                            )
                            .load()
                            .unwrap();
                            let incoming: Vec<_> =
                                rows.iter().filter(|v| v["direction"] == "in").collect();
                            assert_eq!(
                                incoming.len(),
                                1,
                                "SIP 200 requires durable exactly-once incoming commit"
                            );
                            assert_eq!(incoming[0]["content"], "A");
                            assert_eq!(incoming[0]["subscription_id"], self.subscription);
                        }
                        self.incoming += 1;
                        assert!(
                            self.incoming <= 2,
                            "unexpected incoming acknowledgement replay"
                        );
                        self.events.send(Event::IncomingAccepted).await.unwrap();
                    }
                    SUBMIT_ACK_ID => {
                        self.events.send(Event::SubmitRpAckAccepted).await.unwrap();
                    }
                    id => panic!("unexpected client response Call-ID {id}"),
                }
            }
            Message::Request(request) => {
                assert_eq!(request.method.as_str(), "MESSAGE");
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
                assert!(
                    request
                        .data
                        .headers
                        .get("From")
                        .unwrap()
                        .value()
                        .contains("+447700900123")
                );

                match request.data.body[0] {
                    0 => {
                        assert_eq!(request.uri.to_string(), self.target);
                        self.outgoing += 1;
                        assert_eq!(self.outgoing, 1, "durable send replayed RP-DATA");
                        let reference = request.data.body[1];
                        let mut expected_tpdu =
                            proletarius_sms::submit::prepare(RECIPIENT, OUTGOING, reference)
                                .unwrap()
                                .remove(0);
                        // The session compiler binds TP-MR to this RP reference.
                        expected_tpdu[1] = reference;
                        assert_eq!(
                            request.data.body,
                            proletarius_sms::build_mobile_originated_data(
                                reference,
                                "+447802002606",
                                &expected_tpdu
                            )
                            .unwrap()
                        );
                        if self.recover_go_exit {
                            // Run the production Go-exit ledger action after the actual
                            // native RP-DATA, before any carrier acceptance is emitted.
                            let ledger = crate::device_send_store::Store::open(&self.root).unwrap();
                            let query =
                                serde_json::json!({"client_request_id":"production-491-outgoing"});
                            let before = ledger.apply("get", query.clone()).unwrap();
                            assert_eq!(before["state"], "submitting");
                            assert!(
                                before["binding"]
                                    .as_str()
                                    .unwrap()
                                    .starts_with("rust_reader_ims:")
                            );
                            ledger.recover_go_owned().unwrap();
                            assert_eq!(ledger.apply("get", query).unwrap()["state"], "submitting");
                        }
                        let mut wire = reply(&request, 202);
                        // A genuine network RP-ACK accompanies final SIP acceptance.
                        wire.extend(super::sms_peer::request(SUBMIT_ACK_ID, 81, &[3, reference]));
                        write.write_all(&wire).await.unwrap();
                        write.flush().await.unwrap();
                        self.events.send(Event::Submitted(reference)).await.unwrap();
                    }
                    2 => {
                        assert_eq!(request.uri.to_string(), format!("sip:smsc@{DOMAIN}"));
                        assert_eq!(request.data.body, proletarius_sms::build_ack(42));
                        assert_eq!(
                            request.data.headers.get("In-Reply-To").unwrap().value(),
                            INCOMING_ID
                        );
                        let cseq = request.data.headers.get("CSeq").unwrap().value().to_owned();
                        assert!(
                            !self.report_cseqs.contains(&cseq),
                            "duplicate RP-DATA reused a report transaction"
                        );
                        self.report_cseqs.push(cseq);
                        self.reports += 1;
                        assert!(self.reports <= 2, "incoming RP-ACK replayed unexpectedly");
                        write.write_all(&reply(&request, 202)).await.unwrap();
                        write.flush().await.unwrap();
                        self.events.send(Event::IncomingRpAck).await.unwrap();
                    }
                    kind => panic!("unexpected client RP message type {kind}"),
                }
            }
        }
    }
}
