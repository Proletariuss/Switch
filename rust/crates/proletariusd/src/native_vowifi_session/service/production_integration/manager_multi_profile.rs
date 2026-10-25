//! One real Manager and one proxy; immutable wire identity selects each oracle.
//! Only card/carrier peers are synthetic. No readiness or cleanup is injected.
use super::*;
use crate::multistandby_native_card::CleanupReceipt;
use crate::multistandby_native_runtime::{
    Action, ConnectionOptions, Inventory, Manager, ProfileSpec, Resources,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use proletarius_ims::aka_digest::{AkaDigestInput, AkaPassword, DigestQop, compute_aka_digest};
use proletarius_sip::{Message, Request as SipRequest};
use std::sync::atomic::{AtomicBool, AtomicU64};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot},
};

const DOMAIN: &str = "ims.mnc033.mcc234.3gppnetwork.org";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Profile {
    A,
    B,
}
pub(super) struct Material {
    pub(super) res: [u8; 8],
    pub(super) ck: [u8; 16],
    pub(super) ik: [u8; 16],
}
impl Profile {
    fn index(self) -> usize {
        if self == Self::A { 0 } else { 1 }
    }
    fn iccid(self) -> &'static str {
        if self == Self::A {
            "8901234567890123456"
        } else {
            "8901234567890123457"
        }
    }
    fn imsi(self) -> &'static str {
        if self == Self::A {
            "234330123456789"
        } else {
            "234330987654321"
        }
    }
    pub(super) fn nai(self) -> String {
        format!("0{}@nai.epc.mnc033.mcc234.3gppnetwork.org", self.imsi())
    }
    pub(super) fn domain(self) -> &'static str {
        DOMAIN
    }
    fn private_identity(self) -> String {
        format!("{}@{DOMAIN}", self.imsi())
    }
    pub(super) fn public_identity(self) -> String {
        format!("sip:{}@{DOMAIN}", self.imsi())
    }
    fn smsc(self) -> &'static str {
        if self == Self::A {
            "+4478720006260"
        } else {
            "+4478720006261"
        }
    }
    pub(super) fn expiry(self) -> u32 {
        if self == Self::A { 60 } else { 3600 }
    }
    pub(super) fn ike(self) -> Material {
        match self {
            Self::A => Material {
                res: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
                ck: [0x31; 16],
                ik: [0x42; 16],
            },
            Self::B => Material {
                res: [0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87, 0x98],
                ck: [0x51; 16],
                ik: [0x62; 16],
            },
        }
    }
    pub(super) fn ims(self) -> Material {
        match self {
            Self::A => Material {
                res: [1; 8],
                ck: [2; 16],
                ik: [3; 16],
            },
            Self::B => Material {
                res: [4; 8],
                ck: [5; 16],
                ik: [6; 16],
            },
        }
    }
}
#[derive(Default)]
pub(super) struct Facts {
    pub(super) association: AtomicU64,
    pub(super) eap_verified: AtomicUsize,
    pub(super) deletes: AtomicUsize,
    pub(super) joined: AtomicUsize,
    pub(super) ims_joined: AtomicUsize,
    pub(super) stop_requested: AtomicBool,
    initial_digest: AtomicUsize,
    refreshed: AtomicUsize,
    refresh_challenge: AtomicUsize,
    passive_options: AtomicUsize,
    warning_accepted: AtomicUsize,
    warning_rp_acks: AtomicUsize,
}
pub(super) enum Command {
    Probe,
    WarningSms {
        serial: u32,
        rpdu: Vec<u8>,
        reject_report: bool,
    },
}
pub(super) struct Oracle {
    facts: [Arc<Facts>; 2],
    commands: [Mutex<Option<mpsc::Sender<Command>>>; 2],
}
impl Oracle {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            facts: std::array::from_fn(|_| Arc::new(Facts::default())),
            commands: std::array::from_fn(|_| Mutex::new(None)),
        })
    }
    fn facts(&self, profile: Profile) -> &Facts {
        &self.facts[profile.index()]
    }
    pub(super) fn bind_exact_idi(&self, identity: &[u8], association: u64) -> BoundOracle {
        let profile = [Profile::A, Profile::B]
            .into_iter()
            .find(|p| p.nai().as_bytes() == identity)
            .expect("unknown exact decrypted IDi");
        assert_ne!(association, 0);
        let facts = self.facts[profile.index()].clone();
        assert!(
            facts
                .association
                .compare_exchange(0, association, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
            "unexpected repeated association for this exact identity"
        );
        let (send, commands) = mpsc::channel(2);
        assert!(
            self.commands[profile.index()]
                .lock()
                .unwrap()
                .replace(send)
                .is_none()
        );
        BoundOracle {
            profile,
            facts,
            commands,
            call_id: String::new(),
            cseq: 0,
            via: String::new(),
            challenged: false,
            reject_warning_report: false,
        }
    }
    async fn probe(&self, profile: Profile) {
        let send = self.commands[profile.index()]
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        send.send(Command::Probe).await.unwrap();
    }
}
pub(super) struct BoundOracle {
    pub(super) profile: Profile,
    pub(super) facts: Arc<Facts>,
    pub(super) commands: mpsc::Receiver<Command>,
    call_id: String,
    cseq: u32,
    via: String,
    challenged: bool,
    reject_warning_report: bool,
}
fn field<'a>(r: &'a SipRequest, name: &str) -> &'a str {
    r.data.headers.get(name).unwrap().value()
}
impl BoundOracle {
    pub(super) fn nonce(&self, byte: u8) -> String {
        STANDARD.encode([byte; 32])
    }
    fn verify_digest(&self, r: &SipRequest, byte: u8) {
        let value = field(r, "Authorization");
        // The controlled no-qop challenge produces only simple comma-separated
        // values. Reject duplicates; do not reuse the production authorization
        // serializer as the expected result or accept just a username substring.
        let mut parameters = std::collections::BTreeMap::new();
        for part in value.strip_prefix("Digest ").unwrap().split(',') {
            let (name, value) = part.trim().split_once('=').unwrap();
            assert!(parameters.insert(name, value.trim_matches('"')).is_none());
        }
        let user = self.profile.private_identity();
        let nonce = self.nonce(byte);
        let uri = r.uri.to_string();
        assert_eq!(parameters["username"], user);
        assert_eq!(parameters["realm"], DOMAIN);
        assert_eq!(parameters["nonce"], nonce);
        assert_eq!(parameters["uri"], uri);
        assert_eq!(parameters["algorithm"], "AKAv1-MD5");
        assert!(!parameters.contains_key("qop"));
        let material = self.profile.ims();
        let expected = compute_aka_digest(
            &AkaDigestInput {
                algorithm: "AKAv1-MD5",
                username: &user,
                realm: DOMAIN,
                nonce: &nonce,
                method: "REGISTER",
                uri: &uri,
                qop: DigestQop::None,
            },
            AkaPassword::Res(&material.res),
        )
        .unwrap();
        assert_eq!(
            parameters["response"],
            expected.response.as_str(),
            "wrong profile RES must not authenticate"
        );
        assert!(field(r, "From").contains(&format!("<{}>", self.profile.public_identity())));
        assert!(field(r, "To").contains(&format!("<{}>", self.profile.public_identity())));
    }
    pub(super) fn registered(&mut self, r: &SipRequest) {
        self.verify_digest(r, 1);
        self.call_id = field(r, "Call-ID").into();
        self.cseq = field(r, "CSeq")
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        self.via = field(r, "Via").into();
        assert_eq!(self.facts.initial_digest.fetch_add(1, Ordering::SeqCst), 0);
    }
    pub(super) async fn command<W: AsyncWrite + Unpin>(&mut self, command: Command, write: &mut W) {
        if let Command::WarningSms {
            serial,
            rpdu,
            reject_report,
        } = command
        {
            self.reject_warning_report = reject_report;
            let id = format!("warning-756-{serial}");
            let mut wire = format!("MESSAGE {} SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:6060;branch=z9hG4bK-{id}\r\nFrom: <sip:smsc@{DOMAIN}>;tag=peer\r\nP-Asserted-Identity: <sip:smsc@{DOMAIN}>\r\nTo: <{}>\r\nCall-ID: {id}\r\nCSeq: {serial} MESSAGE\r\nContent-Type: application/vnd.3gpp.sms\r\nContent-Length: {}\r\n\r\n", self.profile.public_identity(), self.profile.public_identity(), rpdu.len()).into_bytes();
            wire.extend(rpdu);
            write.write_all(&wire).await.unwrap();
            write.flush().await.unwrap();
            return;
        }
        let id = format!("multi-{:?}-passive", self.profile);
        let wire = format!(
            "OPTIONS {} SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:6060;branch=z9hG4bK-{id}\r\nFrom: <sip:pcscf@{DOMAIN}>;tag=peer\r\nTo: <{}>\r\nCall-ID: {id}\r\nCSeq: 700 OPTIONS\r\nContent-Length: 0\r\n\r\n",
            self.profile.public_identity(),
            self.profile.public_identity()
        );
        write.write_all(wire.as_bytes()).await.unwrap();
        write.flush().await.unwrap();
    }
    pub(super) async fn observe<W: AsyncWrite + Unpin>(&mut self, message: Message, write: &mut W) {
        match message {
            Message::Response(r) => {
                assert_eq!(r.status, 200);
                if r.data
                    .headers
                    .get("Call-ID")
                    .unwrap()
                    .value()
                    .starts_with("warning-756-")
                {
                    self.facts.warning_accepted.fetch_add(1, Ordering::SeqCst);
                    return;
                }
                assert_eq!(
                    r.data.headers.get("Call-ID").unwrap().value(),
                    format!("multi-{:?}-passive", self.profile)
                );
                assert_eq!(r.data.headers.get("CSeq").unwrap().value(), "700 OPTIONS");
                self.facts.passive_options.fetch_add(1, Ordering::SeqCst);
            }
            Message::Request(r) => match r.method.as_str() {
                "OPTIONS" => {
                    write
                        .write_all(&ims_peer::response(&r, 200, "", ""))
                        .await
                        .unwrap();
                    write.flush().await.unwrap();
                }
                "MESSAGE" => {
                    assert!(field(&r, "In-Reply-To").starts_with("warning-756-"));
                    assert_eq!(r.data.body, proletarius_sms::build_ack(42));
                    write
                        .write_all(&ims_peer::response(
                            &r,
                            if self.reject_warning_report { 503 } else { 202 },
                            "",
                            "",
                        ))
                        .await
                        .unwrap();
                    write.flush().await.unwrap();
                    self.facts.warning_rp_acks.fetch_add(1, Ordering::SeqCst);
                }
                "REGISTER" => {
                    assert_eq!(
                        self.profile,
                        Profile::A,
                        "B long lease cannot unexpectedly refresh"
                    );
                    assert_eq!(field(&r, "Call-ID"), self.call_id);
                    let cseq: u32 = field(&r, "CSeq")
                        .split_whitespace()
                        .next()
                        .unwrap()
                        .parse()
                        .unwrap();
                    assert!(cseq > self.cseq);
                    assert_ne!(field(&r, "Via"), self.via);
                    self.cseq = cseq;
                    self.via = field(&r, "Via").into();
                    assert!(field(&r, "Security-Verify").contains("aes-cbc"));
                    if !self.challenged {
                        self.challenged = true;
                        let extra = format!(
                            "WWW-Authenticate: Digest realm=\"{DOMAIN}\",nonce=\"{}\",algorithm=AKAv1-MD5\r\n",
                            self.nonce(2)
                        );
                        write
                            .write_all(&ims_peer::response(&r, 401, &extra, ""))
                            .await
                            .unwrap();
                        write.flush().await.unwrap();
                        self.facts.refresh_challenge.fetch_add(1, Ordering::SeqCst);
                    } else {
                        self.verify_digest(&r, 2);
                        assert_eq!(self.facts.refreshed.load(Ordering::SeqCst), 0);
                        write
                            .write_all(&ims_peer::response(&r, 200, "Expires: 600\r\n", ""))
                            .await
                            .unwrap();
                        write.flush().await.unwrap();
                        self.facts.refreshed.fetch_add(1, Ordering::SeqCst);
                    }
                }
                _ => panic!("unexpected business work in minimum multi-profile oracle"),
            },
        }
    }
}

async fn wait_for(mut test: impl FnMut() -> bool, seconds: u64, label: &str) {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("multi-profile deadline: {label}"));
}
fn row(manager: &Manager, card: &str, id: &str) -> serde_json::Value {
    manager.snapshot(card)["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["profile_id"] == id)
        .unwrap()
        .clone()
}
fn identity(value: &serde_json::Value, profile: Profile) {
    assert_eq!(value["iccid"], profile.iccid());
    assert_eq!(value["imsi"], profile.imsi());
    assert_eq!(value["private_identity"], profile.private_identity());
    assert_eq!(value["public_identity"], profile.public_identity());
    assert_eq!(value["home_domain"], DOMAIN);
    assert_eq!(value["smsc"], profile.smsc());
    assert_eq!(
        value["smsc_psi"],
        format!("sip:{}@{DOMAIN}", profile.smsc())
    );
    assert_eq!(
        value["smsc_target"],
        format!("sip:{}@{DOMAIN}", profile.smsc())
    );
}

// Same bounded/retained child lifecycle as 699v3, with this scenario's longer
// real lease budget. The sealed699 helper and its tests are not modified.
async fn isolated() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("SWITCH_MULTI_PROFILE_700_CHILD") {
        return Some(PathBuf::from(root));
    }
    let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
        "runtime/multi-profile-700-{}",
        crate::multistandby_store::random_token().unwrap()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let config = root.join("carrier.yaml");
    std::fs::write(
        &config,
        "carrier_overrides:\n  '234/33': {ip_stack: ipv4, ims_options_ping_interval_seconds: 1}\n",
    )
    .unwrap();
    async fn bounded(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>, String> {
        use tokio::io::AsyncReadExt;
        let mut data = Vec::new();
        let mut chunk = [0; 8192];
        loop {
            let n = pipe.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Ok(data);
            }
            if n > 1024 * 1024 - data.len() {
                return Err("child output exceeded1MiB".into());
            }
            data.extend_from_slice(&chunk[..n]);
        }
    }
    let (observer, cancelled) = oneshot::channel::<()>();
    let supervisor = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
        let mut child=tokio::process::Command::new(std::env::current_exe().map_err(|e|e.to_string())?).args(["--exact","native_vowifi_session::service::production_integration::manager_multi_profile::manager_distinct_profiles_a_b_a_refresh_preserves_sibling_and_restores_original","--nocapture"]).env("SWITCH_MULTI_PROFILE_700_CHILD",&root).env("SWITCH_DATA_DIR",&root).env("PROLETARIUS_VOWIFI_CONFIG",config).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().map_err(|e|e.to_string())?;
        let result = if let Some((stdout, stderr)) = child.stdout.take().zip(child.stderr.take()) {
            let completion = async {
                tokio::try_join!(
                    async { child.wait().await.map_err(|e| e.to_string()) },
                    bounded(stdout),
                    bounded(stderr)
                )
            };
            tokio::select! {biased;_=cancelled=>Err("child observer cancelled".to_owned()),_=tokio::time::sleep_until(deadline)=>Err("child exceeded150s".to_owned()),r=completion=>r}
        } else {
            Err("missing child capture pipe".into())
        };
        match result {
            Ok(v) => Ok(v),
            Err(error) => {
                let kill = child.start_kill();
                let reap = child.wait().await;
                Err(format!("{error};kill={kill:?};reap={reap:?}"))
            }
        }
    });
    let result = supervisor.await.unwrap();
    drop(observer);
    let (status, stdout, stderr) = result.unwrap_or_else(|e| panic!("multi-profile child: {e}"));
    assert!(
        status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_distinct_profiles_a_b_a_refresh_preserves_sibling_and_restores_original() {
    let Some(root) = isolated().await else { return };
    tokio::time::timeout(Duration::from_secs(120), scenario(root))
        .await
        .unwrap();
}
async fn scenario(root: PathBuf) {
    let physical =
        Arc::new(crate::multistandby_card::production::NativeFixture::with_distinct_profiles());
    let reader = physical.clone();
    let original = tokio::task::spawn_blocking(move || reader.read_identity())
        .await
        .unwrap()
        .unwrap();
    identity(&original, Profile::B);
    assert_eq!(original["eid"], "89049032000000000000000000000001");
    physical.pool.shutdown().await.unwrap(); // Retire only the fixture's idle Pool, before Manager acquisition.
    let a = physical.profile(true);
    let b = physical.profile(false);
    let store = crate::multistandby_store::Store::open(&root).unwrap();
    let catalog=store.apply("catalog",json!({"event_id":"multi-700-catalog","reader":"Workflow Reader","profiles":[{"eid":a.eid,"iccid":a.iccid,"display_name":"A","phone_number":""},{"eid":b.eid,"iccid":b.iccid,"display_name":"B","phone_number":""}]})).unwrap();
    let profiles = catalog["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| ProfileSpec {
            profile_id: p["profile_id"].as_str().unwrap().into(),
            card_id: p["card_id"].as_str().unwrap().into(),
            eid: p["eid"].as_str().unwrap().into(),
            iccid: p["iccid"].as_str().unwrap().into(),
            aid: a.aid.clone(),
            display_name: "synthetic".into(),
            phone_number: String::new(),
            physical_enabled: p["iccid"] == b.iccid,
        })
        .collect::<Vec<_>>();
    let first = profiles
        .iter()
        .find(|p| p.iccid == Profile::A.iccid())
        .unwrap()
        .profile_id
        .clone();
    let second = profiles
        .iter()
        .find(|p| p.iccid == Profile::B.iccid())
        .unwrap()
        .profile_id
        .clone();
    let card = profiles[0].card_id.clone();
    let oracle = Oracle::new();
    let peer = ike_peer::Peer::with_multi_profile_oracle(oracle.clone()).await;
    let manager = Manager::new(Resources {
        owner: physical.owner.clone(),
        gate: physical.gate.clone(),
        card_service: crate::multistandby_card::Service::new(
            physical.owner.clone(),
            physical.gate.clone(),
            None,
            None,
        ),
        data_root: root.clone(),
        store,
        journal: Arc::new(Mutex::new(
            crate::multistandby_runtime_report::Journal::open(&root.join("runtime-report.json"), 1),
        )),
        transactions: Default::default(),
        connection: ConnectionOptions {
            proxy: peer.proxy.to_string(),
            epdg: "epdg.invalid".into(),
            epdg_port: Some(500),
        },
    })
    .unwrap();
    manager
        .install_inventory(Inventory {
            reader: "Workflow Reader".into(),
            generation: 1,
            profiles,
        })
        .await
        .unwrap();
    manager
        .execute(Action::Start, card.clone(), vec![first.clone()])
        .unwrap()
        .wait()
        .await
        .unwrap();
    wait_for(
        || manager.send_readiness(&first).channel_ready,
        15,
        "A registration",
    )
    .await;
    identity(
        physical
            .gate
            .native_reader_snapshot()
            .1
            .unwrap()
            .metadata
            .as_ref()
            .unwrap(),
        Profile::A,
    );
    // Keep this actual Runtime Arc alive through final Stop + borrow + readback.
    // Its Drop cleanup must never manufacture evidence for an early Stop return.
    let actual_runtime = manager.runtime_for_test().unwrap();
    let finished = actual_runtime.cleanup_watch_for_test();
    assert!(finished.borrow().is_none());
    let generation_a = row(&manager, &card, &first)["session_generation"]
        .as_u64()
        .unwrap();
    let expiry = row(&manager, &card, &first)["registration_expires_at_ms"]
        .as_i64()
        .unwrap();
    let mut held = physical.hold_auth_for(Profile::A.iccid(), "usim", [2; 16]);
    manager
        .execute(Action::Start, card.clone(), vec![second.clone()])
        .unwrap()
        .wait()
        .await
        .unwrap();
    wait_for(
        || manager.send_readiness(&second).channel_ready,
        15,
        "B registration",
    )
    .await;
    identity(
        physical
            .gate
            .native_reader_snapshot()
            .1
            .unwrap()
            .metadata
            .as_ref()
            .unwrap(),
        Profile::B,
    );
    let generation_b = row(&manager, &card, &second)["session_generation"]
        .as_u64()
        .unwrap();
    assert_ne!(generation_a, generation_b);
    assert!(manager.send_readiness(&first).channel_ready);
    assert_eq!(physical.transaction_count(), 0);
    tokio::time::timeout(Duration::from_secs(75), held.entered())
        .await
        .unwrap();
    assert_eq!(
        oracle
            .facts(Profile::A)
            .refresh_challenge
            .load(Ordering::SeqCst),
        1
    );
    assert_eq!(physical.transaction_count(), 1);
    let trace = physical.auth_trace();
    assert!(trace.iter().any(|e| e.iccid == Profile::A.iccid()
        && e.imsi == Profile::A.imsi()
        && e.application == "usim"
        && e.rand == [2; 16]
        && e.autn == [2; 16]
        && !e.completed));
    oracle.probe(Profile::B).await;
    wait_for(
        || {
            oracle
                .facts(Profile::B)
                .passive_options
                .load(Ordering::SeqCst)
                == 1
        },
        5,
        "B actual OPTIONS200 while A AUTH held",
    )
    .await;
    assert_eq!(physical.transaction_count(), 1);
    assert!(manager.send_readiness(&second).channel_ready);
    assert_eq!(
        row(&manager, &card, &second)["session_generation"],
        generation_b
    );
    assert_eq!(peer.trace.ike.load(Ordering::SeqCst), 2);
    assert_eq!(oracle.facts(Profile::A).refreshed.load(Ordering::SeqCst), 0);
    held.release();
    wait_for(
        || {
            oracle.facts(Profile::A).refreshed.load(Ordering::SeqCst) == 1
                && row(&manager, &card, &first)["registration_expires_at_ms"]
                    .as_i64()
                    .is_some_and(|v| v > expiry + 100_000)
        },
        10,
        "A genuine same-association refresh600s",
    )
    .await;
    assert_eq!(
        row(&manager, &card, &first)["session_generation"],
        generation_a
    );
    assert!(
        manager.send_readiness(&first).channel_ready
            && manager.send_readiness(&second).channel_ready
    );
    assert_eq!(physical.transaction_count(), 0);
    assert_eq!(
        physical.auth_count(),
        5,
        "exact A/B initial EAP+IMS and one A refresh AUTH"
    );
    let completed_auth = physical.auth_trace();
    assert_eq!(
        completed_auth.len(),
        10,
        "one entry and one completion per real AUTH"
    );
    for (profile, rand, autn) in [
        (Profile::A, [0x11; 16], [0x22; 16]),
        (Profile::A, [1; 16], [1; 16]),
        (Profile::B, [0x11; 16], [0x22; 16]),
        (Profile::B, [1; 16], [1; 16]),
        (Profile::A, [2; 16], [2; 16]),
    ] {
        for completed in [false, true] {
            assert_eq!(
                completed_auth
                    .iter()
                    .filter(|event| event.iccid == profile.iccid()
                        && event.imsi == profile.imsi()
                        && event.application == "usim"
                        && event.rand == rand
                        && event.autn == autn
                        && event.completed == completed)
                    .count(),
                1
            );
        }
    }
    assert_eq!(peer.trace.registers.load(Ordering::SeqCst), 2);
    assert_eq!(peer.trace.ike.load(Ordering::SeqCst), 2);
    for profile in [Profile::A, Profile::B] {
        assert_eq!(oracle.facts(profile).eap_verified.load(Ordering::SeqCst), 1);
        assert_eq!(
            oracle.facts(profile).initial_digest.load(Ordering::SeqCst),
            1
        );
    }
    assert_ne!(
        oracle.facts(Profile::A).association.load(Ordering::SeqCst),
        oracle.facts(Profile::B).association.load(Ordering::SeqCst)
    );
    oracle
        .facts(Profile::A)
        .stop_requested
        .store(true, Ordering::SeqCst);
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    manager
        .execute(Action::Stop, card.clone(), vec![first.clone()])
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(!manager.send_readiness(&first).channel_ready);
    assert!(manager.send_readiness(&second).channel_ready);
    assert!(finished.borrow().is_none());
    wait_for(
        || oracle.facts(Profile::A).joined.load(Ordering::SeqCst) == 1,
        5,
        "A peer actual cleanup/join",
    )
    .await;
    assert_eq!(oracle.facts(Profile::B).joined.load(Ordering::SeqCst), 0);
    oracle
        .facts(Profile::B)
        .stop_requested
        .store(true, Ordering::SeqCst);
    manager
        .execute(Action::Stop, card.clone(), vec![second.clone()])
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(
        matches!(
            &*finished.borrow(),
            Some(Ok(CleanupReceipt::OriginalRestored))
        ),
        "final Stop must already have confirmed original restore; no cleanup helper may supply it"
    );
    assert!(manager.runtime_for_test().is_none());
    assert_eq!(physical.transaction_count(), 0);
    // This new idle admission cannot succeed if Stop retained the old parent.
    // It performs no physical work and is released before independent readback.
    let idle_probe = physical
        .gate
        .acquire_native_multi("Workflow Reader", 1)
        .expect("Stop already released the exact parent Gate owner");
    physical.gate.release_native_multi(&idle_probe).unwrap();
    let reader = physical.clone();
    let restored = tokio::task::spawn_blocking(move || reader.read_identity())
        .await
        .unwrap()
        .unwrap();
    identity(&restored, Profile::B);
    for key in [
        "eid",
        "iccid",
        "imsi",
        "private_identity",
        "public_identity",
        "smsc",
        "smsc_psi",
    ] {
        assert_eq!(original[key], restored[key]);
    }
    assert_eq!(physical.transaction_count(), 0);
    let selected = physical.selected_profiles();
    let mut last = None;
    let sequence = selected
        .iter()
        .filter_map(|p| {
            let fresh = last != Some(p);
            last = Some(p);
            fresh.then_some(p.as_str())
        })
        .collect::<Vec<_>>();
    assert!(
        sequence.windows(4).any(|v| v
            == [
                Profile::A.iccid(),
                Profile::B.iccid(),
                Profile::A.iccid(),
                Profile::B.iccid()
            ]),
        "actual A/B/A/B ENABLE order"
    );
    for profile in [Profile::A, Profile::B] {
        wait_for(
            || oracle.facts(profile).joined.load(Ordering::SeqCst) == 1,
            5,
            "peer joined",
        )
        .await;
        assert_eq!(oracle.facts(profile).deletes.load(Ordering::SeqCst), 1);
        assert_eq!(oracle.facts(profile).ims_joined.load(Ordering::SeqCst), 1);
    }
    // Only after Stop's own receipt + fresh readback may fallback teardown run.
    drop(actual_runtime);
    manager.shutdown().await.unwrap();
    peer.finish().await;
    Arc::try_unwrap(physical)
        .unwrap_or_else(|_| panic!("fixture leaked Arc"))
        .finish()
        .await;
}

mod warning_756 {
    include!("manager_multi_profile/warning_756.rs");
}
