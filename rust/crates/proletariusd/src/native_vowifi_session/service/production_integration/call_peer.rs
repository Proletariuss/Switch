//! Carrier-side SIP and tunneled RTP oracle. No daemon call or media state is
//! injected: public controls must produce every packet observed here.
use proletarius_call::{
    RtpPacket,
    sdp::{self, AudioCodec, AudioNegotiation},
};
use proletarius_sip::{Message, Request, Response};
use proletarius_vowifi::netstack_udp::UdpControl;
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::{mpsc, watch},
};

const DOMAIN: &str = "ims.mnc033.mcc234.3gppnetwork.org";
const INCOMING: &str = "production-492-incoming-call";
pub(super) const PEER: &str = "+447700900456";
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Event {
    RefreshRequest,
    ProbeDuringAka,
    Refreshed,
    Invite,
    Prack,
    Ack,
    Bye,
    Ringing,
    OfferlessTrying,
    ReliableProvisional {
        codec: &'static str,
        rseq: u32,
    },
    ReliableRetransmitted,
    ReliablePrackCompleted {
        status: u16,
        codec: &'static str,
    },
    ReliablePrackReplayed,
    ReliableFinalPendingAck,
    UnassociatedPrackRejected,
    Answer,
    AnswerPendingAck,
    OfferlessAnswerPendingAck {
        codec: &'static str,
        payload: u8,
    },
    AnswerRetransmitted,
    IncomingAckSent,
    DialogProbed(u16),
    AcceptedReplayChecked {
        transactions: usize,
        previous_dialog: bool,
        status: u16,
    },
    CarrierAudio {
        payload: usize,
        ssrc: u32,
        sequence: u16,
        timestamp: u32,
    },
    RemoteByeAccepted,
    Renegotiated,
    RenegotiationRejected(u16),
    ReinvitePendingAck,
    ReinviteRetransmitted,
    ReinviteAckSent,
    NegativeFinal(u16),
    NegativeAckSent,
    CompletedReplayed {
        transactions: usize,
        previous_dialog: bool,
        status: u16,
    },
    Updated,
    UpdateRejected(u16),
    Audio {
        codec: &'static str,
        payload: usize,
        mode: Option<u8>,
        ssrc: u32,
        sequence: u16,
        timestamp: u32,
        peer_speech: bool,
    },
    CmrArmed(u8),
    Dtmf,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RefreshMode {
    Accepted,
    Challenge401,
    Challenge407,
    ChallengeRejected,
    Rejected,
    NewSecurity,
    Expire,
    StopPending,
    StopAka,
}
pub(super) enum Command {
    ProbeDuringHeldAka,
    ReliableRinging,
    AnswerOutgoing,
    Incoming,
    IncomingDeferredAck,
    IncomingOfferless {
        amr_wb: bool,
    },
    IncomingReliable {
        offerless: bool,
        prack_offer_wb: bool,
    },
    ReliablePrack {
        rack: RackCase,
    },
    ReplayReliablePrack {
        previous_dialog: bool,
        malicious: bool,
    },
    UnassociatedPrack,
    AckIncoming,
    ReplayIncomingAck {
        malicious: bool,
        previous_dialog: bool,
    },
    SendIncomingAudio,
    ProbeDialog,
    ReplayAccepted {
        previous_dialog: bool,
    },
    RemoteHangup,
    HoldReinvite,
    AckHeldReinvite,
    NegativeInvite {
        kind: NegativeKind,
    },
    AckCompletedCollision,
    ReplayCompleted {
        previous_dialog: bool,
    },
    AmrWbCmr(u8),
    Update {
        codec: AudioCodec,
        payload: u8,
    },
    Renegotiate {
        codec: AudioCodec,
        payload: u8,
        period: u8,
        neighbor: bool,
    },
}
#[derive(Clone, Copy)]
pub(super) enum RackCase {
    Valid,
    Malformed,
    WrongRseq,
}
#[derive(Clone, Copy)]
pub(super) enum NegativeKind {
    Pending,
    Collision,
    BadContact,
    WrongContentType,
    ShortTimer,
    BadCodec,
}
pub(super) struct Client {
    pub(super) commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<Event>,
}
impl Client {
    pub(super) async fn next(&mut self) -> Event {
        tokio::time::timeout(Duration::from_secs(5), self.events.recv())
            .await
            .expect("call wire event deadline")
            .expect("call peer stopped")
    }
}
enum NativeMedia {
    Decode(
        proletarius_call::evs::CodecProfile,
        Vec<u8>,
        u8,
        tokio::sync::oneshot::Sender<bool>,
    ),
    Encode(
        proletarius_call::evs::CodecProfile,
        Box<[i16; 320]>,
        tokio::sync::oneshot::Sender<(Vec<u8>, u8)>,
    ),
}

fn speech_rtp(
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut wire = vec![0x80, payload_type];
    wire.extend(sequence.to_be_bytes());
    wire.extend(timestamp.to_be_bytes());
    wire.extend(ssrc.to_be_bytes());
    wire.extend_from_slice(payload);
    wire
}

#[derive(Clone)]
struct AcceptedReinvite {
    invite: Vec<u8>,
    response: Response,
    ack: Vec<u8>,
    accepted_at: tokio::time::Instant,
}
struct ReplayBarrier {
    probe: Request,
    transactions: Vec<AcceptedReinvite>,
    previous_dialog: bool,
    checking: bool,
}
struct ReliableIncoming {
    offerless: bool,
    prack_offer_wb: bool,
    provisional: Option<Response>,
    rseq: u32,
    prack_answer: String,
    completed: bool,
}
struct PendingReliablePrack {
    request: Request,
    wire: Vec<u8>,
    offer: Option<String>,
    expected: u16,
}
struct HeldReinvite {
    invite: Vec<u8>,
    response: Option<Response>,
    answer_ack: Option<Vec<u8>>,
    remote: Option<AudioNegotiation>,
}
#[derive(Clone)]
struct CompletedOracle {
    invite: Vec<u8>,
    response: Response,
    received_at: tokio::time::Instant,
    acked: bool,
}
struct PendingNegative {
    invite: Vec<u8>,
    expected: u16,
}
struct CompletedReplay {
    probe: Request,
    remaining: Vec<CompletedOracle>,
    transactions: usize,
    previous_dialog: bool,
}

pub(super) struct Server {
    pub(super) refresh: Option<RefreshMode>,
    register_cseq: Option<u32>,
    register_id: Option<String>,
    register_branch: Option<String>,
    challenged: bool,
    refresh_count: u32,
    pub(super) commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>,
    media: Option<tokio::task::JoinHandle<()>>,
    remote: watch::Sender<Option<AudioNegotiation>>,
    echo_cmr: watch::Sender<u8>,
    port: u16,
    outgoing: Option<Request>,
    incoming_to: Option<String>,
    incoming_id: String,
    incoming_count: u32,
    active_outgoing: bool,
    renegotiation_cseq: u32,
    pending_offer: Option<(u32, String)>,
    pending_offer_wire: Option<Vec<u8>>,
    held_reinvite: Option<HeldReinvite>,
    pending_negative: Option<PendingNegative>,
    completed_invites: Vec<CompletedOracle>,
    completed_replay: Option<CompletedReplay>,
    pending_update: Option<(u32, String)>,
    update_finals: Vec<Response>,
    final_acks: Vec<(Response, Vec<u8>)>,
    accepted_reinvites: Vec<AcceptedReinvite>,
    silent_replays: Vec<Response>,
    replay_barrier: Option<ReplayBarrier>,
    pending_dialog_probe: Option<Request>,
    pending_transport_probes: Vec<Request>,
    pending_bye: Option<Request>,
    pending_unassociated_prack: Option<Request>,
    outgoing_ack_seen: bool,
    defer_initial_ack: bool,
    initial_answer_seen: bool,
    initial_offer: Option<String>,
    offerless_prefer_amr: Option<bool>,
    reliable: Option<ReliableIncoming>,
    pending_reliable_prack: Option<PendingReliablePrack>,
    reliable_prack_history: Vec<(Vec<u8>, Response)>,
    replay_prack_response: Option<Response>,
    held_initial_answer: Option<(Response, Vec<u8>)>,
    initial_ack: Option<Vec<u8>>,
    initial_ack_history: Vec<(String, Vec<u8>)>,
    media_control: mpsc::Sender<()>,
    media_input: Option<mpsc::Receiver<()>>,
}
pub(super) fn new() -> (Server, Client) {
    let (commands, input) = mpsc::channel(8);
    let (events, output) = mpsc::channel(32);
    let (remote, _) = watch::channel(None);
    let (echo_cmr, _) = watch::channel(15);
    let (media_control, media_input) = mpsc::channel(8);
    (
        Server {
            refresh: None,
            register_cseq: None,
            register_id: None,
            register_branch: None,
            challenged: false,
            refresh_count: 0,
            commands: input,
            events,
            media: None,
            remote,
            echo_cmr,
            port: 0,
            outgoing: None,
            incoming_to: None,
            incoming_id: INCOMING.into(),
            incoming_count: 0,
            active_outgoing: false,
            renegotiation_cseq: 10,
            pending_offer: None,
            pending_offer_wire: None,
            held_reinvite: None,
            pending_negative: None,
            completed_invites: Vec::new(),
            completed_replay: None,
            pending_update: None,
            update_finals: Vec::new(),
            final_acks: Vec::new(),
            accepted_reinvites: Vec::new(),
            silent_replays: Vec::new(),
            replay_barrier: None,
            pending_dialog_probe: None,
            pending_transport_probes: Vec::new(),
            pending_bye: None,
            pending_unassociated_prack: None,
            outgoing_ack_seen: false,
            defer_initial_ack: false,
            initial_answer_seen: false,
            initial_offer: None,
            offerless_prefer_amr: None,
            reliable: None,
            pending_reliable_prack: None,
            reliable_prack_history: vec![],
            replay_prack_response: None,
            held_initial_answer: None,
            initial_ack: None,
            initial_ack_history: vec![],
            media_control,
            media_input: Some(media_input),
        },
        Client {
            commands,
            events: output,
        },
    )
}
fn field<'a>(r: &'a Request, name: &str) -> &'a str {
    r.data.headers.get(name).unwrap().value()
}
fn cseq(response: &Response) -> &str {
    response.data.headers.get("CSeq").unwrap().value()
}
fn response(r: &Request, status: u16, body: &str) -> Vec<u8> {
    let headers = ["Via", "From", "To", "Call-ID", "CSeq"]
        .iter()
        .map(|name| {
            let value = field(r, name);
            if *name == "To" && !value.contains(";tag=") {
                format!("To: {value};tag=carrier-492\r\n")
            } else {
                format!("{name}: {value}\r\n")
            }
        })
        .collect::<String>();
    format!("SIP/2.0 {status} Fixture\r\n{headers}Contact: <sip:{PEER}@10.20.30.9:6060>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",body.len()).into_bytes()
}
fn same_final_response(accepted: &Response, response: &Response) -> bool {
    accepted.status == response.status
        && accepted.data.body == response.data.body
        && ["Via", "From", "To", "Call-ID", "CSeq"].iter().all(|name| {
            accepted.data.headers.get(name).map(|h| h.value())
                == response.data.headers.get(name).map(|h| h.value())
        })
}
fn same_response_transaction(accepted: &Response, response: &Response) -> bool {
    ["Via", "From", "To", "Call-ID", "CSeq"].iter().all(|name| {
        accepted.data.headers.get(name).map(|h| h.value())
            == response.data.headers.get(name).map(|h| h.value())
    })
}
fn response_matches_request(request: &Request, response: &Response) -> bool {
    ["Via", "From", "To", "Call-ID", "CSeq"].iter().all(|name| {
        request.data.headers.get(name).map(|h| h.value())
            == response.data.headers.get(name).map(|h| h.value())
    })
}

fn parse_request(wire: &[u8]) -> Request {
    let (message, consumed) = proletarius_sip::Parser::default()
        .parse(wire, true)
        .unwrap();
    assert_eq!(consumed, wire.len());
    let Message::Request(request) = message else {
        panic!("carrier command must encode a request")
    };
    request
}

impl Server {
    fn current_dialog_id(&self) -> &str {
        if self.active_outgoing {
            field(self.outgoing.as_ref().unwrap(), "Call-ID")
        } else {
            &self.incoming_id
        }
    }
    fn next_replay_probe(&mut self) -> (Request, Vec<u8>) {
        let options_cseq = self.renegotiation_cseq;
        self.renegotiation_cseq += 1;
        let mut wire = self.dialog_request("OPTIONS", options_cseq, "");
        self.pending_transport_probes.push(parse_request(&wire));
        let owner_cseq = self.renegotiation_cseq;
        self.renegotiation_cseq += 1;
        let owner = self.dialog_request("INFO", owner_cseq, "");
        let request = parse_request(&owner);
        // OPTIONS is answered by the socket actor before business events are
        // consumed. INFO traverses the registered consumer and calls owner;
        // its response also drains every earlier INVITE/ACK business event.
        wire.extend(owner);
        (request, wire)
    }
    fn retained_ack(&self, response: &Response) -> Option<&[u8]> {
        self.final_acks.iter().find_map(|(accepted, ack)| {
            same_final_response(accepted, response).then_some(ack.as_slice())
        })
    }
    fn retain_ack(&mut self, response: Response, ack: Vec<u8>) {
        // Keep accepted transactions from both fixture dialogs: a delayed 200
        // for the previous dialog must still receive its original tagged ACK.
        if self.final_acks.len() == 32 {
            self.final_acks.remove(0);
        }
        self.final_acks.push((response, ack));
    }
    pub(super) fn registration_seed(&mut self, r: &Request) {
        self.register_cseq = Some(
            field(r, "CSeq")
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap(),
        );
        self.register_id = Some(field(r, "Call-ID").to_owned());
        self.register_branch = Some(field(r, "Via").to_owned());
    }
    pub(super) async fn start_media(&mut self, udp: UdpControl, mut stop: watch::Receiver<bool>) {
        let mut socket = udp.bind("10.20.30.9".parse().unwrap()).await.unwrap();
        self.port = socket.local_addr().port();
        let remote = self.remote.subscribe();
        let echo_cmr = self.echo_cmr.subscribe();
        let events = self.events.clone();
        let mut media_input = self.media_input.take().unwrap();
        self.media = Some(tokio::spawn(async move {
            let mut dtmf = Vec::new();
            let mut previous_audio: Option<(u32, u16, u32)> = None;
            let mut previous_remote = None;
            // Native codec state stays on its dedicated carrier worker. The
            // real decoder validates every uplink speech payload independently
            // before it is echoed back through ESP to the production decoder.
            let (native, input) = std::sync::mpsc::sync_channel::<NativeMedia>(8);
            let decoder = std::thread::Builder::new()
                .name("fixture-carrier-codec".into())
                .spawn(move || {
                    use proletarius_call::evs::{CodecProfile, EvsCodec};
                    let mut state: Option<(bool, EvsCodec)> = None;
                    while let Ok(command) = input.recv() {
                        let profile = match &command {
                            NativeMedia::Decode(profile, ..) | NativeMedia::Encode(profile, ..) => {
                                *profile
                            }
                        };
                        let amr = matches!(profile, CodecProfile::AmrWb { .. });
                        if state.as_ref().is_none_or(|(old, _)| *old != amr) {
                            state = Some((
                                amr,
                                EvsCodec::new(profile).expect("real carrier codec required"),
                            ));
                        }
                        let state = &mut state.as_mut().unwrap().1;
                        match command {
                            NativeMedia::Decode(_, bits, mode, reply) => {
                                let pcm = state
                                    .decode(&bits, mode, false)
                                    .expect("carrier must decode actual uplink speech");
                                reply.send(pcm.iter().any(|sample| *sample != 0)).unwrap();
                            }
                            NativeMedia::Encode(profile, pcm, reply) => {
                                let mode = match profile {
                                    CodecProfile::EvsPrimaryWb132 => 4,
                                    CodecProfile::AmrWb { mode } => {
                                        state.set_amr_wb_mode(mode).unwrap();
                                        mode
                                    }
                                };
                                let bits = state
                                    .encode(&pcm)
                                    .expect("carrier must encode actual downlink speech");
                                reply.send((bits, mode)).unwrap();
                            }
                        }
                    }
                })
                .unwrap();
            let downlink_ssrc = 0x492c_567a;
            let mut downlink_sequence = 4000u16;
            let mut downlink_timestamp = 1_280_000u32;
            loop {
                tokio::select! {
                    packet=socket.recv()=>{
                        let packet=match packet {
                            Ok(packet) => packet,
                            Err(error) => {
                                assert!(*stop.borrow(), "carrier RTP socket failed before verified IKE DELETE: {error}");
                                break;
                            }
                        };
                        let config=remote.borrow().clone().expect("RTP arrived before SDP");
                        assert_eq!(packet.peer,SocketAddr::new(config.remote_ip,config.remote_port));
                        let rtp=RtpPacket::parse(&packet.bytes).unwrap();
                        if Some(rtp.payload_type)==config.telephone_event_payload {
                            assert_eq!(rtp.payload[0],11,"public # DTMF");
                            dtmf.push(rtp.payload.to_vec());
                            if dtmf.len()==7 {
                                assert_eq!(dtmf[6][1]&0x80,0x80);
                                events.send(Event::Dtmf).await.unwrap(); dtmf.clear();
                            }
                        } else {
                            assert_eq!(rtp.payload_type,config.payload_type);
                            let endpoint = (config.remote_ip, config.remote_port);
                            if previous_remote != Some(endpoint) {
                                previous_audio = None;
                                previous_remote = Some(endpoint);
                            }
                            if let Some((ssrc, sequence, timestamp)) = previous_audio {
                                assert_eq!(rtp.ssrc,ssrc,"same dialog must keep its RTP SSRC across codec and CMR changes");
                                assert_eq!(rtp.sequence, sequence.wrapping_add(1));
                                assert_eq!(rtp.timestamp, timestamp.wrapping_add(320));
                            }
                            previous_audio = Some((rtp.ssrc, rtp.sequence, rtp.timestamp));
                            let (codec,mode,bits,frame_mode,profile)=match config.codec {
                                AudioCodec::Evs{header_full}=>{
                                    let frame=proletarius_call::evs::depacketize_primary(rtp.payload,header_full).unwrap();
                                    assert_eq!(frame.mode,4); ("EVS",None,frame.bits.to_vec(),frame.mode,proletarius_call::evs::CodecProfile::EvsPrimaryWb132)
                                },
                                AudioCodec::AmrWb{octet_aligned,policy,..}=>{
                                    let frame=proletarius_call::amr_wb::depacketize(rtp.payload,octet_aligned).unwrap();
                                    assert!(policy.contains(frame.mode),"uplink FT must remain in negotiated mode-set");
                                    assert_eq!(frame.raw_cmr,15,"fixture does not request a peer encoder change");
                                    ("AMR-WB",Some(frame.mode),frame.bits,frame.mode,proletarius_call::evs::CodecProfile::AmrWb { mode: frame.mode })
                                }
                            };
                            let (reply, decoded) = tokio::sync::oneshot::channel();
                            native.send(NativeMedia::Decode(profile,bits,frame_mode,reply)).unwrap();
                            let peer_speech=decoded.await.unwrap();
                            // Echo valid encoded speech through the real outer ESP and
                            // production decoder. Only the downlink CMR is changed;
                            // speech bits, FT and Q stay intact; the carrier's
                            // independent RTP chronology advances continuously.
                            // Carrier RTP owns its own continuous chronology,
                            // including speech sent before the first uplink/ACK.
                            let mut echo=speech_rtp(config.payload_type,downlink_sequence,downlink_timestamp,downlink_ssrc,rtp.payload);
                            if mode.is_some() {
                                echo[12]=(echo[12]&0x0f)|(*echo_cmr.borrow()<<4);
                            }
                            socket.sender().send_to(echo,packet.peer).await.unwrap();
                            downlink_sequence=downlink_sequence.wrapping_add(1);
                            downlink_timestamp=downlink_timestamp.wrapping_add(320);
                            events.send(Event::Audio{codec,payload:rtp.payload.len(),mode,ssrc:rtp.ssrc,sequence:rtp.sequence,timestamp:rtp.timestamp,peer_speech}).await.unwrap();
                        }
                    },
                    Some(())=media_input.recv()=>{
                        let config=remote.borrow().clone().expect("carrier speech requires an accepted SDP answer");
                        let profile=match config.codec {
                            AudioCodec::Evs { .. }=>proletarius_call::evs::CodecProfile::EvsPrimaryWb132,
                            AudioCodec::AmrWb { mode, .. }=>proletarius_call::evs::CodecProfile::AmrWb { mode },
                        };
                        let mut pcm=Box::new([0i16;320]);
                        for (sample,value) in pcm.iter_mut().enumerate() { *value=((sample%40) as i16-20)*500; }
                        let (reply,encoded)=tokio::sync::oneshot::channel();
                        native.send(NativeMedia::Encode(profile,pcm,reply)).unwrap();
                        let (bits,mode)=encoded.await.unwrap();
                        let payload=match config.codec {
                            AudioCodec::Evs { header_full }=>proletarius_call::evs::packetize_primary(&bits,mode,header_full).unwrap(),
                            AudioCodec::AmrWb { octet_aligned, .. }=>proletarius_call::amr_wb::packetize(&bits,mode,octet_aligned).unwrap(),
                        };
                        let wire=speech_rtp(config.payload_type,downlink_sequence,downlink_timestamp,downlink_ssrc,&payload);
                        socket.sender().send_to(wire,SocketAddr::new(config.remote_ip,config.remote_port)).await.unwrap();
                        events.send(Event::CarrierAudio {payload:payload.len(),ssrc:downlink_ssrc,sequence:downlink_sequence,timestamp:downlink_timestamp}).await.unwrap();
                        downlink_sequence=downlink_sequence.wrapping_add(1);
                        downlink_timestamp=downlink_timestamp.wrapping_add(320);
                    },
                    changed=stop.changed()=>{changed.unwrap();assert!(*stop.borrow());break;}
                }
            }
            let _ = socket.close().await;
            drop(native);
            tokio::task::spawn_blocking(move || decoder.join().unwrap())
                .await
                .unwrap();
        }));
    }
    fn sdp(&self, codec: AudioCodec, payload: u8) -> String {
        sdp::build_local_sdp(
            492,
            "10.20.30.9".parse().unwrap(),
            self.port,
            true,
            Some(codec),
            Some(payload),
            Some(102),
            false,
        )
        .unwrap()
    }
    fn incoming_request(&self, method: &str, cseq: u32, body: &str) -> Vec<u8> {
        let to = self
            .incoming_to
            .as_deref()
            .unwrap_or("<sip:+447700900123@ims.mnc033.mcc234.3gppnetwork.org>");
        format!("{method} sip:+447700900123@{DOMAIN} SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:6060;branch=z9hG4bK-492-{method}-{cseq}\r\nFrom: <sip:{PEER}@{DOMAIN}>;tag=carrier-incoming\r\nTo: {to}\r\nCall-ID: {}\r\nCSeq: {cseq} {method}\r\nContact: <sip:{PEER}@10.20.30.9:6060>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",self.incoming_id,body.len()).into_bytes()
    }
    fn dialog_request(&self, method: &str, cseq: u32, body: &str) -> Vec<u8> {
        if !self.active_outgoing {
            return self.incoming_request(method, cseq, body);
        }
        let initial = self.outgoing.as_ref().unwrap();
        let to = field(initial, "From");
        let call_id = field(initial, "Call-ID");
        format!("{method} sip:+447700900123@{DOMAIN} SIP/2.0\r\nVia: SIP/2.0/TCP 10.20.30.9:6060;branch=z9hG4bK-492-{method}-{cseq}\r\nFrom: <sip:{PEER}@{DOMAIN}>;tag=carrier-492\r\nTo: {to}\r\nCall-ID: {call_id}\r\nCSeq: {cseq} {method}\r\nContact: <sip:{PEER}@10.20.30.9:6060>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",body.len()).into_bytes()
    }
    pub(super) async fn command<W: AsyncWrite + Unpin>(&mut self, command: Command, write: &mut W) {
        let defer_incoming = matches!(
            &command,
            Command::IncomingDeferredAck
                | Command::IncomingOfferless { .. }
                | Command::IncomingReliable { .. }
        );
        let reliable_options = match &command {
            Command::IncomingReliable {
                offerless,
                prack_offer_wb,
            } => Some((*offerless, *prack_offer_wb)),
            _ => None,
        };
        let offerless_prefer_amr = match &command {
            Command::IncomingOfferless { amr_wb } => Some(*amr_wb),
            Command::IncomingReliable {
                offerless: true, ..
            } => Some(false),
            _ => None,
        };
        let wire = match command {
            Command::ProbeDuringHeldAka => {
                assert_eq!(self.refresh, Some(RefreshMode::StopAka));
                assert!(self.challenged);
                self.incoming_request("OPTIONS", 9, "")
            }
            Command::ReliableRinging => {
                String::from_utf8(response(self.outgoing.as_ref().unwrap(), 183, ""))
                    .unwrap()
                    .replacen("Contact:", "Require: 100rel\r\nRSeq: 10\r\nContact:", 1)
                    .into_bytes()
            }
            Command::AnswerOutgoing => {
                self.active_outgoing = true;
                response(
                    self.outgoing.as_ref().unwrap(),
                    200,
                    &self.sdp(AudioCodec::Evs { header_full: false }, 96),
                )
            }
            Command::Incoming
            | Command::IncomingDeferredAck
            | Command::IncomingOfferless { .. }
            | Command::IncomingReliable { .. } => {
                self.incoming_count += 1;
                self.incoming_id = if self.incoming_count == 1 {
                    INCOMING.into()
                } else {
                    format!("{INCOMING}-{}", self.incoming_count)
                };
                self.incoming_to = None;
                self.defer_initial_ack = defer_incoming;
                self.offerless_prefer_amr = offerless_prefer_amr;
                self.reliable =
                    reliable_options.map(|(offerless, prack_offer_wb)| ReliableIncoming {
                        offerless,
                        prack_offer_wb,
                        provisional: None,
                        rseq: 0,
                        prack_answer: String::new(),
                        completed: false,
                    });
                self.pending_reliable_prack = None;
                self.replay_prack_response = None;
                self.active_outgoing = false;
                self.initial_answer_seen = false;
                self.held_initial_answer = None;
                self.initial_ack = None;
                let body = if offerless_prefer_amr.is_some() {
                    String::new()
                } else if reliable_options.is_some() {
                    self.sdp(AudioCodec::Evs { header_full: false }, 96)
                } else {
                    self.sdp(
                        AudioCodec::AmrWb {
                            octet_aligned: true,
                            mode: 8,
                            policy: sdp::AmrWbModePolicy::new((1 << 2) | (1 << 8), 1, false)
                                .unwrap(),
                        },
                        98,
                    )
                };
                let wire = self.incoming_request("INVITE", 1, &body);
                self.initial_offer = Some(body);
                if reliable_options.is_some() {
                    String::from_utf8(wire)
                        .unwrap()
                        .replacen(
                            "Contact:",
                            "Require: 100rel\r\nSupported: 100rel\r\nContact:",
                            1,
                        )
                        .into_bytes()
                } else {
                    String::from_utf8(wire)
                        .unwrap()
                        .replacen("Contact:", "Supported: 100rel\r\nContact:", 1)
                        .into_bytes()
                }
            }
            Command::ReliablePrack { rack } => {
                assert!(self.pending_reliable_prack.is_none());
                let reliable = self.reliable.as_ref().expect("reliable incoming exchange");
                assert!(reliable.provisional.is_some() && !reliable.completed);
                let body = if reliable.offerless {
                    reliable.prack_answer.clone()
                } else if reliable.prack_offer_wb {
                    self.sdp(
                        AudioCodec::AmrWb {
                            octet_aligned: true,
                            mode: 8,
                            policy: sdp::AmrWbModePolicy::new((1 << 2) | (1 << 8), 1, false)
                                .unwrap(),
                        },
                        98,
                    )
                    .replace("o=- 492 1 ", "o=- 492 2 ")
                } else {
                    String::new()
                };
                let offer = (!reliable.offerless && reliable.prack_offer_wb).then(|| body.clone());
                let (rack, expected) = match rack {
                    RackCase::Valid => (format!("{} 1 INVITE", reliable.rseq), 200),
                    RackCase::Malformed => ("invalid-rack".into(), 400),
                    RackCase::WrongRseq => (format!("{} 1 INVITE", reliable.rseq + 1), 481),
                };
                let cseq = self.renegotiation_cseq;
                self.renegotiation_cseq += 1;
                let wire = String::from_utf8(self.dialog_request("PRACK", cseq, &body))
                    .unwrap()
                    .replacen("Contact:", &format!("RAck: {rack}\r\nContact:"), 1)
                    .into_bytes();
                self.pending_reliable_prack = Some(PendingReliablePrack {
                    request: parse_request(&wire),
                    wire: wire.clone(),
                    offer,
                    expected,
                });
                wire
            }
            Command::ReplayReliablePrack {
                previous_dialog,
                malicious,
            } => {
                assert!(self.replay_prack_response.is_none());
                let (original, response) = self
                    .reliable_prack_history
                    .iter()
                    .rev()
                    .find(|(_, response)| {
                        (response.data.headers.get("Call-ID").unwrap().value()
                            != self.current_dialog_id())
                            == previous_dialog
                    })
                    .expect("retained successful PRACK");
                self.replay_prack_response = Some(response.clone());
                if malicious {
                    let mut request = parse_request(original);
                    request.data.body =
                        b"v=0\r\nm=audio 9 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n".to_vec();
                    Message::Request(request).encode()
                } else {
                    original.clone()
                }
            }
            Command::AckIncoming => {
                let ack = if let Some((response, ack)) = self.held_initial_answer.take() {
                    self.retain_ack(response, ack.clone());
                    self.initial_ack = Some(ack.clone());
                    self.initial_ack_history
                        .push((self.incoming_id.clone(), ack.clone()));
                    ack
                } else {
                    self.initial_ack
                        .clone()
                        .expect("initial ACK must be explicitly authorized after its answer")
                };
                write.write_all(&ack).await.unwrap();
                write.flush().await.unwrap();
                self.events.send(Event::IncomingAckSent).await.unwrap();
                return;
            }
            Command::ReplayIncomingAck {
                malicious,
                previous_dialog,
            } => {
                let original = if previous_dialog {
                    self.initial_ack_history
                        .iter()
                        .rev()
                        .find(|(id, _)| id != &self.incoming_id)
                        .expect("retained previous incoming ACK")
                        .1
                        .clone()
                } else {
                    self.initial_ack
                        .clone()
                        .expect("authorized current incoming ACK")
                };
                let wire = if malicious {
                    let mut ack = parse_request(&original);
                    ack.data.body = b"v=0\r\no=- 999 999 IN IP4 10.20.30.9\r\ns=malicious-duplicate\r\nc=IN IP4 10.20.30.10\r\nt=0 0\r\nm=audio 9 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n".to_vec();
                    Message::Request(ack).encode()
                } else {
                    original
                };
                write.write_all(&wire).await.unwrap();
                write.flush().await.unwrap();
                self.events.send(Event::IncomingAckSent).await.unwrap();
                return;
            }
            Command::SendIncomingAudio => {
                self.media_control.send(()).await.unwrap();
                return;
            }
            Command::ProbeDialog => {
                assert!(self.pending_dialog_probe.is_none());
                let (probe, wire) = self.next_replay_probe();
                self.pending_dialog_probe = Some(probe);
                wire
            }
            Command::UnassociatedPrack => {
                assert!(self.pending_unassociated_prack.is_none());
                let cseq = self.renegotiation_cseq;
                self.renegotiation_cseq += 1;
                let wire = String::from_utf8(self.dialog_request("PRACK", cseq, ""))
                    .unwrap()
                    .replacen("Contact:", "RAck: 1 1 INVITE\r\nContact:", 1)
                    .into_bytes();
                self.pending_unassociated_prack = Some(parse_request(&wire));
                wire
            }
            Command::ReplayAccepted { previous_dialog } => {
                assert!(self.pending_offer.is_none() && self.pending_update.is_none());
                assert!(self.replay_barrier.is_none());
                let transactions: Vec<_> = self
                    .accepted_reinvites
                    .iter()
                    .filter(|transaction| {
                        let id = transaction
                            .response
                            .data
                            .headers
                            .get("Call-ID")
                            .unwrap()
                            .value();
                        (id != self.current_dialog_id()) == previous_dialog
                    })
                    .cloned()
                    .collect();
                assert!(
                    !transactions.is_empty(),
                    "replay needs actual accepted transactions"
                );
                for transaction in &transactions {
                    assert!(
                        transaction.accepted_at.elapsed() < Duration::from_secs(32),
                        "Accepted replay proof must occur inside Timer L"
                    );
                }
                // First drain the same TCP stream after the original ACKs.
                // Any legitimate 200 already queued before ACK is processed
                // retains the ordinary exact-response re-ACK handling.
                let (probe, wire) = self.next_replay_probe();
                self.replay_barrier = Some(ReplayBarrier {
                    probe,
                    transactions,
                    previous_dialog,
                    checking: false,
                });
                wire
            }
            Command::RemoteHangup => {
                assert!(self.pending_bye.is_none());
                let cseq = self.renegotiation_cseq;
                self.renegotiation_cseq += 1;
                let wire = self.dialog_request("BYE", cseq, "");
                self.pending_bye = Some(parse_request(&wire));
                wire
            }
            Command::HoldReinvite => {
                assert!(
                    self.held_reinvite.is_none()
                        && self.pending_offer.is_none()
                        && self.pending_update.is_none()
                );
                let cseq = self.renegotiation_cseq;
                self.renegotiation_cseq += 1;
                let wire = self.dialog_request("INVITE", cseq, "");
                self.held_reinvite = Some(HeldReinvite {
                    invite: wire.clone(),
                    response: None,
                    answer_ack: None,
                    remote: None,
                });
                wire
            }
            Command::AckHeldReinvite => {
                let held = self.held_reinvite.take().expect("held offerless200");
                let response = held.response.expect("known offerless200");
                let ack = held.answer_ack.expect("retained actual answer ACK");
                let remote = held.remote.expect("retained ACK codec");
                let invite = parse_request(&held.invite);
                let negative = self
                    .completed_invites
                    .iter()
                    .find(|negative| negative.acked && cseq(&negative.response) == cseq(&response))
                    .expect("sameCSeq negative ACK oracle");
                assert_ne!(
                    field(&parse_request(&negative.invite), "Via"),
                    field(&parse_request(&ack), "Via"),
                    "legal2xxACK uses a fresh Via, independent of negativeACK branch"
                );
                assert_ne!(field(&invite, "Via"), field(&parse_request(&ack), "Via"));
                self.remote.send_replace(Some(remote));
                self.accepted_reinvites.push(AcceptedReinvite {
                    invite: held.invite,
                    response: response.clone(),
                    ack: ack.clone(),
                    accepted_at: tokio::time::Instant::now(),
                });
                self.retain_ack(response, ack.clone());
                write.write_all(&ack).await.unwrap();
                write.flush().await.unwrap();
                self.events.send(Event::ReinviteAckSent).await.unwrap();
                return;
            }
            Command::NegativeInvite { kind } => {
                assert!(self.pending_negative.is_none());
                let cseq = if matches!(kind, NegativeKind::Collision) {
                    cseq(
                        &self
                            .held_reinvite
                            .as_ref()
                            .unwrap()
                            .response
                            .as_ref()
                            .unwrap(),
                    )
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse::<u32>()
                    .unwrap()
                } else {
                    let cseq = self.renegotiation_cseq;
                    self.renegotiation_cseq += 1;
                    cseq
                };
                let remote = self.remote.borrow().clone().unwrap();
                let body = if matches!(kind, NegativeKind::BadCodec) {
                    "v=0\r\no=- 492 571 IN IP4 10.20.30.9\r\ns=negative\r\nc=IN IP4 10.20.30.9\r\nt=0 0\r\nm=audio 4000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n".to_owned()
                } else {
                    self.sdp(remote.codec, remote.payload_type)
                };
                let wire = String::from_utf8(self.dialog_request("INVITE", cseq, &body)).unwrap();
                let (wire, expected) = match kind {
                    NegativeKind::Pending => {
                        assert!(self.held_reinvite.is_some());
                        (wire, 491)
                    }
                    NegativeKind::Collision => (
                        wire.replace(
                            &format!("branch=z9hG4bK-492-INVITE-{cseq}"),
                            &format!("branch=z9hG4bK-492-INVITE-{cseq}-negative"),
                        ),
                        491,
                    ),
                    NegativeKind::BadContact => (
                        wire.replacen("Contact:", "Contact: <sip:extra@10.20.30.9>\r\nContact:", 1),
                        400,
                    ),
                    NegativeKind::WrongContentType => (
                        wire.replace("Content-Type: application/sdp", "Content-Type: text/plain"),
                        415,
                    ),
                    NegativeKind::ShortTimer => (
                        wire.replacen(
                            "Contact:",
                            "Session-Expires: 30;refresher=uac\r\nContact:",
                            1,
                        ),
                        422,
                    ),
                    NegativeKind::BadCodec => (wire, 488),
                };
                let wire = wire.into_bytes();
                self.pending_negative = Some(PendingNegative {
                    invite: wire.clone(),
                    expected,
                });
                wire
            }
            Command::AckCompletedCollision => {
                let held = self.held_reinvite.as_ref().unwrap();
                let response = held.response.as_ref().unwrap();
                let index = self
                    .completed_invites
                    .iter()
                    .position(|negative| cseq(&negative.response) == cseq(response))
                    .unwrap();
                let negative = &mut self.completed_invites[index];
                let mut ack = parse_request(&negative.invite);
                ack.method = proletarius_sip::Method::ACK;
                ack.data.body.clear();
                let mut headers = proletarius_sip::Headers::default();
                for header in ack.data.headers.iter() {
                    if header.name().eq_ignore_ascii_case("Via") {
                        // TCP transport tokens are case-insensitive. Legal
                        // LWS around sent-by/semicolon must preserve identity,
                        // including the duplicate ACK after TCP Timer I=0.
                        let protocol = if negative.acked {
                            "SIP/2.0/TCP \t "
                        } else {
                            "SIP/2.0/tcp\t "
                        };
                        let via = header
                            .value()
                            .replacen("SIP/2.0/TCP ", protocol, 1)
                            .replacen(";branch=", " \t;branch=", 1);
                        headers.push(proletarius_sip::Header::new("Via", via).unwrap());
                    } else if !matches!(
                        header.name().to_ascii_lowercase().as_str(),
                        "cseq" | "to" | "content-type" | "content-length"
                    ) {
                        headers.push(header.clone());
                    }
                }
                headers.push(
                    proletarius_sip::Header::new(
                        "To",
                        negative.response.data.headers.get("To").unwrap().value(),
                    )
                    .unwrap(),
                );
                headers.push(
                    proletarius_sip::Header::new("CSeq", &cseq(response).replace("INVITE", "ACK"))
                        .unwrap(),
                );
                ack.data.headers = headers;
                negative.acked = true;
                write
                    .write_all(&Message::Request(ack).encode())
                    .await
                    .unwrap();
                write.flush().await.unwrap();
                self.events.send(Event::NegativeAckSent).await.unwrap();
                return;
            }
            Command::ReplayCompleted { previous_dialog } => {
                assert!(self.completed_replay.is_none());
                let transactions: Vec<_> = self
                    .completed_invites
                    .iter()
                    .filter(|negative| {
                        !negative.acked
                            && (negative
                                .response
                                .data
                                .headers
                                .get("Call-ID")
                                .unwrap()
                                .value()
                                != self.current_dialog_id())
                                == previous_dialog
                    })
                    .cloned()
                    .collect();
                assert!(!transactions.is_empty());
                for transaction in &transactions {
                    assert!(
                        transaction.received_at.elapsed() < Duration::from_secs(32),
                        "Completed replay must occur within TimerH"
                    );
                    write.write_all(&transaction.invite).await.unwrap();
                }
                let (probe, wire) = self.next_replay_probe();
                self.completed_replay = Some(CompletedReplay {
                    probe,
                    transactions: transactions.len(),
                    remaining: transactions,
                    previous_dialog,
                });
                wire
            }
            Command::Renegotiate {
                codec,
                payload,
                period,
                neighbor,
            } => {
                assert!(self.pending_offer.is_none() && self.pending_update.is_none());
                self.echo_cmr.send_replace(15);
                let cseq = self.renegotiation_cseq;
                self.renegotiation_cseq += 1;
                let mut body = self
                    .sdp(codec, payload)
                    .replace("o=- 492 1 ", &format!("o=- 492 {cseq} "));
                if matches!(codec, AudioCodec::AmrWb { .. }) {
                    assert!(matches!(period, 1 | 2));
                    body = body.replace(
                        "max-red=0",
                        &format!(
                            "mode-change-period={period};mode-change-neighbor={};max-red=0",
                            u8::from(neighbor)
                        ),
                    );
                }
                let wire = self.dialog_request("INVITE", cseq, &body);
                self.pending_offer = Some((cseq, body));
                self.pending_offer_wire = Some(wire.clone());
                wire
            }
            Command::Update { codec, payload } => {
                assert!(self.pending_offer.is_none() && self.pending_update.is_none());
                self.echo_cmr.send_replace(15);
                let cseq = self.renegotiation_cseq;
                self.renegotiation_cseq += 1;
                let body = self
                    .sdp(codec, payload)
                    .replace("o=- 492 1 ", &format!("o=- 492 {cseq} "));
                let wire = self.dialog_request("UPDATE", cseq, &body);
                self.pending_update = Some((cseq, body));
                wire
            }
            Command::AmrWbCmr(raw_cmr) => {
                assert!(raw_cmr <= 15);
                assert!(matches!(
                    self.remote.borrow().as_ref().map(|remote| remote.codec),
                    Some(AudioCodec::AmrWb { .. })
                ));
                self.echo_cmr.send_replace(raw_cmr);
                self.events.send(Event::CmrArmed(raw_cmr)).await.unwrap();
                return;
            }
        };
        write.write_all(&wire).await.unwrap();
        write.flush().await.unwrap();
    }
    pub(super) async fn observe<W: AsyncWrite + Unpin>(&mut self, message: Message, write: &mut W) {
        match message {
            Message::Request(r) => match r.method.as_str() {
                "REGISTER" => {
                    self.refresh_count += 1;
                    let mode = self
                        .refresh
                        .expect("unexpected refresh in ordinary long-lease fixture");
                    assert!(field(&r, "Security-Verify").contains("aes-cbc"));
                    let cseq = field(&r, "CSeq")
                        .split_whitespace()
                        .next()
                        .unwrap()
                        .parse::<u32>()
                        .unwrap();
                    if let Some(previous) = self.register_cseq {
                        assert!(cseq > previous);
                        assert_eq!(field(&r, "Call-ID"), self.register_id.as_deref().unwrap());
                        assert_ne!(field(&r, "Via"), self.register_branch.as_deref().unwrap());
                    }
                    self.register_cseq = Some(cseq);
                    self.register_id = Some(field(&r, "Call-ID").to_owned());
                    self.register_branch = Some(field(&r, "Via").to_owned());
                    self.events.send(Event::RefreshRequest).await.unwrap();
                    let extra = if !self.challenged
                        && matches!(
                            mode,
                            RefreshMode::Challenge401
                                | RefreshMode::Challenge407
                                | RefreshMode::ChallengeRejected
                                | RefreshMode::StopAka
                        ) {
                        self.challenged = true;
                        let (status, name) = if mode != RefreshMode::Challenge407 {
                            (401, "WWW-Authenticate")
                        } else {
                            (407, "Proxy-Authenticate")
                        };
                        let extra = format!(
                            "{name}: Digest realm=\"ims.mnc033.mcc234.3gppnetwork.org\",nonce=\"{}\",algorithm=AKAv1-MD5\r\n",
                            "01".repeat(32)
                        );
                        write
                            .write_all(&super::ims_peer::response(&r, status, &extra, ""))
                            .await
                            .unwrap();
                        // StopAka probes only after the actual card worker
                        // entered its hold; OPTIONS is not an entry signal.
                        if mode != RefreshMode::StopAka {
                            write
                                .write_all(&self.incoming_request("OPTIONS", 9, ""))
                                .await
                                .unwrap();
                        }
                        write.flush().await.unwrap();
                        return;
                    } else {
                        if mode == RefreshMode::Accepted && self.refresh_count == 1 {
                            "Expires: 4\r\n".to_owned()
                        } else {
                            "Expires: 3600\r\n".to_owned()
                        }
                    };
                    if matches!(
                        mode,
                        RefreshMode::Challenge401
                            | RefreshMode::Challenge407
                            | RefreshMode::ChallengeRejected
                    ) {
                        let name = if mode != RefreshMode::Challenge407 {
                            "Authorization"
                        } else {
                            "Proxy-Authorization"
                        };
                        let auth = field(&r, name);
                        assert!(auth.contains("integrity-protected=yes"));
                        assert!(auth.contains("response=\""));
                    }
                    match mode {
                        RefreshMode::StopPending|RefreshMode::StopAka=>return,
                        RefreshMode::Expire=>{
                            let mut wrong=r.clone();wrong.data.headers.set(proletarius_sip::Header::new("CSeq",format!("{} REGISTER",cseq+10)).unwrap());
                            write.write_all(&super::ims_peer::response(&wrong,200,"Expires: 3600\r\n","")).await.unwrap();
                            write.write_all(&super::ims_peer::response(&r,100,"","")).await.unwrap();write.flush().await.unwrap();return;
                        },
                        RefreshMode::Rejected|RefreshMode::ChallengeRejected=>write.write_all(&super::ims_peer::response(&r,403,"","")).await.unwrap(),
                        RefreshMode::NewSecurity=>write.write_all(&super::ims_peer::response(&r,401,"Security-Server: ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;spi-c=333;spi-s=444;port-c=6050;port-s=6060\r\n","")).await.unwrap(),
                        _=>{
                            let good=super::ims_peer::response(&r,200,&extra,"");
                            // A wrong-CSeq response and a provisional response cannot grant a lease.
                            let mut wrong=r.clone();wrong.data.headers.set(proletarius_sip::Header::new("CSeq",format!("{} REGISTER",cseq+10)).unwrap());
                            write.write_all(&super::ims_peer::response(&wrong,200,"Expires: 1\r\n","")).await.unwrap();
                            write.write_all(&super::ims_peer::response(&r,100,"","")).await.unwrap();
                            write.write_all(&good).await.unwrap();
                            // Duplicate late response must not revoke the newly granted lease.
                            write.write_all(&super::ims_peer::response(&r,200,"Expires: 0\r\n","")).await.unwrap();
                            self.events.send(Event::Refreshed).await.unwrap();
                        }
                    }
                    write.flush().await.unwrap();
                }
                "OPTIONS" => {
                    write.write_all(&response(&r, 200, "")).await.unwrap();
                }
                "INVITE" => {
                    assert!(
                        self.outgoing.is_none(),
                        "unexpected duplicate outgoing INVITE"
                    );
                    assert!(r.uri.to_string().contains(PEER));
                    assert_eq!(field(&r, "P-Preferred-Identity"), "<tel:+447700900123>");
                    assert!(field(&r, "Security-Verify").contains("aes-cbc"));
                    assert!(
                        r.data
                            .headers
                            .all("Route")
                            .any(|h| h.value().contains("10.20.30.9:5060"))
                    );
                    let body = std::str::from_utf8(&r.data.body).unwrap();
                    assert!(body.contains("EVS/16000"));
                    assert!(body.contains("AMR-WB/16000"));
                    assert!(!body.contains("AMR/8000"));
                    assert!(!body.contains("PCMA"));
                    let remote = sdp::negotiate_audio(body, false).unwrap();
                    assert_eq!(remote.codec, AudioCodec::Evs { header_full: false });
                    self.remote.send_replace(Some(remote));
                    write.write_all(&response(&r, 180, "")).await.unwrap();
                    self.outgoing = Some(r);
                    self.events.send(Event::Invite).await.unwrap();
                }
                "PRACK" => {
                    assert_eq!(field(&r, "RAck"), "10 1 INVITE");
                    assert_eq!(
                        field(&r, "Call-ID"),
                        field(self.outgoing.as_ref().unwrap(), "Call-ID")
                    );
                    write.write_all(&response(&r, 200, "")).await.unwrap();
                    self.events.send(Event::Prack).await.unwrap();
                }
                "ACK" => {
                    let invite = self.outgoing.as_ref().unwrap();
                    assert_eq!(field(&r, "Call-ID"), field(invite, "Call-ID"));
                    assert_eq!(
                        field(&r, "CSeq").split_whitespace().next(),
                        field(invite, "CSeq").split_whitespace().next()
                    );
                    assert!(field(&r, "To").contains("tag=carrier-492"));
                    if !self.outgoing_ack_seen {
                        self.outgoing_ack_seen = true;
                        self.events.send(Event::Ack).await.unwrap();
                    }
                }
                "BYE" => {
                    assert_eq!(
                        field(&r, "Call-ID"),
                        field(self.outgoing.as_ref().unwrap(), "Call-ID")
                    );
                    write.write_all(&response(&r, 200, "")).await.unwrap();
                    self.events.send(Event::Bye).await.unwrap();
                }
                _ => panic!("unexpected voice request {}", r.method.as_str()),
            },
            Message::Response(r) => {
                if let Some(index) = self
                    .pending_transport_probes
                    .iter()
                    .position(|probe| response_matches_request(probe, &r))
                {
                    assert_eq!(
                        r.status, 200,
                        "socket-layer OPTIONS must retain its strict200 response"
                    );
                    self.pending_transport_probes.remove(index);
                    return;
                }
                assert!(
                    !self
                        .silent_replays
                        .iter()
                        .any(|accepted| same_response_transaction(accepted, &r)),
                    "an ACKed Accepted re-INVITE must absorb a replay silently, not emit {} {}",
                    r.status,
                    r.data.headers.get("CSeq").unwrap().value(),
                );
                if self.pending_negative.as_ref().is_some_and(|pending| {
                    response_matches_request(&parse_request(&pending.invite), &r)
                }) {
                    if r.status < 200 {
                        return;
                    }
                    let pending = self.pending_negative.take().unwrap();
                    assert_eq!(r.status, pending.expected, "negative reINVITE oracle");
                    self.completed_invites.push(CompletedOracle {
                        invite: pending.invite,
                        response: r.clone(),
                        received_at: tokio::time::Instant::now(),
                        acked: false,
                    });
                    self.events
                        .send(Event::NegativeFinal(r.status))
                        .await
                        .unwrap();
                    return;
                }
                if self
                    .held_reinvite
                    .as_ref()
                    .is_some_and(|held| response_matches_request(&parse_request(&held.invite), &r))
                {
                    if r.status < 200 {
                        return;
                    }
                    assert_eq!(r.status, 200, "held offerless reINVITE must receive200");
                    if let Some(original) = &self.held_reinvite.as_ref().unwrap().response {
                        assert_eq!(
                            Message::Response(original.clone()).encode(),
                            Message::Response(r).encode(),
                            "wrong-branch negativeACK must not stop or change accepted200 TU"
                        );
                        self.events
                            .send(Event::ReinviteRetransmitted)
                            .await
                            .unwrap();
                        return;
                    }
                    let current = self.remote.borrow().clone().unwrap();
                    let offer = std::str::from_utf8(&r.data.body).unwrap();
                    let remote = sdp::negotiate_audio_offer_with_modes(
                        offer,
                        matches!(current.codec, AudioCodec::Evs { .. }),
                        &(0..=8).collect::<Vec<_>>(),
                    )
                    .unwrap();
                    assert_eq!(
                        remote.codec, current.codec,
                        "offerless exercise retains current real codec"
                    );
                    assert_eq!(remote.payload_type, current.payload_type);
                    let answer = sdp::build_local_audio_answer_to_offer_version(
                        571,
                        1,
                        "10.20.30.9".parse().unwrap(),
                        self.port,
                        offer,
                        &remote,
                        false,
                    )
                    .unwrap();
                    let sequence = cseq(&r)
                        .split_whitespace()
                        .next()
                        .unwrap()
                        .parse::<u32>()
                        .unwrap();
                    let ack = self.dialog_request("ACK", sequence, &answer);
                    let held = self.held_reinvite.as_mut().unwrap();
                    held.response = Some(r);
                    held.answer_ack = Some(ack);
                    held.remote = Some(remote);
                    self.events.send(Event::ReinvitePendingAck).await.unwrap();
                    return;
                }
                if let Some(replay) = &mut self.completed_replay {
                    if let Some(index) = replay
                        .remaining
                        .iter()
                        .position(|negative| same_response_transaction(&negative.response, &r))
                    {
                        let original = replay.remaining.remove(index);
                        assert_eq!(
                            Message::Response(original.response).encode(),
                            Message::Response(r).encode(),
                            "Completed replays retain the entire original final"
                        );
                        return;
                    }
                    if response_matches_request(&replay.probe, &r) {
                        assert!(
                            self.pending_transport_probes.is_empty(),
                            "owner INFO barrier must follow its socket OPTIONS response"
                        );
                        let replay = self.completed_replay.take().unwrap();
                        assert!(
                            replay.remaining.is_empty(),
                            "ordered owner INFO barrier passed without every frozen negative final: previous_dialog={}, total={}, probe={:?}, remaining={:?}",
                            replay.previous_dialog,
                            replay.transactions,
                            cseq(&r),
                            replay
                                .remaining
                                .iter()
                                .map(|negative| (
                                    negative.response.status,
                                    cseq(&negative.response),
                                    negative
                                        .response
                                        .data
                                        .headers
                                        .get("Call-ID")
                                        .map(|h| h.value()),
                                    negative.response.data.headers.get("Via").map(|h| h.value()),
                                    negative.acked,
                                    negative.received_at.elapsed(),
                                ))
                                .collect::<Vec<_>>()
                        );
                        assert!(matches!(r.status, 200 | 481));
                        self.events
                            .send(Event::CompletedReplayed {
                                transactions: replay.transactions,
                                previous_dialog: replay.previous_dialog,
                                status: r.status,
                            })
                            .await
                            .unwrap();
                        return;
                    }
                }
                if self
                    .replay_prack_response
                    .as_ref()
                    .is_some_and(|expected| same_response_transaction(expected, &r))
                {
                    let expected = self.replay_prack_response.take().unwrap();
                    assert_eq!(
                        Message::Response(expected).encode(),
                        Message::Response(r).encode(),
                        "duplicate PRACK must replay the frozen complete response"
                    );
                    self.events
                        .send(Event::ReliablePrackReplayed)
                        .await
                        .unwrap();
                    return;
                }
                if self
                    .pending_reliable_prack
                    .as_ref()
                    .is_some_and(|pending| response_matches_request(&pending.request, &r))
                {
                    let pending = self.pending_reliable_prack.take().unwrap();
                    assert_eq!(r.status, pending.expected, "reliable PRACK response");
                    if r.status == 200 {
                        if let Some(offer) = pending.offer {
                            let remote = sdp::negotiate_audio_answer_to_offer(
                                std::str::from_utf8(&r.data.body).unwrap(),
                                &offer,
                                true,
                                &(0..=8).collect::<Vec<_>>(),
                            )
                            .unwrap();
                            assert!(matches!(remote.codec, AudioCodec::AmrWb { .. }));
                            self.remote.send_replace(Some(remote));
                        } else {
                            assert!(
                                r.data.body.is_empty(),
                                "PRACK acknowledging an answer or supplying an answer must receive an empty200"
                            );
                        }
                        self.reliable.as_mut().unwrap().completed = true;
                        self.reliable_prack_history.push((pending.wire, r.clone()));
                    }
                    let codec = match self.remote.borrow().as_ref().unwrap().codec {
                        AudioCodec::Evs { .. } => "EVS",
                        AudioCodec::AmrWb { .. } => "AMR-WB",
                    };
                    self.events
                        .send(Event::ReliablePrackCompleted {
                            status: r.status,
                            codec,
                        })
                        .await
                        .unwrap();
                    return;
                }
                let initial = r
                    .data
                    .headers
                    .get("CSeq")
                    .is_some_and(|h| h.value() == "1 INVITE")
                    && r.data
                        .headers
                        .get("Call-ID")
                        .is_some_and(|h| h.value() == self.incoming_id);
                if initial && r.status == 180 && self.reliable.is_some() {
                    assert!(r.data.headers.get("Require").is_some_and(|h| {
                        h.value()
                            .split(',')
                            .any(|value| value.trim().eq_ignore_ascii_case("100rel"))
                    }));
                    let rseq = r
                        .data
                        .headers
                        .get("RSeq")
                        .expect("reliable180 RSeq")
                        .value()
                        .parse::<u32>()
                        .unwrap();
                    assert!((1..=0x7fff_ffff).contains(&rseq));
                    let reliable = self.reliable.as_ref().unwrap();
                    if let Some(original) = &reliable.provisional {
                        assert!(
                            !reliable.completed,
                            "reliable180 must stop after PRACK200 on the same ordered SIP connection"
                        );
                        assert_eq!(
                            Message::Response(original.clone()).encode(),
                            Message::Response(r).encode(),
                            "reliable180 retransmits the complete original bytes"
                        );
                        self.events
                            .send(Event::ReliableRetransmitted)
                            .await
                            .unwrap();
                        return;
                    }
                    let body = std::str::from_utf8(&r.data.body).unwrap();
                    let (remote, prack_answer) = if reliable.offerless {
                        let remote = sdp::negotiate_audio_offer_with_modes(
                            body,
                            true,
                            &(0..=8).collect::<Vec<_>>(),
                        )
                        .unwrap();
                        let answer = sdp::build_local_audio_answer_to_offer_version(
                            570,
                            1,
                            "10.20.30.9".parse().unwrap(),
                            self.port,
                            body,
                            &remote,
                            false,
                        )
                        .unwrap();
                        (remote, answer)
                    } else {
                        (
                            sdp::negotiate_audio_answer_to_offer(
                                body,
                                self.initial_offer.as_deref().unwrap(),
                                true,
                                &(0..=8).collect::<Vec<_>>(),
                            )
                            .unwrap(),
                            String::new(),
                        )
                    };
                    assert!(matches!(remote.codec, AudioCodec::Evs { .. }));
                    self.remote.send_replace(Some(remote));
                    self.incoming_to = Some(r.data.headers.get("To").unwrap().value().to_owned());
                    let reliable = self.reliable.as_mut().unwrap();
                    reliable.provisional = Some(r);
                    reliable.rseq = rseq;
                    reliable.prack_answer = prack_answer;
                    self.events
                        .send(Event::ReliableProvisional { codec: "EVS", rseq })
                        .await
                        .unwrap();
                    return;
                }
                if initial
                    && r.status == 200
                    && self.reliable.is_some()
                    && !self.initial_answer_seen
                {
                    let reliable = self.reliable.as_ref().unwrap();
                    assert!(
                        reliable.completed,
                        "user answer must not send final200 before the reliable SDP exchange is acknowledged"
                    );
                    assert!(
                        r.data.body.is_empty(),
                        "reliable O/A completion makes final200 empty"
                    );
                    assert_eq!(
                        r.data.headers.get("To").unwrap().value(),
                        reliable
                            .provisional
                            .as_ref()
                            .unwrap()
                            .data
                            .headers
                            .get("To")
                            .unwrap()
                            .value()
                    );
                    self.initial_answer_seen = true;
                    let ack = self.incoming_request("ACK", 1, "");
                    self.held_initial_answer = Some((r, ack));
                    self.events
                        .send(Event::ReliableFinalPendingAck)
                        .await
                        .unwrap();
                    return;
                }
                if self
                    .pending_unassociated_prack
                    .as_ref()
                    .is_some_and(|request| response_matches_request(request, &r))
                {
                    assert_eq!(
                        r.status, 481,
                        "PRACK without a reliable provisional association must fail closed"
                    );
                    self.pending_unassociated_prack = None;
                    self.events
                        .send(Event::UnassociatedPrackRejected)
                        .await
                        .unwrap();
                    return;
                }
                if self
                    .pending_dialog_probe
                    .as_ref()
                    .is_some_and(|probe| response_matches_request(probe, &r))
                {
                    assert!(
                        self.pending_transport_probes.is_empty(),
                        "owner INFO barrier must follow its socket OPTIONS response"
                    );
                    assert!(matches!(r.status, 200 | 481));
                    self.pending_dialog_probe = None;
                    self.events
                        .send(Event::DialogProbed(r.status))
                        .await
                        .unwrap();
                    return;
                }
                if self
                    .pending_bye
                    .as_ref()
                    .is_some_and(|bye| response_matches_request(bye, &r))
                {
                    assert_eq!(r.status, 200);
                    self.pending_bye = None;
                    self.events.send(Event::RemoteByeAccepted).await.unwrap();
                    return;
                }
                if self
                    .replay_barrier
                    .as_ref()
                    .is_some_and(|barrier| response_matches_request(&barrier.probe, &r))
                {
                    assert!(
                        self.pending_transport_probes.is_empty(),
                        "owner INFO barrier must follow its socket OPTIONS response"
                    );
                    let mut barrier = self.replay_barrier.take().unwrap();
                    assert!(
                        matches!(r.status, 200 | 481),
                        "unexpected processing barrier response"
                    );
                    if barrier.checking {
                        self.events
                            .send(Event::AcceptedReplayChecked {
                                transactions: barrier.transactions.len(),
                                previous_dialog: barrier.previous_dialog,
                                status: r.status,
                            })
                            .await
                            .unwrap();
                    } else {
                        for transaction in &barrier.transactions {
                            assert!(transaction.accepted_at.elapsed() < Duration::from_secs(32));
                            if !self.silent_replays.iter().any(|accepted| {
                                same_response_transaction(accepted, &transaction.response)
                            }) {
                                self.silent_replays.push(transaction.response.clone());
                            }
                            write.write_all(&transaction.invite).await.unwrap();
                            write.write_all(&transaction.ack).await.unwrap();
                        }
                        let (probe, wire) = self.next_replay_probe();
                        barrier.probe = probe;
                        barrier.checking = true;
                        self.replay_barrier = Some(barrier);
                        write.write_all(&wire).await.unwrap();
                        write.flush().await.unwrap();
                    }
                    return;
                }
                if let Some((held, _)) = &self.held_initial_answer {
                    if r.data
                        .headers
                        .get("Call-ID")
                        .is_some_and(|h| h.value() == self.incoming_id)
                        && r.data
                            .headers
                            .get("CSeq")
                            .is_some_and(|h| h.value() == "1 INVITE")
                        && r.status >= 200
                    {
                        assert!(
                            same_final_response(held, &r),
                            "initial200 retransmission must retain its exact dialog/SDP"
                        );
                        assert_eq!(
                            Message::Response(held.clone()).encode(),
                            Message::Response(r.clone()).encode(),
                            "TU must retransmit the complete initial200 unchanged"
                        );
                        self.events.send(Event::AnswerRetransmitted).await.unwrap();
                        return;
                    }
                }
                if let Some(ack) = self.retained_ack(&r) {
                    // SIP 2xx retransmissions continue over reliable transports
                    // until ACK is processed. Re-ACK without admitting SDP,
                    // resetting the decoder or emitting another lifecycle event.
                    write.write_all(ack).await.unwrap();
                    write.flush().await.unwrap();
                    return;
                }
                if self
                    .update_finals
                    .iter()
                    .any(|accepted| same_final_response(accepted, &r))
                {
                    // UPDATE has no ACK. Ignore only an exact retained final,
                    // without touching a later exchange or repeating events.
                    return;
                }
                let cseq = r.data.headers.get("CSeq").unwrap().value();
                if let Some((pending_cseq, offer)) = &self.pending_update {
                    if cseq == format!("{pending_cseq} UPDATE") {
                        let expected_id = if self.active_outgoing {
                            field(self.outgoing.as_ref().unwrap(), "Call-ID")
                        } else {
                            &self.incoming_id
                        };
                        assert_eq!(r.data.headers.get("Call-ID").unwrap().value(), expected_id);
                        if r.status < 200 {
                            return;
                        }
                        if r.status == 200 {
                            let remote = sdp::negotiate_audio_answer_to_offer(
                                std::str::from_utf8(&r.data.body).unwrap(),
                                offer,
                                true,
                                &(0..=8).collect::<Vec<_>>(),
                            )
                            .unwrap();
                            self.remote.send_replace(Some(remote));
                        }
                        self.pending_update = None;
                        if self.update_finals.len() == 32 {
                            self.update_finals.remove(0);
                        }
                        self.update_finals.push(r.clone());
                        self.events
                            .send(if r.status == 200 {
                                Event::Updated
                            } else {
                                Event::UpdateRejected(r.status)
                            })
                            .await
                            .unwrap();
                        // Media selected by this 200 is usable immediately;
                        // sending an ACK here would hide an owner timing bug.
                        return;
                    }
                }
                if let Some((pending_cseq, offer)) = &self.pending_offer {
                    if cseq == format!("{pending_cseq} INVITE") {
                        let expected_id = if self.active_outgoing {
                            field(self.outgoing.as_ref().unwrap(), "Call-ID")
                        } else {
                            &self.incoming_id
                        };
                        assert_eq!(r.data.headers.get("Call-ID").unwrap().value(), expected_id);
                        if r.status < 200 {
                            return;
                        }
                        let pending_cseq = *pending_cseq;
                        if r.status == 200 {
                            let remote = sdp::negotiate_audio_answer_to_offer(
                                std::str::from_utf8(&r.data.body).unwrap(),
                                offer,
                                true,
                                &(0..=8).collect::<Vec<_>>(),
                            )
                            .unwrap();
                            self.remote.send_replace(Some(remote));
                        }
                        let mut ack = self.dialog_request("ACK", pending_cseq, "");
                        if r.status != 200 {
                            ack = String::from_utf8(ack)
                                .unwrap()
                                .replace(
                                    &format!("branch=z9hG4bK-492-ACK-{pending_cseq}"),
                                    &format!("branch=z9hG4bK-492-INVITE-{pending_cseq}"),
                                )
                                .into_bytes();
                        }
                        write.write_all(&ack).await.unwrap();
                        write.flush().await.unwrap();
                        self.pending_offer = None;
                        let invite = self
                            .pending_offer_wire
                            .take()
                            .expect("retained original re-INVITE bytes");
                        if r.status == 200 {
                            assert!(
                                self.accepted_reinvites.len() < 32,
                                "fixture Accepted ledger capacity"
                            );
                            self.accepted_reinvites.push(AcceptedReinvite {
                                invite,
                                response: r.clone(),
                                ack: ack.clone(),
                                accepted_at: tokio::time::Instant::now(),
                            });
                        }
                        self.retain_ack(r.clone(), ack);
                        self.events
                            .send(if r.status == 200 {
                                Event::Renegotiated
                            } else {
                                Event::RenegotiationRejected(r.status)
                            })
                            .await
                            .unwrap();
                        return;
                    }
                }
                assert_eq!(
                    r.data.headers.get("Call-ID").unwrap().value(),
                    self.incoming_id
                );
                match (r.status, cseq) {
                    (200, "9 OPTIONS") => {
                        self.events.send(Event::ProbeDuringAka).await.unwrap();
                    }
                    (100, "1 INVITE") => {
                        if self.offerless_prefer_amr.is_some() {
                            self.events.send(Event::OfferlessTrying).await.unwrap();
                        }
                    }
                    (180, "1 INVITE") => {
                        self.events.send(Event::Ringing).await.unwrap();
                    }
                    (200, "1 INVITE") => {
                        assert!(
                            !self.initial_answer_seen,
                            "changed initial200 cannot be admitted as another answer"
                        );
                        self.initial_answer_seen = true;
                        let (remote, ack_body) = if let Some(amr_wb) = self.offerless_prefer_amr {
                            let offer = std::str::from_utf8(&r.data.body).unwrap();
                            let remote = sdp::negotiate_audio_offer_with_modes(
                                offer,
                                !amr_wb,
                                &(0..=8).collect::<Vec<_>>(),
                            )
                            .unwrap();
                            assert_eq!(matches!(remote.codec, AudioCodec::AmrWb { .. }), amr_wb);
                            let answer = sdp::build_local_audio_answer_to_offer_version(
                                569,
                                1,
                                "10.20.30.9".parse().unwrap(),
                                self.port,
                                offer,
                                &remote,
                                false,
                            )
                            .unwrap();
                            let confirmed = sdp::negotiate_audio_answer_to_offer(
                                &answer,
                                offer,
                                true,
                                &(0..=8).collect::<Vec<_>>(),
                            )
                            .unwrap();
                            assert_eq!(confirmed.codec, remote.codec);
                            assert_eq!(confirmed.payload_type, remote.payload_type);
                            (remote, answer)
                        } else {
                            let remote = sdp::negotiate_audio_answer_to_offer(
                                std::str::from_utf8(&r.data.body).unwrap(),
                                self.initial_offer
                                    .as_deref()
                                    .expect("retained carrier initial offer"),
                                true,
                                &(0..=8).collect::<Vec<_>>(),
                            )
                            .unwrap();
                            assert_eq!(
                                remote.codec,
                                AudioCodec::AmrWb {
                                    octet_aligned: true,
                                    mode: 8,
                                    policy: sdp::AmrWbModePolicy::new(
                                        (1 << 2) | (1 << 8),
                                        1,
                                        false
                                    )
                                    .unwrap(),
                                }
                            );
                            (remote, String::new())
                        };
                        let payload = remote.payload_type;
                        let codec = match remote.codec {
                            AudioCodec::Evs { .. } => "EVS",
                            AudioCodec::AmrWb { .. } => "AMR-WB",
                        };
                        self.remote.send_replace(Some(remote));
                        self.incoming_to =
                            Some(r.data.headers.get("To").unwrap().value().to_owned());
                        let ack = self.incoming_request("ACK", 1, &ack_body);
                        if self.defer_initial_ack {
                            self.held_initial_answer = Some((r.clone(), ack));
                            self.events
                                .send(if self.offerless_prefer_amr.is_some() {
                                    Event::OfferlessAnswerPendingAck { codec, payload }
                                } else {
                                    Event::AnswerPendingAck
                                })
                                .await
                                .unwrap();
                        } else {
                            write.write_all(&ack).await.unwrap();
                            self.initial_ack = Some(ack.clone());
                            self.initial_ack_history
                                .push((self.incoming_id.clone(), ack.clone()));
                            self.retain_ack(r.clone(), ack);
                            self.events.send(Event::Answer).await.unwrap();
                        }
                    }
                    _ => panic!("unexpected incoming response {} {cseq}", r.status),
                }
            }
        }
        write.flush().await.unwrap();
    }
    pub(super) async fn finish(self) {
        self.media.unwrap().await.unwrap();
    }
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    use proletarius_sip::{Header, Headers, MessageData};
    use tokio::io::AsyncReadExt;

    // The socket actor can reply to OPTIONS before calls processes earlier
    // business events. Only the following INFO response completes a barrier.
    async fn observe_socket_probe(
        server: &mut Server,
        client: &mut Client,
        writer: &mut tokio::io::DuplexStream,
        wire: &[u8],
    ) -> Request {
        let (Message::Request(options), consumed) = proletarius_sip::Parser::default()
            .parse(wire, true)
            .unwrap()
        else {
            panic!("OPTIONS probe")
        };
        assert_eq!(options.method, proletarius_sip::Method::OPTIONS);
        let info = parse_request(&wire[consumed..]);
        assert_eq!(info.method, proletarius_sip::Method::INFO);
        assert!(info.data.body.is_empty());
        let sequence = |request: &Request| {
            field(request, "CSeq")
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u32>()
                .unwrap()
        };
        assert!(sequence(&info) > sequence(&options));
        for name in ["Call-ID", "From", "To"] {
            assert_eq!(field(&info, name), field(&options, name));
        }
        let (transport_response, _) = proletarius_sip::Parser::default()
            .parse(&response(&options, 200, ""), true)
            .unwrap();
        server.observe(transport_response, writer).await;
        assert!(server.pending_transport_probes.is_empty());
        assert!(
            matches!(
                client.events.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "OPTIONS alone cannot report a calls-owner processing barrier"
        );
        info
    }

    #[tokio::test]
    async fn offerless_initial200_holds_a_real_answer_and_old_ack_keeps_original_dialog() {
        for amr_wb in [false, true] {
            let (mut server, mut client) = new();
            server.port = 4000;
            let (mut writer, mut reader) = tokio::io::duplex(16384);
            let mut bytes = [0u8; 16384];
            let offer = sdp::build_local_sdp(
                569,
                "10.20.30.1".parse().unwrap(),
                5000,
                false,
                None,
                None,
                None,
                false,
            )
            .unwrap();
            let mut original_ack = None;
            let mut first_id = None;
            for call in 0..2 {
                server
                    .command(Command::IncomingOfferless { amr_wb }, &mut writer)
                    .await;
                let count = reader.read(&mut bytes).await.unwrap();
                let invite = parse_request(&bytes[..count]);
                assert!(invite.data.body.is_empty());
                if let Some(first) = &first_id {
                    assert_ne!(field(&invite, "Call-ID"), first);
                } else {
                    first_id = Some(field(&invite, "Call-ID").to_owned());
                }
                let (answer, _) = proletarius_sip::Parser::default()
                    .parse(&response(&invite, 200, &offer), true)
                    .unwrap();
                let Message::Response(answer) = answer else {
                    panic!("initial200 fixture")
                };
                server
                    .observe(Message::Response(answer.clone()), &mut writer)
                    .await;
                let Event::OfferlessAnswerPendingAck { codec, payload } = client.next().await
                else {
                    panic!("offerless answer event")
                };
                assert_eq!(codec, if amr_wb { "AMR-WB" } else { "EVS" });
                let negotiated = server.remote.borrow().clone().unwrap();
                assert_eq!(payload, negotiated.payload_type);
                assert!(server.initial_ack.is_none());
                assert!(
                    tokio::time::timeout(Duration::from_millis(10), reader.read(&mut bytes))
                        .await
                        .is_err()
                );
                server.observe(Message::Response(answer), &mut writer).await;
                assert_eq!(client.next().await, Event::AnswerRetransmitted);
                assert_eq!(server.remote.borrow().as_ref(), Some(&negotiated));
                assert!(
                    tokio::time::timeout(Duration::from_millis(10), reader.read(&mut bytes))
                        .await
                        .is_err()
                );
                server
                    .command(Command::UnassociatedPrack, &mut writer)
                    .await;
                let count = reader.read(&mut bytes).await.unwrap();
                let prack = parse_request(&bytes[..count]);
                assert_eq!(field(&prack, "Call-ID"), field(&invite, "Call-ID"));
                assert_eq!(field(&prack, "RAck"), "1 1 INVITE");
                let (rejected, _) = proletarius_sip::Parser::default()
                    .parse(&response(&prack, 481, ""), true)
                    .unwrap();
                server.observe(rejected, &mut writer).await;
                assert_eq!(client.next().await, Event::UnassociatedPrackRejected);
                assert!(server.held_initial_answer.is_some());
                server.command(Command::AckIncoming, &mut writer).await;
                assert_eq!(client.next().await, Event::IncomingAckSent);
                let count = reader.read(&mut bytes).await.unwrap();
                let ack = parse_request(&bytes[..count]);
                assert_eq!(field(&ack, "Call-ID"), field(&invite, "Call-ID"));
                let selected = sdp::negotiate_audio_answer_to_offer(
                    std::str::from_utf8(&ack.data.body).unwrap(),
                    &offer,
                    true,
                    &(0..=8).collect::<Vec<_>>(),
                )
                .unwrap();
                assert_eq!(selected.codec, negotiated.codec);
                assert_eq!(selected.remote_port, 4000);
                if call == 0 {
                    original_ack = Some(bytes[..count].to_vec());
                }
                server
                    .command(
                        Command::ReplayIncomingAck {
                            malicious: true,
                            previous_dialog: false,
                        },
                        &mut writer,
                    )
                    .await;
                assert_eq!(client.next().await, Event::IncomingAckSent);
                let count = reader.read(&mut bytes).await.unwrap();
                let malicious = parse_request(&bytes[..count]);
                assert_eq!(field(&malicious, "Call-ID"), field(&ack, "Call-ID"));
                assert_ne!(malicious.data.body, ack.data.body);
                assert_eq!(server.remote.borrow().as_ref(), Some(&negotiated));
            }
            let current = server.remote.borrow().clone();
            server
                .command(
                    Command::ReplayIncomingAck {
                        malicious: false,
                        previous_dialog: true,
                    },
                    &mut writer,
                )
                .await;
            assert_eq!(client.next().await, Event::IncomingAckSent);
            let count = reader.read(&mut bytes).await.unwrap();
            assert_eq!(&bytes[..count], original_ack.unwrap().as_slice());
            assert_eq!(server.remote.borrow().as_ref(), current.as_ref());
            assert_eq!(server.initial_ack_history.len(), 2);
        }
    }

    #[tokio::test]
    async fn reliable_oracle_retains_each_offer_answer_and_never_authorizes_final_ack() {
        for (offerless, prack_offer_wb) in [(false, false), (false, true), (true, false)] {
            let (mut server, mut client) = new();
            server.port = 4000;
            let (mut writer, mut reader) = tokio::io::duplex(16384);
            let mut bytes = [0u8; 8192];
            server
                .command(
                    Command::IncomingReliable {
                        offerless,
                        prack_offer_wb,
                    },
                    &mut writer,
                )
                .await;
            let count = reader.read(&mut bytes).await.unwrap();
            let invite = parse_request(&bytes[..count]);
            assert_eq!(field(&invite, "Require"), "100rel");
            assert_eq!(invite.data.body.is_empty(), offerless);
            let body = if offerless {
                server.sdp(AudioCodec::Evs { header_full: false }, 96)
            } else {
                let offer = std::str::from_utf8(&invite.data.body).unwrap();
                let remote = sdp::negotiate_audio_offer_with_modes(
                    offer,
                    true,
                    &(0..=8).collect::<Vec<_>>(),
                )
                .unwrap();
                sdp::build_local_audio_answer_to_offer_version(
                    570,
                    1,
                    "10.20.30.9".parse().unwrap(),
                    server.port,
                    offer,
                    &remote,
                    false,
                )
                .unwrap()
            };
            let (Message::Response(mut ringing), _) = proletarius_sip::Parser::default()
                .parse(&response(&invite, 180, &body), true)
                .unwrap()
            else {
                panic!("180")
            };
            ringing
                .data
                .headers
                .push(Header::new("Require", "100rel").unwrap());
            ringing
                .data
                .headers
                .push(Header::new("RSeq", "71").unwrap());
            server
                .observe(Message::Response(ringing.clone()), &mut writer)
                .await;
            assert_eq!(
                client.next().await,
                Event::ReliableProvisional {
                    codec: "EVS",
                    rseq: 71
                }
            );
            server
                .observe(Message::Response(ringing), &mut writer)
                .await;
            assert_eq!(client.next().await, Event::ReliableRetransmitted);
            assert!(server.initial_ack.is_none() && server.held_initial_answer.is_none());
            assert!(
                tokio::time::timeout(Duration::from_millis(10), reader.read(&mut bytes))
                    .await
                    .is_err(),
                "180 never auto-generates PRACK or ACK"
            );
            for (rack, status) in [
                (RackCase::Malformed, 400),
                (RackCase::WrongRseq, 481),
                (RackCase::Valid, 200),
            ] {
                server
                    .command(Command::ReliablePrack { rack }, &mut writer)
                    .await;
                let count = reader.read(&mut bytes).await.unwrap();
                let prack = parse_request(&bytes[..count]);
                assert_eq!(prack.data.body.is_empty(), !offerless && !prack_offer_wb);
                let answer = if status == 200 && prack_offer_wb {
                    let offer = std::str::from_utf8(&prack.data.body).unwrap();
                    let remote = sdp::negotiate_audio_offer_with_modes(
                        offer,
                        true,
                        &(0..=8).collect::<Vec<_>>(),
                    )
                    .unwrap();
                    sdp::build_local_audio_answer_to_offer_version(
                        570,
                        2,
                        "10.20.30.9".parse().unwrap(),
                        server.port,
                        offer,
                        &remote,
                        false,
                    )
                    .unwrap()
                } else {
                    String::new()
                };
                let (Message::Response(reply), _) = proletarius_sip::Parser::default()
                    .parse(&response(&prack, status, &answer), true)
                    .unwrap()
                else {
                    panic!("PRACK response")
                };
                server
                    .observe(Message::Response(reply.clone()), &mut writer)
                    .await;
                let codec = if status == 200 && prack_offer_wb {
                    "AMR-WB"
                } else {
                    "EVS"
                };
                assert_eq!(
                    client.next().await,
                    Event::ReliablePrackCompleted { status, codec }
                );
                assert_eq!(server.reliable.as_ref().unwrap().completed, status == 200);
                if status == 200 {
                    let remote = server.remote.borrow().clone();
                    server
                        .command(
                            Command::ReplayReliablePrack {
                                previous_dialog: false,
                                malicious: true,
                            },
                            &mut writer,
                        )
                        .await;
                    let count = reader.read(&mut bytes).await.unwrap();
                    assert_ne!(parse_request(&bytes[..count]).data.body, prack.data.body);
                    server.observe(Message::Response(reply), &mut writer).await;
                    assert_eq!(client.next().await, Event::ReliablePrackReplayed);
                    assert_eq!(server.remote.borrow().as_ref(), remote.as_ref());
                }
            }
            let (Message::Response(final_response), _) = proletarius_sip::Parser::default()
                .parse(&response(&invite, 200, ""), true)
                .unwrap()
            else {
                panic!("final200")
            };
            server
                .observe(Message::Response(final_response.clone()), &mut writer)
                .await;
            assert_eq!(client.next().await, Event::ReliableFinalPendingAck);
            server
                .observe(Message::Response(final_response), &mut writer)
                .await;
            assert_eq!(client.next().await, Event::AnswerRetransmitted);
            assert!(
                tokio::time::timeout(Duration::from_millis(10), reader.read(&mut bytes))
                    .await
                    .is_err(),
                "final200 retry cannot authorize ACK"
            );
            server.command(Command::AckIncoming, &mut writer).await;
            assert_eq!(client.next().await, Event::IncomingAckSent);
            let count = reader.read(&mut bytes).await.unwrap();
            let ack = parse_request(&bytes[..count]);
            assert_eq!(ack.method.as_str(), "ACK");
            assert!(ack.data.body.is_empty());
        }
    }

    #[tokio::test]
    async fn completed_oracle_keeps_frozen_negatives_and_distinct_ack_branches() {
        let (mut server, mut client) = new();
        server.port = 4000;
        server.incoming_to = Some("<sip:local@example.invalid>;tag=local".into());
        let codec = AudioCodec::Evs { header_full: false };
        let body = server.sdp(codec, 96);
        let negotiated =
            sdp::negotiate_audio_offer_with_modes(&body, true, &(0..=8).collect::<Vec<_>>())
                .unwrap();
        server.remote.send_replace(Some(negotiated.clone()));
        let (mut writer, mut reader) = tokio::io::duplex(32768);
        let mut bytes = [0u8; 32768];
        server.command(Command::HoldReinvite, &mut writer).await;
        let count = reader.read(&mut bytes).await.unwrap();
        let held_invite = parse_request(&bytes[..count]);
        assert!(held_invite.data.body.is_empty());
        let (held_response, _) = proletarius_sip::Parser::default()
            .parse(&response(&held_invite, 200, &body), true)
            .unwrap();
        server.observe(held_response.clone(), &mut writer).await;
        assert_eq!(client.next().await, Event::ReinvitePendingAck);
        for kind in [NegativeKind::Pending, NegativeKind::Collision] {
            server
                .command(Command::NegativeInvite { kind }, &mut writer)
                .await;
            let count = reader.read(&mut bytes).await.unwrap();
            let negative = parse_request(&bytes[..count]);
            if matches!(kind, NegativeKind::Collision) {
                assert_eq!(field(&negative, "CSeq"), field(&held_invite, "CSeq"));
                assert_ne!(field(&negative, "Via"), field(&held_invite, "Via"));
            }
            let (rejected, _) = proletarius_sip::Parser::default()
                .parse(&response(&negative, 491, ""), true)
                .unwrap();
            server.observe(rejected, &mut writer).await;
            assert_eq!(client.next().await, Event::NegativeFinal(491));
            assert!(
                tokio::time::timeout(Duration::from_millis(10), reader.read(&mut bytes))
                    .await
                    .is_err(),
                "negative oracle must not auto-ACK away Completed"
            );
        }
        server
            .command(Command::AckCompletedCollision, &mut writer)
            .await;
        assert_eq!(client.next().await, Event::NegativeAckSent);
        let count = reader.read(&mut bytes).await.unwrap();
        let negative_ack = parse_request(&bytes[..count]);
        assert!(negative_ack.data.body.is_empty());
        let original_negative = parse_request(&server.completed_invites[1].invite);
        assert!(
            field(&negative_ack, "Via").starts_with("SIP/2.0/tcp\t 10.20.30.9:6060 \t;branch=")
        );
        assert_eq!(
            field(&negative_ack, "Via")
                .split_once(";branch=")
                .unwrap()
                .1,
            field(&original_negative, "Via")
                .split_once(";branch=")
                .unwrap()
                .1
        );
        assert_ne!(
            field(&negative_ack, "Via"),
            field(&original_negative, "Via")
        );
        server
            .command(Command::AckCompletedCollision, &mut writer)
            .await;
        assert_eq!(client.next().await, Event::NegativeAckSent);
        let count = reader.read(&mut bytes).await.unwrap();
        let repeated_negative_ack = parse_request(&bytes[..count]);
        assert!(
            field(&repeated_negative_ack, "Via")
                .starts_with("SIP/2.0/TCP \t 10.20.30.9:6060 \t;branch=")
        );
        assert_eq!(
            field(&repeated_negative_ack, "Via")
                .split_once(";branch=")
                .unwrap()
                .1,
            field(&negative_ack, "Via")
                .split_once(";branch=")
                .unwrap()
                .1
        );
        assert_ne!(
            field(&repeated_negative_ack, "Via"),
            field(&negative_ack, "Via")
        );
        assert_eq!(
            field(&repeated_negative_ack, "CSeq"),
            field(&negative_ack, "CSeq")
        );
        assert!(repeated_negative_ack.data.body.is_empty());
        server.observe(held_response, &mut writer).await;
        assert_eq!(client.next().await, Event::ReinviteRetransmitted);
        assert!(server.held_reinvite.is_some());
        server.command(Command::AckHeldReinvite, &mut writer).await;
        assert_eq!(client.next().await, Event::ReinviteAckSent);
        let count = reader.read(&mut bytes).await.unwrap();
        let positive_ack = parse_request(&bytes[..count]);
        assert!(!positive_ack.data.body.is_empty());
        assert_ne!(field(&positive_ack, "Via"), field(&negative_ack, "Via"));
        assert_ne!(field(&positive_ack, "Via"), field(&held_invite, "Via"));
        for (kind, status) in [
            (NegativeKind::BadContact, 400),
            (NegativeKind::WrongContentType, 415),
            (NegativeKind::ShortTimer, 422),
            (NegativeKind::BadCodec, 488),
        ] {
            server
                .command(Command::NegativeInvite { kind }, &mut writer)
                .await;
            let count = reader.read(&mut bytes).await.unwrap();
            let request = parse_request(&bytes[..count]);
            match kind {
                NegativeKind::BadContact => {
                    assert_eq!(request.data.headers.all("Contact").count(), 2)
                }
                NegativeKind::WrongContentType => {
                    assert_eq!(field(&request, "Content-Type"), "text/plain")
                }
                NegativeKind::ShortTimer => {
                    assert_eq!(field(&request, "Session-Expires"), "30;refresher=uac")
                }
                NegativeKind::BadCodec => assert!(
                    std::str::from_utf8(&request.data.body)
                        .unwrap()
                        .contains("PCMU/8000")
                ),
                _ => unreachable!(),
            }
            let (rejected, _) = proletarius_sip::Parser::default()
                .parse(&response(&request, status, ""), true)
                .unwrap();
            server.observe(rejected, &mut writer).await;
            assert_eq!(client.next().await, Event::NegativeFinal(status));
        }
        server
            .command(
                Command::ReplayCompleted {
                    previous_dialog: false,
                },
                &mut writer,
            )
            .await;
        let count = reader.read(&mut bytes).await.unwrap();
        let expected: Vec<_> = server
            .completed_invites
            .iter()
            .filter(|negative| !negative.acked)
            .cloned()
            .collect();
        assert_eq!(expected.len(), 5);
        let mut offset = 0;
        for negative in &expected {
            assert_eq!(
                &bytes[offset..offset + negative.invite.len()],
                &negative.invite
            );
            offset += negative.invite.len();
        }
        let probe =
            observe_socket_probe(&mut server, &mut client, &mut writer, &bytes[offset..count])
                .await;
        assert_eq!(
            server.completed_replay.as_ref().unwrap().remaining.len(),
            5,
            "socket OPTIONS may overtake every pending calls-owner final"
        );
        for negative in &expected {
            server
                .observe(Message::Response(negative.response.clone()), &mut writer)
                .await;
        }
        let (barrier, _) = proletarius_sip::Parser::default()
            .parse(&response(&probe, 200, ""), true)
            .unwrap();
        server.observe(barrier, &mut writer).await;
        assert_eq!(
            client.next().await,
            Event::CompletedReplayed {
                transactions: 5,
                previous_dialog: false,
                status: 200
            }
        );
        assert_eq!(server.remote.borrow().as_ref(), Some(&negotiated));
    }

    fn accepted() -> Response {
        let mut headers = Headers::default();
        for (name, value) in [
            ("Via", "SIP/2.0/TCP 10.20.30.9:6060;branch=z9hG4bK-original"),
            ("From", "<sip:peer@example.invalid>;tag=carrier"),
            ("To", "<sip:local@example.invalid>;tag=local"),
            ("Call-ID", "retained-dialog"),
            ("CSeq", "19 INVITE"),
        ] {
            headers.push(Header::new(name, value).unwrap());
        }
        Response {
            status: 200,
            reason: "OK".into(),
            data: MessageData::new("SIP/2.0", headers, b"retained SDP".to_vec()),
        }
    }

    #[tokio::test]
    async fn accepted_replay_retains_wire_and_requires_two_ordered_processing_barriers() {
        let (mut server, mut client) = new();
        server.port = 4000;
        server.incoming_to = Some("<sip:local@example.invalid>;tag=local".into());
        let (mut writer, mut reader) = tokio::io::duplex(16384);
        let codec = AudioCodec::AmrWb {
            octet_aligned: true,
            mode: 8,
            policy: sdp::AmrWbModePolicy::single(8).unwrap(),
        };
        server
            .command(
                Command::Renegotiate {
                    codec,
                    payload: 98,
                    period: 1,
                    neighbor: false,
                },
                &mut writer,
            )
            .await;
        let mut bytes = [0u8; 16384];
        let count = reader.read(&mut bytes).await.unwrap();
        let original_invite = bytes[..count].to_vec();
        let invite = parse_request(&original_invite);
        let (answer, _) = proletarius_sip::Parser::default()
            .parse(&response(&invite, 200, &server.sdp(codec, 98)), true)
            .unwrap();
        server.observe(answer, &mut writer).await;
        assert_eq!(client.next().await, Event::Renegotiated);
        let count = reader.read(&mut bytes).await.unwrap();
        let original_ack = bytes[..count].to_vec();
        let negotiated = server.remote.borrow().clone().unwrap();
        assert_eq!(server.accepted_reinvites.len(), 1);
        assert_eq!(server.accepted_reinvites[0].invite, original_invite);
        assert_eq!(server.accepted_reinvites[0].ack, original_ack);

        server
            .command(
                Command::ReplayAccepted {
                    previous_dialog: false,
                },
                &mut writer,
            )
            .await;
        let count = reader.read(&mut bytes).await.unwrap();
        let drain_probe =
            observe_socket_probe(&mut server, &mut client, &mut writer, &bytes[..count]).await;
        assert_eq!(drain_probe.method, proletarius_sip::Method::INFO);
        assert!(
            server.silent_replays.is_empty(),
            "must first drain in-flight final responses"
        );
        assert!(!server.replay_barrier.as_ref().unwrap().checking);
        let (drained, _) = proletarius_sip::Parser::default()
            .parse(&response(&drain_probe, 200, ""), true)
            .unwrap();
        server.observe(drained, &mut writer).await;
        let count = reader.read(&mut bytes).await.unwrap();
        let replay = &bytes[..count];
        assert!(replay.starts_with(&original_invite));
        let after_invite = &replay[original_invite.len()..];
        assert!(after_invite.starts_with(&original_ack));
        let check_probe = observe_socket_probe(
            &mut server,
            &mut client,
            &mut writer,
            &after_invite[original_ack.len()..],
        )
        .await;
        assert_eq!(check_probe.method, proletarius_sip::Method::INFO);
        assert_ne!(field(&check_probe, "CSeq"), field(&drain_probe, "CSeq"));
        assert!(server.replay_barrier.as_ref().unwrap().checking);
        assert_eq!(server.silent_replays.len(), 1);
        assert!(matches!(
            client.events.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let (checked, _) = proletarius_sip::Parser::default()
            .parse(&response(&check_probe, 200, ""), true)
            .unwrap();
        server.observe(checked, &mut writer).await;
        assert_eq!(
            client.next().await,
            Event::AcceptedReplayChecked {
                transactions: 1,
                previous_dialog: false,
                status: 200,
            }
        );
        assert!(server.replay_barrier.is_none());
        assert_eq!(server.remote.borrow().as_ref(), Some(&negotiated));
        assert_eq!(server.final_acks.len(), 1);
        // BYE and the post-BYE probe share the same monotonic sequence space;
        // a fixed high CSeq must not make later probes or offers look stale.
        server.command(Command::RemoteHangup, &mut writer).await;
        let count = reader.read(&mut bytes).await.unwrap();
        let bye = parse_request(&bytes[..count]);
        let sequence = |request: &Request| {
            field(request, "CSeq")
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u32>()
                .unwrap()
        };
        assert_eq!(bye.method, proletarius_sip::Method::BYE);
        assert!(sequence(&bye) > sequence(&check_probe));
        let (bye_response, _) = proletarius_sip::Parser::default()
            .parse(&response(&bye, 200, ""), true)
            .unwrap();
        server.observe(bye_response, &mut writer).await;
        assert_eq!(client.next().await, Event::RemoteByeAccepted);
        server.command(Command::ProbeDialog, &mut writer).await;
        let count = reader.read(&mut bytes).await.unwrap();
        let late_probe =
            observe_socket_probe(&mut server, &mut client, &mut writer, &bytes[..count]).await;
        assert!(sequence(&late_probe) > sequence(&bye));
        let (late_response, _) = proletarius_sip::Parser::default()
            .parse(&response(&late_probe, 481, ""), true)
            .unwrap();
        server.observe(late_response, &mut writer).await;
        assert_eq!(client.next().await, Event::DialogProbed(481));
        assert_eq!(server.remote.borrow().as_ref(), Some(&negotiated));
    }

    #[tokio::test]
    #[should_panic(expected = "must absorb a replay silently")]
    async fn accepted_replay_oracle_cannot_be_bypassed_by_exact_final_ack_cache() {
        let (mut server, _client) = new();
        let final_response = accepted();
        server.retain_ack(final_response.clone(), b"original ACK".to_vec());
        server.silent_replays.push(final_response.clone());
        let (mut writer, _reader) = tokio::io::duplex(1024);
        server
            .observe(Message::Response(final_response), &mut writer)
            .await;
    }

    #[tokio::test]
    async fn duplicate_final_reacks_without_reentering_media_or_lifecycle() {
        let (mut server, mut client) = new();
        let response = accepted();
        let ack = b"exact original ACK".to_vec();
        server.retain_ack(response.clone(), ack.clone());
        server.pending_offer = Some((20, "new exchange SDP".into()));
        let (mut writer, mut reader) = tokio::io::duplex(128);
        // Both directions, including a late response after switching which
        // dialog is active, must use cached bytes rather than rebuilding tags.
        for outgoing in [false, true] {
            server.active_outgoing = outgoing;
            server
                .observe(Message::Response(response.clone()), &mut writer)
                .await;
            let mut received = vec![0; ack.len()];
            reader.read_exact(&mut received).await.unwrap();
            assert_eq!(received, ack);
            assert_eq!(server.pending_offer.as_ref().unwrap().0, 20);
            assert!(server.remote.borrow().is_none());
            assert!(matches!(
                client.events.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
        }
    }

    #[tokio::test]
    async fn first_negotiated_final_populates_cache_before_duplicate_reack() {
        for outgoing in [false, true] {
            let (mut server, mut client) = new();
            server.port = 4000;
            server.active_outgoing = outgoing;
            server.incoming_to = Some("<sip:local@example.invalid>;tag=local".into());
            if outgoing {
                let mut headers = Headers::default();
                for (name, value) in [
                    ("From", "<sip:local@example.invalid>;tag=local"),
                    ("To", "<sip:peer@example.invalid>"),
                    ("Call-ID", "outgoing-cache-dialog"),
                ] {
                    headers.push(Header::new(name, value).unwrap());
                }
                server.outgoing = Some(Request {
                    method: proletarius_sip::Method::INVITE,
                    uri: proletarius_sip::Uri::parse("sip:peer@example.invalid").unwrap(),
                    data: MessageData::new("SIP/2.0", headers, vec![]),
                });
            }
            let codec = AudioCodec::AmrWb {
                octet_aligned: true,
                mode: 2,
                policy: sdp::AmrWbModePolicy::single(2).unwrap(),
            };
            let (mut writer, mut reader) = tokio::io::duplex(4096);
            server
                .command(
                    Command::Renegotiate {
                        codec,
                        payload: 98,
                        period: 1,
                        neighbor: false,
                    },
                    &mut writer,
                )
                .await;
            let mut bytes = [0u8; 4096];
            let count = reader.read(&mut bytes).await.unwrap();
            let (Message::Request(offer), _) = proletarius_sip::Parser::default()
                .parse(&bytes[..count], true)
                .unwrap()
            else {
                panic!("carrier command must produce a real INVITE")
            };
            let answer = server.sdp(codec, 98);
            let (Message::Response(final_response), _) = proletarius_sip::Parser::default()
                .parse(&response(&offer, 200, &answer), true)
                .unwrap()
            else {
                panic!("expected accepted SDP answer")
            };
            server
                .observe(Message::Response(final_response.clone()), &mut writer)
                .await;
            let count = reader.read(&mut bytes).await.unwrap();
            let first_ack = bytes[..count].to_vec();
            let (Message::Request(ack), _) = proletarius_sip::Parser::default()
                .parse(&first_ack, true)
                .unwrap()
            else {
                panic!("first final must produce ACK")
            };
            assert_eq!(ack.method, proletarius_sip::Method::ACK);
            assert_eq!(field(&ack, "Call-ID"), field(&offer, "Call-ID"));
            assert_eq!(field(&ack, "CSeq"), "10 ACK");
            assert_eq!(client.next().await, Event::Renegotiated);
            let negotiated = server.remote.borrow().clone().unwrap();
            assert_eq!(negotiated.codec, codec);
            assert!(server.pending_offer.is_none());
            assert_eq!(server.final_acks.len(), 1);
            // A retransmitted final must not be negotiated again, even after a
            // different exchange becomes pending on the same dialog.
            server.pending_offer = Some((11, "next offer".into()));
            server
                .observe(Message::Response(final_response), &mut writer)
                .await;
            let count = reader.read(&mut bytes).await.unwrap();
            assert_eq!(&bytes[..count], first_ack.as_slice());
            assert_eq!(server.remote.borrow().as_ref(), Some(&negotiated));
            assert_eq!(server.pending_offer.as_ref().unwrap().0, 11);
            assert_eq!(server.final_acks.len(), 1);
            assert!(matches!(
                client.events.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
        }
    }

    #[tokio::test]
    async fn update_final_changes_media_without_ack_and_rejection_preserves_it() {
        for outgoing in [false, true] {
            let (mut server, mut client) = new();
            server.port = 4000;
            server.active_outgoing = outgoing;
            server.incoming_to = Some("<sip:local@example.invalid>;tag=local".into());
            if outgoing {
                let mut headers = Headers::default();
                for (name, value) in [
                    ("From", "<sip:local@example.invalid>;tag=local"),
                    ("To", "<sip:peer@example.invalid>"),
                    ("Call-ID", "outgoing-update-dialog"),
                ] {
                    headers.push(Header::new(name, value).unwrap());
                }
                server.outgoing = Some(Request {
                    method: proletarius_sip::Method::INVITE,
                    uri: proletarius_sip::Uri::parse("sip:peer@example.invalid").unwrap(),
                    data: MessageData::new("SIP/2.0", headers, vec![]),
                });
            }
            let codec = AudioCodec::AmrWb {
                octet_aligned: true,
                mode: 2,
                policy: sdp::AmrWbModePolicy::single(2).unwrap(),
            };
            let (mut writer, mut reader) = tokio::io::duplex(4096);
            server
                .command(Command::Update { codec, payload: 98 }, &mut writer)
                .await;
            let mut bytes = [0u8; 4096];
            let count = reader.read(&mut bytes).await.unwrap();
            let (Message::Request(offer), _) = proletarius_sip::Parser::default()
                .parse(&bytes[..count], true)
                .unwrap()
            else {
                panic!("carrier command must produce UPDATE")
            };
            assert_eq!(offer.method, proletarius_sip::Method::UPDATE);
            let (Message::Response(final_response), _) = proletarius_sip::Parser::default()
                .parse(&response(&offer, 200, &server.sdp(codec, 98)), true)
                .unwrap()
            else {
                panic!("expected accepted SDP answer")
            };
            server
                .observe(Message::Response(final_response.clone()), &mut writer)
                .await;
            assert_eq!(client.next().await, Event::Updated);
            let negotiated = server.remote.borrow().clone().unwrap();
            assert_eq!(negotiated.codec, codec);
            assert!(server.pending_update.is_none());
            assert!(
                server.final_acks.is_empty(),
                "UPDATE cannot create an ACK transaction"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(10), reader.read(&mut bytes))
                    .await
                    .is_err(),
                "UPDATE200 must not send ACK"
            );
            server.pending_update = Some((11, "next update".into()));
            server
                .observe(Message::Response(final_response), &mut writer)
                .await;
            assert_eq!(server.pending_update.as_ref().unwrap().0, 11);
            assert_eq!(server.remote.borrow().as_ref(), Some(&negotiated));
            assert!(matches!(
                client.events.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            server.pending_update = None;
            server
                .command(Command::Update { codec, payload: 96 }, &mut writer)
                .await;
            let count = reader.read(&mut bytes).await.unwrap();
            let (Message::Request(offer), _) = proletarius_sip::Parser::default()
                .parse(&bytes[..count], true)
                .unwrap()
            else {
                panic!("expected rejected UPDATE transaction")
            };
            let (Message::Response(rejected), _) = proletarius_sip::Parser::default()
                .parse(&response(&offer, 488, ""), true)
                .unwrap()
            else {
                panic!("expected final rejection")
            };
            server
                .observe(Message::Response(rejected), &mut writer)
                .await;
            assert_eq!(client.next().await, Event::UpdateRejected(488));
            assert_eq!(server.remote.borrow().as_ref(), Some(&negotiated));
            assert!(server.pending_update.is_none());
            assert!(server.final_acks.is_empty());
            assert!(
                tokio::time::timeout(Duration::from_millis(10), reader.read(&mut bytes))
                    .await
                    .is_err(),
                "UPDATE488 must not send ACK either"
            );
        }
    }

    #[tokio::test]
    async fn withheld_initial_answer_retransmissions_require_explicit_ack_authorization() {
        let (mut server, mut client) = new();
        server.port = 4000;
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        server
            .command(Command::IncomingDeferredAck, &mut writer)
            .await;
        let mut bytes = [0u8; 4096];
        let count = reader.read(&mut bytes).await.unwrap();
        let (Message::Request(offer), _) = proletarius_sip::Parser::default()
            .parse(&bytes[..count], true)
            .unwrap()
        else {
            panic!("expected initial carrier INVITE")
        };
        let body = std::str::from_utf8(&offer.data.body).unwrap();
        let (Message::Response(answer), _) = proletarius_sip::Parser::default()
            .parse(&response(&offer, 200, body), true)
            .unwrap()
        else {
            panic!("expected initial200 answer")
        };
        server
            .observe(Message::Response(answer.clone()), &mut writer)
            .await;
        assert_eq!(client.next().await, Event::AnswerPendingAck);
        let negotiated = server.remote.borrow().clone().unwrap();
        assert!(server.final_acks.is_empty());
        assert!(server.initial_ack.is_none());
        assert!(server.held_initial_answer.is_some());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), reader.read(&mut bytes))
                .await
                .is_err()
        );
        for _ in 0..2 {
            server
                .observe(Message::Response(answer.clone()), &mut writer)
                .await;
            assert_eq!(client.next().await, Event::AnswerRetransmitted);
            assert!(
                server.final_acks.is_empty(),
                "a retransmission cannot authorize the withheld ACK"
            );
            assert!(server.initial_ack.is_none());
            assert_eq!(server.remote.borrow().as_ref(), Some(&negotiated));
            assert!(
                tokio::time::timeout(Duration::from_millis(10), reader.read(&mut bytes))
                    .await
                    .is_err()
            );
        }
        server.command(Command::AckIncoming, &mut writer).await;
        assert_eq!(client.next().await, Event::IncomingAckSent);
        let count = reader.read(&mut bytes).await.unwrap();
        let first_ack = bytes[..count].to_vec();
        let (Message::Request(ack), _) = proletarius_sip::Parser::default()
            .parse(&first_ack, true)
            .unwrap()
        else {
            panic!("explicit authorization must send ACK")
        };
        assert_eq!(ack.method, proletarius_sip::Method::ACK);
        assert_eq!(field(&ack, "CSeq"), "1 ACK");
        assert!(server.held_initial_answer.is_none());
        assert_eq!(server.final_acks.len(), 1);
        server.observe(Message::Response(answer), &mut writer).await;
        let count = reader.read(&mut bytes).await.unwrap();
        assert_eq!(&bytes[..count], first_ack.as_slice());
        assert!(matches!(
            client.events.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        server.command(Command::AckIncoming, &mut writer).await;
        assert_eq!(client.next().await, Event::IncomingAckSent);
        let count = reader.read(&mut bytes).await.unwrap();
        assert_eq!(&bytes[..count], first_ack.as_slice());
        assert_eq!(server.remote.borrow().as_ref(), Some(&negotiated));
        assert_eq!(server.final_acks.len(), 1);
    }

    #[test]
    fn final_ack_cache_requires_exact_dialog_transaction_and_sdp() {
        let (mut server, _) = new();
        let response = accepted();
        server.retain_ack(response.clone(), b"ACK".to_vec());
        assert!(server.retained_ack(&response).is_some());
        for (name, value) in [
            ("Call-ID", "other-dialog"),
            ("From", "<sip:peer@example.invalid>;tag=other"),
            ("CSeq", "20 INVITE"),
            ("Via", "SIP/2.0/TCP peer;branch=other"),
            ("To", "<sip:local@example.invalid>;tag=other"),
        ] {
            let mut unrelated = response.clone();
            unrelated
                .data
                .headers
                .set(Header::new(name, value).unwrap());
            assert!(server.retained_ack(&unrelated).is_none());
        }
        let mut changed = response.clone();
        changed.data.body.push(b'x');
        assert!(server.retained_ack(&changed).is_none());
        changed = response;
        changed.status = 488;
        assert!(server.retained_ack(&changed).is_none());
    }
}
