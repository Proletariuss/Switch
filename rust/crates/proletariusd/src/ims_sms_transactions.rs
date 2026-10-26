//! Shared IMS SMS transaction owner for native and remaining Go consumers.
//!
//! Rust constructs originating SMS and inbound-delivery-report MESSAGE frames,
//! reserves their transaction identities, and owns multipart acceptance/unknown
//! transitions. Native callers use direct typed operations; the remaining Go
//! adapter writes frames and forwards received response bytes for its sessions.

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use proletarius_sms::{
    build_mobile_originated_data,
    submit::{self, EncodingPolicy},
};
use proletarius_vowifi::sms_transaction::{
    DeliveryReportRequestContext, SmsPartIdentity, SmsRequestContext, SmsResponseDisposition,
    SmsSubmission, SmsSubmissionStatus, build_sms_delivery_report, build_sms_message,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

mod delivery_reports;

const BODY_LIMIT: usize = 128 * 1024;
const MAX_SESSIONS: usize = 256;
const MAX_BATCHES_PER_SESSION: usize = 128;
const RECENT_BATCH_LIMIT: usize = 256;
pub(crate) const INTERNAL_PATH: &str = "/api/internal/ims/sms-transactions";

#[derive(Clone, Default)]
pub(crate) struct Service {
    state: Arc<Mutex<State>>,
}

/// Direct native preparation uses the same sequence owner as the legacy adapter.
/// Wire bytes remain local; no HTTP/base64 round trip is needed.
#[allow(
    dead_code,
    reason = "native session report preparation awaits event loop integration"
)]
pub(crate) struct PreparedDeliveryReportWire {
    pub(crate) call_id: String,
    pub(crate) cseq: u32,
    pub(crate) wire: zeroize::Zeroizing<Vec<u8>>,
}

#[derive(Default)]
struct State {
    next_native_owner: u64,
    sessions: HashMap<String, Session>,
}

#[derive(Default)]
struct Session {
    native_owner: Option<u64>,
    native_generation: Option<crate::native_vowifi_supervisor::SessionGeneration>,
    next_cseq: u32,
    active_batch: Option<String>,
    batches: HashMap<String, Batch>,
    recent_batches: VecDeque<String>,
}

struct Batch {
    submission: SmsSubmission,
    total_parts: u16,
    prepared: Option<PreparedPart>,
}

struct PreparedPart {
    identity: SmsPartIdentity,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Command {
    PrepareText {
        recipient: String,
        smsc: String,
        text: String,
        #[serde(default = "default_status_report")]
        request_status_report: bool,
    },
    DecodeIncomingDelivery {
        sip_call_id: String,
        sip_cseq: u32,
        received_at_unix_ms: i64,
        rpdu_base64: String,
    },
    DecodeAndStoreIncomingDelivery {
        profile_id: String,
        event_id: String,
        sip_call_id: String,
        sip_cseq: u32,
        received_at_unix_ms: i64,
        rpdu_base64: String,
    },
    DecodeAndArchiveIncomingDelivery {
        archive_device: String,
        #[serde(default)]
        subscription_id: String,
        sip_call_id: String,
        sip_cseq: u32,
        received_at_unix_ms: i64,
        rpdu_base64: String,
    },
    Prepare {
        session_id: String,
        batch_id: String,
        total_parts: usize,
        #[serde(default)]
        minimum_cseq: u32,
        target: String,
        public_identity: String,
        local_host: String,
        local_port: u16,
        route_headers: Vec<String>,
        #[serde(default)]
        security_verify: Option<String>,
        #[serde(default)]
        access_network_info: Option<String>,
        rpdu_base64: String,
    },
    PrepareDeliveryReport {
        session_id: String,
        #[serde(default)]
        minimum_cseq: u32,
        target: String,
        in_reply_to: String,
        public_identity: String,
        local_host: String,
        local_port: u16,
        route_headers: Vec<String>,
        #[serde(default)]
        security_verify: Option<String>,
        #[serde(default)]
        access_network_info: Option<String>,
        reference: u8,
    },
    Submit {
        session_id: String,
        batch_id: String,
        call_id: String,
        cseq: u32,
    },
    Observe {
        session_id: String,
        batch_id: String,
        response_base64: String,
    },
    Cancel {
        session_id: String,
        batch_id: String,
    },
    Abort {
        session_id: String,
        batch_id: String,
    },
    Retire {
        session_id: String,
    },
}

#[derive(Serialize)]
struct PreparedResponse {
    disposition: &'static str,
    call_id: String,
    cseq: u32,
    part: u16,
    total_parts: u16,
    wire_base64: String,
}

#[derive(Serialize)]
struct PreparedDeliveryReportResponse {
    disposition: &'static str,
    call_id: String,
    cseq: u32,
    wire_base64: String,
}

#[derive(Serialize)]
struct PreparedTextPartResponse {
    reference: u8,
    rpdu_base64: String,
}

#[derive(Serialize)]
struct PreparedTextResponse {
    parts: Vec<PreparedTextPartResponse>,
}

fn default_status_report() -> bool {
    true
}

#[derive(Serialize)]
struct StateResponse {
    method: &'static str,
    disposition: &'static str,
    accepted_parts: u16,
    total_parts: u16,
    completed: bool,
    status: Option<u16>,
}

#[derive(Serialize)]
struct IncomingDeliveryResponse {
    rpdu_base64: String,
    tpdu_base64: String,
    sip_call_id: String,
    sip_cseq: u32,
    rp_reference: u8,
    originator: String,
    sender: String,
    recipient: String,
    content: String,
    timestamp_unix_ms: i64,
    has_timestamp: bool,
    decoded: bool,
    status_report: bool,
    delivery_status: String,
    status_code: u8,
    message_reference: Option<u8>,
    concat: Option<IncomingConcat>,
    decode_error: String,
}

#[derive(Serialize)]
struct IncomingConcat {
    reference: u16,
    ref_bits: u8,
    total: u8,
    sequence: u8,
}

pub(crate) async fn handle(
    request: Request<Incoming>,
    service: Service,
    multistandby_store: crate::multistandby_store::Store,
    stopping: bool,
) -> Response<Full<Bytes>> {
    if request.method() != Method::POST {
        return failure(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    let body = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        BODY_LIMIT,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(command) = serde_json::from_slice::<Command>(&body) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if stopping
        && matches!(
            &command,
            Command::PrepareText { .. }
                | Command::Prepare { .. }
                | Command::PrepareDeliveryReport { .. }
                | Command::Submit { .. }
        )
    {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "backend_stopping");
    }
    let result = match command {
        Command::DecodeAndStoreIncomingDelivery {
            profile_id,
            event_id,
            sip_call_id,
            sip_cseq,
            received_at_unix_ms,
            rpdu_base64,
        } => {
            decode_and_store_incoming_delivery(
                multistandby_store,
                profile_id,
                event_id,
                sip_call_id,
                sip_cseq,
                received_at_unix_ms,
                rpdu_base64,
            )
            .await
        }
        Command::DecodeAndArchiveIncomingDelivery {
            archive_device,
            subscription_id,
            sip_call_id,
            sip_cseq,
            received_at_unix_ms,
            rpdu_base64,
        } => {
            decode_and_archive_incoming_delivery(
                multistandby_store.data_root().to_path_buf(),
                archive_device,
                subscription_id,
                sip_call_id,
                sip_cseq,
                received_at_unix_ms,
                rpdu_base64,
            )
            .await
        }
        other => service.execute(other),
    };
    match result {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err((status, code)) => failure(status, code),
    }
}

/// Native text parts are never encoded into an internal HTTP command.
pub(crate) struct NativeTextPart {
    pub(crate) reference: u8,
    pub(crate) rpdu: zeroize::Zeroizing<Vec<u8>>,
}
pub(crate) struct NativePreparedPart {
    pub(crate) identity: SmsPartIdentity,
    pub(crate) part: u16,
    pub(crate) total_parts: u16,
    pub(crate) request: proletarius_sip::Request,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeBatchState {
    pub(crate) status: SmsSubmissionStatus,
    pub(crate) total_parts: u16,
}
pub(crate) struct NativeObservation {
    pub(crate) disposition: SmsResponseDisposition,
    pub(crate) state: NativeBatchState,
}

pub(crate) fn prepare_native_text(
    recipient: &str,
    smsc: &str,
    text: &str,
    request_status_report: bool,
) -> Result<Vec<NativeTextPart>, &'static str> {
    let mut seed = [0u8; 1];
    getrandom::fill(&mut seed).map_err(|_| "entropy_unavailable")?;
    prepare_native_text_with_reference(recipient, smsc, text, request_status_report, seed[0])
        .map_err(|(_, code)| code)
}

/// Shared transaction service + explicit registered-session owner identifier.
#[allow(dead_code, reason = "native session startup remains gated")]
pub(crate) struct NativeReportOwner {
    token: u64,
    service: Service,
    session_id: String,
}

impl NativeReportOwner {
    pub(crate) fn new(service: Service, session_id: String) -> Result<Self, &'static str> {
        validate_id(&session_id).map_err(|(_, code)| code)?;
        let mut state = service
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sessions.contains_key(&session_id) {
            return Err("native_transaction_session_exists");
        }
        if state.sessions.len() >= MAX_SESSIONS {
            return Err("transaction_capacity");
        }
        let token = state
            .next_native_owner
            .checked_add(1)
            .ok_or("native_owner_exhausted")?;
        state.next_native_owner = token;
        state.sessions.insert(
            session_id.clone(),
            Session {
                native_owner: Some(token),
                ..Session::default()
            },
        );
        drop(state);
        Ok(Self {
            token,
            service,
            session_id,
        })
    }
}

impl NativeReportOwner {
    pub(crate) fn bind_generation(
        &self,
        generation: crate::native_vowifi_supervisor::SessionGeneration,
    ) -> Result<(), &'static str> {
        let mut state = self
            .service
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = native_session_mut(&mut state, self)?;
        if session
            .native_generation
            .is_some_and(|current| current != generation)
        {
            return Err("native_sms_session_mismatch");
        }
        session.native_generation = Some(generation);
        Ok(())
    }
    /// Atomic CSeq/batch admission shares the report sequence owner. No JSON
    /// command, HTTP request or Go transport is involved.
    pub(crate) fn prepare_part(
        &self,
        batch_id: &str,
        total_parts: usize,
        target: &str,
        routing: &NativeSmsRouting<'_>,
        rpdu: &[u8],
    ) -> Result<NativePreparedPart, &'static str> {
        validate_id(batch_id).map_err(|(_, code)| code)?;
        let total = u16::try_from(total_parts)
            .ok()
            .filter(|n| (1..=255).contains(n))
            .ok_or("invalid_batch_size")?;
        let mut random = [0u8; 40];
        getrandom::fill(&mut random).map_err(|_| "entropy_unavailable")?;
        let identity = SmsPartIdentity {
            call_id: hex(&random[..16]),
            cseq: 0,
        };
        let tag = hex(&random[16..28]);
        let branch = format!("z9hG4bK{}", hex(&random[28..]));
        let mut state = self
            .service
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = native_session_mut(&mut state, self)?;
        if session.native_generation != Some(routing.generation) {
            return Err("native_sms_session_mismatch");
        }
        if session
            .active_batch
            .as_deref()
            .is_some_and(|active| active != batch_id)
        {
            return Err("another_sms_batch_active");
        }
        if session.recent_batches.iter().any(|id| id == batch_id) {
            return Err("sms_batch_already_finished");
        }
        let part = match session.batches.get(batch_id) {
            Some(batch) if batch.total_parts != total || batch.prepared.is_some() => {
                return Err("sms_batch_state_conflict");
            }
            Some(batch) => match batch.submission.status() {
                SmsSubmissionStatus::Ready { accepted_parts, .. } => accepted_parts + 1,
                _ => return Err("sms_batch_not_ready"),
            },
            None if session.batches.len() >= MAX_BATCHES_PER_SESSION => {
                return Err("transaction_capacity");
            }
            None => 1,
        };
        let cseq = session
            .next_cseq
            .max(routing.minimum_cseq)
            .checked_add(1)
            .filter(|n| *n != 0 && *n < (1 << 31))
            .ok_or("cseq_exhausted")?;
        let routes = routing
            .route_headers
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let wire = zeroize::Zeroizing::new(
            build_sms_message(
                &SmsRequestContext {
                    target,
                    public_identity: routing.public_identity,
                    call_id: &identity.call_id,
                    from_tag: &tag,
                    via_branch: &branch,
                    local_host: routing.local_host,
                    local_port: routing.local_port,
                    cseq,
                    route_headers: &routes,
                    security_verify: routing.security_verify,
                    access_network_info: routing.access_network_info,
                },
                rpdu,
            )
            .map_err(|_| "invalid_sms_context")?,
        );
        let (message, used) = proletarius_sip::Parser::default()
            .parse(wire.as_slice(), true)
            .map_err(|_| "invalid_sms_context")?;
        let proletarius_sip::Message::Request(request) = message else {
            return Err("invalid_sms_context");
        };
        if used != wire.len() {
            return Err("invalid_sms_context");
        }
        let identity = SmsPartIdentity { cseq, ..identity };
        let batch = session.batches.entry(batch_id.to_owned()).or_insert(Batch {
            submission: SmsSubmission::new(total_parts).map_err(|_| "invalid_batch_size")?,
            total_parts: total,
            prepared: None,
        });
        batch.prepared = Some(PreparedPart {
            identity: identity.clone(),
        });
        session.next_cseq = cseq;
        session.active_batch = Some(batch_id.to_owned());
        Ok(NativePreparedPart {
            identity,
            part,
            total_parts: total,
            request,
        })
    }

    pub(crate) fn submit_part(
        &self,
        batch_id: &str,
        identity: &SmsPartIdentity,
    ) -> Result<NativeBatchState, &'static str> {
        let mut state = self
            .service
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = native_session_mut(&mut state, self)?;
        if session.active_batch.as_deref() != Some(batch_id) {
            return Err("sms_batch_not_active");
        }
        let batch = session
            .batches
            .get_mut(batch_id)
            .ok_or("sms_batch_not_found")?;
        let prepared = batch.prepared.take().ok_or("sms_part_not_prepared")?;
        if prepared.identity != *identity {
            batch.prepared = Some(prepared);
            return Err("sms_transaction_identity_mismatch");
        }
        if batch
            .submission
            .mark_submitted(identity.call_id.clone(), identity.cseq)
            .is_err()
        {
            batch.prepared = Some(prepared);
            return Err("sms_batch_not_ready");
        }
        Ok(native_state(batch))
    }

    pub(crate) fn observe_part(
        &self,
        batch_id: &str,
        event: &proletarius_vowifi::sip_session_runtime::RegisteredSipEvent,
    ) -> Result<NativeObservation, &'static str> {
        let mut state = self
            .service
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = native_session_mut(&mut state, self)?;
        let batch = session
            .batches
            .get_mut(batch_id)
            .ok_or("sms_batch_not_found")?;
        if batch.prepared.is_some() {
            return Err("sms_part_not_submitted");
        }
        let disposition = event
            .observe_sms_response(&mut batch.submission)
            .map_err(|_| "invalid_sip_response")?;
        let result = NativeObservation {
            disposition,
            state: native_state(batch),
        };
        if matches!(
            result.state.status,
            SmsSubmissionStatus::Accepted { .. } | SmsSubmissionStatus::Rejected { .. }
        ) {
            session.batches.remove(batch_id);
            if session.active_batch.as_deref() == Some(batch_id) {
                session.active_batch = None;
            }
            remember_batch(session, batch_id);
        }
        Ok(result)
    }

    pub(crate) fn unknown_part(&self, batch_id: &str) -> Result<NativeBatchState, &'static str> {
        let mut state = self
            .service
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = native_session_mut(&mut state, self)?;
        let batch = session
            .batches
            .get_mut(batch_id)
            .ok_or("sms_batch_not_found")?;
        if batch.prepared.is_some() {
            return Err("sms_part_not_submitted");
        }
        match batch.submission.status() {
            SmsSubmissionStatus::AwaitingResponse { .. } => batch
                .submission
                .mark_outcome_unknown()
                .map_err(|_| "sms_batch_not_awaiting_response")?,
            SmsSubmissionStatus::OutcomeUnknown { .. } => {}
            _ => return Err("sms_batch_not_awaiting_response"),
        }
        let snapshot = native_state(batch);
        if session.active_batch.as_deref() == Some(batch_id) {
            session.active_batch = None;
        }
        Ok(snapshot)
    }

    /// Aborting unsubmitted work never erases accepted-part counts in the
    /// native caller's retained receipt or an already-submitted part.
    pub(crate) fn abort_part(&self, batch_id: &str) -> Result<(), &'static str> {
        let mut state = self
            .service
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = native_session_mut(&mut state, self)?;
        let batch = session.batches.get(batch_id).ok_or("sms_batch_not_found")?;
        if !matches!(batch.submission.status(), SmsSubmissionStatus::Ready { .. }) {
            return Err("sms_part_already_submitted");
        }
        session.batches.remove(batch_id);
        if session.active_batch.as_deref() == Some(batch_id) {
            session.active_batch = None;
        }
        remember_batch(session, batch_id);
        Ok(())
    }
}
fn native_session_mut<'a>(
    state: &'a mut State,
    owner: &NativeReportOwner,
) -> Result<&'a mut Session, &'static str> {
    let session = state
        .sessions
        .get_mut(&owner.session_id)
        .ok_or("ims_session_not_found")?;
    if session.native_owner != Some(owner.token) {
        return Err("native_transaction_session_mismatch");
    }
    Ok(session)
}
fn native_state(batch: &Batch) -> NativeBatchState {
    NativeBatchState {
        status: batch.submission.status().clone(),
        total_parts: batch.total_parts,
    }
}

impl Drop for NativeReportOwner {
    fn drop(&mut self) {
        let mut state = self
            .service
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(session) = state.sessions.get_mut(&self.session_id) else {
            return;
        };
        if session.native_owner != Some(self.token) {
            return;
        }
        // Unresolved outgoing SMS batches retain their recovery state. Pure
        // report sessions may release capacity immediately on owner teardown.
        if session.batches.is_empty() {
            state.sessions.remove(&self.session_id);
        } else {
            session.native_owner = None;
        }
    }
}

/// Registered routing snapshot, independent of any incoming MESSAGE identity.
/// The session owner supplies this snapshot; receipt and transaction service
/// supply In-Reply-To, target, report Call-ID, tag and branch.
pub(crate) struct NativeSmsRouting<'a> {
    pub(crate) generation: crate::native_vowifi_supervisor::SessionGeneration,
    pub(crate) minimum_cseq: u32,
    pub(crate) public_identity: &'a str,
    pub(crate) local_host: &'a str,
    pub(crate) local_port: u16,
    pub(crate) route_headers: &'a [String],
    pub(crate) security_verify: Option<&'a str>,
    pub(crate) access_network_info: Option<&'a str>,
}

impl<'a> NativeSmsRouting<'a> {
    pub(crate) fn session(
        generation: crate::native_vowifi_supervisor::SessionGeneration,
        minimum_cseq: u32,
        snapshot: &'a proletarius_vowifi::message_routing::RegisteredMessageContext,
        local_host: &'a str,
        access_network_info: Option<&'a str>,
    ) -> Self {
        Self::registered(
            generation,
            minimum_cseq,
            snapshot.routing(),
            local_host,
            snapshot.local_port(),
            snapshot.security_verify(),
            access_network_info,
        )
    }

    pub(crate) fn registered(
        generation: crate::native_vowifi_supervisor::SessionGeneration,
        minimum_cseq: u32,
        routing: &'a proletarius_vowifi::message_routing::RegisteredMessageRouting,
        local_host: &'a str,
        local_port: u16,
        security_verify: Option<&'a str>,
        access_network_info: Option<&'a str>,
    ) -> Self {
        Self {
            generation,
            minimum_cseq,
            public_identity: routing.identity(),
            local_host,
            local_port,
            route_headers: routing.routes(),
            security_verify,
            access_network_info,
        }
    }
}

/// Durable destination chosen by the session owner, never by a SIP sender.
#[derive(Clone, Default)]
pub(crate) enum NativeSmsArchive {
    #[default]
    Ordinary,
    Module,
    Nwait {
        store: crate::multistandby_store::Store,
        profile_id: String,
    },
}

/// Content-free classification of a committed N-wait raw record. A decoded
/// fragment can also require isolation, so this is not a decoder error code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NativeSmsArchiveWarning {
    RawRecordUnmerged,
}
impl NativeSmsArchiveWarning {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::RawRecordUnmerged => {
                "收到无法解码或分组存在歧义的短信，原始记录已保存，未与其他短信合并"
            }
        }
    }
}

/// Opaque proof returned only after the real SMS archive transaction commits.
/// Its fields are private and subscriber identities have no Debug output.
#[allow(dead_code, reason = "native event consumer remains gated")]
pub(crate) struct NativeSmsCommit {
    generation: crate::native_vowifi_supervisor::SessionGeneration,
    call_id: zeroize::Zeroizing<String>,
    reference: u8,
    report_target: Option<zeroize::Zeroizing<String>>,
    archive_warning: Option<NativeSmsArchiveWarning>,
}

impl NativeSmsCommit {
    pub(crate) fn archive_warning(&self) -> Option<NativeSmsArchiveWarning> {
        self.archive_warning
    }

    pub(crate) fn prepare_report(
        self,
        generation: crate::native_vowifi_supervisor::SessionGeneration,
        owner: &NativeReportOwner,
        context: &NativeSmsRouting<'_>,
    ) -> Result<PreparedDeliveryReportWire, &'static str> {
        if generation != self.generation || context.generation != self.generation {
            return Err("native_sms_session_mismatch");
        }
        owner.bind_generation(self.generation)?;
        let target = self
            .report_target
            .as_deref()
            .ok_or("invalid_delivery_report_route")?;
        owner
            .service
            .prepare_delivery_report_wire(
                owner.session_id.clone(),
                context.minimum_cseq,
                target.to_string(),
                self.call_id.to_string(),
                context.public_identity.into(),
                context.local_host.into(),
                context.local_port,
                context
                    .route_headers
                    .iter()
                    .map(|route| route.to_owned())
                    .collect(),
                context.security_verify.map(str::to_owned),
                context.access_network_info.map(str::to_owned),
                self.reference,
                Some(owner),
            )
            .map_err(|(_, code)| code)
    }

    #[allow(dead_code, reason = "consumed by native RP-ACK event handler")]
    pub(crate) fn into_delivery_report(
        self,
        generation: crate::native_vowifi_supervisor::SessionGeneration,
        context: &DeliveryReportRequestContext<'_>,
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, &'static str> {
        if generation != self.generation || context.in_reply_to != self.call_id.as_str() {
            return Err("native_sms_session_mismatch");
        }
        build_sms_delivery_report(context, self.reference)
            .map(zeroize::Zeroizing::new)
            .map_err(|_| "invalid_delivery_report_context")
    }
}

/// Use the same durable writer/decoder as the current production Go adapter.
/// Non-SMS events do not create a receipt; no RP-ACK frame exists before commit.
#[allow(dead_code, reason = "native event consumer remains gated")]
pub(crate) async fn archive_native_sms_event(
    data_root: PathBuf,
    subscription_id: &str,
    generation: crate::native_vowifi_supervisor::SessionGeneration,
    event: &crate::native_ims_actor::Event,
    received_at_unix_ms: i64,
) -> Result<Option<NativeSmsCommit>, &'static str> {
    archive_native_sms_event_to(
        data_root,
        subscription_id,
        generation,
        event,
        received_at_unix_ms,
        &NativeSmsArchive::Ordinary,
    )
    .await
}

pub(crate) async fn archive_native_sms_event_to(
    data_root: PathBuf,
    subscription_id: &str,
    generation: crate::native_vowifi_supervisor::SessionGeneration,
    event: &crate::native_ims_actor::Event,
    received_at_unix_ms: i64,
    archive: &NativeSmsArchive,
) -> Result<Option<NativeSmsCommit>, &'static str> {
    use proletarius_vowifi::sip_session_runtime::{RegisteredSipEvent, RegisteredSipMessage};
    if event.generation != generation {
        return Err("native_sms_session_mismatch");
    }
    let RegisteredSipEvent::Message { message, .. } = &event.event else {
        return Ok(None);
    };
    let RegisteredSipMessage::Request {
        method,
        headers,
        body,
        ..
    } = message.as_ref()
    else {
        return Ok(None);
    };
    if *method != proletarius_sip::Method::MESSAGE {
        return Ok(None);
    }
    for name in ["From", "To", "Call-ID", "CSeq", "Content-Type"] {
        if headers.all(name).count() != 1 {
            return Err("invalid_incoming_sms_headers");
        }
    }
    if headers.all("Via").next().is_none() {
        return Err("invalid_incoming_sms_headers");
    }
    let content_type = headers
        .get("Content-Type")
        .unwrap()
        .value()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    if !content_type.eq_ignore_ascii_case("application/vnd.3gpp.sms") {
        return Ok(None);
    }
    let call_id = headers.get("Call-ID").unwrap().value();
    let mut cseq = headers
        .get("CSeq")
        .unwrap()
        .value()
        .split_ascii_whitespace();
    let number = cseq.next().ok_or("invalid_incoming_sms_headers")?;
    if !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid_incoming_sms_headers");
    }
    let cseq_number: u32 = number.parse().map_err(|_| "invalid_incoming_sms_headers")?;
    if cseq_number >= (1 << 31) || cseq.next() != Some("MESSAGE") || cseq.next().is_some() {
        return Err("invalid_incoming_sms_headers");
    }
    let archived = match archive {
        NativeSmsArchive::Ordinary | NativeSmsArchive::Module => {
            if !valid_subscription_id(subscription_id) {
                return Err("invalid_subscription_id");
            }
            let archived = decode_and_archive_incoming_delivery(
                data_root,
                if matches!(archive, NativeSmsArchive::Module) {
                    "dji"
                } else {
                    "reader"
                }
                .into(),
                subscription_id.to_owned(),
                call_id.to_owned(),
                cseq_number,
                received_at_unix_ms,
                BASE64.encode(body.as_slice()),
            )
            .await
            .map_err(|(_, code)| code)?;
            if !matches!(archived["store_state"].as_str(), Some("archived" | "status_report_archived")) {
                return Ok(None);
            }
            archived
        }
        NativeSmsArchive::Nwait { store, profile_id } => {
            // Each delivery has its own transaction ID. Raw RPDU/profile dedup
            // in the store handles retransmissions even when arrival time differs.
            let key =
                crate::multistandby_store::random_token().map_err(|_| "sms_store_unavailable")?;
            decode_and_store_incoming_delivery(
                store.clone(),
                profile_id.clone(),
                format!("native_mt_{key}"),
                call_id.to_owned(),
                cseq_number,
                received_at_unix_ms,
                BASE64.encode(body.as_slice()),
            )
            .await
            .map_err(|(_, code)| code)?
        }
    };
    let reference = archived["rp_reference"]
        .as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .ok_or("invalid_incoming_rp_reference")?;
    let archive_warning = (matches!(archive, NativeSmsArchive::Nwait { .. })
        && archived["store_state"] == "undecoded")
        .then_some(NativeSmsArchiveWarning::RawRecordUnmerged);
    Ok(Some(NativeSmsCommit {
        generation,
        archive_warning,
        call_id: zeroize::Zeroizing::new(call_id.to_owned()),
        reference,
        report_target: proletarius_vowifi::sms_transaction::incoming_delivery_report_target(
            headers,
        )
        .ok()
        .map(zeroize::Zeroizing::new),
    }))
}

async fn decode_and_archive_incoming_delivery(
    data_root: PathBuf,
    archive_device: String,
    subscription_id: String,
    sip_call_id: String,
    sip_cseq: u32,
    received_at_unix_ms: i64,
    rpdu_base64: String,
) -> Result<serde_json::Value, (StatusCode, &'static str)> {
    let mut event =
        decode_incoming_delivery(sip_call_id, sip_cseq, received_at_unix_ms, rpdu_base64)?;
    if event["decoded"].as_bool() != Some(true) {
        return Ok(event);
    }

    let archive_device = archive_device.trim();
    if !matches!(archive_device, "reader" | "dji") {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid_archive_device"));
    }
    let subscription_id = subscription_id.trim();
    if !subscription_id.is_empty() && !valid_subscription_id(subscription_id) {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid_subscription_id"));
    }
    if event["status_report"].as_bool() == Some(true) {
        delivery_reports::persist(data_root, archive_device, subscription_id, &event)
            .await
            .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "sms_archive_unavailable"))?;
        event["store_state"] = json!("status_report_archived");
        return Ok(event);
    }
    let sender = event["sender"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or((StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_sender"))?;
    let content = event["content"].as_str().unwrap_or_default();
    let received_at = event["timestamp_unix_ms"]
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or((StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_timestamp"))?;
    let timestamp =
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(received_at) * 1_000_000)
            .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_timestamp"))?
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_timestamp"))?;
    let mut record = json!({
        "sender": sender,
        "content": content,
        "timestamp": timestamp,
        "direction": "in",
        "device": archive_device,
        "archived": true,
    });
    if let Some(concat) = event.get("concat").filter(|value| !value.is_null()) {
        record["concat"] = json!({
            "reference": concat["reference"],
            "reference_bits": concat["ref_bits"],
            "total": concat["total"],
            "sequence": concat["sequence"],
        });
    }
    if !subscription_id.is_empty() {
        record["subscription_id"] = json!(subscription_id);
    }
    let archive = proletarius_store::ArchiveStore::new(data_root.join("sms-archive.json"));
    crate::message_archive::ingest_messages_to(archive, vec![record])
        .await
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "sms_archive_unavailable"))?;
    event["store_state"] = json!("archived");
    Ok(event)
}

fn valid_subscription_id(value: &str) -> bool {
    value.len() == 69
        && value.starts_with("sub1_")
        && value[5..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn decode_and_store_incoming_delivery(
    store: crate::multistandby_store::Store,
    profile_id: String,
    event_id: String,
    sip_call_id: String,
    sip_cseq: u32,
    received_at_unix_ms: i64,
    rpdu_base64: String,
) -> Result<serde_json::Value, (StatusCode, &'static str)> {
    let mut event =
        decode_incoming_delivery(sip_call_id, sip_cseq, received_at_unix_ms, rpdu_base64)?;
    let received_at = event["timestamp_unix_ms"]
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or((StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_timestamp"))?;
    let rpdu = BASE64
        .decode(event["rpdu_base64"].as_str().unwrap_or_default())
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_rpdu_encoding"))?;
    let tpdu = BASE64
        .decode(event["tpdu_base64"].as_str().unwrap_or_default())
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_tpdu_encoding"))?;
    let mut record = json!({
        "event_id":event_id,
        "profile_id":profile_id,
        "sender":event["sender"],
        "content":event["content"],
        "timestamp_ms":received_at,
        "timestamp_missing":false,
        "raw_rpdu_hex":hex(&rpdu),
        "raw_tpdu_hex":hex(&tpdu),
        "decoded":event["decoded"],
        "status_report":event["status_report"],
        "delivery_status":event["delivery_status"],
        "status_code":event["status_code"],
    });
    if !event["message_reference"].is_null() {
        record["message_reference"] = event["message_reference"].clone();
    }
    if let Some(concat) = event.get("concat").filter(|value| !value.is_null()) {
        record["concat"] = json!({
            "reference":concat["reference"],
            "reference_bits":concat["ref_bits"],
            "total":concat["total"],
            "sequence":concat["sequence"],
        });
    }
    let stored = crate::archive_io::run(move || store.apply("inbound-event", record))
        .await
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "sms_store_unavailable"))?;
    let store_state = stored
        .get("state")
        .and_then(serde_json::Value::as_str)
        .filter(|state| !state.is_empty())
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "sms_store_unavailable"))?;
    event["store_state"] = json!(store_state);
    Ok(event)
}

fn decode_incoming_delivery(
    sip_call_id: String,
    sip_cseq: u32,
    received_at_unix_ms: i64,
    rpdu_base64: String,
) -> Result<serde_json::Value, (StatusCode, &'static str)> {
    if sip_call_id.trim().is_empty() || sip_call_id.len() > 512 {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid_sip_call_id"));
    }
    let rpdu = BASE64
        .decode(rpdu_base64)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_rpdu_encoding"))?;
    if rpdu.len() > 255 {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "rpdu_too_large"));
    }
    let message = proletarius_sms::MobileTerminatedData::parse(&rpdu)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_incoming_rpdu"))?;
    let originator = message.originator.unwrap_or_default();
    let tpdu = message.tpdu.clone();
    let mut response = IncomingDeliveryResponse {
        rpdu_base64: BASE64.encode(&rpdu),
        tpdu_base64: BASE64.encode(&tpdu),
        sip_call_id,
        sip_cseq,
        rp_reference: message.reference,
        sender: originator.clone(),
        originator,
        recipient: String::new(),
        content: String::new(),
        timestamp_unix_ms: received_at_unix_ms,
        has_timestamp: false,
        decoded: false,
        status_report: false,
        delivery_status: String::new(),
        status_code: 0,
        message_reference: None,
        concat: None,
        decode_error: String::new(),
    };

    match tpdu.first().map(|value| value & 0x03) {
        Some(0) => match proletarius_sms::deliver::parse(&tpdu) {
            Ok(delivery) => match delivery.content {
                proletarius_sms::user_data::Content::Text(content) => {
                    response.sender = delivery.sender;
                    response.content = content;
                    response.decoded = true;
                    if let Some(timestamp) = delivery.service_center_time {
                        response.timestamp_unix_ms = timestamp_unix_ms(&timestamp)
                            .ok_or((StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_timestamp"))?;
                        response.has_timestamp = true;
                    }
                    response.concat = delivery.concatenation.map(|value| IncomingConcat {
                        reference: value.reference,
                        ref_bits: if value.wide_reference { 16 } else { 8 },
                        total: value.total,
                        sequence: value.sequence,
                    });
                }
                proletarius_sms::user_data::Content::Binary(_) => {
                    response.decode_error = "native TPDU decoding failed".into();
                }
            },
            Err(_) => response.decode_error = "native TPDU decoding failed".into(),
        },
        Some(2) => match proletarius_sms::status_report::parse(&tpdu) {
            Ok(report) => {
                response.recipient = report.recipient;
                response.status_report = true;
                response.delivery_status = report.status.as_str().into();
                response.status_code = report.status_code;
                response.message_reference = Some(report.reference);
                response.decoded = true;
            }
            Err(_) => response.decode_error = "native TPDU decoding failed".into(),
        },
        _ => response.decode_error = "TPDU decoding failed".into(),
    }

    Ok(serde_json::to_value(response).expect("incoming SMS event is serializable"))
}

fn timestamp_unix_ms(timestamp: &proletarius_sms::timestamp::Timestamp) -> Option<i64> {
    let date = time::Date::from_calendar_date(
        i32::from(timestamp.year),
        time::Month::try_from(timestamp.month).ok()?,
        timestamp.day,
    )
    .ok()?;
    let clock = time::Time::from_hms(timestamp.hour, timestamp.minute, timestamp.second).ok()?;
    let offset =
        time::UtcOffset::from_whole_seconds(i32::from(timestamp.offset_minutes) * 60).ok()?;
    let nanos = date
        .with_time(clock)
        .assume_offset(offset)
        .unix_timestamp_nanos();
    i64::try_from(nanos / 1_000_000).ok()
}

impl Service {
    fn execute(&self, command: Command) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        match command {
            Command::PrepareText {
                recipient,
                smsc,
                text,
                request_status_report,
            } => prepare_text(&recipient, &smsc, &text, request_status_report),
            Command::DecodeIncomingDelivery {
                sip_call_id,
                sip_cseq,
                received_at_unix_ms,
                rpdu_base64,
            } => decode_incoming_delivery(sip_call_id, sip_cseq, received_at_unix_ms, rpdu_base64),
            Command::DecodeAndStoreIncomingDelivery { .. } => {
                Err((StatusCode::SERVICE_UNAVAILABLE, "sms_store_unavailable"))
            }
            Command::DecodeAndArchiveIncomingDelivery { .. } => {
                Err((StatusCode::SERVICE_UNAVAILABLE, "sms_archive_unavailable"))
            }
            Command::Prepare {
                session_id,
                batch_id,
                total_parts,
                minimum_cseq,
                target,
                public_identity,
                local_host,
                local_port,
                route_headers,
                security_verify,
                access_network_info,
                rpdu_base64,
            } => self.prepare(
                session_id,
                batch_id,
                total_parts,
                minimum_cseq,
                target,
                public_identity,
                local_host,
                local_port,
                route_headers,
                security_verify,
                access_network_info,
                rpdu_base64,
            ),
            Command::PrepareDeliveryReport {
                session_id,
                minimum_cseq,
                target,
                in_reply_to,
                public_identity,
                local_host,
                local_port,
                route_headers,
                security_verify,
                access_network_info,
                reference,
            } => self.prepare_delivery_report(
                session_id,
                minimum_cseq,
                target,
                in_reply_to,
                public_identity,
                local_host,
                local_port,
                route_headers,
                security_verify,
                access_network_info,
                reference,
            ),
            Command::Submit {
                session_id,
                batch_id,
                call_id,
                cseq,
            } => self.submit(&session_id, &batch_id, &call_id, cseq),
            Command::Observe {
                session_id,
                batch_id,
                response_base64,
            } => self.observe(&session_id, &batch_id, response_base64),
            Command::Cancel {
                session_id,
                batch_id,
            } => self.cancel(&session_id, &batch_id),
            Command::Abort {
                session_id,
                batch_id,
            } => self.abort(&session_id, &batch_id),
            Command::Retire { session_id } => self.retire(&session_id),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_delivery_report(
        &self,
        session_id: String,
        minimum_cseq: u32,
        target: String,
        in_reply_to: String,
        public_identity: String,
        local_host: String,
        local_port: u16,
        route_headers: Vec<String>,
        security_verify: Option<String>,
        access_network_info: Option<String>,
        reference: u8,
    ) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        let prepared = self.prepare_delivery_report_wire(
            session_id,
            minimum_cseq,
            target,
            in_reply_to,
            public_identity,
            local_host,
            local_port,
            route_headers,
            security_verify,
            access_network_info,
            reference,
            None,
        )?;
        Ok(serde_json::to_value(PreparedDeliveryReportResponse {
            disposition: "prepared",
            call_id: prepared.call_id,
            cseq: prepared.cseq,
            wire_base64: BASE64.encode(prepared.wire.as_slice()),
        })
        .expect("delivery report response is serializable"))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_delivery_report_wire(
        &self,
        session_id: String,
        minimum_cseq: u32,
        target: String,
        in_reply_to: String,
        public_identity: String,
        local_host: String,
        local_port: u16,
        route_headers: Vec<String>,
        security_verify: Option<String>,
        access_network_info: Option<String>,
        reference: u8,
        owner: Option<&NativeReportOwner>,
    ) -> Result<PreparedDeliveryReportWire, (StatusCode, &'static str)> {
        validate_id(&session_id)?;
        let mut random = [0u8; 40];
        getrandom::fill(&mut random)
            .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "entropy_unavailable"))?;
        let call_id = hex(&random[..16]);
        let from_tag = hex(&random[16..28]);
        let via_branch = format!("z9hG4bK{}", hex(&random[28..]));
        let routes = route_headers.iter().map(String::as_str).collect::<Vec<_>>();

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.sessions.contains_key(&session_id) && state.sessions.len() >= MAX_SESSIONS {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "transaction_capacity"));
        }
        let session = if let Some(owner) = owner {
            if owner.session_id != session_id
                || !std::sync::Arc::ptr_eq(&self.state, &owner.service.state)
            {
                return Err((StatusCode::CONFLICT, "native_transaction_session_replaced"));
            }
            native_session_mut(&mut state, owner).map_err(|code| (StatusCode::CONFLICT, code))?
        } else {
            let session = state.sessions.entry(session_id).or_default();
            require_legacy_session(session)?;
            session
        };
        let cseq = session
            .next_cseq
            .max(minimum_cseq)
            .checked_add(1)
            .filter(|value| *value != 0 && *value < (1 << 31))
            .ok_or((StatusCode::SERVICE_UNAVAILABLE, "cseq_exhausted"))?;
        let context = DeliveryReportRequestContext {
            target: &target,
            in_reply_to: &in_reply_to,
            public_identity: &public_identity,
            call_id: &call_id,
            from_tag: &from_tag,
            via_branch: &via_branch,
            local_host: &local_host,
            local_port,
            cseq,
            route_headers: &routes,
            security_verify: security_verify.as_deref(),
            access_network_info: access_network_info.as_deref(),
        };
        let wire = build_sms_delivery_report(&context, reference).map_err(|_| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_delivery_report_context",
            )
        })?;
        session.next_cseq = cseq;
        Ok(PreparedDeliveryReportWire {
            call_id,
            cseq,
            wire: zeroize::Zeroizing::new(wire),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare(
        &self,
        session_id: String,
        batch_id: String,
        total_parts: usize,
        minimum_cseq: u32,
        target: String,
        public_identity: String,
        local_host: String,
        local_port: u16,
        route_headers: Vec<String>,
        security_verify: Option<String>,
        access_network_info: Option<String>,
        rpdu_base64: String,
    ) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        validate_id(&session_id)?;
        validate_id(&batch_id)?;
        let total_parts = u16::try_from(total_parts)
            .ok()
            .filter(|count| (1..=255).contains(count))
            .ok_or((StatusCode::UNPROCESSABLE_ENTITY, "invalid_batch_size"))?;
        let rpdu = BASE64
            .decode(rpdu_base64)
            .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_rpdu_encoding"))?;
        let mut random = [0u8; 40];
        getrandom::fill(&mut random)
            .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "entropy_unavailable"))?;
        let call_id = hex(&random[..16]);
        let from_tag = hex(&random[16..28]);
        let via_branch = format!("z9hG4bK{}", hex(&random[28..]));
        let routes = route_headers.iter().map(String::as_str).collect::<Vec<_>>();

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.sessions.contains_key(&session_id) && state.sessions.len() >= MAX_SESSIONS {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "transaction_capacity"));
        }
        let session = state.sessions.entry(session_id.clone()).or_default();
        require_legacy_session(session)?;
        if session
            .active_batch
            .as_deref()
            .is_some_and(|active| active != batch_id)
        {
            return Err((StatusCode::CONFLICT, "another_sms_batch_active"));
        }
        if session.recent_batches.iter().any(|seen| seen == &batch_id) {
            return Err((StatusCode::CONFLICT, "sms_batch_already_finished"));
        }
        if let Some(batch) = session.batches.get(&batch_id) {
            if batch.total_parts != total_parts || batch.prepared.is_some() {
                return Err((StatusCode::CONFLICT, "sms_batch_state_conflict"));
            }
            if !matches!(batch.submission.status(), SmsSubmissionStatus::Ready { .. }) {
                return Err((StatusCode::CONFLICT, "sms_batch_not_ready"));
            }
        } else if session.batches.len() >= MAX_BATCHES_PER_SESSION {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "transaction_capacity"));
        }

        let cseq = session
            .next_cseq
            .max(minimum_cseq)
            .checked_add(1)
            .filter(|value| *value != 0 && *value < (1 << 31))
            .ok_or((StatusCode::SERVICE_UNAVAILABLE, "cseq_exhausted"))?;
        let context = SmsRequestContext {
            target: &target,
            public_identity: &public_identity,
            call_id: &call_id,
            from_tag: &from_tag,
            via_branch: &via_branch,
            local_host: &local_host,
            local_port,
            cseq,
            route_headers: &routes,
            security_verify: security_verify.as_deref(),
            access_network_info: access_network_info.as_deref(),
        };
        let wire = build_sms_message(&context, &rpdu)
            .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_context"))?;
        let part = match session.batches.get(&batch_id) {
            Some(batch) => match batch.submission.status() {
                SmsSubmissionStatus::Ready { accepted_parts, .. } => accepted_parts + 1,
                _ => return Err((StatusCode::CONFLICT, "sms_batch_not_ready")),
            },
            None => 1,
        };
        let total = total_parts;
        if session.batches.get(&batch_id).is_none() {
            session.batches.insert(
                batch_id.clone(),
                Batch {
                    submission: SmsSubmission::new(usize::from(total_parts))
                        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_batch_size"))?,
                    total_parts,
                    prepared: None,
                },
            );
        }
        let session = state
            .sessions
            .get_mut(&session_id)
            .expect("inserted session");
        let batch = session
            .batches
            .get_mut(&batch_id)
            .expect("inserted SMS batch");
        batch.prepared = Some(PreparedPart {
            identity: SmsPartIdentity {
                call_id: call_id.clone(),
                cseq,
            },
        });
        session.active_batch = Some(batch_id);
        session.next_cseq = cseq;
        Ok(serde_json::to_value(PreparedResponse {
            disposition: "prepared",
            call_id,
            cseq,
            part,
            total_parts: total,
            wire_base64: BASE64.encode(wire),
        })
        .expect("prepared response is serializable"))
    }

    fn submit(
        &self,
        session_id: &str,
        batch_id: &str,
        call_id: &str,
        cseq: u32,
    ) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        validate_id(session_id)?;
        validate_id(batch_id)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = session_mut(&mut state, session_id)?;
        if session.active_batch.as_deref() != Some(batch_id) {
            return Err((StatusCode::CONFLICT, "sms_batch_not_active"));
        }
        let batch = batch_mut(session, batch_id)?;
        let prepared = batch
            .prepared
            .take()
            .ok_or((StatusCode::CONFLICT, "sms_part_not_prepared"))?;
        if prepared.identity.call_id != call_id || prepared.identity.cseq != cseq {
            batch.prepared = Some(prepared);
            return Err((StatusCode::CONFLICT, "sms_transaction_identity_mismatch"));
        }
        if batch.submission.mark_submitted(call_id, cseq).is_err() {
            batch.prepared = Some(prepared);
            return Err((StatusCode::CONFLICT, "sms_batch_not_ready"));
        }
        Ok(state_response(batch, "submitted", false, None))
    }

    fn observe(
        &self,
        session_id: &str,
        batch_id: &str,
        response_base64: String,
    ) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        validate_id(session_id)?;
        validate_id(batch_id)?;
        let response = BASE64.decode(response_base64).map_err(|_| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_response_encoding",
            )
        })?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = session_mut(&mut state, session_id)?;
        let batch = batch_mut(session, batch_id)?;
        if batch.prepared.is_some() {
            return Err((StatusCode::CONFLICT, "sms_part_not_submitted"));
        }
        let disposition = batch
            .submission
            .observe_response(&response)
            .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_sip_response"))?;
        let (name, completed, status) = match disposition {
            SmsResponseDisposition::Ignored => ("ignored", false, None),
            SmsResponseDisposition::Provisional => ("provisional", false, None),
            SmsResponseDisposition::PartAccepted { completed } => ("accepted", completed, None),
            SmsResponseDisposition::Rejected {
                status,
                accepted_parts: _,
            } => ("rejected", true, Some(status)),
            SmsResponseDisposition::LateResultResolved { accepted, .. } => (
                if accepted {
                    "late_accepted"
                } else {
                    "late_rejected"
                },
                true,
                None,
            ),
        };
        let response = state_response(batch, name, completed, status);
        let terminal = matches!(
            batch.submission.status(),
            SmsSubmissionStatus::Rejected { .. } | SmsSubmissionStatus::Accepted { .. }
        );
        if terminal {
            let session = state
                .sessions
                .get_mut(session_id)
                .expect("session retained");
            if session.active_batch.as_deref() == Some(batch_id) {
                session.active_batch = None;
            }
            session.batches.remove(batch_id);
            remember_batch(session, batch_id);
        }
        Ok(response)
    }

    fn cancel(
        &self,
        session_id: &str,
        batch_id: &str,
    ) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        validate_id(session_id)?;
        validate_id(batch_id)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = session_mut(&mut state, session_id)?;
        let batch = batch_mut(session, batch_id)?;
        if batch.prepared.is_some() {
            return Err((StatusCode::CONFLICT, "sms_part_not_submitted"));
        }
        match batch.submission.status() {
            SmsSubmissionStatus::AwaitingResponse { .. } => batch
                .submission
                .mark_outcome_unknown()
                .map_err(|_| (StatusCode::CONFLICT, "sms_batch_not_awaiting_response"))?,
            SmsSubmissionStatus::OutcomeUnknown { .. } => {}
            SmsSubmissionStatus::Ready { .. }
            | SmsSubmissionStatus::Rejected { .. }
            | SmsSubmissionStatus::Accepted { .. } => {
                return Err((StatusCode::CONFLICT, "sms_batch_not_awaiting_response"));
            }
        }
        let response = state_response(batch, "unknown", false, None);
        let session = state
            .sessions
            .get_mut(session_id)
            .expect("session retained");
        if session.active_batch.as_deref() == Some(batch_id) {
            session.active_batch = None;
        }
        Ok(response)
    }

    fn abort(
        &self,
        session_id: &str,
        batch_id: &str,
    ) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        validate_id(session_id)?;
        validate_id(batch_id)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(session) = state.sessions.get_mut(session_id) else {
            return Ok(
                json!({"disposition":"aborted","accepted_parts":0,"total_parts":0,"completed":true}),
            );
        };
        require_legacy_session(session)?;
        if let Some(batch) = session.batches.get(batch_id) {
            if batch.prepared.is_none()
                || !matches!(batch.submission.status(), SmsSubmissionStatus::Ready { .. })
            {
                return Err((StatusCode::CONFLICT, "sms_part_already_submitted"));
            }
        } else {
            return Ok(
                json!({"disposition":"aborted","accepted_parts":0,"total_parts":0,"completed":true}),
            );
        }
        session.batches.remove(batch_id);
        if session.active_batch.as_deref() == Some(batch_id) {
            session.active_batch = None;
        }
        Ok(json!({"disposition":"aborted","accepted_parts":0,"total_parts":0,"completed":true}))
    }

    fn retire(&self, session_id: &str) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        validate_id(session_id)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sessions.get(session_id).is_some_and(|session| {
            session.native_owner.is_some() || session.native_generation.is_some()
        }) {
            return Err((StatusCode::CONFLICT, "native_transaction_session_owned"));
        }
        state.sessions.remove(session_id);
        Ok(json!({"disposition":"retired"}))
    }
}

fn prepare_text(
    recipient: &str,
    smsc: &str,
    text: &str,
    request_status_report: bool,
) -> Result<serde_json::Value, (StatusCode, &'static str)> {
    let mut seed = [0u8; 1];
    getrandom::fill(&mut seed)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "entropy_unavailable"))?;
    let parts = prepare_text_with_reference(recipient, smsc, text, request_status_report, seed[0])?;
    serde_json::to_value(PreparedTextResponse { parts })
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))
}

fn prepare_text_with_reference(
    recipient: &str,
    smsc: &str,
    text: &str,
    request_status_report: bool,
    seed: u8,
) -> Result<Vec<PreparedTextPartResponse>, (StatusCode, &'static str)> {
    prepare_native_text_with_reference(recipient, smsc, text, request_status_report, seed).map(
        |parts| {
            parts
                .into_iter()
                .map(|part| PreparedTextPartResponse {
                    reference: part.reference,
                    rpdu_base64: BASE64.encode(part.rpdu.as_slice()),
                })
                .collect()
        },
    )
}
fn prepare_native_text_with_reference(
    recipient: &str,
    smsc: &str,
    text: &str,
    request_status_report: bool,
    seed: u8,
) -> Result<Vec<NativeTextPart>, (StatusCode, &'static str)> {
    let tpdus = submit::prepare_with_policy(
        recipient,
        text,
        seed,
        EncodingPolicy::Auto,
        request_status_report,
    )
    .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_text"))?;
    if tpdus.is_empty() || tpdus.len() > 255 {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_text"));
    }
    tpdus
        .into_iter()
        .enumerate()
        .map(|(index, mut tpdu)| {
            if tpdu.len() < 2 {
                return Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_text"));
            }
            let reference = seed.wrapping_add(index as u8);
            tpdu[1] = reference;
            let rpdu = build_mobile_originated_data(reference, smsc, &tpdu)
                .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "invalid_sms_text"))?;
            Ok(NativeTextPart {
                reference,
                rpdu: zeroize::Zeroizing::new(rpdu),
            })
        })
        .collect()
}

fn session_mut<'a>(
    state: &'a mut State,
    session_id: &str,
) -> Result<&'a mut Session, (StatusCode, &'static str)> {
    let session = state
        .sessions
        .get_mut(session_id)
        .ok_or((StatusCode::NOT_FOUND, "ims_session_not_found"))?;
    require_legacy_session(session)?;
    Ok(session)
}

fn require_legacy_session(session: &Session) -> Result<(), (StatusCode, &'static str)> {
    if session.native_owner.is_some() || session.native_generation.is_some() {
        Err((StatusCode::CONFLICT, "native_transaction_session_owned"))
    } else {
        Ok(())
    }
}

fn batch_mut<'a>(
    session: &'a mut Session,
    batch_id: &str,
) -> Result<&'a mut Batch, (StatusCode, &'static str)> {
    session
        .batches
        .get_mut(batch_id)
        .ok_or((StatusCode::NOT_FOUND, "sms_batch_not_found"))
}

fn state_response(
    batch: &Batch,
    disposition: &'static str,
    completed: bool,
    status: Option<u16>,
) -> serde_json::Value {
    let accepted_parts = match batch.submission.status() {
        SmsSubmissionStatus::Ready { accepted_parts, .. }
        | SmsSubmissionStatus::AwaitingResponse { accepted_parts, .. }
        | SmsSubmissionStatus::OutcomeUnknown { accepted_parts, .. }
        | SmsSubmissionStatus::Rejected { accepted_parts, .. } => *accepted_parts,
        SmsSubmissionStatus::Accepted { parts } => *parts,
    };
    serde_json::to_value(StateResponse {
        method: "MESSAGE",
        disposition,
        accepted_parts,
        total_parts: batch.total_parts,
        completed,
        status,
    })
    .expect("state response is serializable")
}

fn validate_id(value: &str) -> Result<(), (StatusCode, &'static str)> {
    if value.len() < 16
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_transaction_owner_id",
        ));
    }
    Ok(())
}

fn remember_batch(session: &mut Session, batch_id: &str) {
    session.recent_batches.push_back(batch_id.to_owned());
    while session.recent_batches.len() > RECENT_BATCH_LIMIT {
        session.recent_batches.pop_front();
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"IMS SMS transaction rejected","code":code,"retryable":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::{Command, Service, StatusCode, prepare_text_with_reference};
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    const SESSION: &str = "fixture-session-0001";
    const BATCH: &str = "fixture-batch-0001";

    #[test]
    fn rust_text_preparation_preserves_sms_submit_and_rpdu_contract() {
        let parts =
            prepare_text_with_reference("+447700900123", "+447802002606", "BAL", true, 42).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].reference, 42);
        let rpdu = BASE64.decode(&parts[0].rpdu_base64).unwrap();
        assert_eq!(&rpdu[..4], &[0x00, 42, 0x00, 0x07]);
        assert_eq!(rpdu[12], 0x21, "status-report request bit remains set");
        assert_eq!(rpdu[13], 42, "TP-MR follows the RP-MR seed");
        assert_eq!(rpdu[14], 12, "recipient length is retained");
    }

    #[test]
    fn rust_text_preparation_allocates_sequential_wrapping_references() {
        let text = "界".repeat(140);
        let parts = prepare_text_with_reference("888", "+447802002606", &text, true, 255).unwrap();
        assert!(parts.len() > 1);
        assert_eq!(parts[0].reference, 255);
        assert_eq!(parts[1].reference, 0);
        for part in parts {
            let rpdu = BASE64.decode(part.rpdu_base64).unwrap();
            assert_eq!(rpdu[1], part.reference);
            let address_len = usize::from(rpdu[3]);
            let tpdu_offset = 5 + address_len;
            assert_eq!(rpdu[tpdu_offset + 1], part.reference);
        }
    }

    struct StoreFixture {
        root: PathBuf,
        store: crate::multistandby_store::Store,
    }

    impl StoreFixture {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "sw-ims-sms-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let store = crate::multistandby_store::Store::open(&root).unwrap();
            store.recover().unwrap();
            store
                .apply(
                    "catalog",
                    json!({"event_id":"catalog","profiles":[
                        {"profile_id":"A","card_id":"card","eid":"eid","iccid":"iccid-a"},
                        {"profile_id":"B","card_id":"card","eid":"eid","iccid":"iccid-b"}
                    ]}),
                )
                .unwrap();
            Self { root, store }
        }
    }

    impl Drop for StoreFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    fn incoming_event(tpdu: &[u8]) -> serde_json::Value {
        let mut rpdu = vec![1, 42, 0, 0, tpdu.len() as u8];
        rpdu.extend_from_slice(tpdu);
        Service::default()
            .execute(Command::DecodeIncomingDelivery {
                sip_call_id: "network-transaction@example.test".into(),
                sip_cseq: 73,
                received_at_unix_ms: 1_800_000_000_123,
                rpdu_base64: BASE64.encode(rpdu),
            })
            .unwrap()
    }

    #[test]
    fn rust_decodes_inbound_sms_and_builds_durable_event_metadata() {
        let deliver = [
            0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
        ];
        let event = incoming_event(&deliver);
        assert_eq!(event["sip_call_id"], "network-transaction@example.test");
        assert_eq!(event["sip_cseq"], 73);
        assert_eq!(event["rp_reference"], 42);
        assert_eq!(event["originator"], "");
        assert_eq!(event["sender"], "888");
        assert_eq!(event["content"], "A");
        assert_eq!(event["decoded"], true);
        let service_center_time = time::Date::from_calendar_date(2026, time::Month::September, 6)
            .unwrap()
            .with_time(time::Time::MIDNIGHT)
            .assume_utc()
            .unix_timestamp_nanos()
            / 1_000_000;
        let service_center_time = i64::try_from(service_center_time).unwrap();
        assert_eq!(event["timestamp_unix_ms"], service_center_time);
        assert_eq!(event["has_timestamp"], true);

        let fragment = [
            0x40, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 8, 5, 0, 3, 9, 2, 1, 0,
            0x41,
        ];
        let event = incoming_event(&fragment);
        assert_eq!(event["concat"]["reference"], 9);
        assert_eq!(event["concat"]["ref_bits"], 8);
        assert_eq!(event["concat"]["total"], 2);
        assert_eq!(event["concat"]["sequence"], 1);

        let report = [
            2, 1, 4, 0x91, 0x44, 0x77, 0x62, 0x80, 0x62, 0x61, 0x21, 0, 0, 0x62, 0x80, 0x62, 0x61,
            0x21, 0, 0, 0,
        ];
        let event = incoming_event(&report);
        assert_eq!(event["decoded"], true);
        assert_eq!(event["status_report"], true);
        assert_eq!(event["recipient"], "+4477");
        assert_eq!(event["message_reference"], 1);
        assert_eq!(event["delivery_status"], "delivered");
        assert_eq!(event["status_code"], 0);
        assert_eq!(event["has_timestamp"], false);
        assert_eq!(event["timestamp_unix_ms"], 1_800_000_000_123_i64);
    }

    #[test]
    fn rust_retains_valid_rpdu_when_tpdu_is_not_decodable() {
        let event = incoming_event(&[0]);
        assert_eq!(event["rp_reference"], 42);
        assert_eq!(event["decoded"], false);
        assert_eq!(event["decode_error"], "native TPDU decoding failed");
        assert_eq!(event["timestamp_unix_ms"], 1_800_000_000_123_i64);
    }

    #[test]
    fn rust_rejects_malformed_rpdu_before_an_event_can_be_persisted() {
        let result = Service::default().execute(Command::DecodeIncomingDelivery {
            sip_call_id: "network-transaction@example.test".into(),
            sip_cseq: 73,
            received_at_unix_ms: 1_800_000_000_123,
            rpdu_base64: BASE64.encode([1, 42, 0, 0, 5, 0]),
        });
        assert_eq!(
            result,
            Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid_incoming_rpdu"))
        );
    }

    #[tokio::test]
    async fn rust_atomically_decodes_and_persists_nwait_inbound_sms() {
        let fixture = StoreFixture::new();
        let tpdu = [
            0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
        ];
        let mut rpdu = vec![1, 42, 0, 0, tpdu.len() as u8];
        rpdu.extend_from_slice(&tpdu);
        let request = |profile: &str, event: &str, call_id: &str, cseq| {
            super::Command::DecodeAndStoreIncomingDelivery {
                profile_id: profile.into(),
                event_id: event.into(),
                sip_call_id: call_id.into(),
                sip_cseq: cseq,
                received_at_unix_ms: 1_800_000_000_123,
                rpdu_base64: BASE64.encode(&rpdu),
            }
        };
        let first = super::decode_and_store_incoming_delivery(
            fixture.store.clone(),
            "A".into(),
            "receive-one".into(),
            "call-one".into(),
            73,
            1_800_000_000_123,
            BASE64.encode(&rpdu),
        )
        .await
        .unwrap();
        assert_eq!(first["sender"], "888");
        assert_eq!(first["content"], "A");
        assert_eq!(first["store_state"], "received");
        let revision = fixture.store.current_revision().unwrap();

        let retry = match request("A", "receive-two", "call-retry", 74) {
            Command::DecodeAndStoreIncomingDelivery {
                profile_id,
                event_id,
                sip_call_id,
                sip_cseq,
                received_at_unix_ms,
                rpdu_base64,
            } => super::decode_and_store_incoming_delivery(
                fixture.store.clone(),
                profile_id,
                event_id,
                sip_call_id,
                sip_cseq,
                received_at_unix_ms,
                rpdu_base64,
            )
            .await
            .unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(retry["store_state"], "received");
        assert_eq!(fixture.store.current_revision().unwrap(), revision);

        let other_profile = super::decode_and_store_incoming_delivery(
            fixture.store.clone(),
            "B".into(),
            "receive-profile-b".into(),
            "call-b".into(),
            75,
            1_800_000_000_123,
            BASE64.encode(&rpdu),
        )
        .await
        .unwrap();
        assert_eq!(other_profile["store_state"], "received");
        assert_eq!(fixture.store.current_revision().unwrap(), revision + 1);

        let failure = super::decode_and_store_incoming_delivery(
            fixture.store.clone(),
            "A".into(),
            "bad-rpdu".into(),
            "call-bad".into(),
            76,
            1_800_000_000_123,
            BASE64.encode([1, 42, 0, 0, 5, 0]),
        )
        .await;
        assert_eq!(
            failure,
            Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid_incoming_rpdu"))
        );
        assert_eq!(fixture.store.current_revision().unwrap(), revision + 1);
    }

    fn native_event(
        generation: crate::native_vowifi_supervisor::SessionGeneration,
    ) -> crate::native_ims_actor::Event {
        use proletarius_vowifi::sip_session_runtime::{RegisteredSipEvent, RegisteredSipMessage};
        let tpdu = [
            0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
        ];
        let mut body = vec![1, 42, 0, 0, tpdu.len() as u8];
        body.extend_from_slice(&tpdu);
        let mut wire = format!("MESSAGE sip:reader@ims.example SIP/2.0\r\nVia: SIP/2.0/TCP pcscf;branch=z9hG4bKnative\r\nFrom: <sip:smsc@ims.example>\r\nP-Asserted-Identity: <sip:smsc@ims.example>\r\nTo: <sip:reader@ims.example>\r\nCall-ID: native-delivery@example\r\nCSeq: 73 MESSAGE\r\nContent-Type: application/vnd.3gpp.sms\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        wire.extend_from_slice(&body);
        let (proletarius_sip::Message::Request(request), _) = proletarius_sip::Parser::default()
            .parse(&wire, false)
            .unwrap()
        else {
            panic!("fixture request")
        };
        crate::native_ims_actor::Event {
            reply_state: crate::native_ims_actor::IncomingReply::Unanswered,
            generation,
            event: RegisteredSipEvent::Message {
                connection: proletarius_vowifi::registration_coordinator::SipConnectionId::Primary,
                message: Box::new(RegisteredSipMessage::Request {
                    method: request.method,
                    uri: request.uri,
                    version: request.data.version,
                    headers: request.data.headers,
                    body: zeroize::Zeroizing::new(request.data.body),
                }),
                response_ticket: None,
            },
        }
    }

    fn native_generation(
        supervisor: &mut crate::native_vowifi_supervisor::NativeVoWiFiSupervisor,
        reader: &str,
    ) -> crate::native_vowifi_supervisor::SessionGeneration {
        supervisor
            .reserve(
                crate::native_vowifi_supervisor::ReaderSessionKey::new(
                    reader,
                    "8901234567890123456",
                )
                .unwrap(),
            )
            .unwrap()
            .generation()
    }

    fn native_report_context(
        generation: crate::native_vowifi_supervisor::SessionGeneration,
    ) -> super::NativeSmsRouting<'static> {
        super::NativeSmsRouting {
            generation,
            public_identity: "sip:reader@ims.example",
            local_host: "10.0.0.2",
            local_port: 5060,
            minimum_cseq: 74,
            route_headers: &[],
            security_verify: None,
            access_network_info: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn registered_tcp_actor_and_consumer_archive_then_exchange_real_rp_ack() {
        registered_tcp_sms_report_exchange(Some(202), false, false).await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn registered_tcp_report_rejection_preserves_archive_without_replay() {
        registered_tcp_sms_report_exchange(Some(403), false, false).await;
    }
    #[tokio::test(flavor = "current_thread")]
    async fn registered_tcp_peer_disconnect_after_report_retains_unknown_result_and_archive() {
        registered_tcp_sms_report_exchange(None, false, false).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn registered_tcp_report_timeout_preserves_archive_and_never_replays_on_live_stream() {
        registered_tcp_sms_report_exchange(None, true, false).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn registered_tcp_archive_failure_never_sends_success_response_or_report() {
        registered_tcp_sms_report_exchange(None, false, true).await;
    }

    async fn registered_tcp_sms_report_exchange(
        final_status: Option<u16>,
        wait_for_timeout: bool,
        archive_failure: bool,
    ) {
        use proletarius_ims::{
            SecAgreeMode,
            security_policy::LocalEndpoints,
            stream::{SipStreamItem, SipStreamReader},
        };
        use proletarius_vowifi::{
            netstack_runtime::TunnelNetwork,
            netstack_tcp::TcpSocketRegistry,
            registration_coordinator::{
                RegisterAttempt, RegistrationPlan, RegistrationServices, register,
            },
            security_runtime::SecurityNetworkRuntime,
            sip_session_runtime::RegisteredSipRuntime,
        };
        use tokio::{
            io::AsyncWriteExt,
            sync::{mpsc, oneshot, watch},
        };
        let fixture = StoreFixture::new();
        if archive_failure {
            std::fs::create_dir(fixture.root.join("sms-archive.json")).unwrap();
        }
        let config = |address: &str| proletarius_vowifi::TunnelConfiguration {
            ipv4_addresses: vec![address.parse().unwrap()],
            ..Default::default()
        };
        let local_network = TunnelNetwork::new(&config("10.0.0.2")).unwrap();
        let remote_network = TunnelNetwork::new(&config("10.20.30.9")).unwrap();
        let (local_wake_tx, local_wake) = watch::channel(0u64);
        let (remote_wake_tx, remote_wake) = watch::channel(0u64);
        let (local_tcp, local_registry) = TcpSocketRegistry::new(local_wake_tx.clone());
        let (remote_tcp, remote_registry) = TcpSocketRegistry::new(remote_wake_tx);
        let (security, _unserviced_security) = SecurityNetworkRuntime::new(
            |_: &mut smoltcp::iface::Interface, _: &mut smoltcp::iface::SocketSet<'static>| {
                Ok::<(), std::convert::Infallible>(())
            },
            local_wake_tx,
        );
        let (local_out, remote_in) = mpsc::channel(64);
        let (remote_out, local_in) = mpsc::channel(64);
        let (network_stop, network_stop_rx) = watch::channel(false);
        let local_runner = local_network.run_managed(
            smoltcp::iface::SocketSet::new(vec![]),
            local_in,
            local_out,
            network_stop_rx.clone(),
            local_wake,
            local_registry,
        );
        let remote_runner = remote_network.run_managed(
            smoltcp::iface::SocketSet::new(vec![]),
            remote_in,
            remote_out,
            network_stop_rx,
            remote_wake,
            remote_registry,
        );
        let application = async {
            let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
            let key = crate::native_vowifi_supervisor::ReaderSessionKey::new(
                "reader",
                "8901234567890123456",
            )
            .unwrap();
            let permit = supervisor.reserve(key.clone()).unwrap();
            supervisor.mark_running(&permit).unwrap();
            let generation = permit.generation();
            let cancel = permit.cancellation_sender();
            let (ready, listening) = oneshot::channel();
            let (report_committed, mut report_confirmed) = oneshot::channel();
            let mut report_committed = Some(report_committed);
            let root = fixture.root.clone();
            let peer = tokio::spawn(async move {
                let listener = remote_tcp
                    .listen(6060, Some("10.0.0.2:5064".parse().unwrap()))
                    .await
                    .unwrap();
                ready.send(()).unwrap();
                let stream = listener.accept().await.unwrap();
                let (read, mut write) = tokio::io::split(stream);
                let mut reader = SipStreamReader::new(read);
                let Some(SipStreamItem::Message(register)) = reader.next().await.unwrap() else {
                    panic!("REGISTER");
                };
                assert!(register.starts_with(b"REGISTER "));
                write.write_all(b"SIP/2.0 200 OK\r\nCall-ID: register-fixture\r\nCSeq: 1 REGISTER\r\nP-Associated-URI: <sip:authorized@ims.example>\r\nService-Route: <sip:registered-route@ims.example;lr>\r\nContent-Length: 0\r\n\r\n").await.unwrap();
                let tpdu = [
                    0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
                ];
                let mut body = vec![1, 42, 0, 0, tpdu.len() as u8];
                body.extend_from_slice(&tpdu);
                let mut incoming = format!("MESSAGE sip:reader@ims.example SIP/2.0\r\nVia: SIP/2.0/TCP pcscf;branch=z9hG4bKincoming\r\nFrom: <sip:smsc@ims.example>\r\nP-Asserted-Identity: <sip:smsc@ims.example>\r\nTo: <sip:reader@ims.example>\r\nCall-ID: incoming-fixture\r\nCSeq: 73 MESSAGE\r\nContent-Type: application/vnd.3gpp.sms\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
                incoming.extend_from_slice(&body);
                write.write_all(&incoming).await.unwrap();
                if archive_failure {
                    report_confirmed.await.unwrap();
                    match tokio::time::timeout(std::time::Duration::from_millis(100), reader.next())
                        .await
                    {
                        Err(_) | Ok(Ok(None)) => {}
                        _ => panic!("archive failure emitted a SIP frame"),
                    }
                    reader.into_inner().unsplit(write).close().await.unwrap();
                    return;
                }
                let Some(SipStreamItem::Message(response)) = reader.next().await.unwrap() else {
                    panic!("SIP200");
                };
                assert!(response.starts_with(b"SIP/2.0 200"));
                assert_eq!(
                    proletarius_store::ArchiveStore::new(root.join("sms-archive.json"))
                        .load()
                        .unwrap()
                        .len(),
                    1,
                    "network confirmation preceded archive commit"
                );
                let Some(SipStreamItem::Message(report)) = reader.next().await.unwrap() else {
                    panic!("RP-ACK");
                };
                let (proletarius_sip::Message::Request(request), _) =
                    proletarius_sip::Parser::default()
                        .parse(&report, false)
                        .unwrap()
                else {
                    panic!("MESSAGE");
                };
                assert_eq!(request.uri.to_string(), "sip:smsc@ims.example");
                assert_eq!(request.data.body, proletarius_sms::build_ack(42));
                assert!(
                    request
                        .data
                        .headers
                        .get("From")
                        .unwrap()
                        .value()
                        .contains("sip:authorized@ims.example")
                );
                assert_eq!(
                    request.data.headers.get("Route").unwrap().value(),
                    "<sip:registered-route@ims.example;lr>"
                );
                if wait_for_timeout {
                    // Observe the live stream until the client sees its timer.
                    // Explicit registry retirement need not produce peer EOF.
                    tokio::select! {
                        item = reader.next() => { assert!(item.unwrap().is_none(), "report was replayed before its timeout"); }
                        _ = report_confirmed => {}
                    }
                    reader.into_inner().unsplit(write).close().await.unwrap();
                    return;
                }
                let Some(status) = final_status else {
                    report_confirmed.await.unwrap();
                    reader.into_inner().unsplit(write).close().await.unwrap();
                    return;
                };
                let mut headers = request.data.headers;
                headers.set(proletarius_sip::Header::new("Content-Length", "0").unwrap());
                let final_reply = proletarius_sip::Message::Response(proletarius_sip::Response {
                    status,
                    reason: "Accepted".into(),
                    data: proletarius_sip::MessageData::new("SIP/2.0", headers, Vec::new()),
                })
                .encode();
                write.write_all(&final_reply).await.unwrap();
                // Exact local socket retirement need not produce peer EOF.
                // Keep observing for replay until the actor's real cleanup
                // completes; do not delay client stop for a peer flush.
                tokio::select! {
                    item = reader.next() => {
                        assert!(item.unwrap().is_none(), "report was replayed after its final response");
                        report_confirmed.await.unwrap();
                    }
                    result = &mut report_confirmed => { result.unwrap(); }
                }
                reader.into_inner().unsplit(write).close().await.unwrap();
            });
            listening.await.unwrap();
            let (registration_events, _registration_input) = mpsc::channel(16);
            let session = register(RegistrationServices::new(&local_tcp, &security, &registration_events),
                RegistrationPlan { call_id: "register-fixture", initial_cseq: 1, sec_agree: SecAgreeMode::Disabled,
                    plain_peer: "10.20.30.9:6060".parse().unwrap(), plain_local_port: 5064, local_ip: "10.0.0.2".parse().unwrap(),
                    local_endpoints: LocalEndpoints { spi_c: 1, spi_s: 2, port_c: 5064, port_s: 5063 }, client_mechanisms: &[], strict_security: false },
                tokio::time::Instant::now() + std::time::Duration::from_secs(5), permit.stop_signal(),
                |attempt| { let RegisterAttempt::Initial { cseq, .. } = attempt else { panic!("unexpected authentication"); };
                    Ok(zeroize::Zeroizing::new(format!("REGISTER sip:ims.example SIP/2.0\r\nVia: SIP/2.0/TCP 10.0.0.2:5064;branch=z9hG4bKregister\r\nFrom: <sip:reader@ims.example>;tag=r\r\nTo: <sip:reader@ims.example>\r\nCall-ID: register-fixture\r\nCSeq: {cseq} REGISTER\r\nContent-Length: 0\r\n\r\n").into_bytes())) },
                |_| async { Err::<proletarius_ims::aka_exchange::AuthenticatedChallenge, _>("unexpected authentication") }).await.unwrap_or_else(|_| panic!("registration"));
            let snapshot = session
                .message_context("sip:temporary@ims.example", &[])
                .unwrap();
            assert!(session.belongs_to_tcp(&local_tcp));
            let (other_wake, _) = watch::channel(0u64);
            let (other_tcp, _other_registry) = TcpSocketRegistry::new(other_wake);
            assert!(!session.belongs_to_tcp(&other_tcp));
            assert_eq!(snapshot.local_port(), 5064);
            assert_eq!(
                snapshot.local_ip(),
                "10.0.0.2".parse::<std::net::IpAddr>().unwrap()
            );
            assert!(snapshot.security_verify().is_none());
            let (commands, command_input, events, event_input) =
                crate::native_ims_actor::channels();
            let (_lease, lease) = watch::channel(false);
            let consumer = crate::native_sms_consumer::Consumer::new(
                fixture.root.clone(),
                format!("sub1_{}", "a".repeat(64)),
                generation,
                commands,
                permit.stop_signal(),
                lease.clone(),
                super::NativeReportOwner::new(
                    Service::default(),
                    "real-stream-fixture-0001".into(),
                )
                .unwrap(),
            );
            let consumer =
                crate::native_sms_consumer::RegisteredConsumer::new(consumer, snapshot, None, 74);
            let (notices, mut inbox) = mpsc::channel(32);
            let consume = tokio::spawn(consumer.run(event_input, notices));
            let actor = tokio::spawn(async move {
                crate::native_ims_actor::run(
                    RegisteredSipRuntime::new(session),
                    &key,
                    permit,
                    lease,
                    command_input,
                    events,
                )
                .await
            });
            let mut submitted_count = 0;
            let mut archive_error = None;
            loop {
                match inbox.recv().await {
                    Some(crate::native_sms_consumer::LoopNotice::Failed(code))
                        if archive_failure =>
                    {
                        assert_eq!(code, "sms_archive_unavailable");
                        archive_error = Some(code);
                        report_committed.take().unwrap().send(()).unwrap();
                        break;
                    }
                    Some(crate::native_sms_consumer::LoopNotice::Outcome(
                        crate::native_sms_consumer::SessionOutcome::ReportFinished {
                            state, ..
                        },
                    )) => {
                        if wait_for_timeout {
                            report_committed.take().unwrap().send(()).unwrap();
                            assert_eq!(state, proletarius_vowifi::sms_transaction::DeliveryReportState::OutcomeUnknown);
                            break;
                        }
                        let status = final_status.expect("peer disconnected before final response");
                        let expected = if status < 300 {
                            proletarius_vowifi::sms_transaction::DeliveryReportState::Accepted
                        } else {
                            proletarius_vowifi::sms_transaction::DeliveryReportState::Rejected(
                                status,
                            )
                        };
                        assert_eq!(state, expected);
                        break;
                    }
                    Some(crate::native_sms_consumer::LoopNotice::Outcome(
                        crate::native_sms_consumer::SessionOutcome::ReportSubmitted(_),
                    )) => {
                        submitted_count += 1;
                        if final_status.is_none() && !wait_for_timeout {
                            report_committed.take().unwrap().send(()).unwrap();
                        }
                    }
                    None if final_status.is_none() && !wait_for_timeout && !archive_failure => {
                        break;
                    }
                    _ => panic!("unexpected consumer notice"),
                }
            }
            assert_eq!(
                submitted_count,
                if archive_failure { 0 } else { 1 },
                "report was lost or replayed"
            );
            cancel.send_replace(true);
            let exit = actor.await.unwrap();
            assert!(exit.cleanup.is_ok());
            let consume = consume.await.unwrap();
            if final_status.is_none() && !wait_for_timeout && !archive_failure {
                assert_eq!(consume.unconfirmed_reports.len(), 1);
                assert_eq!(
                    consume.unconfirmed_reports[0].1,
                    proletarius_vowifi::sms_transaction::DeliveryReportState::OutcomeUnknown
                );
            } else {
                assert!(consume.unconfirmed_reports.is_empty());
            }
            if archive_failure {
                assert!(fixture.root.join("sms-archive.json").is_dir());
            } else {
                assert_eq!(
                    proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"))
                        .load()
                        .unwrap()
                        .len(),
                    1,
                    "report outcome deleted archived SMS"
                );
            }
            assert_eq!(consume.last_error, archive_error);
            if final_status.is_some() && !wait_for_timeout && !archive_failure {
                report_committed.take().unwrap().send(()).unwrap();
            }
            peer.await.unwrap();
            supervisor.worker_finished(&exit.permit);
            network_stop.send_replace(true);
        };
        let (local, remote, ()) = tokio::time::timeout(
            std::time::Duration::from_secs(if wait_for_timeout { 25 } else { 10 }),
            async { tokio::join!(local_runner, remote_runner, application) },
        )
        .await
        .unwrap();
        assert!(local.is_ok());
        assert!(remote.is_ok());
    }

    #[tokio::test]
    async fn native_consumer_commits_before_sip_response_then_submits_ack_without_retry() {
        let fixture = StoreFixture::new();
        let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
        let permit = supervisor
            .reserve(
                crate::native_vowifi_supervisor::ReaderSessionKey::new(
                    "reader",
                    "8901234567890123456",
                )
                .unwrap(),
            )
            .unwrap();
        let generation = permit.generation();
        let (commands, mut input, _events, _output) = crate::native_ims_actor::channels();
        let (_lease, lease) = tokio::sync::watch::channel(false);
        let report_service = Service::default();
        let report_owner = std::sync::Arc::new(
            super::NativeReportOwner::new(
                report_service.clone(),
                "native-session-fixture-0001".into(),
            )
            .unwrap(),
        );
        let consumer = crate::native_sms_consumer::Consumer::new(
            fixture.root.clone(),
            format!("sub1_{}", "a".repeat(64)),
            generation,
            commands,
            permit.stop_signal(),
            lease,
            report_owner.clone(),
        );
        let event = native_event(generation);
        let mut session = crate::native_sms_consumer::SessionConsumer::new(consumer);
        let other_generation = native_generation(&mut supervisor, "other-routing-reader");
        assert!(matches!(
            session
                .process(
                    native_event(generation),
                    1_800_000_000_123,
                    &native_report_context(other_generation)
                )
                .await,
            Err("native_sms_session_mismatch")
        ));
        assert!(input.try_recv().is_err());
        assert_eq!(
            report_service
                .state
                .lock()
                .unwrap()
                .sessions
                .get("native-session-fixture-0001")
                .unwrap()
                .next_cseq,
            0,
            "wrong routing snapshot allocated a report"
        );
        let actor = async {
            let crate::native_ims_actor::Command::Respond {
                generation: received,
                status,
                reply,
                ..
            } = input.recv().await.unwrap()
            else {
                panic!("first command must be SIP response")
            };
            assert_eq!(received, generation);
            assert_eq!(status, 200);
            let archive =
                proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"));
            assert_eq!(
                archive.load().unwrap().len(),
                1,
                "SIP confirmation preceded durable commit"
            );
            reply.send(Ok(())).unwrap();
            let crate::native_ims_actor::Command::Send { request, reply, .. } =
                input.recv().await.unwrap()
            else {
                panic!("second command must be RP ACK")
            };
            assert_eq!(
                request.uri.to_string(),
                "sip:smsc@ims.example",
                "manual target overrode network asserted identity"
            );
            assert!(
                request
                    .data
                    .headers
                    .get("From")
                    .unwrap()
                    .value()
                    .contains("sip:authorized@ims.example")
            );
            assert_eq!(
                request.data.headers.get("Route").unwrap().value(),
                "<sip:registered-route@ims.example;lr>"
            );
            assert_eq!(request.data.body, proletarius_sms::build_ack(42));
            assert_eq!(
                request.data.headers.get("CSeq").unwrap().value(),
                "75 MESSAGE"
            );
            assert_ne!(
                request.data.headers.get("Call-ID").unwrap().value(),
                "report@example"
            );
            assert_eq!(
                request.data.headers.get("In-Reply-To").unwrap().value(),
                "native-delivery@example"
            );
            let _ = reply.send(Err(std::io::Error::other(
                "synthetic lost write confirmation",
            )));
        };
        let final_register = proletarius_ims::SipResponse::parse(b"SIP/2.0 200 OK\r\nP-Associated-URI: <sip:authorized@ims.example>\r\nService-Route: <sip:registered-route@ims.example;lr>\r\nContent-Length: 0\r\n\r\n").unwrap();
        let registered_routing =
            proletarius_vowifi::message_routing::RegisteredMessageRouting::from_response(
                &final_register,
                "sip:temporary@ims.example",
                &[],
            )
            .unwrap();
        let context = super::NativeSmsRouting::registered(
            generation,
            74,
            &registered_routing,
            "10.0.0.2",
            5060,
            None,
            None,
        );
        let receive = session.process(event, 1_800_000_000_123, &context);
        let (result, ()) = tokio::join!(receive, actor);
        assert!(matches!(result, Err("native_sms_report_write_unknown")));
        assert!(
            session.disconnected().is_empty(),
            "uncertain write must not fabricate a ticket"
        );
        let next = report_service
            .prepare_delivery_report_wire(
                "native-session-fixture-0001".into(),
                0,
                "sip:smsc@ims.example".into(),
                "native-delivery@example".into(),
                context.public_identity.into(),
                context.local_host.into(),
                context.local_port,
                context.route_headers.to_vec(),
                context.security_verify.map(str::to_owned),
                context.access_network_info.map(str::to_owned),
                42,
                Some(&report_owner),
            )
            .unwrap();
        assert_eq!(
            next.cseq, 76,
            "native consumer bypassed shared sequence owner"
        );
        assert!(
            input.try_recv().is_err(),
            "consumer retried uncertain network write"
        );
    }

    #[tokio::test]
    async fn native_consumer_stop_after_commit_prevents_report_submission() {
        let fixture = StoreFixture::new();
        let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
        let permit = supervisor
            .reserve(
                crate::native_vowifi_supervisor::ReaderSessionKey::new(
                    "reader",
                    "8901234567890123456",
                )
                .unwrap(),
            )
            .unwrap();
        let generation = permit.generation();
        let (commands, mut input, _events, _output) = crate::native_ims_actor::channels();
        let (_lease, lease) = tokio::sync::watch::channel(false);
        let consumer = crate::native_sms_consumer::Consumer::new(
            fixture.root.clone(),
            format!("sub1_{}", "a".repeat(64)),
            generation,
            commands,
            permit.stop_signal(),
            lease,
            crate::ims_sms_transactions::NativeReportOwner::new(
                crate::ims_sms_transactions::Service::default(),
                "native-session-fixture-0001".into(),
            )
            .unwrap(),
        );
        let actor = async {
            let crate::native_ims_actor::Command::Respond { reply, .. } =
                input.recv().await.unwrap()
            else {
                panic!("SIP response")
            };
            supervisor.request_stop_all();
            let _ = reply.send(Ok(()));
        };
        let context = native_report_context(generation);
        let (result, ()) = tokio::join!(
            consumer.receive(native_event(generation), 1_800_000_000_123, &context),
            actor
        );
        assert!(matches!(result, Err("native_sms_session_stopped")));
        assert!(input.try_recv().is_err());
        assert_eq!(
            proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"))
                .load()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn native_consumer_returns_unhandled_events_without_discarding_or_acknowledging() {
        let fixture = StoreFixture::new();
        let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
        let permit = supervisor
            .reserve(
                crate::native_vowifi_supervisor::ReaderSessionKey::new(
                    "reader",
                    "8901234567890123456",
                )
                .unwrap(),
            )
            .unwrap();
        let generation = permit.generation();
        let (commands, mut input, _events, _output) = crate::native_ims_actor::channels();
        let (_lease, lease) = tokio::sync::watch::channel(false);
        let consumer = crate::native_sms_consumer::Consumer::new(
            fixture.root.clone(),
            format!("sub1_{}", "a".repeat(64)),
            generation,
            commands,
            permit.stop_signal(),
            lease,
            crate::ims_sms_transactions::NativeReportOwner::new(
                crate::ims_sms_transactions::Service::default(),
                "native-session-fixture-0001".into(),
            )
            .unwrap(),
        );
        let event = crate::native_ims_actor::Event {
            reply_state: crate::native_ims_actor::IncomingReply::Unanswered,
            generation,
            event: proletarius_vowifi::sip_session_runtime::RegisteredSipEvent::KeepAlive {
                connection: proletarius_vowifi::registration_coordinator::SipConnectionId::Primary,
            },
        };
        let mut session = crate::native_sms_consumer::SessionConsumer::new(consumer);
        let other_generation = native_generation(&mut supervisor, "other-reader");
        assert!(matches!(
            session
                .process(
                    native_event(other_generation),
                    1_800_000_000_123,
                    &native_report_context(generation)
                )
                .await,
            Err("native_sms_session_mismatch")
        ));
        let Ok(crate::native_sms_consumer::SessionOutcome::Unhandled(returned)) = session
            .process(event, 1_800_000_000_123, &native_report_context(generation))
            .await
        else {
            panic!("non SMS event was consumed")
        };
        assert_eq!(returned.generation, generation);
        assert!(matches!(
            returned.event,
            proletarius_vowifi::sip_session_runtime::RegisteredSipEvent::KeepAlive { .. }
        ));
        assert!(!fixture.root.join("sms-archive.json").exists());
        assert!(input.try_recv().is_err());
    }

    #[tokio::test]
    async fn native_consumer_invalid_report_route_archives_but_never_acknowledges_or_allocates() {
        use proletarius_vowifi::sip_session_runtime::{RegisteredSipEvent, RegisteredSipMessage};
        let fixture = StoreFixture::new();
        let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
        let permit = supervisor
            .reserve(
                crate::native_vowifi_supervisor::ReaderSessionKey::new(
                    "reader",
                    "8901234567890123456",
                )
                .unwrap(),
            )
            .unwrap();
        let generation = permit.generation();
        let (commands, mut input, _events, _output) = crate::native_ims_actor::channels();
        let (_lease, lease) = tokio::sync::watch::channel(false);
        let service = Service::default();
        let consumer = crate::native_sms_consumer::Consumer::new(
            fixture.root.clone(),
            format!("sub1_{}", "a".repeat(64)),
            generation,
            commands,
            permit.stop_signal(),
            lease,
            super::NativeReportOwner::new(service.clone(), SESSION.into()).unwrap(),
        );
        let mut event = native_event(generation);
        let RegisteredSipEvent::Message { message, .. } = &mut event.event else {
            panic!("message");
        };
        let RegisteredSipMessage::Request { headers, .. } = message.as_mut() else {
            panic!("request");
        };
        headers.set(proletarius_sip::Header::new("P-Asserted-Identity", "tel:+123").unwrap());
        assert!(matches!(
            consumer
                .receive(event, 1_800_000_000_123, &native_report_context(generation))
                .await,
            Err("invalid_delivery_report_route")
        ));
        assert_eq!(
            proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"))
                .load()
                .unwrap()
                .len(),
            1
        );
        assert!(input.try_recv().is_err());
        assert_eq!(
            service
                .state
                .lock()
                .unwrap()
                .sessions
                .get(SESSION)
                .unwrap()
                .next_cseq,
            0
        );
    }

    #[tokio::test]
    async fn native_consumer_archive_failure_never_enqueues_success_response() {
        let fixture = StoreFixture::new();
        std::fs::create_dir(fixture.root.join("sms-archive.json")).unwrap();
        let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
        let permit = supervisor
            .reserve(
                crate::native_vowifi_supervisor::ReaderSessionKey::new(
                    "reader",
                    "8901234567890123456",
                )
                .unwrap(),
            )
            .unwrap();
        let generation = permit.generation();
        let (commands, mut input, _events, _output) = crate::native_ims_actor::channels();
        let (_lease, lease) = tokio::sync::watch::channel(false);
        let consumer = crate::native_sms_consumer::Consumer::new(
            fixture.root.clone(),
            format!("sub1_{}", "a".repeat(64)),
            generation,
            commands,
            permit.stop_signal(),
            lease,
            crate::ims_sms_transactions::NativeReportOwner::new(
                crate::ims_sms_transactions::Service::default(),
                "native-session-fixture-0001".into(),
            )
            .unwrap(),
        );
        assert!(matches!(
            consumer
                .receive(
                    native_event(generation),
                    1_800_000_000_123,
                    &native_report_context(generation)
                )
                .await,
            Err("sms_archive_unavailable")
        ));
        assert!(input.try_recv().is_err());
    }

    #[tokio::test]
    async fn native_commit_receipt_requires_real_archive_and_replayed_delivery_deduplicates() {
        let fixture = StoreFixture::new();
        let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
        let generation = native_generation(&mut supervisor, "reader");
        let event = native_event(generation);
        let subscription = format!("sub1_{}", "a".repeat(64));
        let receipt = super::archive_native_sms_event(
            fixture.root.clone(),
            &subscription,
            generation,
            &event,
            1_800_000_000_123,
        )
        .await
        .unwrap()
        .unwrap();
        let archive = proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"));
        assert_eq!(archive.load().unwrap().len(), 1);
        let context = super::DeliveryReportRequestContext {
            target: "sip:smsc@ims.example",
            in_reply_to: "native-delivery@example",
            public_identity: "sip:reader@ims.example",
            call_id: "new-report@example",
            from_tag: "from1",
            via_branch: "z9hG4bKnew",
            local_host: "10.0.0.2",
            local_port: 5060,
            cseq: 74,
            route_headers: &[],
            security_verify: None,
            access_network_info: None,
        };
        let wire = receipt.into_delivery_report(generation, &context).unwrap();
        let (proletarius_sip::Message::Request(report), _) = proletarius_sip::Parser::default()
            .parse(&wire, false)
            .unwrap()
        else {
            panic!("RP ACK fixture")
        };
        assert_eq!(report.data.body, proletarius_sms::build_ack(42));
        assert_eq!(
            report.data.headers.get("In-Reply-To").unwrap().value(),
            "native-delivery@example"
        );
        assert!(
            super::archive_native_sms_event(
                fixture.root.clone(),
                &subscription,
                generation,
                &event,
                1_800_000_000_123
            )
            .await
            .unwrap()
            .is_some()
        );
        assert_eq!(archive.load().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn native_failed_archive_and_stale_generation_never_create_ack_receipt() {
        let fixture = StoreFixture::new();
        let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
        let generation = native_generation(&mut supervisor, "reader");
        let wrong = native_generation(&mut supervisor, "other-reader");
        let event = native_event(generation);
        let subscription = format!("sub1_{}", "a".repeat(64));
        assert!(matches!(
            super::archive_native_sms_event(
                fixture.root.clone(),
                &subscription,
                wrong,
                &event,
                1_800_000_000_123
            )
            .await,
            Err("native_sms_session_mismatch")
        ));
        assert!(!fixture.root.join("sms-archive.json").exists());
        std::fs::create_dir(fixture.root.join("sms-archive.json")).unwrap();
        assert!(matches!(
            super::archive_native_sms_event(
                fixture.root.clone(),
                &subscription,
                generation,
                &event,
                1_800_000_000_123
            )
            .await,
            Err("sms_archive_unavailable")
        ));
    }

    #[tokio::test]
    async fn native_receipt_rejects_replacement_generation_or_wrong_reply_identity() {
        let fixture = StoreFixture::new();
        let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
        let generation = native_generation(&mut supervisor, "reader");
        let wrong = native_generation(&mut supervisor, "replacement");
        let event = native_event(generation);
        let subscription = format!("sub1_{}", "a".repeat(64));
        for other in [false, true] {
            let receipt = super::archive_native_sms_event(
                fixture.root.clone(),
                &subscription,
                generation,
                &event,
                1_800_000_000_123,
            )
            .await
            .unwrap()
            .unwrap();
            let context = super::DeliveryReportRequestContext {
                target: "sip:smsc@ims.example",
                in_reply_to: if other {
                    "wrong"
                } else {
                    "native-delivery@example"
                },
                public_identity: "sip:reader@ims.example",
                call_id: "report@example",
                from_tag: "from1",
                via_branch: "z9hG4bKnew",
                local_host: "10.0.0.2",
                local_port: 5060,
                cseq: 74,
                route_headers: &[],
                security_verify: None,
                access_network_info: None,
            };
            assert_eq!(
                receipt
                    .into_delivery_report(if other { generation } else { wrong }, &context)
                    .unwrap_err(),
                "native_sms_session_mismatch"
            );
        }
    }

    #[tokio::test]
    async fn rust_atomically_decodes_and_archives_reader_inbound_sms() {
        let fixture = StoreFixture::new();
        let tpdu = [
            0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
        ];
        let mut rpdu = vec![1, 42, 0, 0, tpdu.len() as u8];
        rpdu.extend_from_slice(&tpdu);
        let encoded = BASE64.encode(&rpdu);
        let receive = || {
            super::decode_and_archive_incoming_delivery(
                fixture.root.clone(),
                "reader".into(),
                format!("sub1_{}", "a".repeat(64)),
                "network-transaction@example.test".into(),
                73,
                1_800_000_000_123,
                encoded.clone(),
            )
        };

        let first = receive().await.unwrap();
        assert_eq!(first["decoded"], true);
        assert_eq!(first["store_state"], "archived");
        let archive = proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"));
        let messages = archive.load().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["sender"], "888");
        assert_eq!(messages[0]["content"], "A");
        assert_eq!(messages[0]["direction"], "in");
        assert_eq!(messages[0]["device"], "reader");
        assert_eq!(
            messages[0]["subscription_id"],
            format!("sub1_{}", "a".repeat(64))
        );
        assert_eq!(messages[0]["archived"], true);

        let retry = receive().await.unwrap();
        assert_eq!(retry["store_state"], "archived");
        assert_eq!(archive.load().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ordinary_reader_reassembles_identical_text_segments_before_acknowledgement() {
        let fixture = StoreFixture::new();
        let subscription = format!("sub1_{}", "d".repeat(64));
        let first_tpdu = vec![
            0x40, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 8, 5, 0, 3, 9, 2, 1, 0,
            0x41,
        ];
        let mut second_tpdu = first_tpdu.clone();
        second_tpdu[20] = 2;

        let encode_rpdu = |tpdu: &[u8]| {
            let mut rpdu = vec![1, 42, 0, 0, tpdu.len() as u8];
            rpdu.extend_from_slice(tpdu);
            BASE64.encode(rpdu)
        };
        let first = super::decode_and_archive_incoming_delivery(
            fixture.root.clone(),
            "reader".into(),
            subscription.clone(),
            "multipart-first@example.test".into(),
            73,
            1_800_000_000_000,
            encode_rpdu(&first_tpdu),
        )
        .await
        .expect("durably store the first segment before its SIP acknowledgement");
        assert_eq!(first["concat"]["sequence"], 1);
        assert_eq!(first["store_state"], "archived");

        let second = super::decode_and_archive_incoming_delivery(
            fixture.root.clone(),
            "reader".into(),
            subscription.clone(),
            "multipart-second@example.test".into(),
            74,
            1_800_000_000_000,
            encode_rpdu(&second_tpdu),
        )
        .await
        .expect("durably assemble the complete message before its SIP acknowledgement");
        assert_eq!(second["concat"]["sequence"], 2);

        let archive = proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"));
        let messages = archive.load().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "AA");
        assert_eq!(messages[0]["subscription_id"], subscription);
        assert_eq!(messages[0]["multipart_status"], "complete");
        assert!(messages[0].get("concat").is_none());

        let replay = super::decode_and_archive_incoming_delivery(
            fixture.root.clone(),
            "reader".into(),
            format!("sub1_{}", "d".repeat(64)),
            "multipart-second@example.test".into(),
            74,
            1_800_000_000_000,
            encode_rpdu(&second_tpdu),
        )
        .await
        .unwrap();
        assert_eq!(replay["store_state"], "archived");
        assert_eq!(archive.load().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rust_inbound_archive_rejects_bad_identity_and_does_not_archive_reports_or_undecoded_sms()
     {
        let fixture = StoreFixture::new();
        let make_rpdu = |tpdu: &[u8]| {
            let mut rpdu = vec![1, 42, 0, 0, tpdu.len() as u8];
            rpdu.extend_from_slice(tpdu);
            BASE64.encode(rpdu)
        };
        let bad_device = super::decode_and_archive_incoming_delivery(
            fixture.root.clone(),
            "unknown".into(),
            String::new(),
            "call-1".into(),
            1,
            1_800_000_000_123,
            make_rpdu(&[
                0, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 2, 0, 0x41,
            ]),
        )
        .await;
        assert_eq!(
            bad_device,
            Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid_archive_device"))
        );

        let undecoded = super::decode_and_archive_incoming_delivery(
            fixture.root.clone(),
            "reader".into(),
            String::new(),
            "call-2".into(),
            2,
            1_800_000_000_123,
            make_rpdu(&[0]),
        )
        .await
        .unwrap();
        assert_eq!(undecoded["decoded"], false);
        assert!(undecoded.get("store_state").is_none());

        let status_report = [
            2, 1, 4, 0x91, 0x44, 0x77, 0x62, 0x80, 0x62, 0x61, 0x21, 0, 0, 0x62, 0x80, 0x62, 0x61,
            0x21, 0, 0, 0,
        ];
        let report = super::decode_and_archive_incoming_delivery(
            fixture.root.clone(),
            "reader".into(),
            String::new(),
            "call-3".into(),
            3,
            1_800_000_000_123,
            make_rpdu(&status_report),
        )
        .await
        .unwrap();
        assert_eq!(report["status_report"], true);
        assert_eq!(report["store_state"], "status_report_archived");
        let archive = proletarius_store::ArchiveStore::new(fixture.root.join("sms-archive.json"));
        assert!(archive.load().unwrap().is_empty());
    }

    fn prepare(
        service: &Service,
        session: &str,
        batch: &str,
        total: usize,
        rpdu: &[u8],
    ) -> serde_json::Value {
        service
            .execute(Command::Prepare {
                session_id: session.into(),
                batch_id: batch.into(),
                total_parts: total,
                minimum_cseq: 0,
                target: "tel:+10000000000".into(),
                public_identity: "sip:user@ims.example.test".into(),
                local_host: "192.0.2.10".into(),
                local_port: 5060,
                route_headers: vec!["<sip:sms-as.example.test;lr>".into()],
                security_verify: Some("ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc".into()),
                access_network_info: Some(
                    "3GPP-E-UTRAN-FDD;utran-cell-id-3gpp=001010000000001".into(),
                ),
                rpdu_base64: BASE64.encode(rpdu),
            })
            .unwrap()
    }

    fn response(
        prepared: &serde_json::Value,
        status: u16,
        method: &str,
        cseq_delta: u32,
    ) -> Vec<u8> {
        format!(
            "SIP/2.0 {status} Fixture\r\nCall-ID: {}\r\nCSeq: {} {method}\r\nContent-Length: 0\r\n\r\n",
            prepared["call_id"].as_str().unwrap(),
            prepared["cseq"].as_u64().unwrap() + u64::from(cseq_delta)
        )
        .into_bytes()
    }

    fn submit(service: &Service, prepared: &serde_json::Value, session: &str, batch: &str) {
        service
            .execute(Command::Submit {
                session_id: session.into(),
                batch_id: batch.into(),
                call_id: prepared["call_id"].as_str().unwrap().into(),
                cseq: prepared["cseq"].as_u64().unwrap() as u32,
            })
            .unwrap();
    }

    fn observe(service: &Service, session: &str, batch: &str, wire: &[u8]) -> serde_json::Value {
        service
            .execute(Command::Observe {
                session_id: session.into(),
                batch_id: batch.into(),
                response_base64: BASE64.encode(wire),
            })
            .unwrap()
    }

    const RPDU: &[u8] = &[0x00, 0x01, 0x00, 0x02, 0x91, 0x21, 0x02, 0x01, 0x41];

    fn prepare_delivery_report(
        service: &Service,
        session: &str,
        minimum_cseq: u32,
        target: &str,
        in_reply_to: &str,
        reference: u8,
    ) -> Result<serde_json::Value, (StatusCode, &'static str)> {
        service.execute(Command::PrepareDeliveryReport {
            session_id: session.into(),
            minimum_cseq,
            target: target.into(),
            in_reply_to: in_reply_to.into(),
            public_identity: "sip:+8613800000000@ims.example.test".into(),
            local_host: "192.0.2.10".into(),
            local_port: 5060,
            route_headers: vec!["<sip:scscf.example.test;lr>".into()],
            security_verify: Some("ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc".into()),
            access_network_info: Some("IEEE-802.11;i-wlan-node-id=fixture".into()),
            reference,
        })
    }

    fn direct_report(
        service: &Service,
        floor: u32,
    ) -> Result<super::PreparedDeliveryReportWire, (StatusCode, &'static str)> {
        service.prepare_delivery_report_wire(
            SESSION.into(),
            floor,
            "sip:gw@ims.example.test".into(),
            "incoming@example.test".into(),
            "sip:reader@ims.example.test".into(),
            "192.0.2.10".into(),
            5060,
            Vec::new(),
            None,
            None,
            42,
            None,
        )
    }

    #[test]
    fn native_report_owner_releases_capacity_and_protects_active_claim() {
        let service = Service::default();
        for _ in 0..300 {
            let owner = super::NativeReportOwner::new(service.clone(), SESSION.into()).unwrap();
            assert!(matches!(
                super::NativeReportOwner::new(service.clone(), SESSION.into()),
                Err("native_transaction_session_exists")
            ));
            assert!(matches!(
                service.retire(SESSION),
                Err((StatusCode::CONFLICT, "native_transaction_session_owned"))
            ));
            drop(owner);
            assert!(service.state.lock().unwrap().sessions.is_empty());
        }
    }

    #[test]
    fn native_owner_drop_does_not_delete_replacement_or_unresolved_sms_batch() {
        let service = Service::default();
        let old = super::NativeReportOwner::new(service.clone(), SESSION.into()).unwrap();
        // Model a controller replacement while an old destructor is delayed.
        service.state.lock().unwrap().sessions.remove(SESSION);
        let replacement = super::NativeReportOwner::new(service.clone(), SESSION.into()).unwrap();
        drop(old);
        assert!(service.state.lock().unwrap().sessions.contains_key(SESSION));
        let mut supervisor = crate::native_vowifi_supervisor::NativeVoWiFiSupervisor::default();
        let generation = native_generation(&mut supervisor, "retained-originating-reader");
        replacement.bind_generation(generation).unwrap();
        let routing = native_report_context(generation);
        let prepared = replacement
            .prepare_part(BATCH, 1, "tel:+10000000000", &routing, RPDU)
            .unwrap();
        replacement.submit_part(BATCH, &prepared.identity).unwrap();
        replacement.unknown_part(BATCH).unwrap();
        drop(replacement);
        let state = service.state.lock().unwrap();
        let session = state.sessions.get(SESSION).unwrap();
        assert!(session.native_owner.is_none());
        assert!(
            session.batches.contains_key(BATCH),
            "outgoing recovery state was deleted"
        );
        drop(state);
        assert_eq!(
            service.retire(SESSION).unwrap_err().1,
            "native_transaction_session_owned"
        );
        assert_eq!(
            service.cancel(SESSION, BATCH).unwrap_err().1,
            "native_transaction_session_owned"
        );
        assert!(matches!(
            super::NativeReportOwner::new(service.clone(), SESSION.into()),
            Err("native_transaction_session_exists")
        ));
    }

    #[test]
    fn native_report_preparation_shares_legacy_sequence_without_serialization_roundtrip() {
        let service = Service::default();
        let native = direct_report(&service, 17).unwrap();
        assert_eq!(native.cseq, 18);
        let (proletarius_sip::Message::Request(request), used) = proletarius_sip::Parser::default()
            .parse(&native.wire, false)
            .unwrap()
        else {
            panic!("request");
        };
        assert_eq!(used, native.wire.len());
        assert_eq!(request.data.body, proletarius_sms::build_ack(42));
        assert_eq!(
            request.data.headers.get("Call-ID").unwrap().value(),
            native.call_id
        );
        let legacy = prepare_delivery_report(
            &service,
            SESSION,
            0,
            "sip:gw@ims.example.test",
            "incoming@example.test",
            43,
        )
        .unwrap();
        assert_eq!(legacy["cseq"], 19);
        assert_ne!(legacy["call_id"].as_str().unwrap(), native.call_id);
        assert!(matches!(
            direct_report(&service, 1 << 31),
            Err((StatusCode::SERVICE_UNAVAILABLE, "cseq_exhausted"))
        ));
        assert_eq!(
            direct_report(&service, 0).unwrap().cseq,
            20,
            "failed candidate advanced the sequence"
        );
    }

    #[test]
    fn report_sequence_exhausts_at_sip_limit_without_wrapping() {
        let service = Service::default();
        assert_eq!(
            direct_report(&service, (1 << 31) - 2).unwrap().cseq,
            (1 << 31) - 1
        );
        for _ in 0..2 {
            assert!(matches!(
                direct_report(&service, 0),
                Err((StatusCode::SERVICE_UNAVAILABLE, "cseq_exhausted"))
            ));
        }
    }

    #[test]
    fn rust_prepares_inbound_sms_delivery_report_and_shares_cseq_owner() {
        let service = Service::default();
        let prepared = prepare_delivery_report(
            &service,
            SESSION,
            17,
            "sip:ip-sm-gw@ims.example.test",
            "incoming-call@example.test",
            42,
        )
        .unwrap();
        assert_eq!(prepared["disposition"], "prepared");
        assert_eq!(prepared["cseq"], 18);
        let wire = BASE64
            .decode(prepared["wire_base64"].as_str().unwrap())
            .unwrap();
        let text = String::from_utf8_lossy(&wire);
        assert!(text.starts_with("MESSAGE sip:ip-sm-gw@ims.example.test SIP/2.0\r\n"));
        for required in [
            format!("Call-ID: {}\r\n", prepared["call_id"].as_str().unwrap()),
            "CSeq: 18 MESSAGE\r\n".to_owned(),
            "In-Reply-To: incoming-call@example.test\r\n".to_owned(),
            "P-Preferred-Identity: <sip:+8613800000000@ims.example.test>\r\n".to_owned(),
            "Route: <sip:scscf.example.test;lr>\r\n".to_owned(),
            "Security-Verify: ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc\r\n".to_owned(),
        ] {
            assert!(text.contains(&required), "missing {required:?} in {text}");
        }
        assert!(wire.ends_with(b"\r\n\r\n\x02\x2a"));
        assert!(!text.contains("Accept-Contact:"));

        let next = prepare(&service, SESSION, BATCH, 1, RPDU);
        assert_eq!(next["cseq"], 19);
    }

    #[test]
    fn invalid_delivery_report_does_not_consume_session_cseq() {
        let service = Service::default();
        assert!(
            prepare_delivery_report(
                &service,
                SESSION,
                0,
                "tel:+10000000000",
                "incoming-call@example.test",
                1,
            )
            .is_err()
        );
        let valid = prepare_delivery_report(
            &service,
            SESSION,
            0,
            "sip:gw@ims.example.test",
            "incoming-call@example.test",
            1,
        )
        .unwrap();
        assert_eq!(valid["cseq"], 1);
    }

    #[test]
    fn rust_prepares_message_and_requires_submit_ack_before_response() {
        let service = Service::default();
        let prepared = prepare(&service, SESSION, BATCH, 1, RPDU);
        let wire = BASE64
            .decode(prepared["wire_base64"].as_str().unwrap())
            .unwrap();
        let text = String::from_utf8_lossy(&wire);
        assert!(
            text.starts_with("MESSAGE tel:+10000000000 SIP/2.0\r\n"),
            "{text}"
        );
        assert!(text.contains("CSeq: 1 MESSAGE\r\n"));
        assert!(text.contains("Content-Type: application/vnd.3gpp.sms\r\n"));
        assert!(wire.ends_with(b"\r\n\r\n\0\x01\0\x02\x91!\x02\x01A"));
        assert_eq!(
            service.execute(Command::Observe {
                session_id: SESSION.into(),
                batch_id: BATCH.into(),
                response_base64: BASE64.encode(response(&prepared, 202, "MESSAGE", 0)),
            }),
            Err((hyper::StatusCode::CONFLICT, "sms_part_not_submitted"))
        );
        submit(&service, &prepared, SESSION, BATCH);
        assert_eq!(
            observe(
                &service,
                SESSION,
                BATCH,
                &response(&prepared, 202, "MESSAGE", 0)
            )["disposition"],
            "accepted"
        );
    }

    #[test]
    fn foreign_method_and_cseq_do_not_finish_transaction() {
        let service = Service::default();
        let prepared = prepare(&service, SESSION, BATCH, 1, RPDU);
        submit(&service, &prepared, SESSION, BATCH);
        let mut foreign_call = response(&prepared, 202, "MESSAGE", 0);
        let original = prepared["call_id"].as_str().unwrap().as_bytes();
        let replacement = b"other-call-id";
        let position = foreign_call
            .windows(original.len())
            .position(|window| window == original)
            .unwrap();
        foreign_call.splice(
            position..position + original.len(),
            replacement.iter().copied(),
        );
        assert_eq!(
            observe(&service, SESSION, BATCH, &foreign_call)["disposition"],
            "ignored"
        );
        assert_eq!(
            observe(
                &service,
                SESSION,
                BATCH,
                &response(&prepared, 202, "OPTIONS", 0)
            )["disposition"],
            "ignored"
        );
        assert_eq!(
            observe(
                &service,
                SESSION,
                BATCH,
                &response(&prepared, 202, "MESSAGE", 1)
            )["disposition"],
            "ignored"
        );
        assert_eq!(
            observe(
                &service,
                SESSION,
                BATCH,
                &response(&prepared, 202, "MESSAGE", 0)
            )["disposition"],
            "accepted"
        );
    }

    #[test]
    fn repeated_submit_cannot_replay_the_prepared_message() {
        let service = Service::default();
        let prepared = prepare(&service, SESSION, BATCH, 1, RPDU);
        submit(&service, &prepared, SESSION, BATCH);
        assert!(matches!(
            service.execute(Command::Submit {
                session_id: SESSION.into(),
                batch_id: BATCH.into(),
                call_id: prepared["call_id"].as_str().unwrap().into(),
                cseq: prepared["cseq"].as_u64().unwrap() as u32,
            }),
            Err((hyper::StatusCode::CONFLICT, "sms_part_not_prepared"))
        ));
    }

    #[test]
    fn cancellation_freezes_unknown_and_reuse_of_batch_is_rejected() {
        let service = Service::default();
        let prepared = prepare(&service, SESSION, BATCH, 1, RPDU);
        submit(&service, &prepared, SESSION, BATCH);
        assert_eq!(
            service
                .execute(Command::Cancel {
                    session_id: SESSION.into(),
                    batch_id: BATCH.into()
                })
                .unwrap()["disposition"],
            "unknown"
        );
        assert!(matches!(
            service.execute(Command::Prepare {
                session_id: SESSION.into(),
                batch_id: BATCH.into(),
                total_parts: 1,
                minimum_cseq: 0,
                target: "tel:+10000000000".into(),
                public_identity: "sip:user@ims.example.test".into(),
                local_host: "192.0.2.10".into(),
                local_port: 5060,
                route_headers: vec![],
                security_verify: None,
                access_network_info: None,
                rpdu_base64: BASE64.encode(RPDU),
            }),
            Err((hyper::StatusCode::CONFLICT, "sms_batch_not_ready"))
        ));
        assert_eq!(
            observe(
                &service,
                SESSION,
                BATCH,
                &response(&prepared, 202, "MESSAGE", 0)
            )["disposition"],
            "late_accepted"
        );
        assert!(matches!(
            service.execute(Command::Prepare {
                session_id: SESSION.into(),
                batch_id: BATCH.into(),
                total_parts: 1,
                minimum_cseq: 0,
                target: "tel:+10000000000".into(),
                public_identity: "sip:user@ims.example.test".into(),
                local_host: "192.0.2.10".into(),
                local_port: 5060,
                route_headers: vec![],
                security_verify: None,
                access_network_info: None,
                rpdu_base64: BASE64.encode(RPDU),
            }),
            Err((hyper::StatusCode::CONFLICT, "sms_batch_already_finished"))
        ));
    }

    #[test]
    fn retiring_session_drops_inflight_transaction_state() {
        let service = Service::default();
        let prepared = prepare(&service, SESSION, BATCH, 1, RPDU);
        submit(&service, &prepared, SESSION, BATCH);

        service
            .execute(Command::Retire {
                session_id: SESSION.into(),
            })
            .unwrap();
        assert!(matches!(
            service.execute(Command::Observe {
                session_id: SESSION.into(),
                batch_id: BATCH.into(),
                response_base64: BASE64.encode(response(&prepared, 202, "MESSAGE", 0)),
            }),
            Err((hyper::StatusCode::NOT_FOUND, "ims_session_not_found"))
        ));

        let fresh = prepare(&service, SESSION, BATCH, 1, RPDU);
        assert_eq!(fresh["part"], 1);
    }

    #[test]
    fn multipart_requires_final_acceptance_and_partial_rejection_is_known() {
        let service = Service::default();
        let first = prepare(&service, SESSION, BATCH, 2, RPDU);
        submit(&service, &first, SESSION, BATCH);
        let first_ack = observe(
            &service,
            SESSION,
            BATCH,
            &response(&first, 202, "MESSAGE", 0),
        );
        assert_eq!(first_ack["disposition"], "accepted");
        assert_eq!(first_ack["completed"], false);

        let second = prepare(&service, SESSION, BATCH, 2, RPDU);
        assert_eq!(second["part"], 2);
        submit(&service, &second, SESSION, BATCH);
        let rejection = observe(
            &service,
            SESSION,
            BATCH,
            &response(&second, 403, "MESSAGE", 0),
        );
        assert_eq!(rejection["disposition"], "rejected");
        assert_eq!(rejection["accepted_parts"], 1);
        assert_eq!(rejection["status"], 403);
    }

    #[test]
    fn invalid_frame_does_not_reserve_a_submission() {
        let service = Service::default();
        let error = service.execute(Command::Prepare {
            session_id: SESSION.into(),
            batch_id: BATCH.into(),
            total_parts: 1,
            minimum_cseq: 0,
            target: "tel:+10000000000".into(),
            public_identity: "sip:user@ims.example.test".into(),
            local_host: "192.0.2.10".into(),
            local_port: 5060,
            route_headers: vec![],
            security_verify: None,
            access_network_info: None,
            rpdu_base64: BASE64.encode([0u8; 2]),
        });
        assert_eq!(
            error,
            Err((
                hyper::StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_sms_context"
            ))
        );
        let valid = prepare(&service, SESSION, BATCH, 1, RPDU);
        assert_eq!(valid["part"], 1);
        let _ = json!(valid);
    }
    mod nwait_warning_tests {
        include!("ims_sms_transactions/nwait_warning_tests.rs");
    }
}

#[cfg(test)]
mod native_owner_isolation_tests {
    use super::*;
    #[test]
    fn legacy_adapter_cannot_mutate_or_retire_native_owned_transactions() {
        let service = Service::default();
        let owner = NativeReportOwner::new(service.clone(), "native-isolation-session-0001".into())
            .unwrap();
        for command in [
            Command::Cancel {
                session_id: owner.session_id.clone(),
                batch_id: "native-isolation-batch-0001".into(),
            },
            Command::Retire {
                session_id: owner.session_id.clone(),
            },
        ] {
            assert_eq!(
                service.execute(command).unwrap_err().1,
                "native_transaction_session_owned"
            );
        }
        for result in [
            service.submit(&owner.session_id, "native-isolation-batch-0001", "call", 1),
            service.observe(
                &owner.session_id,
                "native-isolation-batch-0001",
                String::new(),
            ),
            service.cancel(&owner.session_id, "native-isolation-batch-0001"),
            service.abort(&owner.session_id, "native-isolation-batch-0001"),
            service.retire(&owner.session_id),
        ] {
            assert_eq!(result.unwrap_err().1, "native_transaction_session_owned");
        }
        assert_eq!(
            service
                .prepare_delivery_report_wire(
                    owner.session_id.clone(),
                    0,
                    "sip:smsc@ims.example".into(),
                    "call".into(),
                    "sip:user@ims.example".into(),
                    "10.0.0.2".into(),
                    5060,
                    vec![],
                    None,
                    None,
                    42,
                    None,
                )
                .err()
                .unwrap()
                .1,
            "native_transaction_session_owned"
        );
        assert_eq!(
            service.state.lock().unwrap().sessions[&owner.session_id].next_cseq,
            0
        );
        let legacy = "legacy-isolation-session-0001".to_owned();
        assert_eq!(
            service
                .execute(Command::Retire { session_id: legacy })
                .unwrap()["disposition"],
            "retired"
        );
        owner
            .bind_generation(
                crate::native_vowifi_supervisor::SessionGeneration::from_value(1).unwrap(),
            )
            .unwrap();
        assert_eq!(
            owner.bind_generation(
                crate::native_vowifi_supervisor::SessionGeneration::from_value(2).unwrap()
            ),
            Err("native_sms_session_mismatch")
        );
    }
}
