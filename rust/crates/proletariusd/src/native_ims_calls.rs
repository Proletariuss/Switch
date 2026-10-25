//! Generation-bound voice dialog owner on the existing registered IMS actor.
//! One dispatcher serializes events, controls and timers; no signaling socket
//! is opened here. Accepted writes and media cleanup are awaited to completion.
use crate::{
    native_ims_actor::{Command, Event},
    native_vowifi_supervisor::SessionGeneration,
};
use proletarius_call::{
    CallPhase, IncomingCallPhase, IncomingCallTracker, IncomingCancelDecision,
    IncomingInviteAdmission, IncomingTimeoutDecision, InviteAction, OutgoingCallTracker,
    OutgoingHangupAction,
    sdp::{self, AudioCodec, AudioNegotiation},
};
use proletarius_sip::{
    Dialog, DialogRole, Header, Headers, MessageData, Method, Request, Response, Uri,
};
use proletarius_vowifi::{
    registration_coordinator::SipConnectionId,
    sip_session_runtime::{RegisteredSipEvent, RegisteredSipMessage},
};
use serde::Serialize;
use std::{future::Future, net::IpAddr, pin::Pin, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};
use zeroize::Zeroize;
const DECISION_TIMEOUT: Duration = Duration::from_secs(90);
const INITIAL_ACK_TIMEOUT: Duration = Duration::from_secs(32);
const REINVITE_ACK_TIMEOUT: Duration = Duration::from_secs(32);
const SESSION_REFRESH_TIMEOUT: Duration = Duration::from_secs(32);
const SESSION_MIN_INTERVAL: u32 = 90;
const SESSION_MAX_INTERVAL: u32 = 86_400;
const MAX_INITIAL_TIMER_RETRIES: usize = 4;
const MAX_RETIRED: usize = 16;
const MAX_ACCEPTED_REINVITES: usize = 16;
const MEDIA_CHECK_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) type MediaFuture<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct CallBinding {
    pub generation: SessionGeneration,
    pub call_id: String,
    pub invite_cseq: u32,
}
pub(crate) struct LocalMedia {
    pub ip: IpAddr,
    pub port: u16,
    pub audio_port: u16,
    pub audio_token: String,
}
/// Implementations own the actual tunneled RTP/audio worker. `stop` completes
/// only after all workers and sockets for this exact call have drained.
pub(crate) trait Media: Send + Sync {
    fn terminal_failure(&self, _: &CallBinding) -> Option<Failure> {
        None
    }
    fn statistics(&self, _: &CallBinding) -> media::Stats {
        Default::default()
    }
    fn allocate(self: Arc<Self>, binding: CallBinding) -> MediaFuture<LocalMedia>;
    /// Validate and retain replacement resources, including receive support,
    /// without changing the transmitter before the answer is accepted.
    fn prepare(self: Arc<Self>, _: CallBinding, _: AudioNegotiation) -> MediaFuture<()> {
        Box::pin(async { Err(Failure::known("ims_media_reconfiguration_unsupported")) })
    }
    /// Prepare every advertised receiver before offering SDP without a remote
    /// endpoint. Keep TX closed; only the validated ACK answer supplies one.
    fn prepare_local_offer(self: Arc<Self>, _: CallBinding, _: Vec<u8>) -> MediaFuture<()> {
        Box::pin(async { Err(Failure::known("ims_media_reconfiguration_unsupported")) })
    }
    fn discard(self: Arc<Self>, _: CallBinding) -> MediaFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn hold_transmit(self: Arc<Self>, _: CallBinding, _: bool) -> MediaFuture<()> {
        Box::pin(async { Err(Failure::known("ims_media_reconfiguration_unsupported")) })
    }
    fn configure(
        self: Arc<Self>,
        binding: CallBinding,
        remote: AudioNegotiation,
    ) -> MediaFuture<()>;
    fn start(self: Arc<Self>, binding: CallBinding, early: bool) -> MediaFuture<()>;
    fn stop(self: Arc<Self>, binding: CallBinding) -> MediaFuture<()>;
    fn dtmf(self: Arc<Self>, binding: CallBinding, digit: char) -> MediaFuture<()>;
}
// No Debug: configuration contains subscriber identities and session security.
pub(crate) struct Config {
    pub local_ip: IpAddr,
    pub port_c: u16,
    pub contact: String,
    pub from_uri: String,
    pub preferred_identity: String,
    pub route: String,
    pub service_routes: Vec<String>,
    pub encoded_icsi: String,
    pub phone_context: String,
    pub home_domain: String,
    pub realm: String,
    pub user_agent: String,
    pub security_verify: String,
    pub pani: String,
    pub early_media_header: bool,
    pub evs_only: bool,
    pub sms_only: bool,
    pub ussd_service_selector: bool,
}
impl Drop for Config {
    fn drop(&mut self) {
        self.from_uri.zeroize();
        self.preferred_identity.zeroize();
        self.contact.zeroize();
        self.security_verify.zeroize();
        self.route.zeroize();
        self.service_routes.zeroize();
    }
}
#[derive(Clone, Serialize)]
pub(crate) struct Snapshot {
    pub state: String,
    pub direction: String,
    pub peer: String,
    pub call_id: String,
    pub codec: String,
    pub audio_sample_rate: u32,
    pub started_at: String,
    pub last_error: String,
    pub media_ready: bool,
    pub early_media: bool,
    pub signaling_ready: bool,
    pub audio_port: u16,
    pub audio_token: String,
    pub uncertain: bool,
    #[serde(flatten)]
    pub media_statistics: media::Stats,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            state: "idle".into(),
            direction: String::new(),
            peer: String::new(),
            call_id: String::new(),
            codec: String::new(),
            audio_sample_rate: 0,
            started_at: String::new(),
            last_error: String::new(),
            media_ready: false,
            early_media: false,
            signaling_ready: true,
            audio_port: 0,
            audio_token: String::new(),
            uncertain: false,
            media_statistics: Default::default(),
        }
    }
}
#[derive(Clone)]
pub(crate) struct Failure {
    pub code: &'static str,
    pub uncertain: bool,
    pub sip_status: Option<u16>,
}
impl Failure {
    fn known(code: &'static str) -> Self {
        Self {
            code,
            uncertain: false,
            sip_status: None,
        }
    }
    fn unknown() -> Self {
        Self {
            code: "ims_call_write_unconfirmed",
            uncertain: true,
            sip_status: None,
        }
    }
    fn sip_rejection(code: &'static str, status: u16) -> Self {
        Self {
            code,
            uncertain: false,
            sip_status: Some(status),
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallFailure")
            .field("code", &self.code)
            .field("uncertain", &self.uncertain)
            .finish()
    }
}
impl std::error::Error for Failure {}
type Result<T> = std::result::Result<T, Failure>;
enum Tracker {
    Incoming(IncomingCallTracker),
    Outgoing(OutgoingCallTracker),
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum InitialAckRole {
    NoSdp,
    AnswerToLocalOffer,
}
struct PendingInitialAnswer {
    response: Response,
    ack_role: InitialAckRole,
    deadline: Instant,
    next_response_retry: Instant,
    response_retry_interval: Duration,
    hangup_requested: bool,
}
struct PendingReinvite {
    cseq: u32,
    response: Response,
    remote: AudioNegotiation,
    offerless: bool,
    local_sdp_version: u64,
    deadline: Instant,
}
/// RFC 6026 Accepted transaction and the independent 2xx TU. Its lifetime
/// belongs to signaling, not to the pending SDP exchange or media allocation.
struct AcceptedReinvite {
    cseq: u32,
    response: Response,
    confirmed: bool,
    acked: bool,
    deadline: Instant,
    next_response_retry: Option<Instant>,
    response_retry_interval: Duration,
    cleanup_on_ack: bool,
    cleanup_target: Option<Uri>,
}
impl Drop for AcceptedReinvite {
    fn drop(&mut self) {
        self.response.data.body.zeroize();
    }
}
struct PendingSessionRefresh {
    request: Request,
    deadline: Instant,
}
struct SessionTimer {
    interval: u32,
    min_se: u32,
    local_refresher: bool,
    expires_at: Instant,
    termination_at: Instant,
    refresh_at: Option<Instant>,
    pending: Option<PendingSessionRefresh>,
    retry_count: u8,
}
struct Call {
    binding: CallBinding,
    request: Request,
    connection: SipConnectionId,
    tracker: Tracker,
    dialog: Option<Dialog>,
    local_tag: String,
    initial_reply: Option<Response>,
    initial_reply_confirmed: bool,
    initial_reply_deadline: Option<Instant>,
    initial_answer: Option<PendingInitialAnswer>,
    reliable: Option<reliable_provisional::ReliableProvisional>,
    prack_replies: std::collections::VecDeque<reliable_provisional::PrackReply>,
    answer_requested: bool,
    early_oa_complete: bool,
    local_sdp: Vec<u8>,
    remote: Option<AudioNegotiation>,
    payload_history: sdp::PayloadHistory,
    local_media: Option<(IpAddr, u16)>,
    local_sdp_version: u64,
    deadline: Option<Instant>,
    pending_reinvite: Option<PendingReinvite>,
    accepted_reinvites: std::collections::VecDeque<AcceptedReinvite>,
    dialog_terminated: bool,
    media_update_replies: Vec<Response>,
    session_timer: Option<SessionTimer>,
    early_session_timer_refreshed: bool,
    peer_allows_update: bool,
    last_refresh_ack: Option<(Request, Request)>,
    late_refresh_invite: Option<Request>,
    initial_timer_retry_acks: Vec<Request>,
    media_allocated: bool,
    media_started: bool,
    uncertain: bool,
}
impl Drop for Call {
    fn drop(&mut self) {
        self.request.data.body.zeroize();
        if let Some(response) = &mut self.initial_reply {
            response.data.body.zeroize();
        }
        if let Some(pending) = &mut self.initial_answer {
            pending.response.data.body.zeroize();
        }
        self.local_sdp.zeroize();
        if let Some(timer) = &mut self.session_timer
            && let Some(pending) = &mut timer.pending
        {
            pending.request.data.body.zeroize();
        }
        if let Some((original, ack)) = &mut self.last_refresh_ack {
            original.data.body.zeroize();
            ack.data.body.zeroize();
        }
        if let Some(request) = &mut self.late_refresh_invite {
            request.data.body.zeroize();
        }
        if let Some(pending) = &mut self.pending_reinvite {
            pending.response.data.body.zeroize();
        }
        for response in &mut self.media_update_replies {
            response.data.body.zeroize();
        }
        for ack in &mut self.initial_timer_retry_acks {
            ack.data.body.zeroize();
        }
    }
}
pub(crate) struct Service {
    generation: SessionGeneration,
    config: Config,
    ussd: Option<ussd::Session>,
    retired_ussd: std::collections::VecDeque<ussd::Session>,
    commands: mpsc::Sender<Command>,
    media: Arc<dyn Media>,
    media_check_deadline: Instant,
    call: Option<Call>,
    retired: std::collections::VecDeque<Call>,
    completed_invites: std::collections::VecDeque<completed_invite::CompletedInvite>,
    snapshot: Snapshot,
    observer: Option<Arc<dyn Fn(SessionGeneration, Snapshot) + Send + Sync>>,
    stopping: bool,
}
impl Service {
    pub(crate) fn new(
        generation: SessionGeneration,
        config: Config,
        commands: mpsc::Sender<Command>,
        media: Arc<dyn Media>,
    ) -> Result<Self> {
        Uri::parse(&config.from_uri).map_err(|_| Failure::known("ims_call_identity_invalid"))?;
        if config.port_c == 0 || config.contact.is_empty() {
            return Err(Failure::known("ims_call_transport_invalid"));
        }
        for (name, value) in [
            ("Contact", &config.contact),
            ("P-Preferred-Identity", &config.preferred_identity),
            ("Route", &config.route),
            ("User-Agent", &config.user_agent),
            ("Security-Verify", &config.security_verify),
            ("P-Access-Network-Info", &config.pani),
        ] {
            Header::new(name, value).map_err(|_| Failure::known("ims_call_config_invalid"))?;
        }
        if config.service_routes.is_empty() || config.service_routes.len() > 8 {
            return Err(Failure::known("ims_call_routes_invalid"));
        }
        for route in &config.service_routes {
            Header::new("Route", route).map_err(|_| Failure::known("ims_call_routes_invalid"))?;
        }
        Header::new(
            "Accept-Contact",
            &format!("*;+g.3gpp.icsi-ref=\"{}\"", config.encoded_icsi),
        )
        .map_err(|_| Failure::known("ims_call_config_invalid"))?;
        Ok(Self {
            generation,
            config,
            ussd: None,
            retired_ussd: Default::default(),
            commands,
            media,
            media_check_deadline: Instant::now() + MEDIA_CHECK_INTERVAL,
            call: None,
            retired: Default::default(),
            completed_invites: Default::default(),
            snapshot: Default::default(),
            observer: None,
            stopping: false,
        })
    }
    pub(crate) fn set_observer(
        &mut self,
        observer: Arc<dyn Fn(SessionGeneration, Snapshot) + Send + Sync>,
    ) {
        self.observer = Some(observer);
    }
    pub(crate) fn generation(&self) -> SessionGeneration {
        self.generation
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.snapshot.clone();
        if let Some(call) = &self.call
            && call.media_allocated
        {
            snapshot.media_statistics = self.media.statistics(&call.binding);
        }
        snapshot
    }
    pub(crate) fn active(&self) -> bool {
        self.ussd_active() || self.voice_active()
    }
    fn voice_active(&self) -> bool {
        self.call
            .as_ref()
            .is_some_and(|c| c.uncertain || !terminal(&self.snapshot.state))
    }
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        if self.stopping {
            return None;
        }
        let ordinary = [
            self.call.as_ref().and_then(|c| c.deadline),
            self.call
                .as_ref()
                .and_then(|c| c.pending_reinvite.as_ref().map(|p| p.deadline)),
            self.call
                .as_ref()
                .and_then(|c| c.session_timer.as_ref().map(|t| t.termination_at)),
            self.call
                .as_ref()
                .and_then(|c| c.session_timer.as_ref().and_then(|t| t.refresh_at)),
            self.call.as_ref().and_then(|c| {
                c.session_timer
                    .as_ref()
                    .and_then(|t| t.pending.as_ref().map(|p| p.deadline))
            }),
            self.ussd_deadline(),
            self.call
                .as_ref()
                .filter(|c| c.media_allocated && !terminal(&self.snapshot.state))
                .map(|_| self.media_check_deadline),
        ]
        .into_iter()
        .flatten()
        .min();
        ordinary
            .into_iter()
            .chain(
                self.call
                    .iter()
                    .chain(self.retired.iter())
                    .flat_map(|call| {
                        [
                            call.initial_reply_deadline,
                            call.initial_answer.as_ref().map(|pending| pending.deadline),
                            call.initial_answer
                                .as_ref()
                                .map(|pending| pending.next_response_retry),
                        ]
                        .into_iter()
                        .flatten()
                    }),
            )
            .chain(
                self.call
                    .iter()
                    .chain(self.retired.iter())
                    .flat_map(|call| {
                        call.accepted_reinvites.iter().flat_map(|accepted| {
                            [Some(accepted.deadline), accepted.next_response_retry]
                                .into_iter()
                                .flatten()
                        })
                    }),
            )
            .chain(
                self.call
                    .iter()
                    .chain(self.retired.iter())
                    .filter_map(Call::reliable_next_deadline),
            )
            .chain(self.completed_next_deadline())
            .min()
    }
    fn check(&self, generation: SessionGeneration) -> Result<()> {
        if self.generation != generation || self.stopping {
            Err(Failure::known("ims_call_stale_generation"))
        } else {
            Ok(())
        }
    }
    fn observe(&self) {
        if let Some(observer) = &self.observer {
            observer(self.generation, self.snapshot());
        }
    }
    fn prune_initial_replies(&mut self, now: Instant) {
        self.prune_completed_invites(now);
        for call in self.call.iter_mut().chain(self.retired.iter_mut()) {
            call.prune_reliable(now);
            call.accepted_reinvites.retain(|accepted| {
                accepted.deadline > now || (!accepted.acked && !call.dialog_terminated)
            });
            if call.initial_answer.is_none()
                && call.initial_reply_deadline.is_some_and(|at| at <= now)
            {
                if let Some(mut response) = call.initial_reply.take() {
                    response.data.body.zeroize();
                }
                call.initial_reply_deadline = None;
                call.initial_reply_confirmed = false;
            }
        }
    }
    fn can_retire(&self) -> bool {
        self.call.is_none()
            || self.retired.len() < MAX_RETIRED
            || self.retired.iter().any(|call| {
                call.initial_answer.is_none()
                    && !call.reliable_protected(Instant::now())
                    && call
                        .initial_reply_deadline
                        .is_none_or(|at| at <= Instant::now())
                    && call.accepted_reinvites.iter().all(|accepted| {
                        accepted.deadline <= Instant::now()
                            && (accepted.acked || call.dialog_terminated)
                    })
            })
    }
    fn retire(&mut self) -> Result<()> {
        if !self.can_retire() {
            return Err(Failure::known("ims_call_busy"));
        }
        self.prune_initial_replies(Instant::now());
        if let Some(old) = self.call.take() {
            self.retired.push_back(old);
            while self.retired.len() > MAX_RETIRED {
                let index = self
                    .retired
                    .iter()
                    .position(|call| {
                        call.initial_reply_deadline.is_none()
                            && call.accepted_reinvites.is_empty()
                            && !call.reliable_protected(Instant::now())
                    })
                    .expect("admission preserves unexpired incoming transactions");
                self.retired.remove(index);
            }
        }
        Ok(())
    }
    async fn initial_response(&mut self, response: Response) -> Result<()> {
        let call = self
            .call
            .as_mut()
            .ok_or_else(|| Failure::known("ims_call_missing"))?;
        if let Some(mut previous) = call.initial_reply.replace(response.clone()) {
            previous.data.body.zeroize();
        }
        call.initial_reply_confirmed = false;
        let connection = call.connection;
        let request = call.request.clone();
        let result = if response.status >= 300 {
            self.completed_response(connection, &request, response)
                .await
        } else {
            if response.status >= 200 {
                self.release_completed_invite(connection, &request);
            }
            self.respond(connection, response).await
        };
        let call = self.call.as_mut().unwrap();
        call.initial_reply_confirmed = result.is_ok();
        call.initial_reply_deadline = Some(Instant::now() + INITIAL_ACK_TIMEOUT);
        result
    }
    async fn finish_reinvite_cleanup(&mut self, retired: Option<usize>, seq: u32) -> Result<()> {
        if retired.is_none() {
            self.revoke_media_bridge();
        }
        let write = self.accepted_reinvite_bye(retired, seq).await;
        if retired.is_none()
            && (!terminal(&self.snapshot.state)
                || self.call.as_ref().is_some_and(|call| call.media_allocated))
        {
            if self.snapshot.last_error.is_empty() {
                self.snapshot.last_error = write
                    .as_ref()
                    .err()
                    .map_or("ims_reinvite_ack_timeout", |error| error.code)
                    .into();
            }
            // Even an uncertain BYE must drain the current media. Marking
            // the dialog ended prevents a second send, not local cleanup.
            let cleanup = self.finish("failed").await;
            return write.and(cleanup);
        }
        write
    }
    async fn send(&self, request: Request) -> Result<()> {
        let (reply, receipt) = oneshot::channel();
        self.commands
            .send(Command::SendUntracked {
                generation: self.generation,
                request,
                reply,
            })
            .await
            .map_err(|_| Failure::known("ims_call_transport_stopped"))?;
        receipt
            .await
            .map_err(|_| Failure::unknown())?
            .map_err(|_| Failure::unknown())
    }
    async fn respond(&self, connection: SipConnectionId, response: Response) -> Result<()> {
        let (reply, receipt) = oneshot::channel();
        self.commands
            .send(Command::RespondDialog {
                generation: self.generation,
                connection,
                response,
                reply,
            })
            .await
            .map_err(|_| Failure::known("ims_call_transport_stopped"))?;
        receipt
            .await
            .map_err(|_| Failure::unknown())?
            .map_err(|_| Failure::unknown())
    }
    fn via(&self) -> String {
        let host = if self.config.local_ip.is_ipv6() {
            format!("[{}]", self.config.local_ip)
        } else {
            self.config.local_ip.to_string()
        };
        format!(
            "SIP/2.0/TCP {host}:{};branch=z9hG4bK{};rport",
            self.config.port_c,
            unique()
        )
    }
    fn decorate(&self, request: &mut Request) -> Result<()> {
        for (name, value) in [
            ("Supported", "100rel,timer,replaces"),
            (
                "Allow",
                "INVITE,ACK,CANCEL,BYE,UPDATE,PRACK,INFO,OPTIONS,MESSAGE",
            ),
            ("User-Agent", self.config.user_agent.as_str()),
            ("Security-Verify", self.config.security_verify.as_str()),
            ("P-Access-Network-Info", self.config.pani.as_str()),
        ] {
            if !value.is_empty() {
                push(&mut request.data.headers, name, value)?;
            }
        }
        Ok(())
    }
    fn initial(
        &self,
        target: &str,
        call_id: &str,
        cseq: u32,
        tag: &str,
        body: Vec<u8>,
    ) -> Result<Request> {
        let mut headers = Headers::default();
        for (name, value) in [
            ("Via", self.via()),
            ("From", format!("<{}>;tag={tag}", self.config.from_uri)),
            ("To", format!("<{target}>")),
            ("Call-ID", call_id.into()),
            ("CSeq", format!("{cseq} INVITE")),
            ("Max-Forwards", "70".into()),
            ("Route", self.config.route.clone()),
            ("Contact", self.config.contact.clone()),
            (
                "P-Preferred-Identity",
                format!("<{}>", self.config.preferred_identity),
            ),
            (
                "P-Preferred-Service",
                "urn:urn-7:3gpp-service.ims.icsi.mmtel".into(),
            ),
            (
                "Accept-Contact",
                format!("*;+g.3gpp.icsi-ref=\"{}\"", self.config.encoded_icsi),
            ),
            ("Session-Expires", "1800;refresher=uac".into()),
            ("Min-SE", "90".into()),
            ("Accept", "application/sdp".into()),
            ("Content-Type", "application/sdp".into()),
        ] {
            if !value.is_empty() {
                push(&mut headers, name, &value)?;
            }
        }
        if self.config.early_media_header {
            push(&mut headers, "P-Early-Media", "supported")?;
        }
        let mut request = Request {
            method: Method::INVITE,
            uri: Uri::parse(target).map_err(|_| Failure::known("ims_call_target_invalid"))?,
            data: MessageData::new("SIP/2.0", headers, body),
        };
        self.decorate(&mut request)?;
        Ok(request)
    }
    pub(crate) async fn dial(
        &mut self,
        generation: SessionGeneration,
        peer: &str,
    ) -> Result<Snapshot> {
        self.check(generation)?;
        if self.config.sms_only {
            return Err(Failure::known("ims_voice_disabled_for_sms_session"));
        }
        if self.voice_active() || !self.can_retire() {
            return Err(Failure::known("ims_call_busy"));
        }
        let target = proletarius_ims::call_target::normalize(
            peer,
            &self.config.phone_context,
            &self.config.home_domain,
            &self.config.realm,
        )
        .map_err(|_| Failure::known("ims_call_target_invalid"))?;
        let cseq = 1;
        let call_id = unique();
        let tag = unique();
        let binding = CallBinding {
            generation,
            call_id: call_id.clone(),
            invite_cseq: cseq,
        };
        // Reserve the logical call before the asynchronous media allocation.
        self.retire()?;
        let empty = self.initial(&target, &call_id, cseq, &tag, Vec::new())?;
        self.call = Some(Call {
            binding: binding.clone(),
            request: empty,
            connection: SipConnectionId::Primary,
            tracker: Tracker::Outgoing(
                OutgoingCallTracker::begin(cseq)
                    .map_err(|_| Failure::known("ims_call_policy_invalid"))?,
            ),
            dialog: None,
            local_tag: tag.clone(),
            initial_reply: None,
            initial_reply_confirmed: false,
            initial_reply_deadline: None,
            initial_answer: None,
            reliable: None,
            prack_replies: Default::default(),
            answer_requested: false,
            early_oa_complete: false,
            local_sdp: Vec::new(),
            remote: None,
            payload_history: Default::default(),
            local_media: None,
            local_sdp_version: 1,
            deadline: Some(Instant::now() + DECISION_TIMEOUT),
            pending_reinvite: None,
            accepted_reinvites: Default::default(),
            dialog_terminated: false,
            media_update_replies: Vec::new(),
            session_timer: None,
            early_session_timer_refreshed: false,
            peer_allows_update: false,
            last_refresh_ack: None,
            late_refresh_invite: None,
            initial_timer_retry_acks: Vec::new(),
            media_allocated: false,
            media_started: false,
            uncertain: false,
        });
        self.snapshot = Snapshot {
            state: "outgoing".into(),
            direction: "outgoing".into(),
            peer: peer.into(),
            call_id,
            ..Default::default()
        };
        self.observe();
        let result = async {
            let local = self.media.clone().allocate(binding).await?;
            self.call.as_mut().unwrap().media_allocated = true;
            self.call.as_mut().unwrap().local_media = Some((local.ip, local.port));
            self.snapshot.audio_port = local.audio_port;
            self.snapshot.audio_token = local.audio_token;
            let sdp = sdp::build_local_sdp(
                generation.value(),
                local.ip,
                local.port,
                false,
                None,
                None,
                None,
                self.config.evs_only,
            )
            .map_err(|_| Failure::known("ims_call_local_sdp_invalid"))?;
            let request = self.initial(
                &target,
                &self.snapshot.call_id,
                cseq,
                &tag,
                sdp.into_bytes(),
            )?;
            self.call.as_mut().unwrap().local_sdp = request.data.body.clone();
            self.call
                .as_mut()
                .unwrap()
                .payload_history
                .admit_sdp(
                    std::str::from_utf8(&request.data.body)
                        .map_err(|_| Failure::known("ims_call_local_sdp_invalid"))?,
                )
                .map_err(|_| Failure::known("ims_call_local_sdp_invalid"))?;
            self.call.as_mut().unwrap().request = request.clone();
            self.send(request).await
        }
        .await;
        if let Err(error) = result {
            self.fail(&error).await?;
            return Err(error);
        }
        self.observe();
        Ok(self.snapshot())
    }
    async fn fail(&mut self, error: &Failure) -> Result<()> {
        if let Some(call) = &mut self.call {
            call.stop_reliable_tu();
            call.uncertain |= error.uncertain;
            if let Some(pending) = &mut call.initial_answer {
                pending.hangup_requested = true;
            }
            if error.uncertain
                && let Tracker::Outgoing(tracker) = &mut call.tracker
            {
                if matches!(tracker.phase(), CallPhase::Inviting | CallPhase::Early) {
                    tracker
                        .cancel()
                        .map_err(|_| Failure::known("ims_call_policy_invalid"))?;
                }
            }
        }
        self.snapshot.uncertain |= error.uncertain;
        self.snapshot.last_error = error.code.into();
        self.finish("failed").await
    }
    fn revoke_media_bridge(&mut self) {
        self.snapshot.media_ready = false;
        self.snapshot.early_media = false;
        self.snapshot.audio_port = 0;
        self.snapshot.audio_token.zeroize();
        self.observe();
    }
    async fn finish(&mut self, state: &str) -> Result<()> {
        // Revoke the UI/audio bridge's capabilities before waiting for a real
        // codec or transport join. Resource ownership remains in `call` until
        // stop confirms completion; a slow finalizer is never media readiness.
        self.revoke_media_bridge();
        let mut cleanup_error = None;
        if let Some(call) = &mut self.call {
            call.stop_reliable_tu();
            call.answer_requested = false;
            if call.media_allocated {
                self.snapshot.media_statistics = self.media.statistics(&call.binding);
                match self.media.clone().stop(call.binding.clone()).await {
                    Ok(()) => {
                        call.media_allocated = false;
                        call.media_started = false;
                    }
                    Err(mut error) => {
                        // Cleanup is unconfirmed even if the underlying error
                        // was classified as known. Retain the resource fence,
                        // but terminalize signaling and cancel every deadline.
                        call.uncertain = true;
                        error.uncertain = true;
                        self.snapshot.uncertain = true;
                        if self.snapshot.last_error.is_empty() {
                            self.snapshot.last_error = error.code.into();
                        }
                        cleanup_error = Some(error);
                    }
                }
            }
            call.deadline = None;
            if let Some(mut pending) = call.pending_reinvite.take() {
                pending.response.data.body.zeroize();
            }
            for mut response in call.media_update_replies.drain(..) {
                response.data.body.zeroize();
            }
            if let Some(timer) = &mut call.session_timer
                && let Some(mut pending) = timer.pending.take()
            {
                if pending.request.method == Method::INVITE {
                    if let Some(mut previous) = call.late_refresh_invite.replace(pending.request) {
                        previous.data.body.zeroize();
                    }
                } else {
                    pending.request.data.body.zeroize();
                }
            }
            call.session_timer = None;
        }
        self.snapshot.state = if cleanup_error.is_some() {
            "failed"
        } else {
            state
        }
        .into();
        self.observe();
        cleanup_error.map_or(Ok(()), Err)
    }
    fn negotiate_offer(&self, body: &[u8]) -> Result<AudioNegotiation> {
        let text =
            std::str::from_utf8(body).map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))?;
        let (evs, modes) = proletarius_call::evs::available_audio_codecs(self.config.evs_only);
        sdp::negotiate_audio_offer_with_modes(text, evs, &modes)
            .map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))
    }
    fn negotiate_answer(&self, body: &[u8], offer: &[u8]) -> Result<AudioNegotiation> {
        let answer =
            std::str::from_utf8(body).map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))?;
        let offer =
            std::str::from_utf8(offer).map_err(|_| Failure::known("ims_call_local_sdp_invalid"))?;
        let (evs, modes) = proletarius_call::evs::available_audio_codecs(self.config.evs_only);
        sdp::negotiate_audio_answer_to_offer(answer, offer, evs, &modes)
            .map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))
    }
    fn proposed_history(&self, body: &[u8]) -> Result<sdp::PayloadHistory> {
        let mut history = self
            .call
            .as_ref()
            .ok_or_else(|| Failure::known("ims_call_missing"))?
            .payload_history
            .clone();
        history
            .admit_sdp(
                std::str::from_utf8(body)
                    .map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))?,
            )
            .map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))?;
        Ok(history)
    }
    fn publish_codec(&mut self, codec: AudioCodec) {
        self.snapshot.codec = match codec {
            AudioCodec::Evs { .. } => "EVS",
            AudioCodec::AmrWb { .. } => "AMR-WB",
        }
        .into();
        self.snapshot.audio_sample_rate = 16000;
    }
    async fn discard_prepared(&mut self, binding: CallBinding) -> Result<()> {
        if let Err(error) = self.media.clone().discard(binding).await {
            self.fail(&error).await?;
            return Err(error);
        }
        Ok(())
    }
    async fn failed_answer_write(&mut self, binding: CallBinding, error: Failure) -> Result<()> {
        if let Some(call) = self.call.as_mut() {
            call.uncertain |= error.uncertain;
        }
        self.snapshot.uncertain |= error.uncertain;
        self.revoke_media_bridge();
        // A discard error must not bypass the original uncertain-write fence
        // or final stop, which drains every active and staged resource.
        let discarded = self.media.clone().discard(binding).await;
        self.fail(&error).await?;
        discarded?;
        Err(error)
    }
    async fn commit_media(&mut self, remote: AudioNegotiation) -> Result<()> {
        let binding = self
            .call
            .as_ref()
            .ok_or_else(|| Failure::known("ims_call_missing"))?
            .binding
            .clone();
        if self.call.as_ref().unwrap().media_started
            && self.call.as_ref().unwrap().remote.as_ref() == Some(&remote)
        {
            self.discard_prepared(binding).await?;
            self.publish_codec(remote.codec);
            return Ok(());
        }
        if let Err(error) = self.media.clone().configure(binding, remote.clone()).await {
            self.revoke_media_bridge();
            // SDP has already been accepted. Do not silently resume an old
            // format; end the dialog and retain the original cleanup fences.
            if let Ok(bye) = self.dialog_request(Method::BYE, None) {
                if let Err(write) = self.send(bye).await {
                    self.fail(&write).await?;
                    return Err(write);
                }
            }
            self.fail(&error).await?;
            return Err(error);
        }
        self.publish_codec(remote.codec);
        Ok(())
    }
    async fn start_media(&mut self, early: bool) -> Result<()> {
        let call = self
            .call
            .as_mut()
            .ok_or_else(|| Failure::known("ims_call_missing"))?;
        let remote = call
            .remote
            .clone()
            .ok_or_else(|| Failure::known("ims_call_remote_sdp_invalid"))?;
        self.media
            .clone()
            .configure(call.binding.clone(), remote.clone())
            .await?;
        if !call.media_started || self.snapshot.early_media != early {
            self.media
                .clone()
                .start(call.binding.clone(), early)
                .await?;
            call.media_started = true;
        }
        self.snapshot.codec = match remote.codec {
            AudioCodec::Evs { .. } => "EVS",
            AudioCodec::AmrWb { .. } => "AMR-WB",
        }
        .into();
        self.snapshot.audio_sample_rate = 16000;
        self.snapshot.media_ready = true;
        self.snapshot.early_media = early;
        if !early {
            self.snapshot.started_at = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
        }
        self.observe();
        Ok(())
    }
    pub(crate) async fn answer(&mut self, generation: SessionGeneration) -> Result<Snapshot> {
        self.check(generation)?;
        let call = self
            .call
            .as_ref()
            .ok_or_else(|| Failure::known("ims_call_missing"))?;
        if call.uncertain
            || !matches!(&call.tracker,Tracker::Incoming(t) if t.phase()==IncomingCallPhase::Ringing)
        {
            return Err(Failure::known("ims_call_not_ringing"));
        }
        if call
            .reliable
            .as_ref()
            .is_some_and(|pending| !pending.pracked)
            || (call.reliable.is_some() && !call.early_oa_complete)
        {
            self.call.as_mut().unwrap().answer_requested = true;
            return Ok(self.snapshot());
        }
        // UPDATE owns the latest accepted early target, CSeq and timer. The
        // final response must not recreate those facts from the first INVITE.
        let preserve_early_timer = call.early_session_timer_refreshed;
        let session_header = if preserve_early_timer {
            call.session_timer.as_ref().map(|timer| {
                format!(
                    "{};refresher={}",
                    timer.interval,
                    if timer.local_refresher { "uas" } else { "uac" }
                )
            })
        } else {
            inbound_session_timer_header(&call.request.data.headers)?
        };
        let binding = call.binding.clone();
        let early_oa_complete = call.early_oa_complete;
        let offerless = call.request.data.body.is_empty() && !early_oa_complete;
        let remote = call.remote.clone();
        if !offerless && remote.is_none() {
            return Err(Failure::known("ims_call_remote_sdp_invalid"));
        }
        let answer = if early_oa_complete {
            Vec::new()
        } else {
            let local = match self.media.clone().allocate(binding.clone()).await {
                Ok(local) => local,
                Err(error) => {
                    self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                        .await?;
                    return Err(error);
                }
            };
            self.call.as_mut().unwrap().media_allocated = true;
            self.call.as_mut().unwrap().local_media = Some((local.ip, local.port));
            self.snapshot.audio_port = local.audio_port;
            self.snapshot.audio_token = local.audio_token;
            let answer = if offerless {
                sdp::build_local_sdp(
                    generation.value(),
                    local.ip,
                    local.port,
                    false,
                    None,
                    None,
                    None,
                    self.config.evs_only,
                )
            } else {
                sdp::build_local_audio_answer_to_offer_version(
                    generation.value(),
                    1,
                    local.ip,
                    local.port,
                    std::str::from_utf8(&self.call.as_ref().unwrap().request.data.body)
                        .map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))?,
                    remote.as_ref().unwrap(),
                    self.config.evs_only,
                )
            }
            .map_err(|_| Failure::known("ims_call_local_sdp_invalid"))?
            .into_bytes();
            if offerless {
                // Freeze all advertised PT meanings, even the codec not selected
                // by the ACK answer, for every later exchange in this dialog.
                self.call.as_mut().unwrap().payload_history = self.proposed_history(&answer)?;
            }
            answer
        };
        let call = self.call.as_mut().unwrap();
        if !early_oa_complete {
            call.local_sdp = answer.clone();
        }
        let mut response = reply_for(
            &call.request,
            200,
            "OK",
            Some(&call.local_tag),
            answer,
            Some(&self.config.contact),
        )?;
        if let Some(session_header) = &session_header {
            push(
                &mut response.data.headers,
                "Session-Expires",
                session_header,
            )?;
            push(&mut response.data.headers, "Require", "timer")?;
        }
        let timer_epoch = Instant::now();
        let mut session_timer = if preserve_early_timer {
            None
        } else {
            let mut timer =
                session_timer_from_response(&response.data.headers, false, timer_epoch)?;
            apply_request_minimum(&mut timer, &call.request.data.headers)?;
            timer
        };
        let preparation = if early_oa_complete {
            Ok(())
        } else if offerless {
            self.media
                .clone()
                .prepare_local_offer(binding.clone(), response.data.body.clone())
                .await
        } else {
            self.media
                .clone()
                .prepare(binding.clone(), remote.clone().unwrap())
                .await
        };
        if let Err(error) = preparation {
            self.end_reliable_early(error.clone(), 488, "Not Acceptable Here")
                .await?;
            return Err(error);
        }
        let call = self.call.as_mut().unwrap();
        call.dialog
            .as_mut()
            .ok_or_else(|| Failure::known("ims_call_dialog_invalid"))?
            .establish()
            .map_err(|_| Failure::known("ims_call_dialog_invalid"))?;
        // Commit answer policy before write so a queued CANCEL cannot undo 200.
        if let Tracker::Incoming(tracker) = &mut call.tracker {
            tracker
                .answered()
                .map_err(|_| Failure::known("ims_call_policy_invalid"))?;
        }
        call.answer_requested = false;
        call.deadline = None;
        // Retain cleanup intent even if the first write is uncertain. TU
        // retries stay disabled until its successful receipt is known.
        let provisional_deadline = Instant::now() + INITIAL_ACK_TIMEOUT;
        self.call.as_mut().unwrap().initial_answer = Some(PendingInitialAnswer {
            response: response.clone(),
            ack_role: if offerless {
                InitialAckRole::AnswerToLocalOffer
            } else {
                InitialAckRole::NoSdp
            },
            deadline: provisional_deadline,
            next_response_retry: provisional_deadline,
            response_retry_interval: Duration::from_millis(500),
            hangup_requested: false,
        });
        let write = self.initial_response(response).await;
        let call = self.call.as_mut().unwrap();
        let pending = call.initial_answer.as_mut().unwrap();
        pending.deadline = call.initial_reply_deadline.unwrap();
        pending.next_response_retry = if write.is_ok() {
            Instant::now() + Duration::from_millis(500)
        } else {
            pending.deadline
        };
        if let Err(error) = write {
            self.fail(&error).await?;
            return Err(error);
        }
        if !preserve_early_timer {
            confirm_session_timer(&mut session_timer, timer_epoch);
            self.call.as_mut().unwrap().session_timer = session_timer;
        }
        self.snapshot.state = "connecting".into();
        if early_oa_complete {
            // Offer/answer already completed in reliable 1xx/PRACK. Final
            // INVITE 200/ACK carry no SDP and never reconfigure the worker.
            self.snapshot.media_ready = true;
            self.snapshot.early_media = true;
            self.observe();
            return Ok(self.snapshot());
        }
        if offerless {
            // Receivers and bounded pre-ACK ingress are ready; no endpoint,
            // codec selection, playback or transmitter is published yet.
            self.snapshot.audio_sample_rate = 16000;
            self.observe();
            return Ok(self.snapshot());
        }
        let remote = remote.unwrap();
        // Initial RX is prepared before the answer. After the accepted write,
        // commit it once and keep the transmitter in receive-only early mode.
        let prepared = async {
            self.media
                .clone()
                .configure(binding.clone(), remote.clone())
                .await?;
            self.media.clone().start(binding, true).await
        }
        .await;
        if let Err(error) = prepared {
            self.fail(&error).await?;
            return Err(error);
        }
        self.call.as_mut().unwrap().media_started = true;
        self.snapshot.media_ready = true;
        self.snapshot.early_media = true;
        self.publish_codec(remote.codec);
        self.observe();
        Ok(self.snapshot())
    }
    pub(crate) async fn reject(&mut self, generation: SessionGeneration) -> Result<Snapshot> {
        self.check(generation)?;
        let call = self
            .call
            .as_ref()
            .ok_or_else(|| Failure::known("ims_call_missing"))?;
        if call.uncertain {
            return Err(Failure::unknown());
        }
        if !matches!(&call.tracker,Tracker::Incoming(t) if matches!(t.phase(),IncomingCallPhase::Trying|IncomingCallPhase::Ringing))
        {
            return Err(Failure::known("ims_call_not_ringing"));
        }
        if let Err(error) = self.reject_pending(486, "Busy Here").await {
            return Err(error);
        }
        Ok(self.snapshot())
    }
    async fn reject_pending(&mut self, status: u16, reason: &str) -> Result<()> {
        let call = self
            .call
            .as_mut()
            .ok_or_else(|| Failure::known("ims_call_missing"))?;
        let Tracker::Incoming(tracker) = &mut call.tracker else {
            return Err(Failure::known("ims_call_not_incoming"));
        };
        if !matches!(
            tracker.phase(),
            IncomingCallPhase::Trying | IncomingCallPhase::Ringing | IncomingCallPhase::Ended
        ) {
            return Err(Failure::known("ims_call_not_ringing"));
        }
        tracker.end();
        self.terminate_incoming(status, reason, Ok(())).await
    }
    /// CANCEL/BYE and the original INVITE are independent transactions. A
    /// failed response to one must not skip the other's final or media drain.
    async fn terminate_incoming(
        &mut self,
        status: u16,
        reason: &str,
        prior: Result<()>,
    ) -> Result<()> {
        let call = self
            .call
            .as_mut()
            .ok_or_else(|| Failure::known("ims_call_missing"))?;
        call.stop_reliable_tu();
        call.answer_requested = false;
        if let Tracker::Incoming(tracker) = &mut call.tracker {
            tracker.end();
        }
        let response = reply_for(
            &call.request,
            status,
            reason,
            Some(&call.local_tag),
            Vec::new(),
            None,
        );
        let final_write = match response {
            Ok(response) => self.initial_response(response).await,
            Err(error) => Err(error),
        };
        let result = match (prior, final_write) {
            (Err(mut error), Err(final_error)) => {
                error.uncertain |= final_error.uncertain;
                Err(error)
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        };
        if let Err(error) = result {
            self.fail(&error).await?;
            return Err(error);
        }
        self.finish("ended").await
    }
    fn transaction_request(&self, method: Method, response: Option<&Response>) -> Result<Request> {
        let call = self
            .call
            .as_ref()
            .ok_or_else(|| Failure::known("ims_call_missing"))?;
        let mut request = call.request.clone();
        request.method = method.clone();
        request.data.body.clear();
        let mut headers = Headers::default();
        for header in request.data.headers.iter() {
            if !matches!(
                header.name().to_ascii_lowercase().as_str(),
                "cseq"
                    | "content-type"
                    | "content-length"
                    | "to"
                    | "contact"
                    | "p-preferred-service"
                    | "p-preferred-identity"
                    | "accept-contact"
                    | "session-expires"
                    | "min-se"
                    | "accept"
            ) {
                headers.push(header.clone());
            }
        }
        let to = if method == Method::ACK {
            response
                .and_then(|r| r.data.headers.get("To"))
                .map(|h| h.value())
                .unwrap_or(single(&call.request.data.headers, "To")?)
        } else {
            single(&call.request.data.headers, "To")?
        };
        push(&mut headers, "To", to)?;
        push(
            &mut headers,
            "CSeq",
            &format!(
                "{} {}",
                cseq(&call.request.data.headers)?.0,
                method.as_str()
            ),
        )?;
        request.data.headers = headers;
        Ok(request)
    }
    fn dialog_request(&mut self, method: Method, rack: Option<String>) -> Result<Request> {
        let via = self.via();
        let call = self
            .call
            .as_mut()
            .ok_or_else(|| Failure::known("ims_call_missing"))?;
        if method == Method::BYE && call.dialog_terminated {
            return Err(Failure::known("ims_call_dialog_terminated"));
        }
        let terminating = method == Method::BYE;
        let dialog = call
            .dialog
            .as_mut()
            .ok_or_else(|| Failure::known("ims_call_dialog_invalid"))?;
        let mut request = if method == Method::ACK {
            dialog.build_ack(via)
        } else if method == Method::PRACK {
            dialog.build_prack(via, rack.unwrap_or_default(), Vec::new())
        } else {
            dialog.build_request(method, via, Vec::new())
        }
        .map_err(|_| Failure::known("ims_call_dialog_invalid"))?;
        if terminating {
            call.dialog_terminated = true;
        }
        self.decorate(&mut request)?;
        Ok(request)
    }
    pub(crate) async fn hangup(&mut self, generation: SessionGeneration) -> Result<Snapshot> {
        self.check(generation)?;
        if self.call.as_ref().is_some_and(|c| c.uncertain) {
            return Err(Failure::unknown());
        }
        match self.hangup_inner().await {
            Ok(snapshot) => Ok(snapshot),
            Err(error) => {
                if !terminal(&self.snapshot.state) {
                    self.fail(&error).await?;
                }
                Err(error)
            }
        }
    }
    async fn hangup_inner(&mut self) -> Result<Snapshot> {
        if terminal(&self.snapshot.state) {
            return Ok(self.snapshot());
        }
        let Some(call) = &mut self.call else {
            return Ok(self.snapshot());
        };
        if call.uncertain {
            return Err(Failure::unknown());
        }
        if let Some(pending) = call.initial_answer.as_mut() {
            if !pending.hangup_requested {
                pending.hangup_requested = true;
                let binding = call.binding.clone();
                self.snapshot.state = "cancelling".into();
                self.media.clone().hold_transmit(binding, true).await?;
                self.observe();
            }
            return Ok(self.snapshot());
        }
        match &mut call.tracker {
            Tracker::Outgoing(tracker) => match tracker
                .hangup()
                .map_err(|_| Failure::known("ims_call_policy_invalid"))?
            {
                OutgoingHangupAction::SendCancel { .. } => {
                    let request = self.transaction_request(Method::CANCEL, None)?;
                    self.send(request).await?;
                    self.snapshot.state = "cancelling".into();
                }
                OutgoingHangupAction::SendBye { .. } => {
                    let request = self.dialog_request(Method::BYE, None)?;
                    self.send(request).await?;
                    self.finish("ended").await?;
                }
                OutgoingHangupAction::Ignore => {}
            },
            Tracker::Incoming(tracker) => {
                if matches!(
                    tracker.phase(),
                    IncomingCallPhase::Trying | IncomingCallPhase::Ringing
                ) {
                    self.reject_pending(486, "Busy Here").await?;
                } else if tracker.phase() != IncomingCallPhase::Ended {
                    let request = self.dialog_request(Method::BYE, None)?;
                    self.send(request).await?;
                    if let Tracker::Incoming(t) = &mut self.call.as_mut().unwrap().tracker {
                        t.end();
                    }
                    self.finish("ended").await?;
                }
            }
        }
        self.observe();
        Ok(self.snapshot())
    }
    pub(crate) async fn dtmf(&mut self, generation: SessionGeneration, digit: char) -> Result<()> {
        self.check(generation)?;
        proletarius_call::dtmf_event(digit).map_err(|_| Failure::known("ims_call_dtmf_invalid"))?;
        let call = self
            .call
            .as_ref()
            .ok_or_else(|| Failure::known("ims_call_missing"))?;
        if self.snapshot.state != "active"
            || call.uncertain
            || call
                .remote
                .as_ref()
                .is_none_or(|r| r.telephone_event_payload.is_none())
        {
            return Err(Failure::known("ims_call_dtmf_unavailable"));
        }
        self.media.clone().dtmf(call.binding.clone(), digit).await
    }
    async fn retired_initial_bye(&mut self, index: usize) -> Result<()> {
        if self.retired[index].dialog_terminated {
            return Ok(());
        }
        let via = self.via();
        let mut bye = self.retired[index]
            .dialog
            .as_mut()
            .ok_or_else(|| Failure::known("ims_call_dialog_invalid"))?
            .build_request(Method::BYE, via, Vec::new())
            .map_err(|_| Failure::known("ims_call_dialog_invalid"))?;
        self.decorate(&mut bye)?;
        self.retired[index].dialog_terminated = true;
        let result = self.send(bye).await;
        if let Err(error) = &result {
            self.retired[index].uncertain |= error.uncertain;
        }
        if let Tracker::Incoming(tracker) = &mut self.retired[index].tracker {
            tracker.end();
        }
        result
    }
    async fn failed_initial_ack(&mut self, error: Failure) -> Result<()> {
        self.revoke_media_bridge();
        let write = match self.dialog_request(Method::BYE, None) {
            Ok(bye) => self.send(bye).await,
            Err(error) => Err(error),
        };
        if let Err(write) = &write {
            self.call.as_mut().unwrap().uncertain |= write.uncertain;
            self.snapshot.uncertain |= write.uncertain;
        }
        // An invalid accepted answer or failed media commit must release all
        // prepared receivers even when the cleanup BYE cannot be confirmed.
        let cleanup = self.fail(&error).await;
        write?;
        cleanup?;
        Err(error)
    }
    async fn acknowledge_initial(
        &mut self,
        connection: SipConnectionId,
        request: &Request,
    ) -> Result<bool> {
        let Ok((seq, method)) = cseq(&request.data.headers) else {
            return Ok(false);
        };
        if method != Method::ACK {
            return Ok(false);
        }
        if let Some(index) = self.retired.iter().position(|call| {
            matches!(call.tracker, Tracker::Incoming(_))
                && connection == call.connection
                && same_dialog(call, request)
                && seq == call.binding.invite_cseq
        }) {
            if let Some(mut pending) = self.retired[index].initial_answer.take() {
                let bye = pending.hangup_requested;
                pending.response.data.body.zeroize();
                if bye {
                    self.retired_initial_bye(index).await?;
                }
            }
            return Ok(true);
        }
        let matched = self.call.as_ref().is_some_and(|call| {
            matches!(call.tracker, Tracker::Incoming(_))
                && connection == call.connection
                && same_dialog(call, request)
                && seq == call.binding.invite_cseq
        });
        if !matched {
            return Ok(false);
        }
        let Some(mut pending) = self.call.as_mut().unwrap().initial_answer.take() else {
            // A matching ACK is proof that an unconfirmed initial 2xx reached
            // the peer. Consume the cleanup intent before writing BYE; never
            // start media or replay an answer whose write was uncertain.
            let call = self.call.as_mut().unwrap();
            let unknown_answer = call.uncertain
                && !call.initial_reply_confirmed
                && !call.dialog_terminated
                && call.dialog.is_some()
                && matches!(&call.tracker, Tracker::Incoming(t) if t.phase() == IncomingCallPhase::Connecting);
            if unknown_answer {
                if let Tracker::Incoming(tracker) = &mut call.tracker {
                    tracker.end();
                }
                let bye = self.dialog_request(Method::BYE, None)?;
                if let Err(error) = self.send(bye).await {
                    self.fail(&error).await?;
                    return Err(error);
                }
            }
            return Ok(true);
        };
        let expired = pending.deadline <= Instant::now();
        let hangup = pending.hangup_requested;
        let answer = if pending.ack_role == InitialAckRole::AnswerToLocalOffer
            && !hangup
            && !expired
            && !self.call.as_ref().unwrap().uncertain
            && !terminal(&self.snapshot.state)
        {
            let content_type = single(&request.data.headers, "Content-Type")
                .ok()
                .and_then(|value| value.split(';').next())
                .map(str::trim)
                .is_some_and(|value| value.eq_ignore_ascii_case("application/sdp"));
            let candidate = if content_type
                && !request.data.body.is_empty()
                && !unsupported_media_direction(&request.data.body)
            {
                self.negotiate_answer(&request.data.body, &pending.response.data.body)
                    .and_then(|remote| {
                        self.proposed_history(&request.data.body)
                            .map(|history| (remote, history))
                    })
                    .ok()
            } else {
                None
            };
            if candidate.is_none() {
                pending.response.data.body.zeroize();
                return self
                    .failed_initial_ack(Failure::known("ims_initial_ack_sdp_invalid"))
                    .await
                    .map(|_| true);
            }
            candidate
        } else {
            None
        };
        pending.response.data.body.zeroize();
        if hangup || (expired && !terminal(&self.snapshot.state)) {
            self.revoke_media_bridge();
            let failed = self.snapshot.state == "failed" || (expired && !hangup);
            let bye = self.dialog_request(Method::BYE, None)?;
            if let Err(error) = self.send(bye).await {
                self.fail(&error).await?;
                return Err(error);
            }
            if let Tracker::Incoming(tracker) = &mut self.call.as_mut().unwrap().tracker {
                tracker.end();
            }
            if expired && !hangup {
                self.snapshot.last_error = "ims_initial_ack_timeout".into();
            }
            self.finish(if failed { "failed" } else { "ended" }).await?;
        } else if !self.call.as_ref().unwrap().uncertain && !terminal(&self.snapshot.state) {
            let binding = self.call.as_ref().unwrap().binding.clone();
            if let Some((remote, history)) = answer {
                if let Err(error) = self
                    .media
                    .clone()
                    .configure(binding.clone(), remote.clone())
                    .await
                {
                    return self.failed_initial_ack(error).await.map(|_| true);
                }
                self.publish_codec(remote.codec);
                let call = self.call.as_mut().unwrap();
                call.remote = Some(remote);
                call.payload_history = history;
            }
            if let Err(error) = self.media.clone().start(binding, false).await {
                return self.failed_initial_ack(error).await.map(|_| true);
            }
            self.call.as_mut().unwrap().media_started = true;
            if let Tracker::Incoming(tracker) = &mut self.call.as_mut().unwrap().tracker {
                tracker
                    .acknowledged()
                    .map_err(|_| Failure::known("ims_call_policy_invalid"))?;
            }
            self.snapshot.state = "active".into();
            self.snapshot.media_ready = true;
            self.snapshot.early_media = false;
            self.snapshot.started_at = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
            self.observe();
        }
        Ok(true)
    }
    async fn tick_initial_answers(&mut self, now: Instant) -> Result<()> {
        let expired = self
            .call
            .as_ref()
            .and_then(|call| call.initial_answer.as_ref())
            .is_some_and(|pending| pending.deadline <= now);
        if expired {
            let mut pending = self.call.as_mut().unwrap().initial_answer.take().unwrap();
            let end_dialog = pending.hangup_requested || !terminal(&self.snapshot.state);
            let cancelled = self.snapshot.state == "cancelling";
            pending.response.data.body.zeroize();
            if end_dialog {
                let bye = self.dialog_request(Method::BYE, None)?;
                if let Err(error) = self.send(bye).await {
                    self.fail(&error).await?;
                    return Err(error);
                }
                if let Tracker::Incoming(tracker) = &mut self.call.as_mut().unwrap().tracker {
                    tracker.end();
                }
                if !cancelled && self.snapshot.last_error.is_empty() {
                    self.snapshot.last_error = "ims_initial_ack_timeout".into();
                }
                self.finish(if cancelled { "ended" } else { "failed" })
                    .await?;
            }
        }
        let retry = self.call.as_ref().and_then(|call| {
            call.initial_answer
                .as_ref()
                .filter(|pending| pending.next_response_retry <= now && now < pending.deadline)
                .map(|pending| (call.connection, pending.response.clone()))
        });
        if let Some((connection, response)) = retry {
            if let Err(error) = self.respond(connection, response).await {
                // The original 200 remains known. Stop retrying on this
                // transport, keep its transaction identity and fence media.
                if let Some(pending) = self.call.as_mut().unwrap().initial_answer.as_mut() {
                    pending.next_response_retry = pending.deadline;
                }
                if !terminal(&self.snapshot.state) {
                    self.fail(&error).await?;
                }
                return Err(error);
            }
            let pending = self.call.as_mut().unwrap().initial_answer.as_mut().unwrap();
            pending.response_retry_interval =
                (pending.response_retry_interval * 2).min(Duration::from_secs(4));
            pending.next_response_retry = now + pending.response_retry_interval;
        }
        for index in 0..self.retired.len() {
            let expired = self.retired[index]
                .initial_answer
                .as_ref()
                .is_some_and(|pending| pending.deadline <= now);
            if expired {
                let mut pending = self.retired[index].initial_answer.take().unwrap();
                let bye = pending.hangup_requested;
                pending.response.data.body.zeroize();
                if bye {
                    self.retired_initial_bye(index).await?;
                }
            }
            let retry = self.retired[index]
                .initial_answer
                .as_ref()
                .filter(|pending| pending.next_response_retry <= now && now < pending.deadline)
                .map(|pending| pending.response.clone());
            if let Some(response) = retry {
                if let Err(error) = self.respond(self.retired[index].connection, response).await {
                    if let Some(pending) = self.retired[index].initial_answer.as_mut() {
                        pending.next_response_retry = pending.deadline;
                    }
                    return Err(error);
                }
                let pending = self.retired[index].initial_answer.as_mut().unwrap();
                pending.response_retry_interval =
                    (pending.response_retry_interval * 2).min(Duration::from_secs(4));
                pending.next_response_retry = now + pending.response_retry_interval;
            }
        }
        self.prune_initial_replies(now);
        Ok(())
    }
    /// Send cleanup on the dialog that owned the transaction. A late ACK or
    /// timer on a retired call must never borrow the new call's route/media.
    async fn accepted_reinvite_bye(&mut self, retired: Option<usize>, seq: u32) -> Result<()> {
        let call = match retired {
            Some(index) => &mut self.retired[index],
            None => self.call.as_mut().unwrap(),
        };
        if call.dialog_terminated {
            return Ok(());
        }
        if let Some(accepted) = call.accepted_reinvites.back()
            && accepted.cseq == seq
            && !accepted.confirmed
            && let Some(target) = &accepted.cleanup_target
            && let Some(dialog) = &mut call.dialog
        {
            // The write may have reached the peer. Use its candidate target
            // for this dialog's one bounded cleanup, never for live media.
            dialog.remote_target = target.clone();
        }
        if let Some(index) = retired {
            return self.retired_initial_bye(index).await;
        }
        let bye = self.dialog_request(Method::BYE, None)?;
        let result = self.send(bye).await;
        if let Err(error) = &result {
            self.call.as_mut().unwrap().uncertain |= error.uncertain;
            self.snapshot.uncertain |= error.uncertain;
        }
        if let Tracker::Incoming(tracker) = &mut self.call.as_mut().unwrap().tracker {
            tracker.end();
        }
        result
    }
    /// Return false only when the first valid ACK still owes a live SDP
    /// commit. ACK uses dialog/connection/CSeq, not the INVITE Via branch.
    async fn acknowledge_reinvite(
        &mut self,
        connection: SipConnectionId,
        request: &Request,
    ) -> Result<bool> {
        let Ok((seq, Method::ACK)) = cseq(&request.data.headers) else {
            return Ok(false);
        };
        let current = self.call.as_ref().is_some_and(|call| {
            call.connection == connection
                && same_dialog(call, request)
                && call.accepted_reinvites.iter().any(|a| a.cseq == seq)
        });
        let retired = if current {
            None
        } else if let Some(index) = self.retired.iter().position(|call| {
            call.connection == connection
                && same_dialog(call, request)
                && call.accepted_reinvites.iter().any(|a| a.cseq == seq)
        }) {
            Some(index)
        } else {
            return Ok(false);
        };
        let call = match retired {
            Some(index) => &mut self.retired[index],
            None => self.call.as_mut().unwrap(),
        };
        let accepted = call
            .accepted_reinvites
            .iter_mut()
            .find(|a| a.cseq == seq)
            .unwrap();
        if accepted.acked {
            return Ok(true);
        }
        accepted.acked = true;
        accepted.next_response_retry = None;
        let expired = accepted.deadline <= Instant::now();
        let cleanup = (accepted.cleanup_on_ack || expired) && !call.dialog_terminated;
        let live_commit = current
            && !cleanup
            && !call.dialog_terminated
            && !call.uncertain
            && !terminal(&self.snapshot.state)
            && call
                .pending_reinvite
                .as_ref()
                .is_some_and(|p| p.cseq == seq);
        if cleanup {
            self.finish_reinvite_cleanup(retired, seq).await?;
        }
        Ok(!live_commit)
    }
    async fn tick_accepted_reinvites(&mut self, now: Instant) -> Result<()> {
        // None denotes the current call. Retired indexes stay stable here;
        // pruning individual records does not evict a call during this pass.
        let owners = std::iter::once(None)
            .chain((0..self.retired.len()).map(Some))
            .collect::<Vec<_>>();
        for owner in owners {
            let Some(call) = (match owner {
                Some(index) => self.retired.get(index),
                None => self.call.as_ref(),
            }) else {
                continue;
            };
            let sequences = call
                .accepted_reinvites
                .iter()
                .map(|a| a.cseq)
                .collect::<Vec<_>>();
            for seq in sequences {
                let call = match owner {
                    Some(index) => &self.retired[index],
                    None => self.call.as_ref().unwrap(),
                };
                let accepted = call
                    .accepted_reinvites
                    .iter()
                    .find(|a| a.cseq == seq)
                    .unwrap();
                if accepted.deadline <= now {
                    if !accepted.acked && !call.dialog_terminated {
                        self.finish_reinvite_cleanup(owner, seq).await?;
                    }
                    continue;
                }
                if !accepted.confirmed
                    || accepted.acked
                    || accepted.next_response_retry.is_none_or(|at| at > now)
                {
                    continue;
                }
                let result = self
                    .respond(call.connection, accepted.response.clone())
                    .await;
                let call = match owner {
                    Some(index) => &mut self.retired[index],
                    None => self.call.as_mut().unwrap(),
                };
                let accepted = call
                    .accepted_reinvites
                    .iter_mut()
                    .find(|a| a.cseq == seq)
                    .unwrap();
                if let Err(error) = result {
                    // The first 200 remains known, but this transport cannot
                    // safely retry. Keep its identity and once-only cleanup.
                    accepted.next_response_retry = None;
                    accepted.cleanup_on_ack = true;
                    call.uncertain |= error.uncertain;
                    if owner.is_none() && !terminal(&self.snapshot.state) {
                        self.fail(&error).await?;
                    }
                    return Err(error);
                }
                accepted.response_retry_interval =
                    (accepted.response_retry_interval * 2).min(Duration::from_secs(4));
                accepted.next_response_retry = Some(now + accepted.response_retry_interval);
            }
        }
        self.prune_initial_replies(now);
        Ok(())
    }
    pub(crate) async fn tick(&mut self, generation: SessionGeneration, now: Instant) -> Result<()> {
        self.check(generation)?;
        self.prune_completed_invites(now);
        // A confirmed native exit takes precedence over fresh provisional
        // retries. Independent retained transaction timers still run on later
        // terminal ticks; no broken codec may renew early-media readiness.
        if !terminal(&self.snapshot.state)
            && let Some(call) = &self.call
            && call.media_allocated
        {
            // Advancing only after an actual health check keeps snapshot and
            // SIP/control traffic from indefinitely postponing this deadline.
            self.media_check_deadline = now + MEDIA_CHECK_INTERVAL;
            if let Some(error) = self.media.terminal_failure(&call.binding) {
                let early_incoming = matches!(&call.tracker, Tracker::Incoming(t)
                    if matches!(t.phase(), IncomingCallPhase::Trying | IncomingCallPhase::Ringing));
                // Network writes may await a transport receipt. Revoke host
                // audio immediately on confirmed media failure, without
                // waiting for that receipt or releasing the media allocation.
                self.revoke_media_bridge();
                if early_incoming {
                    return self
                        .end_reliable_early(error, 500, "Server Internal Error")
                        .await;
                }
                if self.snapshot.state == "active" {
                    let bye = self.dialog_request(Method::BYE, None)?;
                    if let Err(write) = self.send(bye).await {
                        self.fail(&write).await?;
                        return Err(write);
                    }
                }
                self.fail(&error).await?;
                return Ok(());
            }
        }
        self.tick_ussd(now).await?;
        // Accepted INVITE transactions outlive the associated media/dialog,
        // including when a newer call owns the live media allocation.
        self.tick_initial_answers(now).await?;
        self.tick_accepted_reinvites(now).await?;
        self.tick_reliable_provisionals(now).await?;
        if terminal(&self.snapshot.state) {
            // A failed stop retains the allocation fence for an explicit
            // cleanup retry; it must not restart SIP timers or send BYE again.
            return Ok(());
        }
        if self
            .call
            .as_ref()
            .and_then(|call| call.session_timer.as_ref())
            .is_some_and(|timer| timer.termination_at <= now)
        {
            if self.call.as_ref().is_some_and(|call| {
                matches!(&call.tracker,
                Tracker::Incoming(tracker) if matches!(tracker.phase(),
                    IncomingCallPhase::Trying | IncomingCallPhase::Ringing))
            }) {
                return self
                    .end_reliable_early(
                        Failure::known("ims_session_expired"),
                        408,
                        "Request Timeout",
                    )
                    .await;
            }
            let bye = self.dialog_request(Method::BYE, None)?;
            if let Err(error) = self.send(bye).await {
                self.fail(&error).await?;
                return Err(error);
            }
            self.snapshot.last_error = "ims_session_expired".into();
            self.finish("failed").await?;
            return Ok(());
        }
        if self
            .call
            .as_ref()
            .and_then(|call| call.session_timer.as_ref())
            .and_then(|timer| timer.pending.as_ref())
            .is_some_and(|pending| pending.deadline <= now)
        {
            let bye = self.dialog_request(Method::BYE, None)?;
            if let Err(error) = self.send(bye).await {
                self.fail(&error).await?;
                return Err(error);
            }
            self.snapshot.last_error = "ims_session_refresh_timeout".into();
            self.finish("failed").await?;
            return Ok(());
        }
        let refresh_due = self.snapshot.state == "active"
            && self.call.as_ref().is_some_and(|call| {
                call.session_timer.as_ref().is_some_and(|timer| {
                    timer.local_refresher && timer.refresh_at.is_some_and(|at| at <= now)
                })
            });
        if refresh_due {
            if self
                .call
                .as_ref()
                .is_some_and(|call| call.pending_reinvite.is_some())
            {
                self.call
                    .as_mut()
                    .unwrap()
                    .session_timer
                    .as_mut()
                    .unwrap()
                    .refresh_at = Some(now + Duration::from_secs(1));
            } else {
                let timer = self.call.as_ref().unwrap().session_timer.as_ref().unwrap();
                let (interval, min_se) = (timer.interval, timer.min_se);
                let (method, local_sdp) = if self.call.as_ref().unwrap().peer_allows_update {
                    (Method::UPDATE, None)
                } else {
                    let sdp = &self.call.as_ref().unwrap().local_sdp;
                    if sdp.is_empty() {
                        self.end_expired_session("ims_session_refresh_sdp_unavailable")
                            .await?;
                        return Ok(());
                    }
                    (Method::INVITE, Some(sdp.clone()))
                };
                let mut update = self.dialog_request(method, None)?;
                if let Some(sdp) = local_sdp {
                    update.data.body = sdp;
                    push(&mut update.data.headers, "Content-Type", "application/sdp")?;
                }
                push(
                    &mut update.data.headers,
                    "Session-Expires",
                    &format!("{interval};refresher=uac"),
                )?;
                push(&mut update.data.headers, "Min-SE", &min_se.to_string())?;
                let pending_request = update.clone();
                if let Err(error) = self.send(update).await {
                    self.fail(&error).await?;
                    return Err(error);
                }
                let timer = self.call.as_mut().unwrap().session_timer.as_mut().unwrap();
                timer.refresh_at = None;
                timer.pending = Some(PendingSessionRefresh {
                    request: pending_request,
                    deadline: now + SESSION_REFRESH_TIMEOUT,
                });
            }
        }
        if self
            .call
            .as_ref()
            .is_none_or(|c| c.deadline.is_none_or(|d| d > now))
        {
            return Ok(());
        }
        if let Some(Call {
            tracker: Tracker::Incoming(tracker),
            binding,
            ..
        }) = &mut self.call
        {
            if tracker.timeout(binding.invite_cseq) == IncomingTimeoutDecision::Expire {
                self.reject_pending(480, "Temporarily Unavailable").await?;
            }
        } else {
            let cancelled = self.snapshot.state == "cancelling";
            if let Some(Call {
                tracker: Tracker::Outgoing(tracker),
                ..
            }) = &mut self.call
            {
                if matches!(tracker.phase(), CallPhase::Inviting | CallPhase::Early) {
                    tracker
                        .cancel()
                        .map_err(|_| Failure::known("ims_call_policy_invalid"))?;
                }
            }
            self.finish(if cancelled { "ended" } else { "failed" })
                .await?;
            if !cancelled {
                self.snapshot.last_error = "ims_invite_timeout".into();
                self.observe();
            }
        }
        Ok(())
    }
    pub(crate) async fn shutdown(&mut self, generation: SessionGeneration) -> Result<()> {
        if self.generation != generation {
            return Err(Failure::known("ims_call_stale_generation"));
        }
        self.shutdown_ussd();
        self.stopping = true; // Actor shutdown closes signaling; never emit extra BYE here.
        self.clear_completed_invites();
        let cleanup = self.finish("ended").await;
        for call in self.call.iter_mut().chain(self.retired.iter_mut()) {
            call.clear_reliable();
            if let Some(mut response) = call.initial_reply.take() {
                response.data.body.zeroize();
            }
            if let Some(mut pending) = call.initial_answer.take() {
                pending.response.data.body.zeroize();
            }
            call.initial_reply_deadline = None;
            call.initial_reply_confirmed = false;
            call.accepted_reinvites.clear();
        }
        self.snapshot.signaling_ready = false;
        self.observe();
        cleanup
    }
    pub(crate) async fn handle_event(&mut self, event: &Event) -> Result<bool> {
        self.check(event.generation)?;
        if self.handle_ussd(event).await? {
            return Ok(true);
        }
        let RegisteredSipEvent::Message {
            connection,
            message,
            ..
        } = &event.event
        else {
            return Ok(false);
        };
        match message.as_ref() {
            RegisteredSipMessage::Request {
                method,
                uri,
                version,
                headers,
                body,
            } => {
                if !matches!(
                    *method,
                    Method::INVITE
                        | Method::ACK
                        | Method::CANCEL
                        | Method::BYE
                        | Method::PRACK
                        | Method::UPDATE
                        | Method::INFO
                ) {
                    return Ok(false);
                }
                let request = Request {
                    method: method.clone(),
                    uri: uri.clone(),
                    data: MessageData::new(version.clone(), headers.clone(), body.to_vec()),
                };
                self.request(*connection, request).await?;
                Ok(true)
            }
            RegisteredSipMessage::Response {
                status,
                reason,
                version,
                headers,
                body,
            } => {
                let Ok(id) = single(headers, "Call-ID") else {
                    return Ok(false);
                };
                let Ok((_, method)) = cseq(headers) else {
                    return Ok(false);
                };
                if method == Method::UPDATE {
                    let response = Response {
                        status: *status,
                        reason: reason.clone(),
                        data: MessageData::new(version.clone(), headers.clone(), body.to_vec()),
                    };
                    return self.session_refresh_response(*connection, response).await;
                }
                if method != Method::INVITE {
                    return Ok(false);
                }
                let response = Response {
                    status: *status,
                    reason: reason.clone(),
                    data: MessageData::new(version.clone(), headers.clone(), body.to_vec()),
                };
                if self.call.as_ref().is_some_and(|call| {
                    call.session_timer
                        .as_ref()
                        .and_then(|timer| timer.pending.as_ref())
                        .is_some_and(|pending| pending.request.method == Method::INVITE)
                        || call.last_refresh_ack.is_some()
                        || call.late_refresh_invite.is_some()
                }) && self
                    .session_refresh_response(*connection, response.clone())
                    .await?
                {
                    return Ok(true);
                }
                if self.call.as_ref().is_some_and(|c| {
                    c.binding.call_id == id && matches!(c.tracker, Tracker::Outgoing(_))
                }) {
                    if *connection != SipConnectionId::Primary {
                        return Ok(false);
                    }
                    self.invite_response(response).await?;
                    Ok(true)
                } else {
                    if let Some(index) = self.retired.iter().position(|c| {
                        c.binding.call_id == id
                            && (c.last_refresh_ack.is_some()
                                || c.late_refresh_invite.is_some()
                                || c.session_timer.as_ref().is_some_and(|timer| {
                                    timer.pending.as_ref().is_some_and(|pending| {
                                        pending.request.method == Method::INVITE
                                    })
                                }))
                    }) {
                        let old = self.retired.remove(index).unwrap();
                        let current = self.call.replace(old);
                        let saved = self.snapshot();
                        let observer = self.observer.take();
                        let result = self
                            .session_refresh_response(*connection, response.clone())
                            .await;
                        self.observer = observer;
                        let old = self.call.take().unwrap();
                        self.call = current;
                        self.retired.insert(index, old);
                        self.snapshot = saved;
                        if result? {
                            return Ok(true);
                        }
                    }
                    if let Some(index) = self.retired.iter().position(|c| {
                        c.binding.call_id == id && matches!(c.tracker, Tracker::Outgoing(_))
                    }) {
                        // Retransmitted final responses still need their own ACK,
                        // without attaching old media to a newly admitted call.
                        if *connection != SipConnectionId::Primary {
                            return Ok(false);
                        }
                        let old = self.retired.remove(index).unwrap();
                        let current = self.call.replace(old);
                        let saved = self.snapshot();
                        let observer = self.observer.take();
                        let result = self.invite_response(response).await;
                        self.observer = observer;
                        let old = self.call.take().unwrap();
                        self.call = current;
                        self.retired.insert(index, old);
                        self.snapshot = saved;
                        result?;
                        Ok(true)
                    } else {
                        Ok(false)
                    }
                }
            }
        }
    }
    async fn session_refresh_response(
        &mut self,
        connection: SipConnectionId,
        response: Response,
    ) -> Result<bool> {
        let Some(call) = self.call.as_ref() else {
            return Ok(false);
        };
        if let Some((original, ack)) = &call.last_refresh_ack
            && refresh_response_matches(call, original, connection, &response)
            && response.status >= 200
        {
            self.send(ack.clone()).await?;
            return Ok(true);
        }
        if let Some(original) = &call.late_refresh_invite
            && refresh_response_matches(call, original, connection, &response)
        {
            if response.status < 200 {
                return Ok(true);
            }
            let original = original.clone();
            let ack = if response.status < 300 {
                self.dialog_request(Method::ACK, None)?
            } else {
                refresh_transaction_ack(&original, &response)?
            };
            self.send(ack.clone()).await?;
            let call = self.call.as_mut().unwrap();
            if let Some(mut pending) = call.late_refresh_invite.take() {
                pending.data.body.zeroize();
            }
            if let Some((mut previous, mut old_ack)) =
                call.last_refresh_ack.replace((original, ack))
            {
                previous.data.body.zeroize();
                old_ack.data.body.zeroize();
            }
            return Ok(true);
        }
        let Some(pending) = call
            .session_timer
            .as_ref()
            .and_then(|timer| timer.pending.as_ref())
        else {
            return Ok(false);
        };
        if !refresh_response_matches(call, &pending.request, connection, &response) {
            return Ok(false);
        }
        if response.status < 200 {
            return Ok(true);
        }
        let refresh_request = pending.request.clone();
        let is_invite = refresh_request.method == Method::INVITE;
        let mut accepted_media_failure = None;
        if is_invite && (200..300).contains(&response.status) {
            let content_type = single(&response.data.headers, "Content-Type")
                .ok()
                .and_then(|value| value.split(';').next())
                .map(str::trim)
                .is_some_and(|value| value.eq_ignore_ascii_case("application/sdp"));
            let candidate = if content_type && !unsupported_media_direction(&response.data.body) {
                self.negotiate_answer(&response.data.body, &refresh_request.data.body)
                    .ok()
                    .filter(|_| self.proposed_history(&response.data.body).is_ok())
            } else {
                None
            };
            let changed = candidate
                .as_ref()
                .is_none_or(|remote| self.call.as_ref().unwrap().remote.as_ref() != Some(remote));
            if changed {
                let binding = self.call.as_ref().unwrap().binding.clone();
                // The answer is effective on receipt, independently of ACK
                // transport latency. Stop old-format TX while allocating, then
                // commit the new media before awaiting the mandatory ACK write.
                let hold = self
                    .media
                    .clone()
                    .hold_transmit(binding.clone(), true)
                    .await;
                if let Some(remote) = candidate {
                    let prepared = if hold.is_ok() {
                        self.media
                            .clone()
                            .prepare(binding.clone(), remote.clone())
                            .await
                    } else {
                        hold
                    };
                    let committed = if prepared.is_ok() {
                        self.media
                            .clone()
                            .configure(binding.clone(), remote.clone())
                            .await
                    } else {
                        prepared
                    };
                    let resumed = if committed.is_ok() {
                        self.media.clone().hold_transmit(binding, false).await
                    } else {
                        committed
                    };
                    if resumed.is_ok() {
                        self.call.as_mut().unwrap().payload_history =
                            self.proposed_history(&response.data.body)?;
                        self.call.as_mut().unwrap().remote = Some(remote.clone());
                        self.publish_codec(remote.codec);
                        self.observe();
                    } else if let Err(error) = resumed {
                        self.snapshot.uncertain |= error.uncertain;
                        self.call.as_mut().unwrap().uncertain |= error.uncertain;
                        accepted_media_failure = Some(error.code);
                    }
                } else {
                    accepted_media_failure = Some("ims_session_refresh_sdp_invalid");
                }
            }
        }
        if is_invite && (200..300).contains(&response.status) {
            if let Ok(Some(target)) = contact_target(&response.data.headers) {
                self.call
                    .as_mut()
                    .unwrap()
                    .dialog
                    .as_mut()
                    .unwrap()
                    .remote_target = target;
            }
        }
        if accepted_media_failure.is_some() {
            self.revoke_media_bridge();
        }
        if is_invite {
            let ack = if response.status < 300 {
                self.dialog_request(Method::ACK, None)?
            } else {
                refresh_transaction_ack(&refresh_request, &response)?
            };
            if let Err(error) = self.send(ack.clone()).await {
                self.fail(&error).await?;
                return Err(error);
            }
            let call = self.call.as_mut().unwrap();
            if let Some((mut previous, mut old_ack)) = call
                .last_refresh_ack
                .replace((refresh_request.clone(), ack))
            {
                previous.data.body.zeroize();
                old_ack.data.body.zeroize();
            }
        }
        let now = Instant::now();
        if response.status < 300 {
            if let Some(code) = accepted_media_failure {
                self.end_expired_session(code).await?;
                return Ok(true);
            }
            if is_invite {
                let content_type = single(&response.data.headers, "Content-Type")
                    .ok()
                    .and_then(|value| value.split(';').next())
                    .map(str::trim)
                    .is_some_and(|value| value.eq_ignore_ascii_case("application/sdp"));
                let candidate = if content_type && !response.data.body.is_empty() {
                    self.negotiate_answer(&response.data.body, &refresh_request.data.body)
                        .ok()
                } else {
                    None
                };
                let compatible = candidate.is_some()
                    && self.proposed_history(&response.data.body).is_ok()
                    && !unsupported_media_direction(&response.data.body);
                if !compatible {
                    self.end_expired_session("ims_session_refresh_sdp_invalid")
                        .await?;
                    return Ok(true);
                }
                let remote = candidate.unwrap();
                let changed = self.call.as_ref().unwrap().remote.as_ref() != Some(&remote);
                if changed {
                    let binding = self.call.as_ref().unwrap().binding.clone();
                    if let Err(error) = self.media.clone().prepare(binding, remote.clone()).await {
                        self.end_expired_session("ims_session_refresh_media_invalid")
                            .await?;
                        return Err(error);
                    }
                    if let Err(error) = self.commit_media(remote.clone()).await {
                        self.fail(&error).await?;
                        return Err(error);
                    }
                    self.call.as_mut().unwrap().remote = Some(remote);
                }
                self.call.as_mut().unwrap().payload_history =
                    self.proposed_history(&response.data.body)?;
                if let Ok(Some(target)) = contact_target(&response.data.headers) {
                    self.call
                        .as_mut()
                        .unwrap()
                        .dialog
                        .as_mut()
                        .unwrap()
                        .remote_target = target;
                }
            }
            let mut timer = match session_timer_from_response(&response.data.headers, true, now) {
                Ok(Some(timer)) => timer,
                Err(_) => {
                    self.end_expired_session("ims_session_timer_invalid")
                        .await?;
                    return Ok(true);
                }
                Ok(None) => {
                    self.call.as_mut().unwrap().session_timer = None;
                    return Ok(true);
                }
            };
            let minimum = self
                .call
                .as_ref()
                .unwrap()
                .session_timer
                .as_ref()
                .unwrap()
                .min_se;
            if timer.interval < minimum {
                self.end_expired_session("ims_session_timer_invalid")
                    .await?;
                return Ok(true);
            }
            timer.min_se = minimum;
            self.call.as_mut().unwrap().session_timer = Some(timer);
            return Ok(true);
        }
        if response.status == 422 {
            let minimum = single(&response.data.headers, "Min-SE")
                .ok()
                .and_then(|value| value.parse::<u32>().ok());
            let timer = self.call.as_mut().unwrap().session_timer.as_mut().unwrap();
            if let Some(minimum) = minimum.filter(|minimum| {
                *minimum > timer.interval
                    && *minimum <= SESSION_MAX_INTERVAL
                    && timer.retry_count == 0
            }) {
                timer.interval = minimum;
                timer.min_se = minimum;
                timer.retry_count = 1;
                timer.pending = None;
                timer.refresh_at = Some(now);
                return Ok(true);
            }
        }
        if refresh_request.method == Method::UPDATE && matches!(response.status, 405 | 501) {
            let call = self.call.as_mut().unwrap();
            call.peer_allows_update = false;
            let timer = call.session_timer.as_mut().unwrap();
            timer.pending = None;
            timer.refresh_at = Some(now);
            return Ok(true);
        }
        if matches!(response.status, 408 | 481) {
            self.end_expired_session("ims_session_refresh_rejected")
                .await?;
            return Ok(true);
        }
        let timer = self.call.as_mut().unwrap().session_timer.as_mut().unwrap();
        timer.pending = None;
        if timer.retry_count == 0 && now + Duration::from_secs(5) < timer.termination_at {
            timer.retry_count = 1;
            timer.refresh_at = Some(now + Duration::from_secs(5));
        }
        Ok(true)
    }
    async fn end_expired_session(&mut self, code: &'static str) -> Result<()> {
        self.revoke_media_bridge();
        let bye = self.dialog_request(Method::BYE, None)?;
        if let Err(error) = self.send(bye).await {
            self.fail(&error).await?;
            return Err(error);
        }
        self.snapshot.last_error = code.into();
        self.finish("failed").await
    }
    async fn request(&mut self, connection: SipConnectionId, request: Request) -> Result<()> {
        // A negative INVITE ACK belongs to its Via branch, unlike a 2xx ACK.
        // Classify it before the Accepted/dialog paths can consume its CSeq.
        if self.handle_completed_invite(connection, &request).await? {
            return Ok(());
        }
        if matches!(request.method, Method::INVITE | Method::CANCEL)
            && let Ok((seq, method)) = cseq(&request.data.headers)
            && method == request.method
            && let Some(call) = self.call.iter().chain(self.retired.iter()).find(|call| {
                same_dialog(call, &request)
                    && call.accepted_reinvites.iter().any(|accepted| {
                        accepted.cseq == seq
                            && same_via(&request.data.headers, &accepted.response.data.headers)
                    })
            })
        {
            if connection != call.connection {
                let response = reply_for(
                    &request,
                    481,
                    "Call/Transaction Does Not Exist",
                    None,
                    Vec::new(),
                    None,
                )?;
                return if request.method == Method::INVITE {
                    self.completed_response(connection, &request, response)
                        .await
                } else {
                    self.respond(connection, response).await
                };
            }
            // Accepted INVITE retransmissions are absorbed. A late CANCEL
            // acknowledges its transaction without changing the accepted SDP.
            if request.method == Method::CANCEL {
                return self
                    .respond(
                        connection,
                        reply_for(&request, 200, "OK", None, Vec::new(), None)?,
                    )
                    .await;
            }
            return Ok(());
        }
        if request.method == Method::INVITE {
            if self
                .call
                .as_ref()
                .is_some_and(|call| same_dialog(call, &request))
            {
                return self.in_dialog_invite(connection, request).await;
            }
            if single(&request.data.headers, "To")
                .ok()
                .and_then(tag)
                .is_some()
            {
                return self
                    .completed_response(
                        connection,
                        &request,
                        reply_for(
                            &request,
                            481,
                            "Call/Transaction Does Not Exist",
                            None,
                            Vec::new(),
                            None,
                        )?,
                    )
                    .await;
            }
            return self.incoming(connection, request).await;
        }
        if request.method == Method::PRACK {
            return self.handle_reliable_prack(connection, request).await;
        }
        let matches = self.call.as_ref().is_some_and(|c| same_dialog(c, &request));
        if request.method == Method::CANCEL {
            let transaction_matches = |c: &Call| {
                connection == c.connection
                    && matches!(c.tracker, Tracker::Incoming(_))
                    && same_transaction(c, &request)
                    && request.uri == c.request.uri
                    && single(&request.data.headers, "To").ok().and_then(tag)
                        == single(&c.request.data.headers, "To").ok().and_then(tag)
                    && cseq(&request.data.headers).is_ok_and(|(_, method)| method == Method::CANCEL)
                    && (c
                        .initial_reply_deadline
                        .is_some_and(|at| at > Instant::now())
                        || matches!(&c.tracker, Tracker::Incoming(t) if matches!(t.phase(), IncomingCallPhase::Trying | IncomingCallPhase::Ringing)))
            };
            let matched = self
                .call
                .iter()
                .chain(self.retired.iter())
                .any(transaction_matches);
            let accept = self.call.as_mut().is_some_and(|c| {
                if !transaction_matches(c) {
                    return false;
                }
                if let Tracker::Incoming(t) = &mut c.tracker {
                    t.cancel(c.binding.invite_cseq) == IncomingCancelDecision::Accept
                } else {
                    false
                }
            });
            let write = self
                .completed_cancel_response(
                    connection,
                    &request,
                    reply_for(
                        &request,
                        if matched { 200 } else { 481 },
                        if matched {
                            "OK"
                        } else {
                            "Call/Transaction Does Not Exist"
                        },
                        None,
                        Vec::new(),
                        None,
                    )?,
                )
                .await;
            if accept {
                return self
                    .terminate_incoming(487, "Request Terminated", write)
                    .await;
            }
            write?;
            return Ok(());
        }
        if request.method == Method::ACK {
            if self.acknowledge_reinvite(connection, &request).await? {
                return Ok(());
            }
            if self.acknowledge_initial(connection, &request).await? {
                return Ok(());
            }
            if matches && connection == self.call.as_ref().unwrap().connection {
                let (seq, cseq_method) = cseq(&request.data.headers)?;
                if cseq_method != Method::ACK {
                    return Ok(());
                }
                let pending = self.call.as_mut().and_then(|call| {
                    if call
                        .pending_reinvite
                        .as_ref()
                        .is_some_and(|p| p.cseq == seq)
                    {
                        call.pending_reinvite
                            .take()
                            .map(|p| (call.binding.clone(), p))
                    } else {
                        None
                    }
                });
                if let Some((binding, mut pending)) = pending {
                    let remote = if pending.offerless {
                        let content_type = single(&request.data.headers, "Content-Type")
                            .ok()
                            .and_then(|value| value.split(';').next())
                            .map(str::trim)
                            .is_some_and(|value| value.eq_ignore_ascii_case("application/sdp"));
                        let candidate = if content_type && !request.data.body.is_empty() {
                            self.negotiate_answer(&request.data.body, &pending.response.data.body)
                                .ok()
                        } else {
                            None
                        };
                        if let Some(remote) = candidate.filter(|_| {
                            self.proposed_history(&request.data.body).is_ok()
                                && !unsupported_media_direction(&request.data.body)
                        }) {
                            remote
                        } else {
                            pending.response.data.body.zeroize();
                            self.revoke_media_bridge();
                            let bye = self.dialog_request(Method::BYE, None)?;
                            if let Err(error) = self.send(bye).await {
                                self.fail(&error).await?;
                                return Err(error);
                            }
                            self.snapshot.last_error = "ims_reinvite_ack_sdp_invalid".into();
                            self.finish("failed").await?;
                            return Ok(());
                        }
                    } else {
                        pending.remote.clone()
                    };
                    if pending.offerless {
                        if let Err(error) =
                            self.media.clone().prepare(binding, remote.clone()).await
                        {
                            pending.response.data.body.zeroize();
                            self.end_expired_session("ims_reinvite_ack_media_invalid")
                                .await?;
                            return Err(error);
                        }
                        self.commit_media(remote.clone()).await?;
                        self.call.as_mut().unwrap().payload_history =
                            self.proposed_history(&request.data.body)?;
                    }
                    let call = self.call.as_mut().unwrap();
                    call.remote = Some(remote);
                    call.local_sdp_version = pending.local_sdp_version;
                    call.local_sdp.zeroize();
                    call.local_sdp = pending.response.data.body.clone();
                    pending.response.data.body.zeroize();
                    self.observe();
                    return Ok(());
                }
            }
            return Ok(());
        }
        if !matches || connection != self.call.as_ref().unwrap().connection {
            self.respond(
                connection,
                reply_for(
                    &request,
                    481,
                    "Call/Transaction Does Not Exist",
                    None,
                    Vec::new(),
                    None,
                )?,
            )
            .await?;
            return Ok(());
        }
        if matches!(request.method, Method::UPDATE | Method::PRACK)
            && (self.call.as_ref().unwrap().uncertain || terminal(&self.snapshot.state))
        {
            return self
                .respond(
                    connection,
                    reply_for(
                        &request,
                        481,
                        "Call/Transaction Does Not Exist",
                        None,
                        Vec::new(),
                        None,
                    )?,
                )
                .await;
        }
        if let Some(response) = self
            .call
            .as_ref()
            .unwrap()
            .media_update_replies
            .iter()
            .find(|response| {
                same_via(&request.data.headers, &response.data.headers)
                    && cseq(&request.data.headers).ok() == cseq(&response.data.headers).ok()
            })
            .cloned()
        {
            return self.respond(connection, response).await;
        }
        if matches!(request.method, Method::UPDATE | Method::PRACK)
            && !request.data.body.is_empty()
            && (self.call.as_ref().unwrap().pending_reinvite.is_some()
                || self
                    .call
                    .as_ref()
                    .unwrap()
                    .initial_answer
                    .as_ref()
                    .is_some_and(|p| p.ack_role == InitialAckRole::AnswerToLocalOffer)
                || self
                    .call
                    .as_ref()
                    .unwrap()
                    .reliable
                    .as_ref()
                    .is_some_and(|p| !p.pracked))
        {
            return self
                .respond(
                    connection,
                    reply_for(&request, 491, "Request Pending", None, Vec::new(), None)?,
                )
                .await;
        }
        if request.method == Method::UPDATE
            && self
                .call
                .as_ref()
                .unwrap()
                .session_timer
                .as_ref()
                .is_some_and(|timer| timer.pending.is_some())
        {
            self.respond(
                connection,
                reply_for(&request, 491, "Request Pending", None, Vec::new(), None)?,
            )
            .await?;
            return Ok(());
        }
        let (seq, method) = cseq(&request.data.headers)?;
        let accepted = self
            .call
            .as_mut()
            .unwrap()
            .dialog
            .as_mut()
            .is_some_and(|d| d.accept_remote_cseq(seq, &method).is_ok());
        if !accepted {
            self.respond(
                connection,
                reply_for(
                    &request,
                    500,
                    "Server Internal Error",
                    None,
                    Vec::new(),
                    None,
                )?,
            )
            .await?;
            return Ok(());
        }
        if matches!(request.method, Method::PRACK | Method::UPDATE) && !request.data.body.is_empty()
        {
            return self.media_update(connection, &request).await;
        }
        let session_header = if request.method == Method::UPDATE {
            match inbound_session_timer_header(&request.data.headers) {
                Ok(header) => header,
                Err(error) => {
                    return self
                        .respond(connection, timer_rejection_response(&request, &error)?)
                        .await;
                }
            }
        } else {
            None
        };
        let refreshed_target = if request.method == Method::UPDATE {
            match contact_target(&request.data.headers) {
                Ok(target) => target,
                Err(_) => {
                    return self
                        .respond_media_update(
                            connection,
                            reply_for(&request, 400, "Bad Request", None, Vec::new(), None)?,
                        )
                        .await;
                }
            }
        } else {
            None
        };
        let mut response = reply_for(&request, 200, "OK", None, Vec::new(), None)?;
        if let Some(session_header) = &session_header {
            push(
                &mut response.data.headers,
                "Session-Expires",
                session_header,
            )?;
            push(&mut response.data.headers, "Require", "timer")?;
        }
        let timer_epoch = Instant::now();
        let mut session_timer = if request.method == Method::UPDATE {
            session_timer_from_response(&response.data.headers, false, timer_epoch)?
        } else {
            None
        };
        apply_request_minimum(&mut session_timer, &request.data.headers)?;
        if request.method == Method::BYE {
            let call = self.call.as_mut().unwrap();
            let early = call.initial_reply.is_none()
                && matches!(&call.tracker, Tracker::Incoming(tracker) if matches!(tracker.phase(),
                    IncomingCallPhase::Trying | IncomingCallPhase::Ringing));
            call.dialog_terminated = true;
            if let Some(pending) = &mut call.initial_answer {
                pending.hangup_requested = false;
            }
            if let Tracker::Incoming(tracker) = &mut call.tracker {
                tracker.end();
            }
            self.revoke_media_bridge();
            let write = self.respond(connection, response).await;
            if early {
                return self
                    .terminate_incoming(487, "Request Terminated", write)
                    .await;
            }
            let cleanup = self.finish("ended").await;
            return write.and(cleanup);
        }
        if request.method == Method::UPDATE {
            if let Err(error) = self.respond_media_update(connection, response).await {
                if self.call.as_ref().is_some_and(|call| {
                    call.initial_reply.is_none()
                        && matches!(&call.tracker, Tracker::Incoming(t) if matches!(t.phase(),
                        IncomingCallPhase::Trying | IncomingCallPhase::Ringing))
                }) {
                    self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                        .await?;
                } else {
                    self.fail(&error).await?;
                }
                return Err(error);
            }
            if let Some(target) = refreshed_target {
                self.call
                    .as_mut()
                    .unwrap()
                    .dialog
                    .as_mut()
                    .unwrap()
                    .remote_target = target;
            }
        } else {
            self.respond(connection, response).await?;
        }
        if request.method == Method::UPDATE {
            confirm_session_timer(&mut session_timer, timer_epoch);
            let call = self.call.as_mut().unwrap();
            call.session_timer = session_timer;
            call.early_session_timer_refreshed |= matches!(&call.tracker, Tracker::Incoming(t)
                if matches!(t.phase(), IncomingCallPhase::Trying | IncomingCallPhase::Ringing));
            call.peer_allows_update = true;
        }
        Ok(())
    }
    async fn reject_reinvite(
        &mut self,
        connection: SipConnectionId,
        request: &Request,
        status: u16,
        reason: &str,
    ) -> Result<()> {
        let response = reply_for(request, status, reason, None, Vec::new(), None)?;
        self.completed_response(connection, request, response).await
    }
    /// An INVITE carrying this dialog's tags is a session modification, not
    /// a second incoming call. Accept prepared media and target/timer refreshes
    /// at the successful 2xx write; ACK completes the retained SIP exchange.
    /// Rejecting an unsupported offer leaves the established call alive.
    async fn in_dialog_invite(
        &mut self,
        connection: SipConnectionId,
        request: Request,
    ) -> Result<()> {
        let (seq, method) = cseq(&request.data.headers)?;
        if method != Method::INVITE {
            return self
                .respond(
                    connection,
                    reply_for(&request, 400, "Bad Request", None, Vec::new(), None)?,
                )
                .await;
        }
        self.reserve_completed_invite(connection, &request)?;
        let call = self.call.as_ref().unwrap();
        if connection != call.connection {
            return self
                .completed_response(
                    connection,
                    &request,
                    reply_for(
                        &request,
                        481,
                        "Call/Transaction Does Not Exist",
                        None,
                        Vec::new(),
                        None,
                    )?,
                )
                .await;
        }
        if call.dialog_terminated {
            return self
                .completed_response(
                    connection,
                    &request,
                    reply_for(
                        &request,
                        481,
                        "Call/Transaction Does Not Exist",
                        None,
                        Vec::new(),
                        None,
                    )?,
                )
                .await;
        }
        if call.pending_reinvite.is_some() {
            return self
                .completed_response(
                    connection,
                    &request,
                    reply_for(&request, 491, "Request Pending", None, Vec::new(), None)?,
                )
                .await;
        }
        if call.session_timer.as_ref().is_some_and(|timer| {
            timer
                .pending
                .as_ref()
                .is_some_and(|pending| pending.request.method == Method::INVITE)
        }) {
            return self
                .completed_response(
                    connection,
                    &request,
                    reply_for(&request, 491, "Request Pending", None, Vec::new(), None)?,
                )
                .await;
        }
        if self.snapshot.state != "active"
            || call.uncertain
            || !call.media_allocated
            || call.local_media.is_none()
        {
            return self
                .completed_response(
                    connection,
                    &request,
                    reply_for(&request, 491, "Request Pending", None, Vec::new(), None)?,
                )
                .await;
        }
        let accepted = self
            .call
            .as_mut()
            .unwrap()
            .dialog
            .as_mut()
            .is_some_and(|dialog| dialog.accept_remote_cseq(seq, &Method::INVITE).is_ok());
        if !accepted {
            return self
                .completed_response(
                    connection,
                    &request,
                    reply_for(
                        &request,
                        500,
                        "Server Internal Error",
                        None,
                        Vec::new(),
                        None,
                    )?,
                )
                .await;
        }
        self.prune_initial_replies(Instant::now());
        if self.call.as_ref().unwrap().accepted_reinvites.len() == MAX_ACCEPTED_REINVITES {
            let mut response =
                reply_for(&request, 503, "Service Unavailable", None, Vec::new(), None)?;
            push(&mut response.data.headers, "Retry-After", "1")?;
            return self
                .completed_response(connection, &request, response)
                .await;
        }
        let offerless = request.data.body.is_empty();
        let content_type = single(&request.data.headers, "Content-Type")
            .ok()
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .is_some_and(|value| value.eq_ignore_ascii_case("application/sdp"));
        if !offerless && !content_type {
            return self
                .reject_reinvite(connection, &request, 415, "Unsupported Media Type")
                .await;
        }
        let target = match contact_target(&request.data.headers) {
            Ok(target) => target,
            Err(_) => {
                return self
                    .reject_reinvite(connection, &request, 400, "Bad Request")
                    .await;
            }
        };
        let session_header = match inbound_session_timer_header(&request.data.headers) {
            Ok(header) => header,
            Err(error) => {
                return self
                    .completed_response(
                        connection,
                        &request,
                        timer_rejection_response(&request, &error)?,
                    )
                    .await;
            }
        };
        let call = self.call.as_ref().unwrap();
        let candidate = if offerless {
            call.remote
                .clone()
                .ok_or_else(|| Failure::known("ims_call_remote_sdp_invalid"))
        } else {
            self.negotiate_offer(&request.data.body)
        };
        let supported = candidate.is_ok()
            && (offerless || self.proposed_history(&request.data.body).is_ok())
            && !unsupported_media_direction(&request.data.body);
        if !supported {
            return self
                .reject_reinvite(connection, &request, 488, "Not Acceptable Here")
                .await;
        }
        let remote = candidate.unwrap();
        let binding = call.binding.clone();
        let (local_ip, local_port) = call.local_media.unwrap();
        let Some(version) = call.local_sdp_version.checked_add(1) else {
            return self
                .reject_reinvite(connection, &request, 500, "Server Internal Error")
                .await;
        };
        let answer_result = if offerless {
            sdp::build_local_sdp_version_with_events(
                self.generation.value(),
                version,
                local_ip,
                local_port,
                true,
                Some(remote.codec),
                Some(remote.payload_type),
                remote.telephone_event_payload,
                remote.telephone_event_mask,
                self.config.evs_only,
            )
        } else {
            sdp::build_local_audio_answer_to_offer_version(
                self.generation.value(),
                version,
                local_ip,
                local_port,
                std::str::from_utf8(&request.data.body)
                    .map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))?,
                &remote,
                self.config.evs_only,
            )
        };
        let answer = match answer_result {
            Ok(answer) => answer.into_bytes(),
            Err(_) => {
                return self
                    .reject_reinvite(connection, &request, 500, "Server Internal Error")
                    .await;
            }
        };
        let mut response = reply_for(
            &request,
            200,
            "OK",
            None,
            answer,
            Some(&self.config.contact),
        )?;
        if let Some(session_header) = &session_header {
            push(
                &mut response.data.headers,
                "Session-Expires",
                session_header,
            )?;
            push(&mut response.data.headers, "Require", "timer")?;
        }
        let timer_epoch = Instant::now();
        let mut accepted_timer =
            session_timer_from_response(&response.data.headers, false, timer_epoch)?;
        apply_request_minimum(&mut accepted_timer, &request.data.headers)?;
        if !offerless {
            if let Err(_) = self
                .media
                .clone()
                .prepare(binding.clone(), remote.clone())
                .await
            {
                let discarded = self.media.clone().discard(binding).await;
                if discarded.is_err() {
                    self.revoke_media_bridge();
                }
                let rejected = self
                    .reject_reinvite(connection, &request, 488, "Not Acceptable Here")
                    .await;
                if let Err(mut error) = discarded {
                    // The old dialog has now lost its safe media resources.
                    // Complete this INVITE and terminate that established call;
                    // neither failed SIP write may bypass the one real drain.
                    let bye = match self.dialog_request(Method::BYE, None) {
                        Ok(request) => self.send(request).await,
                        Err(error) => Err(error),
                    };
                    for result in [&rejected, &bye] {
                        if let Err(write) = result {
                            error.uncertain |= write.uncertain;
                        }
                    }
                    let cleanup = self.fail(&error).await;
                    rejected?;
                    bye?;
                    cleanup?;
                    return Err(error);
                }
                return rejected;
            }
        }
        // Reserve identity before handing the first 200 to the writer. Its
        // receipt, not prepare/configure latency, anchors Timer L and TU T1.
        self.call
            .as_mut()
            .unwrap()
            .accepted_reinvites
            .push_back(AcceptedReinvite {
                cseq: seq,
                response: response.clone(),
                confirmed: false,
                acked: false,
                deadline: Instant::now() + REINVITE_ACK_TIMEOUT,
                next_response_retry: None,
                response_retry_interval: Duration::from_millis(500),
                cleanup_on_ack: false,
                cleanup_target: target.clone(),
            });
        self.release_completed_invite(connection, &request);
        let write = self.respond(connection, response.clone()).await;
        let receipt = Instant::now();
        let accepted = self
            .call
            .as_mut()
            .unwrap()
            .accepted_reinvites
            .back_mut()
            .unwrap();
        accepted.deadline = receipt + REINVITE_ACK_TIMEOUT;
        if let Err(error) = write {
            accepted.cleanup_on_ack = true;
            return self.failed_answer_write(binding.clone(), error).await;
        }
        accepted.confirmed = true;
        accepted.next_response_retry = Some(receipt + Duration::from_millis(500));
        confirm_session_timer(&mut accepted_timer, timer_epoch);
        self.call.as_mut().unwrap().session_timer = accepted_timer;
        // A successful target refresh is effective before ACK, including for
        // a locally generated BYE if media commit or the ACK timer fails.
        if let Some(target) = target {
            self.call
                .as_mut()
                .unwrap()
                .dialog
                .as_mut()
                .unwrap()
                .remote_target = target;
        }
        if !offerless {
            self.call.as_mut().unwrap().payload_history =
                self.proposed_history(&request.data.body)?;
            self.commit_media(remote.clone()).await?;
        }
        self.call.as_mut().unwrap().pending_reinvite = Some(PendingReinvite {
            cseq: seq,
            response,
            remote,
            offerless,
            local_sdp_version: version,
            deadline: receipt + REINVITE_ACK_TIMEOUT,
        });
        self.observe();
        Ok(())
    }
    /// Answer an in-dialog SDP offer only after the retained call can use its
    /// selected codec and local RTP socket. A 200 without SDP would falsely
    /// accept changed media while continuing to send to the old endpoint.
    async fn respond_media_update(
        &mut self,
        connection: SipConnectionId,
        response: Response,
    ) -> Result<()> {
        self.respond(connection, response.clone()).await?;
        if let Some(call) = self.call.as_mut() {
            if call.media_update_replies.len() == MAX_RETIRED {
                call.media_update_replies.remove(0).data.body.zeroize();
            }
            call.media_update_replies.push(response);
        }
        Ok(())
    }
    async fn media_update(&mut self, connection: SipConnectionId, request: &Request) -> Result<()> {
        let refreshed_target = if request.method == Method::UPDATE {
            match contact_target(&request.data.headers) {
                Ok(target) => target,
                Err(_) => {
                    return self
                        .respond_media_update(
                            connection,
                            reply_for(request, 400, "Bad Request", None, Vec::new(), None)?,
                        )
                        .await;
                }
            }
        } else {
            None
        };
        let content_type = single(&request.data.headers, "Content-Type")
            .ok()
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .is_some_and(|value| value.eq_ignore_ascii_case("application/sdp"));
        if !content_type {
            return self
                .respond_media_update(
                    connection,
                    reply_for(
                        request,
                        415,
                        "Unsupported Media Type",
                        None,
                        Vec::new(),
                        None,
                    )?,
                )
                .await;
        }
        let candidate = self.negotiate_offer(&request.data.body);
        let call = self.call.as_ref().unwrap();
        let supported = candidate.as_ref().ok().is_some_and(|_| {
            call.media_allocated
                && call.local_media.is_some()
                && matches!(self.snapshot.state.as_str(), "early" | "active")
                && !unsupported_media_direction(&request.data.body)
                && self.proposed_history(&request.data.body).is_ok()
        });
        if !supported {
            return self
                .respond_media_update(
                    connection,
                    reply_for(request, 488, "Not Acceptable Here", None, Vec::new(), None)?,
                )
                .await;
        }
        let candidate = candidate.unwrap();
        let binding = call.binding.clone();
        let (local_ip, local_port) = call.local_media.unwrap();
        let version = call
            .local_sdp_version
            .checked_add(1)
            .ok_or_else(|| Failure::known("ims_call_local_sdp_invalid"))?;
        let answer = sdp::build_local_audio_answer_to_offer_version(
            self.generation.value(),
            version,
            local_ip,
            local_port,
            std::str::from_utf8(&request.data.body)
                .map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))?,
            &candidate,
            self.config.evs_only,
        )
        .map_err(|_| Failure::known("ims_call_local_sdp_invalid"))?
        .into_bytes();
        let session_header = if request.method == Method::UPDATE {
            match inbound_session_timer_header(&request.data.headers) {
                Ok(header) => header,
                Err(error) => {
                    return self
                        .respond(connection, timer_rejection_response(request, &error)?)
                        .await;
                }
            }
        } else {
            None
        };
        let mut response = reply_for(
            request,
            200,
            "OK",
            None,
            answer.clone(),
            Some(&self.config.contact),
        )?;
        if let Some(session_header) = &session_header {
            push(
                &mut response.data.headers,
                "Session-Expires",
                session_header,
            )?;
            push(&mut response.data.headers, "Require", "timer")?;
        }
        let timer_epoch = Instant::now();
        let mut session_timer = if request.method == Method::UPDATE {
            session_timer_from_response(&response.data.headers, false, timer_epoch)?
        } else {
            None
        };
        apply_request_minimum(&mut session_timer, &request.data.headers)?;
        if self
            .media
            .clone()
            .prepare(binding.clone(), candidate.clone())
            .await
            .is_err()
        {
            self.discard_prepared(binding.clone()).await?;
            return self
                .respond_media_update(
                    connection,
                    reply_for(request, 488, "Not Acceptable Here", None, Vec::new(), None)?,
                )
                .await;
        }
        if let Err(error) = self.respond_media_update(connection, response).await {
            return self.failed_answer_write(binding, error).await;
        }
        if request.method == Method::UPDATE {
            confirm_session_timer(&mut session_timer, timer_epoch);
            let call = self.call.as_mut().unwrap();
            call.session_timer = session_timer;
            call.peer_allows_update = true;
        }
        if let Some(target) = refreshed_target {
            self.call
                .as_mut()
                .unwrap()
                .dialog
                .as_mut()
                .unwrap()
                .remote_target = target;
        }
        self.call.as_mut().unwrap().payload_history = self.proposed_history(&request.data.body)?;
        self.commit_media(candidate.clone()).await?;
        let call = self.call.as_mut().unwrap();
        call.remote = Some(candidate);
        call.local_sdp_version = version;
        call.local_sdp.zeroize();
        call.local_sdp = answer;
        self.observe();
        Ok(())
    }
    async fn incoming(&mut self, connection: SipConnectionId, request: Request) -> Result<()> {
        let id = single(&request.data.headers, "Call-ID")?.to_owned();
        let (seq, method) = cseq(&request.data.headers)?;
        if method != Method::INVITE {
            return Err(Failure::known("ims_call_transaction_invalid"));
        }
        if self.config.sms_only {
            return self
                .completed_response(
                    connection,
                    &request,
                    reply_for(&request, 405, "Method Not Allowed", None, Vec::new(), None)?,
                )
                .await;
        }
        if let Some(call) = self.call.iter().chain(self.retired.iter()).find(|call| {
            call.binding.call_id == id
                && same_transaction(call, &request)
                && matches!(call.tracker, Tracker::Incoming(_))
        }) {
            if connection != call.connection {
                return self
                    .completed_response(
                        connection,
                        &request,
                        reply_for(
                            &request,
                            481,
                            "Call/Transaction Does Not Exist",
                            None,
                            Vec::new(),
                            None,
                        )?,
                    )
                    .await;
            }
            if call.initial_reply.is_some() {
                // Accepted INVITEs are absorbed; only their TU resends 2xx.
                // The Service Completed table owns negative final replay,
                // including the distinct ACK/I=0 tombstone lifetime.
                return Ok(());
            }
            if call.uncertain {
                return Ok(());
            }
            if let Some(reliable) = &call.reliable {
                // The RAck identity can outlive the final-response cache.
                // Never replay a provisional response after this INVITE ended.
                let unfinished = matches!(&call.tracker, Tracker::Incoming(tracker)
                    if matches!(tracker.phase(), IncomingCallPhase::Trying | IncomingCallPhase::Ringing));
                return if reliable.confirmed && unfinished {
                    self.respond(connection, reliable.response.clone()).await
                } else {
                    Ok(())
                };
            }
            if let Tracker::Incoming(t) = &call.tracker {
                let (status, reason) = match t.retransmission(seq, false) {
                    IncomingInviteAdmission::ResendRinging => (180, "Ringing"),
                    _ => (486, "Busy Here"),
                };
                let response = reply_for(
                    &request,
                    status,
                    reason,
                    Some(&call.local_tag),
                    Vec::new(),
                    Some(&self.config.contact),
                )?;
                return if status >= 300 {
                    self.completed_response(connection, &request, response)
                        .await
                } else {
                    self.respond(connection, response).await
                };
            }
        }
        if self.voice_active() || !self.can_retire() {
            return self
                .completed_response(
                    connection,
                    &request,
                    reply_for(&request, 486, "Busy Here", None, Vec::new(), None)?,
                )
                .await;
        }
        let reliable = token(&request.data.headers, "Require", "100rel");
        self.reserve_completed_invite(connection, &request)?;
        if let Err(error) = self
            .respond(
                connection,
                reply_for(&request, 100, "Trying", None, Vec::new(), None)?,
            )
            .await
        {
            // No Call/media exists yet. Complete the reserved INVITE even
            // when its provisional write failed; its identity still expires.
            let final_response = reply_for(
                &request,
                500,
                "Server Internal Error",
                None,
                Vec::new(),
                None,
            )?;
            let _ = self
                .completed_response(connection, &request, final_response)
                .await;
            return Err(error);
        }
        if let Err(error) = inbound_session_timer_header(&request.data.headers) {
            return self
                .completed_response(
                    connection,
                    &request,
                    timer_rejection_response(&request, &error)?,
                )
                .await;
        }
        let offerless = request.data.body.is_empty();
        let remote = if offerless {
            None
        } else {
            match self.negotiate_offer(&request.data.body) {
                Ok(remote) => Some(remote),
                Err(_) => {
                    return self
                        .completed_response(
                            connection,
                            &request,
                            reply_for(
                                &request,
                                488,
                                "Not Acceptable Here",
                                None,
                                Vec::new(),
                                None,
                            )?,
                        )
                        .await;
                }
            }
        };
        let mut payload_history = sdp::PayloadHistory::default();
        if !offerless
            && payload_history
                .admit_sdp(
                    std::str::from_utf8(&request.data.body)
                        .map_err(|_| Failure::known("ims_call_remote_sdp_invalid"))?,
                )
                .is_err()
        {
            return self
                .completed_response(
                    connection,
                    &request,
                    reply_for(&request, 488, "Not Acceptable Here", None, Vec::new(), None)?,
                )
                .await;
        }
        let tag = unique();
        let ringing = reply_for(
            &request,
            180,
            "Ringing",
            Some(&tag),
            Vec::new(),
            Some(&self.config.contact),
        )?;
        if contact_target(&request.data.headers).is_err() {
            return self
                .completed_response(
                    connection,
                    &request,
                    reply_for(&request, 400, "Bad Request", None, Vec::new(), None)?,
                )
                .await;
        }
        let dialog =
            match Dialog::from_initial_exchange(DialogRole::UserAgentServer, &request, &ringing) {
                Ok(dialog) => dialog,
                Err(_) => {
                    return self
                        .completed_response(
                            connection,
                            &request,
                            reply_for(&request, 400, "Bad Request", None, Vec::new(), None)?,
                        )
                        .await;
                }
            };
        let peer_allows_update = token(&request.data.headers, "Allow", "UPDATE");
        self.retire()?;
        self.snapshot = Snapshot {
            state: "incoming".into(),
            direction: "incoming".into(),
            peer: incoming_peer(single(&request.data.headers, "From")?).into(),
            call_id: id.clone(),
            codec: remote
                .as_ref()
                .map(|remote| match remote.codec {
                    AudioCodec::Evs { .. } => "EVS",
                    AudioCodec::AmrWb { .. } => "AMR-WB",
                })
                .unwrap_or("")
                .into(),
            audio_sample_rate: 16000,
            ..Default::default()
        };
        self.call = Some(Call {
            binding: CallBinding {
                generation: self.generation,
                call_id: id,
                invite_cseq: seq,
            },
            request,
            connection,
            tracker: Tracker::Incoming(
                IncomingCallTracker::begin(seq)
                    .map_err(|_| Failure::known("ims_call_policy_invalid"))?,
            ),
            dialog: Some(dialog),
            local_tag: tag,
            initial_reply: None,
            initial_reply_confirmed: false,
            initial_reply_deadline: None,
            initial_answer: None,
            reliable: None,
            prack_replies: Default::default(),
            answer_requested: false,
            early_oa_complete: false,
            local_sdp: Vec::new(),
            remote,
            payload_history,
            local_media: None,
            local_sdp_version: 1,
            deadline: None,
            pending_reinvite: None,
            accepted_reinvites: Default::default(),
            dialog_terminated: false,
            media_update_replies: Vec::new(),
            session_timer: None,
            early_session_timer_refreshed: false,
            peer_allows_update,
            last_refresh_ack: None,
            late_refresh_invite: None,
            initial_timer_retry_acks: Vec::new(),
            media_allocated: false,
            media_started: false,
            uncertain: false,
        });
        if reliable {
            return self.start_reliable_ringing().await;
        }
        if let Err(error) = self.respond(connection, ringing).await {
            self.end_reliable_early(error.clone(), 500, "Server Internal Error")
                .await?;
            return Err(error);
        }
        if let Tracker::Incoming(t) = &mut self.call.as_mut().unwrap().tracker {
            t.ringing()
                .map_err(|_| Failure::known("ims_call_policy_invalid"))?;
        }
        self.call.as_mut().unwrap().deadline = Some(Instant::now() + DECISION_TIMEOUT);
        self.observe();
        Ok(())
    }
    async fn retry_initial_timer_invite(&mut self, response: &Response) -> Result<bool> {
        let Some(call) = self.call.as_ref() else {
            return Ok(false);
        };
        let Tracker::Outgoing(tracker) = &call.tracker else {
            return Ok(false);
        };
        // A tagged provisional dialog or early media has already acquired
        // separate transaction state. Do not silently replay that state.
        if !matches!(tracker.phase(), CallPhase::Inviting | CallPhase::Early)
            || call.dialog.is_some()
            || call.remote.is_some()
            || call.media_started
            || call.initial_timer_retry_acks.len() >= MAX_INITIAL_TIMER_RETRIES
        {
            return Ok(false);
        }
        let Ok((current_interval, refresher)) =
            parse_session_expires(single(&call.request.data.headers, "Session-Expires")?)
        else {
            return Ok(false);
        };
        let minimum = single(&response.data.headers, "Min-SE")
            .ok()
            .and_then(|value| value.split(';').next()?.trim().parse::<u32>().ok())
            .filter(|value| (SESSION_MIN_INTERVAL..=SESSION_MAX_INTERVAL).contains(value));
        let Some(minimum) = minimum.filter(|minimum| *minimum > current_interval) else {
            return Ok(false);
        };
        let old_cseq = cseq(&call.request.data.headers)?.0;
        let Some(next_cseq) = old_cseq
            .checked_add(1)
            .filter(|value| *value <= i32::MAX as u32)
        else {
            return Ok(false);
        };
        let ack = self.transaction_request(Method::ACK, Some(response))?;
        let mut retry = call.request.clone();
        retry.data.headers.set(
            Header::new("Via", self.via())
                .map_err(|_| Failure::known("ims_call_header_invalid"))?,
        );
        retry.data.headers.set(
            Header::new("CSeq", format!("{next_cseq} INVITE"))
                .map_err(|_| Failure::known("ims_call_header_invalid"))?,
        );
        retry.data.headers.set(
            Header::new(
                "Session-Expires",
                format!(
                    "{minimum};refresher={}",
                    if refresher == Some(false) {
                        "uas"
                    } else {
                        "uac"
                    }
                ),
            )
            .map_err(|_| Failure::known("ims_call_header_invalid"))?,
        );
        retry.data.headers.set(
            Header::new("Min-SE", minimum.to_string())
                .map_err(|_| Failure::known("ims_call_header_invalid"))?,
        );
        if let Err(error) = self.send(ack.clone()).await {
            self.fail(&error).await?;
            return Err(error);
        }
        let call = self.call.as_mut().unwrap();
        call.initial_timer_retry_acks.push(ack);
        call.request.data.body.zeroize();
        call.request = retry.clone();
        call.tracker = Tracker::Outgoing(
            OutgoingCallTracker::begin(next_cseq)
                .map_err(|_| Failure::known("ims_call_policy_invalid"))?,
        );
        call.deadline = Some(Instant::now() + DECISION_TIMEOUT);
        self.snapshot.state = "outgoing".into();
        self.snapshot.early_media = false;
        if let Err(error) = self.send(retry).await {
            self.fail(&error).await?;
            return Err(error);
        }
        Ok(true)
    }
    async fn invite_response(&mut self, mut response: Response) -> Result<()> {
        let (seq, method) = cseq(&response.data.headers)?;
        let cancelled = {
            let call = self.call.as_ref().unwrap();
            if method != Method::INVITE
                || single(&response.data.headers, "Call-ID")? != call.binding.call_id.as_str()
                || single(&response.data.headers, "From")?
                    != single(&call.request.data.headers, "From")?
            {
                return Ok(());
            }
            if seq != cseq(&call.request.data.headers)?.0
                || !same_via(&response.data.headers, &call.request.data.headers)
            {
                if response.status == 422
                    && let Some(ack) = call.initial_timer_retry_acks.iter().find(|ack| {
                        cseq(&ack.data.headers)
                            .ok()
                            .is_some_and(|(old, _)| old == seq)
                            && same_via(&response.data.headers, &ack.data.headers)
                            && single(&response.data.headers, "To").ok()
                                == single(&ack.data.headers, "To").ok()
                    })
                {
                    self.send(ack.clone()).await?;
                }
                return Ok(());
            }
            matches!(&call.tracker,Tracker::Outgoing(t) if t.phase()==CallPhase::Cancelling)
        };
        if response.status == 422
            && !cancelled
            && self.retry_initial_timer_invite(&response).await?
        {
            self.observe();
            return Ok(());
        }
        let call = self.call.as_ref().unwrap();
        let mut remote = if !cancelled
            && (180..300).contains(&response.status)
            && !response.data.body.is_empty()
        {
            self.negotiate_answer(&response.data.body, &call.request.data.body)
                .ok()
                .filter(|_| self.proposed_history(&response.data.body).is_ok())
        } else {
            None
        };
        if response.status > 100
            && response.status < 300
            && single(&response.data.headers, "To")
                .ok()
                .and_then(tag)
                .is_some()
        {
            if response.data.headers.get("Contact").is_none() {
                push(
                    &mut response.data.headers,
                    "Contact",
                    &format!("<{}>", call.request.uri),
                )?;
            }
            let call = self.call.as_mut().unwrap();
            if let Some(dialog) = &mut call.dialog {
                dialog
                    .update_from_initial_response(&call.request, &response)
                    .map_err(|_| Failure::known("ims_call_dialog_invalid"))?;
            } else {
                call.dialog = Some(
                    Dialog::from_initial_exchange(
                        DialogRole::UserAgentClient,
                        &call.request,
                        &response,
                    )
                    .map_err(|_| Failure::known("ims_call_dialog_invalid"))?,
                );
            }
        }
        let reliable = if (180..200).contains(&response.status)
            && token(&response.data.headers, "Require", "100rel")
        {
            Some(
                single(&response.data.headers, "RSeq")?
                    .parse::<u32>()
                    .map_err(|_| Failure::known("ims_call_rseq_invalid"))?,
            )
        } else {
            None
        };
        let action = if let Tracker::Outgoing(t) = &mut self.call.as_mut().unwrap().tracker {
            t.accept_invite_response(response.status, seq, remote.is_some(), reliable)
                .map_err(|_| Failure::known("ims_call_policy_invalid"))?
        } else {
            return Ok(());
        };
        if (200..300).contains(&response.status) {
            self.call.as_mut().unwrap().peer_allows_update =
                token(&response.data.headers, "Allow", "UPDATE");
        }
        // A provisional response can advance the tracker without starting
        // media (180 without SDP, or a bodyless reliable response). Swift
        // needs that phase to play local ringback while capture stays off.
        if response.status < 200 {
            let Tracker::Outgoing(tracker) = &self.call.as_ref().unwrap().tracker else {
                unreachable!();
            };
            self.snapshot.state = match tracker.phase() {
                CallPhase::Cancelling => "cancelling",
                CallPhase::Early if response.status >= 180 => "early",
                _ => "outgoing",
            }
            .into();
        }
        if !matches!(action, InviteAction::Wait | InviteAction::AckOnly) {
            if let Some(remote) = remote.take() {
                self.call.as_mut().unwrap().payload_history =
                    self.proposed_history(&response.data.body)?;
                self.call.as_mut().unwrap().remote = Some(remote);
            }
        }
        match action {
            InviteAction::Wait => {}
            InviteAction::StartEarlyMedia => {
                self.snapshot.state = "early".into();
                self.start_media(true).await?;
            }
            InviteAction::SendPrack { start_early_media } => {
                let request = self.dialog_request(
                    Method::PRACK,
                    Some(format!("{} {seq} INVITE", reliable.unwrap())),
                )?;
                self.send(request).await?;
                if start_early_media {
                    self.snapshot.state = "early".into();
                    self.start_media(true).await?;
                }
            }
            InviteAction::AckAndStartMedia => {
                let request = self.dialog_request(Method::ACK, None)?;
                self.send(request).await?;
                let session_timer =
                    match session_timer_from_response(&response.data.headers, true, Instant::now())
                    {
                        Ok(timer) => timer,
                        Err(_) => {
                            self.end_expired_session("ims_session_timer_invalid")
                                .await?;
                            return Ok(());
                        }
                    };
                if session_timer.as_ref().is_some_and(|timer| {
                    request_minimum(&self.call.as_ref().unwrap().request.data.headers)
                        .is_ok_and(|minimum| timer.interval < minimum)
                }) {
                    self.end_expired_session("ims_session_timer_invalid")
                        .await?;
                    return Ok(());
                }
                self.call.as_mut().unwrap().uncertain = false;
                self.call.as_mut().unwrap().deadline = None;
                self.snapshot.uncertain = false;
                self.snapshot.state = "active".into();
                self.start_media(false).await?;
                self.call.as_mut().unwrap().session_timer = session_timer;
            }
            InviteAction::AckAndBye | InviteAction::AckAndFail { status: 200..=299 } => {
                let ack = self.dialog_request(Method::ACK, None)?;
                self.send(ack).await?;
                let bye = self.dialog_request(Method::BYE, None)?;
                self.send(bye).await?;
                self.call.as_mut().unwrap().uncertain = false;
                self.snapshot.uncertain = false;
                self.finish(if cancelled { "ended" } else { "failed" })
                    .await?;
            }
            InviteAction::AckAndEnd | InviteAction::AckAndFail { .. } => {
                let ack = self.transaction_request(Method::ACK, Some(&response))?;
                self.send(ack).await?;
                self.call.as_mut().unwrap().uncertain = false;
                self.snapshot.uncertain = false;
                if !cancelled {
                    self.snapshot.last_error = format!("ims_call_rejected_{}", response.status);
                }
                self.finish(if cancelled { "ended" } else { "failed" })
                    .await?;
            }
            InviteAction::AckOnly => {
                let ack = if response.status < 300 {
                    self.dialog_request(Method::ACK, None)?
                } else {
                    self.transaction_request(Method::ACK, Some(&response))?
                };
                self.send(ack).await?;
            }
        }
        self.observe();
        Ok(())
    }
}
fn terminal(state: &str) -> bool {
    matches!(state, "idle" | "ended" | "failed")
}
fn unique() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS randomness unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn push(headers: &mut Headers, name: &str, value: &str) -> Result<()> {
    headers.push(Header::new(name, value).map_err(|_| Failure::known("ims_call_header_invalid"))?);
    Ok(())
}
fn single<'a>(headers: &'a Headers, name: &str) -> Result<&'a str> {
    let mut values = headers.all(name);
    let value = values
        .next()
        .ok_or_else(|| Failure::known("ims_call_header_missing"))?;
    if values.next().is_some() || value.value().trim().is_empty() {
        return Err(Failure::known("ims_call_header_invalid"));
    }
    Ok(value.value().trim())
}
fn session_timer_from_response(
    headers: &Headers,
    local_uac: bool,
    now: Instant,
) -> Result<Option<SessionTimer>> {
    if headers.get("Session-Expires").is_none() {
        return if token(headers, "Require", "timer") {
            Err(Failure::known("ims_session_timer_invalid"))
        } else {
            Ok(None)
        };
    }
    let value = single(headers, "Session-Expires")?;
    let (interval, refresher) = parse_session_expires(value)?;
    if interval < SESSION_MIN_INTERVAL {
        return Err(Failure::known("ims_session_timer_invalid"));
    }
    let refresher_uac = refresher.unwrap_or(true);
    let expires_at = now + Duration::from_secs(u64::from(interval));
    let early = (interval / 3).min(32);
    Ok(Some(SessionTimer {
        interval,
        min_se: SESSION_MIN_INTERVAL,
        local_refresher: refresher_uac == local_uac,
        expires_at,
        termination_at: expires_at - Duration::from_secs(u64::from(early)),
        refresh_at: (refresher_uac == local_uac)
            .then_some(now + Duration::from_secs(u64::from(interval / 2))),
        pending: None,
        retry_count: 0,
    }))
}
fn parse_session_expires(value: &str) -> Result<(u32, Option<bool>)> {
    let mut parts = value.split(';');
    let interval = parts
        .next()
        .unwrap_or_default()
        .trim()
        .parse::<u32>()
        .map_err(|_| Failure::known("ims_session_timer_invalid"))?;
    if !(1..=SESSION_MAX_INTERVAL).contains(&interval) {
        return Err(Failure::known("ims_session_timer_invalid"));
    }
    let mut refresher = None;
    for part in parts {
        let Some((name, value)) = part.trim().split_once('=') else {
            return Err(Failure::known("ims_session_timer_invalid"));
        };
        if name.trim().eq_ignore_ascii_case("refresher") {
            if refresher.is_some() {
                return Err(Failure::known("ims_session_timer_invalid"));
            }
            refresher = Some(if value.trim().eq_ignore_ascii_case("uac") {
                true
            } else if value.trim().eq_ignore_ascii_case("uas") {
                false
            } else {
                return Err(Failure::known("ims_session_timer_invalid"));
            });
        }
    }
    Ok((interval, refresher))
}
fn inbound_session_timer_header(headers: &Headers) -> Result<Option<String>> {
    if headers.get("Session-Expires").is_none() {
        return if token(headers, "Require", "timer") {
            Err(Failure::known("ims_session_timer_invalid"))
        } else {
            Ok(None)
        };
    }
    let (interval, refresher_uac) = parse_session_expires(single(headers, "Session-Expires")?)?;
    let minimum = request_minimum(headers)?;
    if interval < minimum {
        return Err(Failure::known("ims_session_timer_too_small"));
    }
    let role = if refresher_uac.unwrap_or(true) {
        "uac"
    } else {
        "uas"
    };
    Ok(Some(format!("{interval};refresher={role}")))
}
fn timer_rejection_response(request: &Request, error: &Failure) -> Result<Response> {
    if error.code == "ims_session_timer_too_small" {
        let minimum = request_minimum(&request.data.headers)?;
        let mut response = reply_for(
            request,
            422,
            "Session Interval Too Small",
            None,
            Vec::new(),
            None,
        )?;
        push(&mut response.data.headers, "Min-SE", &minimum.to_string())?;
        Ok(response)
    } else {
        reply_for(request, 400, "Bad Request", None, Vec::new(), None)
    }
}
fn request_minimum(headers: &Headers) -> Result<u32> {
    headers
        .get("Min-SE")
        .map(|_| single(headers, "Min-SE"))
        .transpose()?
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|value| (SESSION_MIN_INTERVAL..=SESSION_MAX_INTERVAL).contains(value))
                .ok_or_else(|| Failure::known("ims_session_timer_invalid"))
        })
        .transpose()
        .map(|value| value.unwrap_or(SESSION_MIN_INTERVAL))
}
fn confirm_session_timer(timer: &mut Option<SessionTimer>, prepared_at: Instant) {
    if let Some(timer) = timer.as_mut() {
        // The granted interval begins at the successful final-response write,
        // independently of time spent preparing media or waiting for a writer.
        let elapsed = prepared_at.elapsed();
        timer.expires_at += elapsed;
        timer.termination_at += elapsed;
        if let Some(refresh_at) = timer.refresh_at.as_mut() {
            *refresh_at += elapsed;
        }
    }
}
fn apply_request_minimum(timer: &mut Option<SessionTimer>, headers: &Headers) -> Result<()> {
    if let Some(timer) = timer.as_mut() {
        timer.min_se = request_minimum(headers)?;
    }
    Ok(())
}
fn cseq(headers: &Headers) -> Result<(u32, Method)> {
    let value = single(headers, "CSeq")?;
    let mut parts = value.split_whitespace();
    let seq = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0 && *v <= i32::MAX as u32)
        .ok_or_else(|| Failure::known("ims_call_cseq_invalid"))?;
    let method = Method::new(parts.next().unwrap_or_default())
        .map_err(|_| Failure::known("ims_call_cseq_invalid"))?;
    if parts.next().is_some() {
        return Err(Failure::known("ims_call_cseq_invalid"));
    }
    Ok((seq, method))
}
fn tag(value: &str) -> Option<&str> {
    value
        .rsplit(';')
        .find_map(|v| v.trim().strip_prefix("tag="))
}
fn refresh_response_matches(
    call: &Call,
    original: &Request,
    connection: SipConnectionId,
    response: &Response,
) -> bool {
    connection == call.connection
        && single(&response.data.headers, "Call-ID").ok() == Some(call.binding.call_id.as_str())
        && cseq(&response.data.headers).ok() == cseq(&original.data.headers).ok()
        && same_via(&response.data.headers, &original.data.headers)
        && single(&response.data.headers, "From").ok()
            == single(&original.data.headers, "From").ok()
        && single(&response.data.headers, "To").ok() == single(&original.data.headers, "To").ok()
}
fn same_via(a: &Headers, b: &Headers) -> bool {
    // Registered signaling is TCP. Use the same normalized top-Via identity
    // for Proceeding, Accepted, refresh and Completed transactions so an
    // equivalent header cannot pass one ledger then miss the Call owner.
    completed_invite::same_tcp_via_identity(a, b)
}
fn refresh_transaction_ack(request: &Request, response: &Response) -> Result<Request> {
    let mut ack = request.clone();
    ack.method = Method::ACK;
    ack.data.body.zeroize();
    let mut headers = Headers::default();
    for header in request.data.headers.iter() {
        if !matches!(
            header.name().to_ascii_lowercase().as_str(),
            "cseq"
                | "to"
                | "content-type"
                | "content-length"
                | "contact"
                | "p-preferred-service"
                | "p-preferred-identity"
                | "accept-contact"
                | "session-expires"
                | "min-se"
                | "accept"
        ) {
            headers.push(header.clone());
        }
    }
    push(&mut headers, "To", single(&response.data.headers, "To")?)?;
    push(
        &mut headers,
        "CSeq",
        &format!("{} ACK", cseq(&request.data.headers)?.0),
    )?;
    ack.data.headers = headers;
    Ok(ack)
}
fn same_transaction(call: &Call, request: &Request) -> bool {
    single(&request.data.headers, "Call-ID").ok() == Some(call.binding.call_id.as_str())
        && cseq(&request.data.headers)
            .ok()
            .is_some_and(|(seq, _)| seq == call.binding.invite_cseq)
        && same_via(&request.data.headers, &call.request.data.headers)
        && single(&request.data.headers, "From").ok()
            == single(&call.request.data.headers, "From").ok()
}
fn same_dialog(call: &Call, request: &Request) -> bool {
    let Some(dialog) = &call.dialog else {
        return false;
    };
    single(&request.data.headers, "Call-ID").ok() == Some(dialog.id.call_id.as_str())
        && single(&request.data.headers, "To").ok().and_then(tag)
            == Some(dialog.id.local_tag.as_str())
        && single(&request.data.headers, "From").ok().and_then(tag)
            == Some(dialog.id.remote_tag.as_str())
}
fn token(headers: &Headers, name: &str, wanted: &str) -> bool {
    headers.all(name).any(|h| {
        h.value()
            .split(',')
            .any(|s| s.trim().eq_ignore_ascii_case(wanted))
    })
}
fn contact_target(headers: &Headers) -> Result<Option<Uri>> {
    let mut contacts = headers.all("Contact");
    let Some(contact) = contacts.next() else {
        return Ok(None);
    };
    if contacts.next().is_some() {
        return Err(Failure::known("ims_call_contact_invalid"));
    }
    let value = contact.value().trim();
    let uri = if let Some((_, rest)) = value.split_once('<') {
        rest.split_once('>')
            .map(|(uri, _)| uri.trim())
            .ok_or_else(|| Failure::known("ims_call_contact_invalid"))?
    } else {
        value
    };
    if uri.is_empty() || uri.contains(',') {
        return Err(Failure::known("ims_call_contact_invalid"));
    }
    let target = Uri::parse(uri).map_err(|_| Failure::known("ims_call_contact_invalid"))?;
    // A dialog target must be routable SIP/SIPS. The general URI parser also
    // accepts "*" for OPTIONS; REGISTER's wildcard is not an INVITE target.
    if target.wildcard
        || (!uri
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("sip:"))
            && !uri
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("sips:")))
    {
        return Err(Failure::known("ims_call_contact_invalid"));
    }
    Ok(Some(target))
}
fn unsupported_media_direction(body: &[u8]) -> bool {
    std::str::from_utf8(body).is_ok_and(|sdp| {
        sdp.lines().any(|line| {
            ["a=inactive", "a=sendonly", "a=recvonly"]
                .iter()
                .any(|direction| line.trim().eq_ignore_ascii_case(direction))
        })
    })
}
// Match the existing reader API's caller-number projection. Dialog routing
// still uses the original From header, including its URI and remote tag.
fn incoming_peer(from: &str) -> &str {
    let from = from.trim();
    let address = from
        .split_once('<')
        .and_then(|(_, tail)| tail.split_once('>'))
        .map(|(uri, _)| uri.trim())
        .unwrap_or_else(|| from.split(';').next().unwrap_or(from).trim());
    let address = address.strip_prefix("sip:").unwrap_or(address);
    let address = address.strip_prefix("tel:").unwrap_or(address);
    let peer = address.split('@').next().unwrap_or(address);
    let peer = peer.split(';').next().unwrap_or(peer).trim();
    if peer.is_empty() { from } else { peer }
}
fn reply_for(
    request: &Request,
    status: u16,
    reason: &str,
    tag: Option<&str>,
    body: Vec<u8>,
    contact: Option<&str>,
) -> Result<Response> {
    let mut headers = Headers::default();
    if request.data.headers.get("Via").is_none() {
        return Err(Failure::known("ims_call_header_missing"));
    }
    for h in request.data.headers.all("Via") {
        headers.push(h.clone());
    }
    for name in ["From", "To", "Call-ID", "CSeq"] {
        let value = single(&request.data.headers, name)?;
        let value = if name == "To" && tag.is_some() && self::tag(value).is_none() {
            format!("{value};tag={}", tag.unwrap())
        } else {
            value.into()
        };
        push(&mut headers, name, &value)?;
    }
    for h in request.data.headers.all("Record-Route") {
        headers.push(h.clone());
    }
    if let Some(contact) = contact {
        push(&mut headers, "Contact", contact)?;
    }
    if !body.is_empty() {
        push(&mut headers, "Content-Type", "application/sdp")?;
    }
    Ok(Response {
        status,
        reason: reason.into(),
        data: MessageData::new(request.data.version.clone(), headers, body),
    })
}
mod completed_invite;
pub(crate) mod media;
mod reliable_provisional;
#[cfg(test)]
mod tests;
pub(crate) mod ussd;
