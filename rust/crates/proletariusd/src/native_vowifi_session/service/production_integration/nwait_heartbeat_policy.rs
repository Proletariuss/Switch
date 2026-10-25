//! Actual daemon SMS-only/ordinary launch -> synthetic APDU -> real local SOCKS,
//! IKE/ESP and SIP. The peer observes packets; it never injects daemon health.
use super::*;
use crate::native_registered_session::Control;
use crate::native_sms_originating::{Phase as SmsPhase, RpResult, TextSend};
use crate::native_vowifi_session::multi::{NativeMultiSession, Runtime};
use proletarius_sip::{Message, Request as SipRequest};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
};

const VODAFONE: &str = "234150123456789";
const OTHER: &str = "234330123456789";
const PROFILE: &str = "heartbeat-profile";
const INCOMING: &str = "heartbeat-incoming";
const OUTGOING_ACK: &str = "heartbeat-outgoing-rp-ack";
const INCOMING_OPTIONS: &str = "heartbeat-incoming-options";

#[derive(Default)]
pub(super) struct Facts {
    options: AtomicUsize,
    incoming_options: AtomicUsize,
    incoming_accepted: AtomicUsize,
    incoming_report: AtomicUsize,
    outgoing: AtomicUsize,
    outgoing_ack: AtomicUsize,
    refresh_challenges: AtomicUsize,
    refreshes: AtomicUsize,
}
pub(super) enum Command {
    IncomingOptions,
    CloseSignaling,
}
pub(super) struct Server {
    pub(super) imsi: String,
    pub(super) domain: String,
    pub(super) expiry: u32,
    pub(super) commands: mpsc::Receiver<Command>,
    facts: Arc<Facts>,
    store: Option<crate::multistandby_store::Store>,
    blackhole_options: bool,
    challenge_refresh: bool,
    challenged: bool,
    call_id: String,
    cseq: u32,
    branch: String,
}
fn field<'a>(request: &'a SipRequest, name: &str) -> &'a str {
    request.data.headers.get(name).unwrap().value()
}
impl Server {
    fn new(
        imsi: &str,
        store: Option<crate::multistandby_store::Store>,
        blackhole_options: bool,
        challenge_refresh: bool,
    ) -> (Self, Arc<Facts>, mpsc::Sender<Command>) {
        let facts = Arc::new(Facts::default());
        let (commands, input) = mpsc::channel(4);
        (
            Self {
                imsi: imsi.into(),
                domain: format!("ims.mnc0{}.mcc234.3gppnetwork.org", &imsi[3..5]),
                expiry: if challenge_refresh { 4 } else { 3600 },
                commands: input,
                facts: facts.clone(),
                store,
                blackhole_options,
                challenge_refresh,
                challenged: false,
                call_id: String::new(),
                cseq: 0,
                branch: String::new(),
            },
            facts,
            commands,
        )
    }
    pub(super) fn registered(&mut self, request: &SipRequest) {
        self.call_id = field(request, "Call-ID").into();
        self.cseq = field(request, "CSeq")
            .split_ascii_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        self.branch = field(request, "Via").into();
        assert!(field(request, "Authorization").contains(&self.imsi));
    }
    fn incoming(&self, method: &str, id: &str, body: &[u8]) -> Vec<u8> {
        let mut wire = format!(
            "{method} sip:+447700900123@{} SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:6060;branch=z9hG4bK-{id}\r\nFrom: <sip:smsc@{}>;tag=carrier\r\nP-Asserted-Identity: <sip:smsc@{}>\r\nTo: <sip:+447700900123@{}>\r\nCall-ID: {id}\r\nCSeq: 73 {method}\r\nContent-Type: application/vnd.3gpp.sms\r\nContent-Length: {}\r\n\r\n",
            self.domain, self.domain, self.domain, self.domain, body.len()
        )
        .into_bytes();
        wire.extend(body);
        wire
    }
    pub(super) fn delivery(&self) -> Vec<u8> {
        if self.store.is_none() {
            return Vec::new();
        }
        let tpdu = [
            0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
        ];
        let mut rpdu = vec![1, 42, 0, 0, tpdu.len() as u8];
        rpdu.extend(tpdu);
        self.incoming("MESSAGE", INCOMING, &rpdu)
    }
    pub(super) async fn command<W: AsyncWrite + Unpin>(
        &mut self,
        command: Command,
        write: &mut W,
    ) -> bool {
        match command {
            Command::IncomingOptions => {
                write
                    .write_all(&self.incoming("OPTIONS", INCOMING_OPTIONS, &[]))
                    .await
                    .unwrap();
                write.flush().await.unwrap();
                false
            }
            Command::CloseSignaling => true,
        }
    }
    pub(super) async fn observe<W: AsyncWrite + Unpin>(&mut self, message: Message, write: &mut W) {
        match message {
            Message::Response(response) => {
                assert_eq!(response.status, 200);
                match response.data.headers.get("Call-ID").unwrap().value() {
                    INCOMING => {
                        let (inbox, _) = self.store.as_ref().unwrap().forwarding_page(0).unwrap();
                        assert_eq!(inbox.len(), 1, "real SIP ACK follows durable Nwait commit");
                        assert!(inbox[0].id().starts_with(&format!("nwait:{PROFILE}:")));
                        self.facts.incoming_accepted.fetch_add(1, Ordering::SeqCst);
                    }
                    OUTGOING_ACK => {
                        self.facts.outgoing_ack.fetch_add(1, Ordering::SeqCst);
                    }
                    INCOMING_OPTIONS => {
                        assert!(
                            response
                                .data
                                .headers
                                .get("CSeq")
                                .unwrap()
                                .value()
                                .ends_with(" OPTIONS")
                        );
                        self.facts.incoming_options.fetch_add(1, Ordering::SeqCst);
                    }
                    other => panic!("unexpected heartbeat peer response {other}"),
                }
            }
            Message::Request(request) => match request.method.as_str() {
                "OPTIONS" => {
                    self.facts.options.fetch_add(1, Ordering::SeqCst);
                    // A wrong production policy really loses its responses;
                    // no successful heartbeat is fabricated for the exception.
                    if !self.blackhole_options {
                        write
                            .write_all(&ims_peer::response(&request, 200, "", ""))
                            .await
                            .unwrap();
                        write.flush().await.unwrap();
                    }
                }
                "REGISTER" => {
                    assert_eq!(field(&request, "Call-ID"), self.call_id);
                    let cseq: u32 = field(&request, "CSeq")
                        .split_ascii_whitespace()
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    assert!(cseq > self.cseq);
                    assert_ne!(field(&request, "Via"), self.branch);
                    self.cseq = cseq;
                    self.branch = field(&request, "Via").into();
                    assert!(field(&request, "Security-Verify").contains("aes-cbc"));
                    if self.challenge_refresh && !self.challenged {
                        self.challenged = true;
                        let extra = format!(
                            "WWW-Authenticate: Digest realm=\"{}\",nonce=\"{}\",algorithm=AKAv1-MD5\r\n",
                            self.domain,
                            "02".repeat(32)
                        );
                        write
                            .write_all(&ims_peer::response(&request, 401, &extra, ""))
                            .await
                            .unwrap();
                        self.facts.refresh_challenges.fetch_add(1, Ordering::SeqCst);
                    } else {
                        assert!(
                            !self.challenge_refresh
                                || field(&request, "Authorization").contains("nonce=\"0202")
                        );
                        write
                            .write_all(&ims_peer::response(&request, 200, "Expires: 30\r\n", ""))
                            .await
                            .unwrap();
                        self.facts.refreshes.fetch_add(1, Ordering::SeqCst);
                    }
                    write.flush().await.unwrap();
                }
                "MESSAGE" => {
                    assert!(field(&request, "Security-Verify").contains("aes-cbc"));
                    assert!(field(&request, "Route").contains("10.20.30.9:6060"));
                    match request.data.body[0] {
                        0 => {
                            assert_eq!(
                                request.uri.to_string(),
                                format!("sip:smsc@{}", self.domain)
                            );
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
                            assert_eq!(
                                self.facts.outgoing.fetch_add(1, Ordering::SeqCst),
                                0,
                                "no repeated RP-DATA"
                            );
                            let mut wire = ims_peer::response(&request, 202, "", "");
                            wire.extend(self.incoming("MESSAGE", OUTGOING_ACK, &[3, reference]));
                            write.write_all(&wire).await.unwrap();
                            write.flush().await.unwrap();
                        }
                        2 => {
                            assert_eq!(request.data.body, proletarius_sms::build_ack(42));
                            write
                                .write_all(&ims_peer::response(&request, 202, "", ""))
                                .await
                                .unwrap();
                            write.flush().await.unwrap();
                            self.facts.incoming_report.fetch_add(1, Ordering::SeqCst);
                        }
                        kind => panic!("unexpected RP message kind {kind}"),
                    }
                }
                other => panic!("unexpected heartbeat peer method {other}"),
            },
        }
    }
}

async fn until(mut predicate: impl FnMut() -> bool, label: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("heartbeat integration deadline: {label}"));
}
async fn nwait_ready(session: &NativeMultiSession) {
    until(
        || {
            let state = session.snapshot();
            assert!(
                state.termination.is_none(),
                "real startup failed: {:?}",
                state.termination
            );
            state.phase == ordinary::Phase::Registered && state.sms_ready
        },
        "Nwait real registration",
    )
    .await;
    assert!(!session.snapshot().call_ready);
}
fn plan(proxy: std::net::SocketAddr) -> PlanRequest {
    PlanRequest {
        reader: "Workflow Reader".into(),
        proxy: proxy.to_string(),
        epdg: "epdg.invalid".into(),
        epdg_port: Some(500),
    }
}
async fn outgoing(session: &NativeMultiSession, facts: &Facts) {
    let (reply, wait) = oneshot::channel();
    session
        .controls()
        .send(Control::SendText {
            input: TextSend {
                generation: session.generation(),
                batch_id: "heartbeat-real-sms".into(),
                recipient: sms_peer::RECIPIENT.to_owned().into(),
                text: sms_peer::OUTGOING.to_owned().into(),
                request_status_report: true,
            },
            reply,
        })
        .await
        .unwrap();
    wait.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (reply, wait) = oneshot::channel();
            session
                .controls()
                .send(Control::SmsStatus {
                    generation: session.generation(),
                    batch_id: "heartbeat-real-sms".into(),
                    reply,
                })
                .await
                .unwrap();
            let receipt = wait.await.unwrap().unwrap().unwrap();
            if receipt.phase == SmsPhase::Accepted {
                assert_eq!(receipt.rp_results, vec![(1, RpResult::Ack)]);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    until(
        || facts.outgoing_ack.load(Ordering::SeqCst) == 1,
        "real carrier RP ACK acknowledged",
    )
    .await;
}

// Carrier/override caches are process-scoped. Each real scenario has an isolated
// child config/root rather than mutating process environment under sibling tests.
async fn isolated(test: &str) -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("SWITCH_NWAIT_HEARTBEAT_699_CHILD") {
        return Some(PathBuf::from(root));
    }
    let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
        "runtime/nwait-heartbeat-699-{}",
        crate::multistandby_store::random_token().unwrap()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let config = root.join("carrier.yaml");
    // Crypto overrides match this existing synthetic IKE oracle. They do not
    // override carrier_id or the new session-mode/OPTIONS policy.
    std::fs::write(&config, "carrier_overrides:\n  '234/15': {ip_stack: ipv4, ike_proposals: [aes128-sha256-prfsha256-modp2048], esp_proposals: [aes128-sha256], ims_tcp_keepalive_seconds: 1, ims_options_ping_interval_seconds: 1}\n  '234/33': {ip_stack: ipv4, ims_tcp_keepalive_seconds: 1, ims_options_ping_interval_seconds: 1}\n").unwrap();
    let exact = format!(
        "native_vowifi_session::service::production_integration::nwait_heartbeat_policy::{test}"
    );
    async fn bounded_output(
        mut pipe: impl tokio::io::AsyncRead + Unpin,
        name: &str,
    ) -> Result<Vec<u8>, String> {
        use tokio::io::AsyncReadExt;
        const LIMIT: usize = 1024 * 1024;
        let mut output = Vec::new();
        let mut chunk = [0; 8192];
        loop {
            let n = pipe
                .read(&mut chunk)
                .await
                .map_err(|e| format!("child {name} read: {e}"))?;
            if n == 0 {
                return Ok(output);
            }
            if n > LIMIT - output.len() {
                return Err(format!("child {name} exceeded {LIMIT} bytes"));
            }
            output.extend_from_slice(&chunk[..n]);
        }
    }
    // The retained supervisor owns Child before its first wait. If this test
    // waiter is cancelled, dropping its sender requests actual kill + reap;
    // dropping the JoinHandle does not abort the supervisor.
    let (observer, cancelled) = oneshot::channel::<()>();
    let supervisor = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let mut child =
            tokio::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
                .args(["--exact", &exact, "--nocapture"])
                .env("SWITCH_NWAIT_HEARTBEAT_699_CHILD", &root)
                .env("SWITCH_DATA_DIR", &root)
                .env("PROLETARIUS_VOWIFI_CONFIG", config)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| format!("child spawn: {e}"))?;
        let result = if let Some((stdout, stderr)) = child.stdout.take().zip(child.stderr.take()) {
            let completed = async {
                tokio::try_join!(
                    async { child.wait().await.map_err(|e| format!("child wait: {e}")) },
                    bounded_output(stdout, "stdout"),
                    bounded_output(stderr, "stderr"),
                )
            };
            tokio::select! {
                biased;
                _ = cancelled => Err("child observer cancelled".to_owned()),
                _ = tokio::time::sleep_until(deadline) => Err("child exceeded 60s wall deadline".to_owned()),
                result = completed => result,
            }
        } else {
            Err("child output pipes unavailable".to_owned())
        };
        match result {
            Ok((status, stdout, stderr)) => Ok(std::process::Output {
                status,
                stdout,
                stderr,
            }),
            Err(reason) => {
                // All wait/read futures above are dropped before terminating.
                // Never report a timeout/overflow as finished while Child lives.
                let kill = child.start_kill();
                let reaped = child.wait().await;
                Err(format!("{reason}; kill={kill:?}; reap={reaped:?}"))
            }
        }
    });
    let result = supervisor.await.expect("child supervisor joined");
    drop(observer);
    let output = result.unwrap_or_else(|error| panic!("heartbeat child failed: {error}"));
    assert!(
        output.status.success(),
        "heartbeat child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nwait_vodafone_no_active_options_keeps_real_sms_refresh_and_remote_eof_cleanup() {
    let Some(root) =
        isolated("nwait_vodafone_no_active_options_keeps_real_sms_refresh_and_remote_eof_cleanup")
            .await
    else {
        return;
    };
    tokio::time::timeout(Duration::from_secs(35), nwait_scenario(root, true, true))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nwait_other_carrier_retains_active_options_and_real_stop_cleanup() {
    let Some(root) =
        isolated("nwait_other_carrier_retains_active_options_and_real_stop_cleanup").await
    else {
        return;
    };
    tokio::time::timeout(Duration::from_secs(30), nwait_scenario(root, false, false))
        .await
        .unwrap();
}

async fn nwait_scenario(root: PathBuf, vodafone: bool, remote_eof: bool) {
    let imsi = if vodafone { VODAFONE } else { OTHER };
    let physical = crate::multistandby_card::production::NativeFixture::with_imsi(imsi);
    let runtime = Runtime::new(
        physical.pool.clone(),
        physical.gate.clone(),
        root.clone(),
        Default::default(),
    )
    .unwrap();
    let store = crate::multistandby_store::Store::open(&root).unwrap();
    store.apply("catalog", json!({"event_id":"heartbeat-catalog", "profiles":[{"profile_id":PROFILE,"card_id":"heartbeat-card"}]})).unwrap();
    let (server, facts, commands) = Server::new(imsi, Some(store.clone()), vodafone, vodafone);
    let peer = ike_peer::Peer::with_heartbeat_policy(server).await;
    let (_stop, signal) = watch::channel(false);
    let session = runtime
        .start_for_profile(
            physical.profile(true),
            plan(peer.proxy),
            signal,
            store.clone(),
            PROFILE.into(),
        )
        .await
        .unwrap();
    nwait_ready(&session).await;
    let generation = session.generation();
    assert_eq!(physical.auth_count(), 2, "real IKE AKA plus IMS AKA");
    assert_eq!(physical.transaction_count(), 0);
    until(
        || {
            facts.incoming_accepted.load(Ordering::SeqCst) == 1
                && facts.incoming_report.load(Ordering::SeqCst) == 1
        },
        "durable incoming and RP report",
    )
    .await;
    outgoing(&session, &facts).await;
    commands.send(Command::IncomingOptions).await.unwrap();
    until(
        || facts.incoming_options.load(Ordering::SeqCst) == 1,
        "passive incoming OPTIONS still gets real 200",
    )
    .await;
    if vodafone {
        until(
            || facts.refreshes.load(Ordering::SeqCst) == 1,
            "same-association challenged REGISTER refresh",
        )
        .await;
        assert_eq!(facts.refresh_challenges.load(Ordering::SeqCst), 1);
        assert_eq!(
            physical.auth_count(),
            3,
            "refresh 401 must really call frozen profile AKA"
        );
        // Cross the original 4s grant as well as two old 1s probe periods AFTER
        // renewal. A flag-only lease update or reinstalled probe cannot pass.
        tokio::time::sleep(Duration::from_millis(2300)).await;
        assert_eq!(facts.options.load(Ordering::SeqCst), 0);
        assert_eq!(session.generation(), generation);
        assert!(session.snapshot().sms_ready);
        assert!(
            session
                .snapshot()
                .registration_expires_at_ms
                .is_some_and(|expiry| {
                    expiry
                        > (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000)
                            as i64
                            + 20_000
                })
        );
    } else {
        until(
            || facts.options.load(Ordering::SeqCst) >= 2,
            "other SMS-only carrier probes twice",
        )
        .await;
        assert!(session.snapshot().sms_ready);
    }
    assert_eq!(peer.trace.ike.load(Ordering::SeqCst), 1);
    assert_eq!(
        peer.trace.registers.load(Ordering::SeqCst),
        1,
        "no replacement registration association"
    );
    assert!(
        !root.join("sms-archive.json").exists(),
        "real Nwait archive remains profile-scoped"
    );
    if remote_eof {
        peer.trace
            .planned_refresh_failure
            .store(true, Ordering::SeqCst);
        commands.send(Command::CloseSignaling).await.unwrap();
    } else {
        peer.trace.stop_requested.store(true, Ordering::SeqCst);
        session.request_stop();
    }
    let exit = tokio::time::timeout(Duration::from_secs(8), session.wait_exit())
        .await
        .unwrap();
    assert!(exit.joined && exit.cleanup_confirmed);
    assert!(exit.snapshot.cleanup_confirmed && exit.snapshot.lease_released);
    assert!(!exit.snapshot.sms_ready && !exit.snapshot.call_ready);
    assert_eq!(exit.snapshot.phase, ordinary::Phase::Released);
    let reason = exit.snapshot.termination.as_ref().unwrap().reason;
    assert_eq!(
        reason,
        if remote_eof {
            ordinary::status::EndReason::RemoteFailure
        } else {
            ordinary::status::EndReason::UserStop
        }
    );
    assert!(session.controls().is_closed());
    runtime.shutdown().await.unwrap();
    assert_eq!(physical.transaction_count(), 0);
    let trace = peer.trace.clone();
    peer.finish().await;
    assert_eq!(trace.deletes.load(Ordering::SeqCst), 1);
    physical.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinary_vodafone_keeps_active_options_and_real_stop_cleanup() {
    let Some(root) = isolated("ordinary_vodafone_keeps_active_options_and_real_stop_cleanup").await
    else {
        return;
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        let physical = crate::multistandby_card::production::NativeFixture::with_imsi(VODAFONE);
        let (server, facts, _commands) = Server::new(VODAFONE, None, false, false);
        let peer = ike_peer::Peer::with_heartbeat_policy(server).await;
        let card = physical.pool.prepare(physical.profile(true)).await.unwrap();
        let context = prepare_card_context(plan(peer.proxy), SessionCard::on_demand(card), true)
            .await
            .unwrap_or_else(|(error, _)| panic!("ordinary real context: {}", error.code()));
        assert_eq!(context.ims.carrier_id(), "apple_vodafone_uk");
        assert_eq!(
            context.ims.tcp_keepalive_seconds(),
            1,
            "real positive TCP policy remains configured"
        );
        let lifecycle = Arc::new(NativeLifecycle::default());
        lifecycle.activate().unwrap();
        let handle = lifecycle
            .launch_ordinary(
                context,
                ordinary::Inputs {
                    sms_archive: Default::default(),
                    reader_gate: physical.gate.clone(),
                    settings: startup_settings().unwrap(),
                    data_root: root.clone(),
                    transactions: Default::default(),
                    call_facts: crate::call_facts::CallFacts::new(
                        proletarius_store::ArchiveStore::new(root.join("calls.json")),
                        "heartbeat-ordinary".into(),
                    ),
                },
            )
            .unwrap_or_else(|(_, _)| panic!("ordinary production admission"));
        until(
            || {
                handle.snapshot().phase == ordinary::Phase::Registered
                    && handle.snapshot().call_ready
            },
            "ordinary voice registration",
        )
        .await;
        until(
            || facts.options.load(Ordering::SeqCst) >= 2,
            "same carrier ordinary voice still probes",
        )
        .await;
        assert!(handle.snapshot().sms_ready && handle.snapshot().call_ready);
        peer.trace.stop_requested.store(true, Ordering::SeqCst);
        handle.request_stop();
        lifecycle
            .finish_shutdown(tokio::time::Instant::now() + Duration::from_secs(8))
            .await
            .expect("ordinary actual owner/network cleanup confirmed");
        let state = handle.snapshot();
        assert_eq!(state.phase, ordinary::Phase::Released);
        assert!(state.cleanup_confirmed && state.lease_released);
        assert!(!state.sms_ready && !state.call_ready);
        assert!(handle.controls.is_closed());
        physical.pool.shutdown().await.unwrap();
        let trace = peer.trace.clone();
        peer.finish().await;
        assert_eq!(trace.deletes.load(Ordering::SeqCst), 1);
        physical.finish().await;
    })
    .await
    .unwrap();
}
