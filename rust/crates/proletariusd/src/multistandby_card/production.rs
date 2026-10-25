//! Only the card replies are simulated. HTTP, service receipts, Gate business
//! authority, physical owner, ES10c, metadata decoding and bound AKA are real.
use super::*;
use crate::reader_pcsc::{Failure as PcscFailure, Physical};
use crate::reader_reservation::{
    NativeAuthority, NativeReaderFacts, NativeTransitionKind, SelectionSnapshot,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use proletarius_sim::ApduTransport;
use std::{sync::atomic::AtomicUsize, time::Duration};
const READER: &str = "Workflow Reader";
const ICCID: &str = "8901234567890123456";
const IMSI: &str = "234330123456789";
const EID: &str = "89049032000000000000000000000001";

#[derive(Clone)]
struct SyntheticProfile {
    iccid: &'static str,
    imsi: &'static str,
    smsc: &'static str,
    ike_res: [u8; 8],
    ike_ck: [u8; 16],
    ike_ik: [u8; 16],
    ims_res: [u8; 8],
    ims_ck: [u8; 16],
    ims_ik: [u8; 16],
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthTrace {
    pub(crate) iccid: String,
    pub(crate) imsi: String,
    pub(crate) application: String,
    pub(crate) rand: [u8; 16],
    pub(crate) autn: [u8; 16],
    pub(crate) completed: bool,
}
struct TargetedAuth {
    iccid: String,
    application: String,
    rand: [u8; 16],
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
/// Only the actual matching AUTH APDU consumes this one-shot barrier.
/// Dropping its controller releases the physical fixture on assertion unwind.
pub(crate) struct TargetedAuthHold {
    entered: Option<tokio::sync::oneshot::Receiver<()>>,
    release: Option<std::sync::mpsc::Sender<()>>,
}
impl TargetedAuthHold {
    pub(crate) async fn entered(&mut self) {
        self.entered
            .take()
            .expect("one AUTH barrier waiter")
            .await
            .expect("targeted AUTH did not enter");
    }
    pub(crate) fn release(mut self) {
        self.release
            .take()
            .expect("one AUTH release")
            .send(())
            .expect("targeted AUTH owner lost");
    }
}
impl Drop for TargetedAuthHold {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}
const FIXTURE_TRACE_LIMIT: usize = 256;

#[derive(Default)]
struct Trace {
    synthetic_imsi: Option<String>,
    profiles: Option<[SyntheticProfile; 2]>,
    auth_trace: Mutex<Vec<AuthTrace>>,
    selected_profiles: Mutex<Vec<String>>,
    targeted_auth: Mutex<Option<TargetedAuth>>,
    commands: Mutex<Vec<Vec<u8>>>,
    enabled: AtomicBool,
    opened: AtomicUsize,
    closed: AtomicUsize,
    held_close: Mutex<Option<(std::sync::mpsc::Receiver<()>, Arc<AtomicBool>)>>,
    fail_close: AtomicBool,
    fail_next_apdu: AtomicBool,
    auth: AtomicUsize,
    enables: AtomicUsize,
    multi: AtomicBool,
    transactions: AtomicUsize,
    held: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    held_next: Mutex<Option<(std::sync::mpsc::Receiver<()>, Arc<AtomicBool>)>>,
}
fn native_text(file: [u8; 2], imsi: &str) -> Option<Vec<u8>> {
    let domain = format!("ims.mnc0{}.mcc{}.3gppnetwork.org", &imsi[3..5], &imsi[..3]);
    if file == [0x6f, 0x42] {
        let mut smsp = vec![0xff; 28];
        smsp[13..25].copy_from_slice(&[7, 0x91, 0x44, 0x87, 0x20, 0x00, 0x62, 0x60, 0, 0, 0, 0]);
        return Some(smsp);
    }
    let value = match file {
        [0x6f, 0x02] => format!("{imsi}@{domain}"),
        [0x6f, 0x03] => domain.clone(),
        [0x6f, 0x04] => format!("sip:{imsi}@{domain}"),
        [0x6f, 0xe5] => format!("sip:smsc@{domain}"),
        _ => return None,
    };
    Some(tlv(&[0x80], value.as_bytes()))
}
struct Sim {
    trace: Arc<Trace>,
    selected: [u8; 2],
    application: &'static str,
}
impl Sim {
    fn profile(&self) -> Option<&SyntheticProfile> {
        self.trace.profiles.as_ref().map(|profiles| {
            &profiles[if self.trace.enabled.load(Ordering::SeqCst) {
                0
            } else {
                1
            }]
        })
    }
    fn imsi(&self) -> &str {
        self.profile()
            .map(|p| p.imsi)
            .unwrap_or_else(|| self.trace.synthetic_imsi.as_deref().unwrap_or(IMSI))
    }
    fn text(&self, file: [u8; 2]) -> Option<Vec<u8>> {
        let Some(profile) = self.profile() else {
            return native_text(file, self.imsi());
        };
        if file == [0x6f, 0x42] {
            let digits = profile.smsc.trim_start_matches('+').as_bytes();
            let mut record = vec![0xff; 28];
            record[13] = (1 + digits.len().div_ceil(2)) as u8;
            record[14] = 0x91;
            for (index, pair) in digits.chunks(2).enumerate() {
                record[15 + index] =
                    (pair[0] - b'0') | (pair.get(1).map_or(0x0f, |v| v - b'0') << 4);
            }
            return Some(record);
        }
        if file == [0x6f, 0xe5] {
            return Some(tlv(
                &[0x80],
                format!("sip:{}@ims.mnc033.mcc234.3gppnetwork.org", profile.smsc).as_bytes(),
            ));
        }
        native_text(file, profile.imsi)
    }
}
fn tlv(tag: &[u8], value: &[u8]) -> Vec<u8> {
    let mut v = tag.to_vec();
    v.push(value.len() as u8);
    v.extend(value);
    v
}
impl ApduTransport for Sim {
    type Error = PcscFailure;
    fn transmit(&mut self, c: &[u8]) -> Result<Vec<u8>, PcscFailure> {
        self.trace.commands.lock().unwrap().push(c.to_vec());
        if self.trace.fail_next_apdu.swap(false, Ordering::SeqCst) {
            return Err(PcscFailure {
                status: hyper::StatusCode::CONFLICT,
                code: "fixture_apdu_failed",
                message: "fixture APDU failure".into(),
                submitted: true,
                uncertain: false,
            });
        }
        if let Some((wait, entered)) = self.trace.held_next.lock().unwrap().take() {
            entered.store(true, Ordering::SeqCst);
            wait.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        let mut reply = match c[1] {
            0x70 => {
                if c[2] == 0 {
                    vec![1]
                } else {
                    vec![]
                }
            }
            0xa4 => {
                if c[2] == 4 {
                    let aid = &c[5..5 + c[4] as usize];
                    self.application = if aid.starts_with(proletarius_sim::ISIM_AID_PREFIX) {
                        "isim"
                    } else if aid.starts_with(proletarius_sim::USIM_AID_PREFIX) {
                        "usim"
                    } else {
                        ""
                    };
                    if aid != proletarius_euicc::GSMA_ISDR_APPLICATION_AID
                        && !aid.starts_with(proletarius_sim::USIM_AID_PREFIX)
                        && !aid.starts_with(proletarius_sim::ISIM_AID_PREFIX)
                    {
                        return Ok(vec![0x6a, 0x82]);
                    }
                }
                if c.len() >= 7
                    && (c[4] == 2
                        || (c[2] == 8 && c[4] == 4 && self.trace.multi.load(Ordering::SeqCst)))
                {
                    let end = 5 + c[4] as usize;
                    self.selected = c[end - 2..end].try_into().unwrap();
                    if self.trace.multi.load(Ordering::SeqCst) {
                        if let Some(data) = self.text(self.selected) {
                            let record =
                                matches!(self.selected, [0x6f, 0x04] | [0x6f, 0xe5] | [0x6f, 0x42]);
                            let desc = if record {
                                vec![0x42, 0x21, 0, data.len() as u8, 1]
                            } else {
                                vec![0x41, 0x21]
                            };
                            let mut fields = tlv(&[0x82], &desc);
                            // The real metadata decoder requires the two-byte
                            // TS 102 221 file-size value for transparent ISIM EF.
                            // Keep the old fixture responses unchanged unless the
                            // new distinct-profile scenario was explicitly chosen.
                            let size = (data.len() as u16).to_be_bytes();
                            fields.extend(tlv(
                                &[0x80],
                                if self.trace.profiles.is_some() {
                                    &size
                                } else {
                                    &size[1..]
                                },
                            ));
                            let mut fcp = tlv(&[0x62], &fields);
                            fcp.extend([0x90, 0]);
                            return Ok(fcp);
                        }
                    }
                }
                vec![]
            }
            0xe2 => {
                let body = &c[5..];
                if body.starts_with(&[0xbf, 0x3e]) {
                    tlv(
                        &[0xbf, 0x3e],
                        &tlv(
                            &[0x5a],
                            &[0x89, 0x04, 0x90, 0x32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
                        ),
                    )
                } else if body.starts_with(&[0xbf, 0x2d]) {
                    let mut fields = tlv(
                        &[0x5a],
                        &[0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6],
                    );
                    fields.extend(tlv(
                        &[0x9f, 0x70],
                        &[if self.trace.enabled.load(Ordering::SeqCst) {
                            1
                        } else {
                            0
                        }],
                    ));
                    fields.extend(tlv(&[0x91], b"Test Operator"));
                    let mut profiles = tlv(&[0xe3], &fields);
                    if self.trace.multi.load(Ordering::SeqCst) {
                        let mut other = tlv(
                            &[0x5a],
                            &[0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf7],
                        );
                        other.extend(tlv(
                            &[0x9f, 0x70],
                            &[if self.trace.enabled.load(Ordering::SeqCst) {
                                0
                            } else {
                                1
                            }],
                        ));
                        profiles.extend(tlv(&[0xe3], &other));
                    }
                    tlv(&[0xbf, 0x2d], &tlv(&[0xa0], &profiles))
                } else if body.starts_with(&[0xbf, 0x31]) {
                    self.trace.enables.fetch_add(1, Ordering::SeqCst);
                    let target = !self.trace.multi.load(Ordering::SeqCst)
                        || body.windows(10).any(|v| {
                            v == [0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6]
                        });
                    self.trace.enabled.store(target, Ordering::SeqCst);
                    if let Some(profile) = self.profile() {
                        let mut trace = self.trace.selected_profiles.lock().unwrap();
                        assert!(
                            trace.len() < FIXTURE_TRACE_LIMIT,
                            "selected profile trace capacity exceeded"
                        );
                        trace.push(profile.iccid.to_owned());
                    }
                    tlv(&[0xbf, 0x31], &tlv(&[0x80], &[0]))
                } else if body.starts_with(&[0xbf, 0x28]) {
                    tlv(&[0xbf, 0x28], &tlv(&[0xa0], &[]))
                } else {
                    return Ok(vec![0x6a, 0x81]);
                }
            }
            0xb2 => {
                if self.trace.multi.load(Ordering::SeqCst) && c[2] == 1 {
                    if let Some(mut data) = self.text(self.selected) {
                        data.extend([0x90, 0]);
                        return Ok(data);
                    }
                }
                if self.selected == [0x2f, 0x00] {
                    vec![
                        0x61, 11, 0x4f, 9, 0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff, 1, 0x61, 11, 0x4f,
                        9, 0xa0, 0, 0, 0, 0x87, 0x10, 4, 0xff, 1,
                    ]
                } else {
                    return Ok(vec![0x6a, 0x83]);
                }
            }
            0xb0 if self.trace.multi.load(Ordering::SeqCst)
                && self.text(self.selected).is_some() =>
            {
                self.text(self.selected).unwrap()
            }
            0xb0 => match self.selected {
                [0x6f, 0xad] => vec![0, 0, 0, 2],
                [0x6f, 0x07] if c[4] == 1 => vec![8],
                [0x6f, 0x07] => {
                    let digits = self.imsi().as_bytes();
                    let mut encoded = vec![8, 9 | ((digits[0] - b'0') << 4)];
                    encoded.extend(
                        digits[1..]
                            .chunks_exact(2)
                            .map(|pair| (pair[0] - b'0') | ((pair[1] - b'0') << 4)),
                    );
                    encoded
                }
                [0x2f, 0xe2] => vec![
                    0x98,
                    0x10,
                    0x32,
                    0x54,
                    0x76,
                    0x98,
                    0x10,
                    0x32,
                    0x54,
                    if self.trace.enabled.load(Ordering::SeqCst) {
                        0xf6
                    } else {
                        0xf7
                    },
                ],
                _ => return Ok(vec![0x6a, 0x82]),
            },
            0x88 => {
                self.trace.auth.fetch_add(1, Ordering::SeqCst);
                if let Some(profile) = self.profile().cloned() {
                    assert!(
                        c.len() >= 39 && c[4] == 34 && c[5] == 16 && c[22] == 16,
                        "invalid AUTH fixture request"
                    );
                    let rand: [u8; 16] = c[6..22].try_into().unwrap();
                    let autn: [u8; 16] = c[23..39].try_into().unwrap();
                    let mut event = AuthTrace {
                        iccid: profile.iccid.into(),
                        imsi: profile.imsi.into(),
                        application: self.application.into(),
                        rand,
                        autn,
                        completed: false,
                    };
                    {
                        let mut trace = self.trace.auth_trace.lock().unwrap();
                        assert!(
                            trace.len() < FIXTURE_TRACE_LIMIT,
                            "AUTH trace capacity exceeded"
                        );
                        trace.push(event.clone());
                    }
                    let targeted = {
                        let mut barrier = self.trace.targeted_auth.lock().unwrap();
                        if barrier.as_ref().is_some_and(|b| {
                            b.iccid == event.iccid
                                && b.application == event.application
                                && b.rand == rand
                        }) {
                            barrier.take()
                        } else {
                            None
                        }
                    };
                    if let Some(barrier) = targeted {
                        let _ = barrier.entered.send(());
                        barrier
                            .release
                            .recv_timeout(Duration::from_secs(15))
                            .expect("targeted AUTH release deadline");
                    }
                    let (res, ck, ik) = if rand == [0x11; 16] && autn == [0x22; 16] {
                        assert_eq!(
                            self.application, "usim",
                            "IKE must use actual USIM selection"
                        );
                        (profile.ike_res, profile.ike_ck, profile.ike_ik)
                    } else {
                        assert!(
                            (rand == [1; 16] || rand == [2; 16]) && autn == rand,
                            "unexpected IMS fixture challenge"
                        );
                        assert_eq!(
                            self.application, "usim",
                            "234/33 derived IMS identity must use actual USIM selection"
                        );
                        (profile.ims_res, profile.ims_ck, profile.ims_ik)
                    };
                    let mut result = vec![0xdb, 8];
                    result.extend(res);
                    result.push(16);
                    result.extend(ck);
                    result.push(16);
                    result.extend(ik);
                    event.completed = true;
                    let mut trace = self.trace.auth_trace.lock().unwrap();
                    assert!(
                        trace.len() < FIXTURE_TRACE_LIMIT,
                        "AUTH trace capacity exceeded"
                    );
                    trace.push(event);
                    result.extend([0x90, 0]);
                    return Ok(result);
                }
                if let Some(wait) = self.trace.held.lock().unwrap().take() {
                    wait.recv_timeout(Duration::from_secs(10)).unwrap();
                }
                let ike = self.trace.multi.load(Ordering::SeqCst)
                    && c.windows(16).any(|v| v == [0x11; 16]);
                let mut result = vec![0xdb, 8];
                result.extend(if ike {
                    vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
                } else {
                    vec![1; 8]
                });
                result.push(16);
                result.extend([if ike { 0x31 } else { 2 }; 16]);
                result.push(16);
                result.extend([if ike { 0x42 } else { 3 }; 16]);
                result
            }
            _ => panic!("unexpected APDU {c:x?}"),
        };
        reply.extend([0x90, 0]);
        Ok(reply)
    }
}
impl Physical for Sim {
    fn transaction(
        &mut self,
        body: &mut dyn FnMut(&mut dyn ApduTransport<Error = PcscFailure>),
    ) -> Result<(), PcscFailure> {
        self.trace.transactions.fetch_add(1, Ordering::SeqCst);
        body(self);
        self.trace.transactions.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }
    fn close(&mut self) -> Result<(), PcscFailure> {
        if let Some((wait, entered)) = self.trace.held_close.lock().unwrap().take() {
            entered.store(true, Ordering::SeqCst);
            wait.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        if self.trace.fail_close.load(Ordering::SeqCst) {
            return Err(PcscFailure {
                status: hyper::StatusCode::CONFLICT,
                code: "fixture_close_failed",
                message: "fixture close failure".into(),
                submitted: true,
                uncertain: false,
            });
        }
        self.trace.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Fixture {
    service: Service,
    gate: Arc<Gate>,
    owner: Owner,
    tx: CardTransaction,
    pcsc: Binding,
    session: crate::reader_pcsc::LifecycleBinding,
    trace: Arc<Trace>,
}
impl Fixture {
    fn new() -> Self {
        let gate = Arc::new(Gate::default());
        gate.initialize_native_reader_producer("native-card-workflow");
        gate.initialize_business_producer("go-card-workflow");
        gate.initialize_native_lifecycle("native-card-workflow")
            .unwrap();
        let trace = Arc::new(Trace::default());
        let factory = trace.clone();
        let owner = Owner::with_factory(
            "native-card-workflow".into(),
            Arc::new(move |_| {
                factory.opened.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Sim {
                    trace: factory.clone(),
                    selected: [0, 0],
                    application: "",
                }))
            }),
            Arc::new(|| Ok(vec![READER.into()])),
            Arc::new(|_, _| panic!("ordinary reader must not power reset")),
        );
        let session = owner
            .native_open(READER)
            .unwrap_or_else(|e| panic!("{}", e.message));
        let transition = gate
            .prepare_native_transition(
                "native-card-workflow",
                &SelectionSnapshot::default(),
                NativeTransitionKind::Connect,
                READER,
                NativeAuthority::Idle,
            )
            .unwrap();
        gate.commit_native_transition(&transition, Some(&session), NativeReaderFacts::default())
            .unwrap();
        gate.test_business_command(json!({"action":"acquire","producer_id":"go-card-workflow","token":"business-token-0000000000000000000000000000","sequence":1,"role":"multistandby","reader":READER,"reader_generation":1}),false).unwrap();
        let binding = json!({"producer_id":"go-card-workflow","operation_token":"operation-token-000000000000000000000000000","operation_sequence":1,"reader":READER,"reader_generation":1,"purpose":"card","authority":{"kind":"business","token":"business-token-0000000000000000000000000000","sequence":1,"role":"multistandby"}});
        gate.test_operation_command("acquire", binding.clone())
            .unwrap();
        let tx = serde_json::from_value(json!({"kind":"operation","binding":binding})).unwrap();
        let pcsc = Binding {
            producer_id: session.producer.clone(),
            reader: READER.into(),
            session_id: session.session_id.clone(),
            lease_id: "physical-workflow-000000000000000000000001".into(),
            lease_seq: 1,
        };
        owner
            .native_acquire_binding(&pcsc, Duration::from_secs(120), Arc::new(|| true))
            .unwrap_or_else(|e| panic!("{}", e.message));
        let service = Service::new(owner.clone(), gate.clone(), None, None);
        Self {
            service,
            gate,
            owner,
            tx,
            pcsc,
            session,
            trace,
        }
    }
    fn submit(&self, id: &str, imsi: &str) -> Value {
        json!({"operation_id":id,"action":"authenticate","reader":READER,"reader_generation":1,"transaction":self.tx,"pcsc":self.pcsc,"eid":EID,"aid":crate::reader_esim_download::encode_hex_lower(proletarius_euicc::GSMA_ISDR_APPLICATION_AID),"iccid":ICCID,"expected_imsi":imsi,"application":"isim","rand":vec![0x11;16],"autn":vec![0x22;16]})
    }
    async fn finish(self) {
        self.service.shutdown().await;
        self.owner
            .release(&self.pcsc)
            .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(
            self.owner
                .status(&self.pcsc)
                .unwrap_or_else(|e| panic!("{}", e.message))["terminal"],
            true
        );
        self.owner
            .native_close(&self.session)
            .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(
            self.trace.closed.load(Ordering::SeqCst),
            self.trace.opened.load(Ordering::SeqCst)
        );
    }
}
async fn rpc(service: Service, action: &str, value: Value) -> Value {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                hyper_util::rt::TokioIo::new(stream),
                hyper::service::service_fn(move |req| {
                    let service = service.clone();
                    async move {
                        Ok::<_, std::convert::Infallible>(handle_http(req, service, false).await)
                    }
                }),
            )
            .await
            .unwrap();
    });
    let stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .unwrap();
    let connection = tokio::spawn(connection);
    let request = hyper::Request::builder()
        .method("POST")
        .uri(format!("/api/internal/multistandby/card/{action}"))
        .header("host", "localhost")
        .body(Full::new(Bytes::from(serde_json::to_vec(&value).unwrap())))
        .unwrap();
    let reply = sender.send_request(request).await.unwrap();
    let status = reply.status();
    let body = reply.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let result = serde_json::from_slice(&body).unwrap();
    drop(sender);
    connection.await.unwrap().unwrap();
    task.await.unwrap();
    result
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multistandby_real_http_service_retains_pcsc_switches_and_binds_isim_aka() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let fixture = Fixture::new();
        let submit = fixture.submit("real-physical-workflow", IMSI);
        let query = json!({"operation_id":submit["operation_id"],"transaction":fixture.tx});
        assert_eq!(
            rpc(fixture.service.clone(), "submit", submit.clone()).await["state"],
            "running"
        );
        let receipt = loop {
            let r = rpc(fixture.service.clone(), "status", query.clone()).await;
            if r["state"] == "completed" {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(receipt["worker_finished"], true);
        assert!(receipt.get("error").is_none(), "{receipt}");
        assert_eq!(receipt["result"]["identity"]["iccid"], ICCID);
        assert_eq!(receipt["result"]["identity"]["imsi"], IMSI);
        assert!(receipt["result"]["identity"]["application_aid"].is_string());
        assert_eq!(receipt["result"]["aka"]["res"], json!(vec![1; 8]));
        assert_eq!(receipt["result"]["aka"]["ck"], json!(vec![2; 16]));
        assert_eq!(receipt["result"]["aka"]["ik"], json!(vec![3; 16]));
        assert_eq!(fixture.trace.enables.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.trace.auth.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.trace.opened.load(Ordering::SeqCst), 2);
        assert_eq!(
            rpc(fixture.service.clone(), "submit", submit).await,
            receipt
        );
        assert_eq!(fixture.trace.auth.load(Ordering::SeqCst), 1);
        let commands = fixture.trace.commands.lock().unwrap();
        let auth = commands.iter().position(|c| c[1] == 0x88).unwrap();
        assert!(
            commands[..auth]
                .iter()
                .any(|c| c.windows(2).any(|v| v == [0xbf, 0x28]))
        );
        assert!(commands[..auth].iter().any(|c| {
            c[1] == 0xa4
                && c.windows(proletarius_sim::ISIM_AID_PREFIX.len())
                    .any(|v| v == proletarius_sim::ISIM_AID_PREFIX)
        }));
        drop(commands);
        assert_eq!(
            fixture
                .gate
                .native_reader_snapshot()
                .1
                .unwrap()
                .metadata
                .unwrap()["iccid"],
            ICCID
        );
        // A distinct request with a changed frozen IMSI must never issue AUTH.
        let second = fixture.submit("identity-changed", "234330123456780");
        let query = json!({"operation_id":second["operation_id"],"transaction":fixture.tx});
        rpc(fixture.service.clone(), "submit", second).await;
        let denied = loop {
            let r = rpc(fixture.service.clone(), "status", query.clone()).await;
            if r["state"] == "completed" {
                break r;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(denied["error"]["code"], "multistandby_identity_changed");
        assert_eq!(fixture.trace.auth.load(Ordering::SeqCst), 1);
        fixture.finish().await;
    })
    .await
    .unwrap();
}

/// Reuses real ES10c/metadata/AKA decoding with a two-profile simulated card.
/// No Go principal or socket participates in this native provider fixture.
pub(crate) struct NativeFixture {
    pub(crate) pool: Arc<crate::multistandby_native_card::Pool>,
    pub(crate) gate: Arc<Gate>,
    pub(crate) owner: Owner,
    session: crate::reader_pcsc::LifecycleBinding,
    trace: Arc<Trace>,
}
impl NativeFixture {
    pub(crate) fn new() -> Self {
        Self::with_imsi(IMSI)
    }
    /// Synthetic APDU/ISIM identity only; carrier resolution remains production.
    pub(crate) fn with_imsi(imsi: &str) -> Self {
        assert!(imsi.len() == 15 && imsi.bytes().all(|v| v.is_ascii_digit()));
        Self::with_profile_data(imsi, None)
    }
    pub(crate) fn with_distinct_profiles() -> Self {
        Self::with_profile_data(
            IMSI,
            Some([
                SyntheticProfile {
                    iccid: ICCID,
                    imsi: IMSI,
                    smsc: "+4478720006260",
                    ike_res: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
                    ike_ck: [0x31; 16],
                    ike_ik: [0x42; 16],
                    ims_res: [1; 8],
                    ims_ck: [2; 16],
                    ims_ik: [3; 16],
                },
                SyntheticProfile {
                    iccid: "8901234567890123457",
                    imsi: "234330987654321",
                    smsc: "+4478720006261",
                    ike_res: [0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87, 0x98],
                    ike_ck: [0x51; 16],
                    ike_ik: [0x62; 16],
                    ims_res: [4; 8],
                    ims_ck: [5; 16],
                    ims_ik: [6; 16],
                },
            ]),
        )
    }
    fn with_profile_data(imsi: &str, profiles: Option<[SyntheticProfile; 2]>) -> Self {
        let gate = Arc::new(Gate::default());
        gate.initialize_native_reader_producer("native-multi-provider");
        gate.initialize_native_lifecycle("native-multi-provider")
            .unwrap();
        let trace = Arc::new(Trace {
            synthetic_imsi: Some(imsi.to_owned()),
            profiles,
            ..Default::default()
        });
        trace.multi.store(true, Ordering::SeqCst);
        let factory = trace.clone();
        let owner = Owner::with_factory(
            "native-multi-provider".into(),
            Arc::new(move |_| {
                factory.opened.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Sim {
                    trace: factory.clone(),
                    selected: [0, 0],
                    application: "",
                }))
            }),
            Arc::new(|| Ok(vec![READER.into()])),
            Arc::new(|_, _| panic!("not eSTK")),
        );
        let session = owner.native_open(READER).unwrap();
        let transition = gate
            .prepare_native_transition(
                "native-multi-provider",
                &SelectionSnapshot::default(),
                NativeTransitionKind::Connect,
                READER,
                NativeAuthority::Idle,
            )
            .unwrap();
        gate.commit_native_transition(&transition, Some(&session), NativeReaderFacts::default())
            .unwrap();
        let service = Service::new(owner.clone(), gate.clone(), None, None);
        let pool = crate::multistandby_native_card::Pool::acquire(
            owner.clone(),
            gate.clone(),
            service,
            READER.into(),
            1,
        )
        .unwrap();
        Self {
            pool,
            gate,
            owner,
            session,
            trace,
        }
    }
    pub(crate) fn profile(&self, first: bool) -> crate::multistandby_native_card::Profile {
        crate::multistandby_native_card::Profile {
            eid: EID.into(),
            aid: crate::reader_esim_download::encode_hex_lower(
                proletarius_euicc::GSMA_ISDR_APPLICATION_AID,
            ),
            iccid: if first {
                ICCID.into()
            } else {
                "8901234567890123457".into()
            },
        }
    }
    pub(crate) fn auth_count(&self) -> usize {
        self.trace.auth.load(Ordering::SeqCst)
    }
    pub(crate) fn switch_count(&self) -> usize {
        self.trace.enables.load(Ordering::SeqCst)
    }
    pub(crate) fn transaction_count(&self) -> usize {
        self.trace.transactions.load(Ordering::SeqCst)
    }
    pub(crate) fn inventory_resources(&self) -> (Owner, Arc<Gate>, Service) {
        (
            self.owner.clone(),
            self.gate.clone(),
            Service::new(self.owner.clone(), self.gate.clone(), None, None),
        )
    }
    pub(crate) fn auth_trace(&self) -> Vec<AuthTrace> {
        self.trace.auth_trace.lock().unwrap().clone()
    }
    pub(crate) fn selected_profiles(&self) -> Vec<String> {
        self.trace.selected_profiles.lock().unwrap().clone()
    }
    pub(crate) fn hold_auth_for(
        &self,
        iccid: &str,
        application: &str,
        rand: [u8; 16],
    ) -> TargetedAuthHold {
        assert!(
            self.trace.profiles.is_some(),
            "targeted barrier requires distinct profiles"
        );
        assert!(matches!(application, "usim" | "isim"));
        let (entered, receive) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let mut target = self.trace.targeted_auth.lock().unwrap();
        assert!(target.is_none(), "only one targeted AUTH barrier");
        *target = Some(TargetedAuth {
            iccid: iccid.into(),
            application: application.into(),
            rand,
            entered,
            release: wait,
        });
        TargetedAuthHold {
            entered: Some(receive),
            release: Some(release),
        }
    }
    /// A new complete physical transaction and logical-channel EID/metadata read.
    /// This never reads synthetic identity fields or runs a Pool workflow.
    pub(crate) fn read_identity(&self) -> Result<Value, String> {
        self.owner
            .native_work_on_session(&self.session, |transport| {
                let channel = proletarius_reader::euicc_transport::open_euicc_channel(
                    FixtureBorrowed(&mut *transport),
                    proletarius_euicc::GSMA_ISDR_APPLICATION_AID,
                )
                .map_err(|e| e.to_string())?;
                let number = channel.channel();
                let mut card = proletarius_euicc::Euicc::with_default_mss(channel, number)
                    .map_err(|e| e.to_string())?;
                let mut raw = [0; proletarius_euicc::EID_LENGTH_BYTES];
                let result = card.get_eid(&mut raw).map_err(|e| e.to_string());
                let close = card.transport_mut().close().map_err(|e| e.to_string());
                let length = result?;
                close?;
                let eid = crate::reader_esim_download::encode_hex_lower(&raw[..length]);
                drop(card);
                let metadata = proletarius_reader::read_sim_metadata_auto_with_transport(transport)
                    .map_err(|e| e.to_string())?;
                let mut identity =
                    crate::reader_lifecycle::metadata_value(metadata).map_err(|e| e.to_string())?;
                identity["eid"] = json!(eid);
                Ok(identity)
            })
            .map_err(|e| e.to_string())?
    }
    pub(crate) fn hold_next_apdu(&self) -> (std::sync::mpsc::Sender<()>, Arc<AtomicBool>) {
        let (send, recv) = std::sync::mpsc::channel();
        let entered = Arc::new(AtomicBool::new(false));
        *self.trace.held_next.lock().unwrap() = Some((recv, entered.clone()));
        (send, entered)
    }
    pub(crate) fn physical_binding(&self) -> crate::reader_pcsc::LifecycleBinding {
        self.session.clone()
    }
    pub(crate) fn hold_close(&self) -> (std::sync::mpsc::Sender<()>, Arc<AtomicBool>) {
        let (send, recv) = std::sync::mpsc::channel();
        let entered = Arc::new(AtomicBool::new(false));
        *self.trace.held_close.lock().unwrap() = Some((recv, entered.clone()));
        (send, entered)
    }
    pub(crate) fn fail_close(&self, fail: bool) {
        self.trace.fail_close.store(fail, Ordering::SeqCst);
    }
    pub(crate) fn fail_next_apdu(&self) {
        self.trace.fail_next_apdu.store(true, Ordering::SeqCst);
    }
    pub(crate) fn closed_count(&self) -> usize {
        self.trace.closed.load(Ordering::SeqCst)
    }
    pub(crate) fn original_profile_enabled(&self) -> bool {
        !self.trace.enabled.load(Ordering::SeqCst)
    }
    pub(crate) fn hold_auth(&self) -> std::sync::mpsc::Sender<()> {
        let (send, recv) = std::sync::mpsc::channel();
        *self.trace.held.lock().unwrap() = Some(recv);
        send
    }
    pub(crate) async fn finish(self) {
        self.pool.shutdown().await.unwrap();
        assert!(
            !self.trace.enabled.load(Ordering::SeqCst),
            "original profile must be physically restored"
        );
        assert_eq!(self.transaction_count(), 0);
        self.owner.native_close(&self.session).unwrap();
        assert_eq!(
            self.trace.opened.load(Ordering::SeqCst),
            self.trace.closed.load(Ordering::SeqCst)
        );
    }
}

struct FixtureBorrowed<'a, T>(&'a mut T);
impl<T: ApduTransport> ApduTransport for FixtureBorrowed<'_, T> {
    type Error = T::Error;
    fn transmit(&mut self, command: &[u8]) -> Result<Vec<u8>, Self::Error> {
        self.0.transmit(command)
    }
}
