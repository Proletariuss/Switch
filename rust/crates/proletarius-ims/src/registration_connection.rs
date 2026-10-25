//! Owns reliable REGISTER IO. Failed/abandoned partial writes retire the
//! stream; callers cannot reuse its remaining bytes as a new transaction.
use crate::{
    PlainSecurityDisposition, RegisterAction, RegisterPhase, RegisterTracker, SecAgreeMode,
    SipError, SipResponse,
    aka_authorization::AuthorizationHeader,
    aka_exchange::{AuthenticatedChallenge, SecurityKeys},
    registration_io::{RegisterWaitError, await_registration, await_registration_retained},
    security_offer::{ClientSecurityMechanism, OfferError, negotiate_security_server},
    security_policy::{LocalEndpoints, PolicyError, SecurityBinding, SecurityPolicy},
    security_slot::InstallationReceipt,
    stream::{SipStreamItem, SipStreamReader},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf},
    sync::{mpsc, watch},
    time::Instant,
};

use std::{fmt, net::IpAddr};

pub struct RegistrationConnection<T> {
    reader: Option<SipStreamReader<ReadHalf<T>>>,
    writer: Option<WriteHalf<T>>,
    tracker: RegisterTracker,
    retain_events: bool,
    pending_events: std::collections::VecDeque<SipStreamItem>,
}

/// Consuming progress result for the explicit registration state machine. The
/// legacy borrowed `exchange` API remains fail-closed when sec-agree is
/// required; only this move-only path can carry a challenge into an installed
/// transport continuation.
pub enum RegistrationProgress<T> {
    Registered(RegistrationConnection<T>, SipResponse),
    Challenge(RegistrationChallenge<T>),
}

pub struct RegistrationChallenge<T> {
    connection: RegistrationConnection<T>,
    response: SipResponse,
    install_ipsec: bool,
}

pub enum ChallengeResolution<T> {
    /// AUTS, or successful AKA when sec-agree is disabled/absent. The next
    /// REGISTER remains on the original reliable transport.
    Plain(PlainRegistration<T>),
    /// Successful AKA plus a Security-Server offer. The old transport and
    /// tracker can only be resumed after an installation receipt exists.
    Security(PendingSecAgree<T>),
}

pub struct PlainRegistration<T> {
    connection: RegistrationConnection<T>,
    authorization: AuthorizationHeader,
    response: SipResponse,
    security_disposition: PlainSecurityDisposition,
}

#[derive(Debug)]
pub enum PlainAdvanceError {
    Template(SipError),
    Transport(RegisterWaitError),
}

/// Consuming failure for the initial plain REGISTER. The failed reliable
/// association remains move-only and can only be handed to its transport
/// owner for an exact retirement barrier before the same four-tuple is reused.
pub struct RegistrationAdvanceFailure<T> {
    error: RegisterWaitError,
    retired: RetiredRegistrationTransport<T>,
}

/// Consuming failure for a plain authenticated REGISTER. Template failures
/// are just as terminal as partial-write/read failures: the old association
/// is never returned as a reusable `RegistrationConnection`.
pub struct PlainAdvanceFailure<T> {
    error: PlainAdvanceError,
    retired: RetiredRegistrationTransport<T>,
}

pub struct PendingSecAgree<T> {
    connection: RegistrationConnection<T>,
    authorization: AuthorizationHeader,
    keys: SecurityKeys,
    response: SipResponse,
}

pub struct SecAgreeParameters<'a> {
    pub local_ip: IpAddr,
    pub remote_ip: IpAddr,
    pub local_endpoints: LocalEndpoints,
    pub client_mechanisms: &'a [ClientSecurityMechanism<'a>],
    pub strict: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SecAgreePrepareError {
    MissingSecurityServer,
    AmbiguousSecurityServer,
    Offer(OfferError),
    Policy(PolicyError),
    Authorization(crate::aka_digest::DigestError),
    Sip(SipError),
}

pub struct PreparedSecAgree<T> {
    retired: RetiredRegistrationTransport<T>,
    ready: ReadySecAgree,
}

pub struct ReadySecAgree {
    policy: SecurityPolicy,
    continuation: SecAgreeContinuation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectedRegisterContext {
    local_port_c: u16,
    local_port_s: u16,
    remote_ip: IpAddr,
    remote_port_c: u16,
    remote_port_s: u16,
    security_verify: String,
}

pub struct ProtectedRegisterAttempt<'a> {
    pub authorization: &'a AuthorizationHeader,
    pub context: &'a ProtectedRegisterContext,
}

/// Move-only REGISTER tracker captured after a valid AKA challenge. It does
/// not expose a constructor and cannot be resumed without an installation
/// receipt produced by the packet owner.
pub(crate) struct SecAgreeContinuation {
    tracker: RegisterTracker,
    authorization: AuthorizationHeader,
    context: ProtectedRegisterContext,
    binding: SecurityBinding,
}

/// Opaque proof that the packet owner atomically installed the exact policy
/// carried by one `ReadySecAgree`. The receipt and continuation cannot be
/// separated or paired with another AKA transaction.
pub struct InstalledSecAgree {
    continuation: SecAgreeContinuation,
    receipt: InstallationReceipt,
}

/// REGISTER transport bound to a packet-owner installation receipt. This
/// wrapper only accepts the authenticated protected request and a matching
/// protected 2xx result.
pub struct InstalledRegistrationConnection<T> {
    connection: RegistrationConnection<T>,
    _receipt: InstallationReceipt,
    authorization: AuthorizationHeader,
    context: ProtectedRegisterContext,
}

pub struct InstalledExchangeFailure<T> {
    error: RegisterWaitError,
    connection: Box<InstalledRegistrationConnection<T>>,
}

/// A reliable transport whose SIP transaction is no longer reusable. The
/// underlying stream is never returned directly; it can only be consumed by a
/// caller-supplied retirement operation such as an exact close barrier.
pub struct RetiredRegistrationTransport<T> {
    stream: T,
}

/// Exact retirement primitive implemented by the transport owner. It has no
/// accessor for the stream, so a failed SIP transaction cannot be repackaged
/// as a fresh connection.
pub trait RegistrationTransportRetire {
    fn retire(self) -> impl std::future::Future<Output = Result<(), std::io::Error>>;
}

impl<T> fmt::Debug for InstalledExchangeFailure<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstalledExchangeFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl<T> fmt::Debug for RegistrationAdvanceFailure<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegistrationAdvanceFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl<T> fmt::Debug for PlainAdvanceFailure<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlainAdvanceFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum RegistrationError<E> {
    Transport(RegisterWaitError),
    Template(SipError),
    Authentication(E),
}

struct Exchange<'a, T> {
    connection: &'a mut RegistrationConnection<T>,
    complete: bool,
}
impl<T> Drop for Exchange<'_, T> {
    fn drop(&mut self) {
        if !self.complete {
            self.connection.tracker.phase = RegisterPhase::Failed;
            self.connection.reader = None;
            self.connection.writer = None;
        }
    }
}

impl<T> RegistrationChallenge<T> {
    pub fn response(&self) -> &SipResponse {
        &self.response
    }

    pub fn requires_security_install(&self) -> bool {
        self.install_ipsec
    }

    pub fn next_cseq(&self) -> Result<u32, SipError> {
        self.connection
            .tracker
            .cseq
            .checked_add(1)
            .ok_or(SipError::InvalidCSeq)
    }

    /// A synchronization failure carries no CK/IK and therefore stays on the
    /// original plain connection. Successful AKA enters the security path
    /// only when the challenge actually negotiated sec-agree.
    pub fn resolve(self, answer: AuthenticatedChallenge) -> ChallengeResolution<T> {
        let (header, keys) = answer.into_parts();
        match (self.install_ipsec, keys) {
            (true, Some(keys)) => ChallengeResolution::Security(PendingSecAgree {
                connection: self.connection,
                authorization: header,
                keys,
                response: self.response,
            }),
            (_, keys) => {
                // In an ordinary/plain registration CK/IK are neither needed
                // nor retained. Dropping them here zeroizes both key arrays.
                drop(keys);
                ChallengeResolution::Plain(PlainRegistration {
                    connection: self.connection,
                    authorization: header,
                    response: self.response,
                    security_disposition: if self.install_ipsec {
                        PlainSecurityDisposition::RetainOffer
                    } else {
                        PlainSecurityDisposition::OmitOffer
                    },
                })
            }
        }
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> RegistrationChallenge<T> {
    pub fn into_retired_transport(self) -> RetiredRegistrationTransport<T> {
        self.connection.into_retired_transport_infallible()
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> PlainRegistration<T> {
    pub fn response(&self) -> &SipResponse {
        &self.response
    }

    /// Returns the exact sec-agree decision made from the preceding challenge.
    /// Request builders must not reconstruct it from their own template.
    pub const fn security_disposition(&self) -> PlainSecurityDisposition {
        self.security_disposition
    }

    /// Build and send the next authenticated REGISTER on the same transport.
    /// The AUTS/header and connection never become separately ownable.
    pub async fn advance_with<B>(
        self,
        deadline: Instant,
        stop: watch::Receiver<bool>,
        events: &mpsc::Sender<SipStreamItem>,
        build: B,
    ) -> Result<RegistrationProgress<T>, PlainAdvanceFailure<T>>
    where
        B: FnOnce(
            u32,
            &AuthorizationHeader,
            PlainSecurityDisposition,
        ) -> Result<zeroize::Zeroizing<Vec<u8>>, SipError>,
    {
        let Self {
            connection,
            authorization,
            response: _,
            security_disposition,
        } = self;
        let cseq = match connection.next_cseq() {
            Ok(cseq) => cseq,
            Err(error) => {
                return Err(PlainAdvanceFailure::new(
                    PlainAdvanceError::Template(error),
                    connection.into_retired_transport_infallible(),
                ));
            }
        };
        let request = match build(cseq, &authorization, security_disposition) {
            Ok(request) => request,
            Err(error) => {
                return Err(PlainAdvanceFailure::new(
                    PlainAdvanceError::Template(error),
                    connection.into_retired_transport_infallible(),
                ));
            }
        };
        match connection.advance(&request, deadline, stop, events).await {
            Ok(progress) => Ok(progress),
            Err(failure) => {
                let (error, retired) = failure.into_error_and_retired();
                Err(PlainAdvanceFailure::new(
                    PlainAdvanceError::Transport(error),
                    retired,
                ))
            }
        }
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> PendingSecAgree<T> {
    /// Traffic preceding the security upgrade is not authenticated dialog input.
    /// Reject requests on this exact plaintext writer before retiring it.
    pub async fn reject_retained_requests(
        mut self,
        deadline: Instant,
        mut stop: watch::Receiver<bool>,
    ) -> Result<Self, PlainAdvanceFailure<T>> {
        let result = async {
            if *stop.borrow() || stop.has_changed().is_err() { return Err(RegisterWaitError::Cancelled); }
            while let Some(item) = self.connection.pending_events.pop_front() {
                let SipStreamItem::Message(frame) = item else { continue; };
                let (message, consumed) = proletarius_sip::Parser::default().parse(&frame, false).map_err(|_| RegisterWaitError::Sip(SipError::InvalidHeader))?;
                if consumed != frame.len() { return Err(RegisterWaitError::Sip(SipError::InvalidContentLength)); }
                let proletarius_sip::Message::Request(request) = message else { continue; };
                if request.method == proletarius_sip::Method::ACK { continue; }
                let mut headers = proletarius_sip::Headers::default();
                for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
                    let values = request.data.headers.all(name).collect::<Vec<_>>();
                    if values.is_empty() || name != "Via" && values.len() != 1 { return Err(RegisterWaitError::Sip(SipError::InvalidHeader)); }
                    for value in values { headers.push(value.clone()); }
                }
                let cancel = request.method == proletarius_sip::Method::CANCEL;
                let response = proletarius_sip::Message::Response(proletarius_sip::Response {
                    status: if cancel { 481 } else { 503 }, reason: if cancel { "Call/Transaction Does Not Exist" } else { "Service Unavailable" }.into(), data: proletarius_sip::MessageData::new("SIP/2.0", headers, Vec::new()),
                }).encode();
                let writer = self.connection.writer.as_mut().ok_or(RegisterWaitError::Disconnected)?;
                tokio::select! {
                    biased;
                    _ = stop.changed() => return Err(RegisterWaitError::Cancelled),
                    result = tokio::time::timeout_at(deadline, async { writer.write_all(&response).await?; writer.flush().await }) => {
                        result.map_err(|_| RegisterWaitError::Deadline)?.map_err(RegisterWaitError::Io)?;
                    }
                }
            }
            Ok(())
        }.await;
        match result {
            Ok(()) => Ok(self),
            Err(error) => Err(PlainAdvanceFailure::new(
                PlainAdvanceError::Transport(error),
                self.connection.into_retired_transport_infallible(),
            )),
        }
    }

    pub fn response(&self) -> &SipResponse {
        &self.response
    }

    pub fn next_cseq(&self) -> Result<u32, SipError> {
        self.connection.next_cseq()
    }

    /// Resolve the exact server offer and bind the resulting policy, tracker,
    /// credential and old transport into one move-only transition.
    pub fn prepare(
        self,
        parameters: SecAgreeParameters<'_>,
    ) -> Result<PreparedSecAgree<T>, SecAgreePrepareError> {
        self.prepare_with_retirement(parameters)
            .map_err(|(error, retired)| {
                drop(retired);
                error
            })
    }

    pub fn prepare_with_retirement(
        mut self,
        parameters: SecAgreeParameters<'_>,
    ) -> Result<PreparedSecAgree<T>, (SecAgreePrepareError, RetiredRegistrationTransport<T>)> {
        let prepared = (|| {
            if self.connection.tracker.phase != RegisterPhase::ChallengeReceived {
                return Err(SecAgreePrepareError::Sip(SipError::InvalidTransition));
            }
            let authorization = self
                .authorization
                .with_integrity_protected()
                .map_err(SecAgreePrepareError::Authorization)?;
            let raw = self
                .connection
                .tracker
                .security_server
                .as_deref()
                .ok_or(SecAgreePrepareError::MissingSecurityServer)?;
            let negotiated =
                negotiate_security_server(raw, parameters.client_mechanisms, parameters.strict)
                    .map_err(SecAgreePrepareError::Offer)?;
            let context = ProtectedRegisterContext {
                local_port_c: parameters.local_endpoints.port_c,
                local_port_s: parameters.local_endpoints.port_s,
                remote_ip: parameters.remote_ip,
                remote_port_c: negotiated.selected.port_c,
                remote_port_s: negotiated.selected.port_s,
                security_verify: negotiated.security_verify().to_owned(),
            };
            let policy = SecurityPolicy::negotiated(
                parameters.local_ip,
                parameters.remote_ip,
                parameters.local_endpoints,
                &negotiated.selected,
                self.keys,
            )
            .map_err(SecAgreePrepareError::Policy)?;
            Ok((authorization, context, policy))
        })();
        let (authorization, context, policy) = match prepared {
            Ok(value) => value,
            Err(error) => return Err((error, self.connection.into_retired_transport_infallible())),
        };
        let binding = policy.binding();
        // The move-only challenge owns both halves. All fallible policy work
        // completed before extracting them, so failure retains an exact close
        // capability rather than silently dropping the transport.
        let reader = self
            .connection
            .reader
            .take()
            .expect("challenge retains reader");
        let writer = self
            .connection
            .writer
            .take()
            .expect("challenge retains writer");
        let stream = reader.into_inner().unsplit(writer);
        let tracker = self.connection.tracker;
        Ok(PreparedSecAgree {
            retired: RetiredRegistrationTransport { stream },
            ready: ReadySecAgree {
                policy,
                continuation: SecAgreeContinuation {
                    tracker,
                    authorization,
                    context,
                    binding,
                },
            },
        })
    }
}

impl<T: RegistrationTransportRetire> PreparedSecAgree<T> {
    /// The old plain association must reach its exact close barrier before the
    /// policy or continuation can be obtained for installation.
    pub async fn retire_plain(self) -> Result<ReadySecAgree, std::io::Error> {
        self.retired.retire().await?;
        Ok(self.ready)
    }
}

impl ReadySecAgree {
    pub fn context(&self) -> &ProtectedRegisterContext {
        &self.continuation.context
    }

    pub(crate) fn into_install_parts(self) -> (SecurityPolicy, SecAgreeContinuation) {
        (self.policy, self.continuation)
    }
}

impl ProtectedRegisterContext {
    pub fn local_port_c(&self) -> u16 {
        self.local_port_c
    }

    pub fn local_port_s(&self) -> u16 {
        self.local_port_s
    }

    pub fn remote_ip(&self) -> IpAddr {
        self.remote_ip
    }

    pub fn remote_port_c(&self) -> u16 {
        self.remote_port_c
    }

    pub fn remote_port_s(&self) -> u16 {
        self.remote_port_s
    }

    pub fn security_verify(&self) -> &str {
        &self.security_verify
    }
}

impl SecAgreeContinuation {
    pub(crate) fn context(&self) -> &ProtectedRegisterContext {
        &self.context
    }

    pub(crate) fn binding(&self) -> SecurityBinding {
        self.binding
    }

    pub(crate) fn next_cseq(&self) -> Result<u32, SipError> {
        self.tracker
            .cseq
            .checked_add(1)
            .ok_or(SipError::InvalidCSeq)
    }

    fn bind_installed<T: AsyncRead + AsyncWrite + Unpin>(
        self,
        stream: T,
        receipt: InstallationReceipt,
    ) -> InstalledRegistrationConnection<T> {
        debug_assert_eq!(self.tracker.phase, RegisterPhase::ChallengeReceived);
        debug_assert_eq!(receipt.binding(), self.binding);
        let (reader, writer) = tokio::io::split(stream);
        InstalledRegistrationConnection {
            connection: RegistrationConnection {
                reader: Some(SipStreamReader::new(reader)),
                writer: Some(writer),
                tracker: self.tracker,
                retain_events: false,
                pending_events: Default::default(),
            },
            _receipt: receipt,
            authorization: self.authorization,
            context: self.context,
        }
    }
}

impl InstalledSecAgree {
    pub(crate) fn new(continuation: SecAgreeContinuation, receipt: InstallationReceipt) -> Self {
        Self {
            continuation,
            receipt,
        }
    }

    pub fn context(&self) -> &ProtectedRegisterContext {
        self.continuation.context()
    }

    pub fn next_cseq(&self) -> Result<u32, SipError> {
        self.continuation.next_cseq()
    }

    pub fn generation(&self) -> crate::security_slot::SecurityGeneration {
        self.receipt.generation()
    }

    pub fn bind<T: AsyncRead + AsyncWrite + Unpin>(
        self,
        stream: T,
    ) -> InstalledRegistrationConnection<T> {
        self.continuation.bind_installed(stream, self.receipt)
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> InstalledRegistrationConnection<T> {
    pub fn with_retained_events(mut self) -> Self {
        self.connection.retain_events = true;
        self
    }
    pub fn take_retained_events(&mut self) -> std::collections::VecDeque<SipStreamItem> {
        self.connection.take_retained_events()
    }

    pub fn phase(&self) -> RegisterPhase {
        self.connection.phase()
    }

    pub fn next_cseq(&self) -> Result<u32, SipError> {
        self.connection.next_cseq()
    }

    pub fn request_context(&self) -> ProtectedRegisterAttempt<'_> {
        ProtectedRegisterAttempt {
            authorization: &self.authorization,
            context: &self.context,
        }
    }

    /// Send the authenticated request on the receipt-bound stream. A further
    /// challenge or a result that is not marked IPsec-protected fails closed;
    /// re-keying remains a whole-session rebuild in stage 2.07.
    pub async fn exchange_authenticated(
        mut self,
        request: &[u8],
        deadline: Instant,
        stop: watch::Receiver<bool>,
        events: &mpsc::Sender<SipStreamItem>,
    ) -> Result<(Self, SipResponse), InstalledExchangeFailure<T>> {
        if let Err(error) = validate_protected_request(request, self.request_context()) {
            return Err(InstalledExchangeFailure {
                error: RegisterWaitError::Sip(error),
                connection: Box::new(self),
            });
        }
        let (action, response) = match self
            .connection
            .exchange_inner(request, deadline, stop, events)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                return Err(InstalledExchangeFailure {
                    error,
                    connection: Box::new(self),
                });
            }
        };
        if action != (RegisterAction::Registered { ipsec: true }) {
            return Err(InstalledExchangeFailure {
                error: RegisterWaitError::SecurityTransportUnavailable,
                connection: Box::new(self),
            });
        }
        Ok((self, response))
    }

    /// Recombine the protected halves solely for an explicit transport close
    /// barrier. Any buffered SIP data is zeroized and not reinterpreted.
    pub fn into_retired_transport(self) -> Result<RetiredRegistrationTransport<T>, SipError> {
        Ok(RetiredRegistrationTransport {
            stream: self.connection.into_stream()?,
        })
    }

    /// Preserve any request already coalesced after the final REGISTER 200 so
    /// the runtime can dispatch immediate MT calls or SMS on this connection.
    pub fn into_registered_parts(
        self,
    ) -> Result<(SipStreamReader<ReadHalf<T>>, WriteHalf<T>), SipError> {
        self.connection.into_registered_parts()
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> InstalledExchangeFailure<T> {
    pub fn error(&self) -> &RegisterWaitError {
        &self.error
    }

    pub fn into_error_and_retired(
        self,
    ) -> Result<(RegisterWaitError, RetiredRegistrationTransport<T>), SipError> {
        Ok((self.error, (*self.connection).into_retired_transport()?))
    }
}

impl<T> RegistrationAdvanceFailure<T> {
    fn new(error: RegisterWaitError, retired: RetiredRegistrationTransport<T>) -> Self {
        Self { error, retired }
    }

    pub fn error(&self) -> &RegisterWaitError {
        &self.error
    }

    pub fn into_error_and_retired(self) -> (RegisterWaitError, RetiredRegistrationTransport<T>) {
        (self.error, self.retired)
    }
}

impl<T> PlainAdvanceFailure<T> {
    fn new(error: PlainAdvanceError, retired: RetiredRegistrationTransport<T>) -> Self {
        Self { error, retired }
    }

    pub fn error(&self) -> &PlainAdvanceError {
        &self.error
    }

    pub fn into_error_and_retired(self) -> (PlainAdvanceError, RetiredRegistrationTransport<T>) {
        (self.error, self.retired)
    }
}

impl<T: RegistrationTransportRetire> RetiredRegistrationTransport<T> {
    pub async fn retire(self) -> Result<(), std::io::Error> {
        self.stream.retire().await
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> RegistrationConnection<T> {
    pub fn new(
        stream: T,
        call_id: impl Into<String>,
        cseq: u32,
        mode: SecAgreeMode,
    ) -> Result<Self, SipError> {
        let mut tracker = RegisterTracker::begin(call_id, cseq, mode)?;
        tracker.phase = RegisterPhase::Idle;
        let (reader, writer) = tokio::io::split(stream);
        Ok(Self {
            reader: Some(SipStreamReader::new(reader)),
            writer: Some(writer),
            tracker,
            retain_events: false,
            pending_events: Default::default(),
        })
    }

    pub fn with_retained_events(mut self) -> Self {
        self.retain_events = true;
        self
    }
    pub fn take_retained_events(&mut self) -> std::collections::VecDeque<SipStreamItem> {
        std::mem::take(&mut self.pending_events)
    }

    pub fn phase(&self) -> RegisterPhase {
        self.tracker.phase()
    }

    pub fn next_cseq(&self) -> Result<u32, SipError> {
        self.tracker
            .cseq
            .checked_add(1)
            .ok_or(SipError::InvalidCSeq)
    }

    /// Consuming counterpart to `exchange`. It preserves the challenge and
    /// connection as a move-only state instead of treating a Security-Server
    /// offer as proof that a security transport already exists.
    pub async fn advance(
        self,
        request: &[u8],
        deadline: Instant,
        stop: watch::Receiver<bool>,
        events: &mpsc::Sender<SipStreamItem>,
    ) -> Result<RegistrationProgress<T>, RegistrationAdvanceFailure<T>> {
        self.advance_initial_with_fallback(request, deadline, stop, events, |_, _| Ok(None))
            .await
    }

    /// Only a fully parsed, transaction-matched rejection of an initial
    /// REGISTER may retain this association for another initial variant.
    /// Authentication/protected errors and interrupted/framing failures still
    /// leave through the existing move-only retired transport path.
    pub async fn advance_initial_with_fallback<B>(
        mut self,
        request: &[u8],
        deadline: Instant,
        stop: watch::Receiver<bool>,
        events: &mpsc::Sender<SipStreamItem>,
        mut next: B,
    ) -> Result<RegistrationProgress<T>, RegistrationAdvanceFailure<T>>
    where
        B: FnMut(u16, u32) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, SipError>,
    {
        let mut request = zeroize::Zeroizing::new(request.to_vec());
        let (action, response) = loop {
            let initial = self.tracker.phase == RegisterPhase::Idle;
            match self
                .exchange_inner(&request, deadline, stop.clone(), events)
                .await
            {
                Ok(result) => break result,
                Err(error) => {
                    if initial
                        && let RegisterWaitError::Sip(SipError::RegistrationRejected(status)) =
                            &error
                    {
                        let retry = match self.tracker.cseq.checked_add(1) {
                            Some(cseq) => {
                                next(*status, cseq).map(|bytes| bytes.map(|bytes| (cseq, bytes)))
                            }
                            None => Err(SipError::InvalidCSeq),
                        };
                        match retry {
                            Ok(Some((cseq, bytes))) => {
                                // Reset only this explicit initial rejection.
                                // Old Security-Server/challenge state must not
                                // leak into the next transaction's policy.
                                self.tracker = match RegisterTracker::begin(
                                    self.tracker.call_id.clone(),
                                    cseq,
                                    self.tracker.sec_agree,
                                ) {
                                    Ok(tracker) => tracker,
                                    Err(error) => {
                                        return Err(RegistrationAdvanceFailure::new(
                                            RegisterWaitError::Sip(error),
                                            self.into_retired_transport_infallible(),
                                        ));
                                    }
                                };
                                self.tracker.phase = RegisterPhase::Idle;
                                request = bytes;
                                continue;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                return Err(RegistrationAdvanceFailure::new(
                                    RegisterWaitError::Sip(error),
                                    self.into_retired_transport_infallible(),
                                ));
                            }
                        }
                    }
                    return Err(RegistrationAdvanceFailure::new(
                        error,
                        self.into_retired_transport_infallible(),
                    ));
                }
            }
        };
        match action {
            RegisterAction::AnswerAka { install_ipsec } => {
                Ok(RegistrationProgress::Challenge(RegistrationChallenge {
                    connection: self,
                    response,
                    install_ipsec,
                }))
            }
            RegisterAction::Registered { ipsec: false } => {
                Ok(RegistrationProgress::Registered(self, response))
            }
            RegisterAction::Registered { ipsec: true } | RegisterAction::Wait => {
                let retired = self.into_retired_transport_infallible();
                Err(RegistrationAdvanceFailure::new(
                    RegisterWaitError::SecurityTransportUnavailable,
                    retired,
                ))
            }
        }
    }

    /// Own the connection across the whole registration, including SIM waits.
    /// Any failure/cancellation drops both halves; no half-registered stream
    /// escapes. Template policy owns fresh Via branches and carrier headers.
    pub async fn register_with<B, A, F, E>(
        mut self,
        deadline: Instant,
        mut stop: watch::Receiver<bool>,
        events: &mpsc::Sender<SipStreamItem>,
        mut build: B,
        mut authenticate: A,
    ) -> Result<Self, RegistrationError<E>>
    where
        B: FnMut(
            u32,
            Option<&crate::aka_authorization::AuthorizationHeader>,
        ) -> Result<zeroize::Zeroizing<Vec<u8>>, SipError>,
        A: FnMut(SipResponse) -> F,
        F: std::future::Future<Output = Result<crate::aka_exchange::AuthenticatedChallenge, E>>,
    {
        let mut cseq = self.tracker.cseq;
        let mut authorization = None;
        loop {
            if *stop.borrow() || stop.has_changed().is_err() {
                return Err(RegistrationError::Transport(RegisterWaitError::Cancelled));
            }
            if Instant::now() >= deadline {
                return Err(RegistrationError::Transport(RegisterWaitError::Deadline));
            }
            let request =
                build(cseq, authorization.as_ref()).map_err(RegistrationError::Template)?;
            let (action, response) = self
                .exchange(&request, deadline, stop.clone(), events)
                .await
                .map_err(RegistrationError::Transport)?;
            match action {
                RegisterAction::Registered { .. } => return Ok(self),
                RegisterAction::AnswerAka { .. } => {
                    let future = authenticate(response);
                    tokio::pin!(future);
                    let answer = loop {
                        if *stop.borrow() || stop.has_changed().is_err() {
                            return Err(RegistrationError::Transport(RegisterWaitError::Cancelled));
                        }
                        tokio::select! {
                            biased;
                            _=stop.changed()=>continue,
                            _=tokio::time::sleep_until(deadline)=>return Err(RegistrationError::Transport(RegisterWaitError::Deadline)),
                            result=&mut future=>break result.map_err(RegistrationError::Authentication)?,
                        }
                    };
                    // exchange() rejects uninstalled IMS IPsec. No keys are
                    // retained in this ordinary reliable-transport path.
                    authorization = Some(answer.into_parts().0);
                    cseq = cseq
                        .checked_add(1)
                        .ok_or(RegistrationError::Template(SipError::InvalidCSeq))?;
                }
                RegisterAction::Wait => {
                    return Err(RegistrationError::Template(SipError::InvalidTransition));
                }
            }
        }
    }

    /// Templates/authentication are supplied by the session owner. One
    /// absolute deadline includes write, flush, provisional and final reply.
    pub async fn exchange(
        &mut self,
        request: &[u8],
        deadline: Instant,
        stop: watch::Receiver<bool>,
        events: &mpsc::Sender<SipStreamItem>,
    ) -> Result<(RegisterAction, SipResponse), RegisterWaitError> {
        let mut pending = Exchange {
            connection: self,
            complete: false,
        };
        let result = pending
            .connection
            .exchange_inner(request, deadline, stop, events)
            .await;
        let result = result.and_then(|result| {
            if matches!(
                result.0,
                RegisterAction::AnswerAka {
                    install_ipsec: true
                } | RegisterAction::Registered { ipsec: true }
            ) {
                Err(RegisterWaitError::SecurityTransportUnavailable)
            } else {
                Ok(result)
            }
        });
        if result.is_ok() {
            pending.complete = true;
        }
        result
    }

    async fn exchange_inner(
        &mut self,
        request: &[u8],
        deadline: Instant,
        mut stop: watch::Receiver<bool>,
        events: &mpsc::Sender<SipStreamItem>,
    ) -> Result<(RegisterAction, SipResponse), RegisterWaitError> {
        let (call_id, cseq) = request_transaction(request).map_err(RegisterWaitError::Sip)?;
        let initial = self.tracker.phase == RegisterPhase::Idle;
        if call_id != self.tracker.call_id
            || !(initial && cseq == self.tracker.cseq
                || self.tracker.phase == RegisterPhase::ChallengeReceived
                    && cseq > self.tracker.cseq)
        {
            return Err(RegisterWaitError::Sip(SipError::InvalidTransition));
        }
        if *stop.borrow() || stop.has_changed().is_err() {
            return Err(RegisterWaitError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(RegisterWaitError::Deadline);
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or(RegisterWaitError::Disconnected)?;
        let cancelled = async {
            loop {
                if *stop.borrow() {
                    return;
                }
                if stop.changed().await.is_err() {
                    return;
                }
            }
        };
        tokio::select! {
            biased;
            _=cancelled=>return Err(RegisterWaitError::Cancelled),
            result=tokio::time::timeout_at(deadline,async { writer.write_all(request).await?;writer.flush().await })=>{
                result.map_err(|_|RegisterWaitError::Deadline)?.map_err(RegisterWaitError::Io)?;
            }
        }
        if initial {
            self.tracker.phase = RegisterPhase::AwaitingInitial;
        } else {
            self.tracker
                .authenticated_request_sent(cseq)
                .map_err(RegisterWaitError::Sip)?;
        }
        let reader = self
            .reader
            .as_mut()
            .ok_or(RegisterWaitError::Disconnected)?;
        let result = if self.retain_events {
            await_registration_retained(
                reader,
                &mut self.tracker,
                deadline,
                stop,
                &mut self.pending_events,
            )
            .await?
        } else {
            await_registration(reader, &mut self.tracker, deadline, stop, events).await?
        };
        Ok(result)
    }

    fn into_stream(mut self) -> Result<T, SipError> {
        let reader = self.reader.take().ok_or(SipError::InvalidTransition)?;
        let writer = self.writer.take().ok_or(SipError::InvalidTransition)?;
        Ok(reader.into_inner().unsplit(writer))
    }

    /// The consuming plain REGISTER paths enter with both halves exclusively
    /// owned, and none of their transitions can remove either half before an
    /// error is returned. Keep this assertion private so callers receive only
    /// the non-reusable retirement capability.
    fn into_retired_transport_infallible(mut self) -> RetiredRegistrationTransport<T> {
        let reader = self
            .reader
            .take()
            .expect("owned REGISTER failure retains its reader half");
        let writer = self
            .writer
            .take()
            .expect("owned REGISTER failure retains its writer half");
        RetiredRegistrationTransport {
            stream: reader.into_inner().unsplit(writer),
        }
    }

    /// Preserve buffered post-REGISTER traffic and the same connection.
    /// The caller must continue reading it for incoming calls/SMS/liveness.
    pub fn into_registered_parts(
        mut self,
    ) -> Result<(SipStreamReader<ReadHalf<T>>, WriteHalf<T>), SipError> {
        if self.tracker.phase != RegisterPhase::Registered {
            return Err(SipError::InvalidTransition);
        }
        Ok((self.reader.take().unwrap(), self.writer.take().unwrap()))
    }
}

pub fn request_transaction(request: &[u8]) -> Result<(&str, u32), SipError> {
    if request.len() > crate::MAX_SIP_MESSAGE_LENGTH {
        return Err(SipError::MessageTooLarge);
    }
    let boundary = request
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or(SipError::MissingHeaderBoundary)?;
    if crate::stream::framed_length(&request[..boundary])? != request.len() {
        return Err(SipError::InvalidContentLength);
    }
    let head = std::str::from_utf8(&request[..boundary]).map_err(|_| SipError::InvalidHeader)?;
    let mut lines = head.split("\r\n");
    let mut start = lines.next().ok_or(SipError::InvalidHeader)?.split(' ');
    if start.next() != Some("REGISTER")
        || !start
            .next()
            .is_some_and(|uri| uri.starts_with("sip:") || uri.starts_with("sips:"))
        || start.next() != Some("SIP/2.0")
        || start.next().is_some()
    {
        return Err(SipError::InvalidHeader);
    }
    let (mut call_id, mut cseq) = (None, None);
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(SipError::InvalidHeader)?;
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case("Call-ID") || name.eq_ignore_ascii_case("i") {
            if call_id.is_some() || value.is_empty() {
                return Err(SipError::DuplicateTransactionHeader("Call-ID"));
            }
            call_id = Some(value);
        }
        if name.eq_ignore_ascii_case("CSeq") {
            if cseq.is_some() {
                return Err(SipError::DuplicateTransactionHeader("CSeq"));
            }
            let mut parts = value.split_whitespace();
            let number = parts.next().ok_or(SipError::InvalidCSeq)?;
            if !number.bytes().all(|b| b.is_ascii_digit())
                || parts.next() != Some("REGISTER")
                || parts.next().is_some()
            {
                return Err(SipError::InvalidCSeq);
            }
            let number = number.parse::<u32>().map_err(|_| SipError::InvalidCSeq)?;
            if number == 0 {
                return Err(SipError::InvalidCSeq);
            }
            cseq = Some(number);
        }
    }
    Ok((
        call_id.ok_or(SipError::MissingTransactionHeader("Call-ID"))?,
        cseq.ok_or(SipError::MissingTransactionHeader("CSeq"))?,
    ))
}

fn validate_protected_request(
    request: &[u8],
    expected: ProtectedRegisterAttempt<'_>,
) -> Result<(), SipError> {
    request_transaction(request)?;
    let boundary = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or(SipError::MissingHeaderBoundary)?;
    let head = std::str::from_utf8(&request[..boundary]).map_err(|_| SipError::InvalidHeader)?;
    let mut authorization = None;
    let mut opposite_authorization = false;
    let mut security_verify = None;
    for line in head.split("\r\n").skip(1) {
        let (name, value) = line.split_once(':').ok_or(SipError::InvalidHeader)?;
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case(expected.authorization.name()) {
            if authorization.replace(value).is_some() {
                return Err(SipError::DuplicateTransactionHeader(
                    expected.authorization.name(),
                ));
            }
        } else if matches!(
            expected.authorization.name(),
            "Authorization" | "Proxy-Authorization"
        ) && (name.eq_ignore_ascii_case("Authorization")
            || name.eq_ignore_ascii_case("Proxy-Authorization"))
        {
            opposite_authorization = true;
        }
        if name.eq_ignore_ascii_case("Security-Verify") && security_verify.replace(value).is_some()
        {
            return Err(SipError::DuplicateTransactionHeader("Security-Verify"));
        }
    }
    if opposite_authorization
        || authorization != Some(expected.authorization.value())
        || security_verify != Some(expected.context.security_verify())
    {
        return Err(SipError::InvalidHeader);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use zeroize::Zeroizing;

    impl RegistrationTransportRetire for tokio::io::DuplexStream {
        async fn retire(self) -> Result<(), std::io::Error> {
            let mut stream = self;
            stream.shutdown().await
        }
    }

    mod initial_fallback;

    fn request(cseq: u32) -> Vec<u8> {
        request_with_authorization(cseq, None)
    }
    fn request_with_authorization(
        cseq: u32,
        authorization: Option<&crate::aka_authorization::AuthorizationHeader>,
    ) -> Vec<u8> {
        let sequence = format!("{cseq} REGISTER");
        crate::register_request::RegisterRequest {
            uri: "sip:ims.example",
            headers: &[
                ("Via", "SIP/2.0/TCP 10.0.0.2:5060;branch=z9hG4bK-test"),
                ("From", "<sip:user@ims.example>;tag=test"),
                ("To", "<sip:user@ims.example>"),
                ("Call-ID", "own"),
                ("CSeq", &sequence),
                ("Max-Forwards", "70"),
                ("Contact", "<sip:user@10.0.0.2:5060;transport=tcp>"),
            ],
            body: b"",
        }
        .encode(authorization)
        .unwrap()
        .to_vec()
    }

    fn protected_request(cseq: u32, attempt: ProtectedRegisterAttempt<'_>) -> Vec<u8> {
        let sequence = format!("{cseq} REGISTER");
        let via = format!(
            "SIP/2.0/TCP 10.0.0.2:{};branch=z9hG4bK-protected",
            attempt.context.local_port_c()
        );
        let contact = format!(
            "<sip:user@10.0.0.2:{};transport=tcp>",
            attempt.context.local_port_s()
        );
        let route = format!(
            "<sip:{}:{};transport=tcp;lr>",
            attempt.context.remote_ip(),
            attempt.context.remote_port_s()
        );
        crate::register_request::RegisterRequest {
            uri: "sip:ims.example",
            headers: &[
                ("Via", &via),
                ("From", "<sip:user@ims.example>;tag=test"),
                ("To", "<sip:user@ims.example>"),
                ("Call-ID", "own"),
                ("CSeq", &sequence),
                ("Max-Forwards", "70"),
                ("Contact", &contact),
                ("Route", &route),
                ("Security-Verify", attempt.context.security_verify()),
            ],
            body: b"",
        }
        .encode(Some(attempt.authorization))
        .unwrap()
        .to_vec()
    }
    #[tokio::test]
    async fn automatic_registration_runs_aka_and_keeps_incoming_requests() {
        use crate::{
            aka_authorization::AuthorizationRequest,
            aka_exchange::{SimAnswer, answer_challenge},
        };
        let (client, server) = tokio::io::duplex(4096);
        let connection =
            RegistrationConnection::new(client, "own", 1, SecAgreeMode::Disabled).unwrap();
        let (_stop_tx, stop) = watch::channel(false);
        let (events, _rx) = mpsc::channel(4);
        let deadline = Instant::now() + Duration::from_secs(1);
        let remote = async {
            let (read, mut write) = tokio::io::split(server);
            let mut read = SipStreamReader::new(read);
            for seq in [1, 2] {
                let Some(SipStreamItem::Message(wire)) = read.next().await.unwrap() else {
                    panic!("missing REGISTER");
                };
                assert_eq!(request_transaction(&wire).unwrap(), ("own", seq));
                if seq == 2 {
                    assert!(
                        String::from_utf8_lossy(&wire)
                            .contains("Authorization: Digest username=\"user\"")
                    );
                }
                let status = if seq == 1 {
                    format!(
                        "401 Challenge\r\nWWW-Authenticate: Digest realm=\"ims.example\",nonce=\"{}\",algorithm=AKAv1-MD5",
                        "01".repeat(32)
                    )
                } else {
                    "200 OK".into()
                };
                let mut response = format!(
                    "SIP/2.0 {status}\r\nCall-ID: own\r\nCSeq: {seq} REGISTER\r\nContent-Length: 0\r\n\r\n"
                );
                if seq == 2 {
                    response.push_str("INVITE sip:user@ims SIP/2.0\r\nContent-Length: 0\r\n\r\n");
                }
                write.write_all(response.as_bytes()).await.unwrap();
            }
        };
        let local = async {
            let auth_stop = stop.clone();
            let connection = connection
                .register_with(
                    deadline,
                    stop,
                    &events,
                    |seq, auth| Ok(Zeroizing::new(request_with_authorization(seq, auth))),
                    |response| {
                        let stop = auth_stop.clone();
                        async move {
                            let request = AuthorizationRequest {
                                username: "user",
                                method: "REGISTER",
                                uri: "sip:ims.example",
                                body: b"",
                                cnonce: "fixed",
                                nonce_count: 1,
                            };
                            answer_challenge(
                                &response,
                                &request,
                                deadline,
                                stop,
                                |rand, autn| async move {
                                    assert_eq!(*rand, [1; 16]);
                                    assert_eq!(*autn, [1; 16]);
                                    Ok::<_, ()>(SimAnswer::Success {
                                        res: Zeroizing::new(vec![1; 8]),
                                        ck: Zeroizing::new([2; 16]),
                                        ik: Zeroizing::new([3; 16]),
                                    })
                                },
                            )
                            .await
                        }
                    },
                )
                .await
                .unwrap();
            assert_eq!(connection.phase(), RegisterPhase::Registered);
            let (mut read, _write) = connection.into_registered_parts().unwrap();
            let Some(SipStreamItem::Message(wire)) = read.next().await.unwrap() else {
                panic!("lost INVITE");
            };
            assert!(wire.starts_with(b"INVITE "));
        };
        tokio::join!(local, remote);
    }

    #[tokio::test]
    async fn sec_agree_continuation_crosses_connection_only_with_receipt() {
        use crate::{security_policy::LocalEndpoints, security_slot::SecuritySlot};

        let (plain_client, mut plain_server) = tokio::io::duplex(4096);
        let connection =
            RegistrationConnection::new(plain_client, "own", 1, SecAgreeMode::Auto).unwrap();
        let (_stop_tx, stop) = watch::channel(false);
        let (events, _rx) = mpsc::channel(4);
        let deadline = Instant::now() + Duration::from_secs(1);
        let server_offer = "ipsec-3gpp;q=0.98;alg=hmac-sha-1-96;ealg=aes-cbc;prot=esp;mod=trans;spi-c=111;spi-s=222;port-c=6064;port-s=6063";

        let remote = async {
            let expected = request(1);
            let mut wire = vec![0; expected.len()];
            plain_server.read_exact(&mut wire).await.unwrap();
            assert_eq!(request_transaction(&wire).unwrap(), ("own", 1));
            plain_server
                .write_all(
                    format!(
                        "SIP/2.0 401 Challenge\r\nCall-ID: own\r\nCSeq: 1 REGISTER\r\nWWW-Authenticate: Digest realm=\"ims\",nonce=\"x\",algorithm=AKAv1-MD5\r\nSecurity-Server: {server_offer}\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        };
        let initial_request = request(1);
        let local = connection.advance(&initial_request, deadline, stop.clone(), &events);
        let (progress, ()) = tokio::join!(local, remote);
        let RegistrationProgress::Challenge(challenge) = progress.unwrap() else {
            panic!("expected sec-agree challenge")
        };
        assert!(challenge.requires_security_install());
        assert_eq!(challenge.next_cseq().unwrap(), 2);

        let answer = AuthenticatedChallenge::from_parts(
            AuthorizationHeader::from_parts(
                "Authorization",
                "Digest username=\"user\", realm=\"ims\", nonce=\"x\", uri=\"sip:ims.example\", algorithm=AKAv1-MD5, response=\"r\"",
            ),
            Some(SecurityKeys {
                ck: Zeroizing::new([2; 16]),
                ik: Zeroizing::new([3; 16]),
            }),
        );
        let ChallengeResolution::Security(pending) = challenge.resolve(answer) else {
            panic!("successful AKA must require installed sec-agree")
        };
        assert_eq!(pending.next_cseq().unwrap(), 2);
        let mechanisms = [crate::security_offer::ClientSecurityMechanism {
            algorithm: "hmac-sha-1-96",
            encryption: "aes-cbc",
            protocol: Some("esp"),
            mode: Some("trans"),
        }];
        let prepared = pending
            .prepare(SecAgreeParameters {
                local_ip: "10.0.0.2".parse().unwrap(),
                remote_ip: "10.0.0.9".parse().unwrap(),
                local_endpoints: LocalEndpoints {
                    spi_c: 333,
                    spi_s: 444,
                    port_c: 5064,
                    port_s: 5063,
                },
                client_mechanisms: &mechanisms,
                strict: true,
            })
            .unwrap();
        let ready = prepared.retire_plain().await.unwrap();
        let mut slot = SecuritySlot::new();
        let installed = slot.install_registration(ready).unwrap();
        let generation = installed.generation();
        let (protected_client, mut protected_server) = tokio::io::duplex(4096);
        let installed = installed.bind(protected_client);
        let protected_request = protected_request(2, installed.request_context());

        let remote = async {
            let mut wire = vec![0; protected_request.len()];
            protected_server.read_exact(&mut wire).await.unwrap();
            assert_eq!(request_transaction(&wire).unwrap(), ("own", 2));
            assert!(String::from_utf8_lossy(&wire).contains("integrity-protected=yes"));
            protected_server
                .write_all(b"SIP/2.0 200 OK\r\nCall-ID: own\r\nCSeq: 2 REGISTER\r\nContent-Length: 0\r\n\r\nINVITE sip:user@ims SIP/2.0\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        };
        let local = installed.exchange_authenticated(&protected_request, deadline, stop, &events);
        let (registered, ()) = tokio::join!(local, remote);
        let (registered, final_response) = registered.unwrap();
        assert_eq!(final_response.status, 200);
        assert_eq!(registered.phase(), RegisterPhase::Registered);
        let (mut reader, writer) = registered.into_registered_parts().unwrap();
        let Some(SipStreamItem::Message(invite)) = reader.next().await.unwrap() else {
            panic!("lost protected post-REGISTER request")
        };
        assert!(invite.starts_with(b"INVITE "));
        drop(reader);
        drop(writer);
        slot.clear_if(generation).unwrap();
    }

    #[tokio::test]
    async fn auto_without_security_server_marks_authenticated_retry_as_plain() {
        let (client, mut server) = tokio::io::duplex(4096);
        let connection = RegistrationConnection::new(client, "own", 1, SecAgreeMode::Auto).unwrap();
        let (_stop_tx, stop) = watch::channel(false);
        let (events, _rx) = mpsc::channel(4);
        let deadline = Instant::now() + Duration::from_secs(1);

        let remote = async {
            let expected = request(1);
            let mut wire = vec![0; expected.len()];
            server.read_exact(&mut wire).await.unwrap();
            server
                .write_all(
                    b"SIP/2.0 401 Challenge\r\nCall-ID: own\r\nCSeq: 1 REGISTER\r\nWWW-Authenticate: Digest realm=\"ims\",nonce=\"x\",algorithm=AKAv1-MD5\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .unwrap();
        };
        let local = async {
            let RegistrationProgress::Challenge(challenge) = connection
                .advance(&request(1), deadline, stop, &events)
                .await
                .unwrap()
            else {
                panic!("expected plain challenge")
            };
            assert!(!challenge.requires_security_install());
            let answer = AuthenticatedChallenge::from_parts(
                AuthorizationHeader::from_parts(
                    "Authorization",
                    "Digest username=\"user\",response=\"ok\"",
                ),
                Some(SecurityKeys {
                    ck: Zeroizing::new([2; 16]),
                    ik: Zeroizing::new([3; 16]),
                }),
            );
            let ChallengeResolution::Plain(plain) = challenge.resolve(answer) else {
                panic!("missing Security-Server must not enter sec-agree")
            };
            assert_eq!(
                plain.security_disposition(),
                PlainSecurityDisposition::OmitOffer
            );
        };
        tokio::join!(local, remote);
    }

    #[tokio::test]
    async fn receipt_cannot_resume_a_different_identical_registration() {
        use crate::{security_policy::LocalEndpoints, security_slot::SecuritySlot};

        let offer = crate::security_offer::parse_security_server(
            "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;prot=esp;mod=trans;spi-c=111;spi-s=222;port-c=6064;port-s=6063",
        )
        .unwrap()
        .remove(0);
        let policy = || {
            SecurityPolicy::aes_sha1(
                "10.0.0.2".parse().unwrap(),
                "10.0.0.9".parse().unwrap(),
                LocalEndpoints {
                    spi_c: 333,
                    spi_s: 444,
                    port_c: 5064,
                    port_s: 5063,
                },
                &offer,
                SecurityKeys {
                    ck: Zeroizing::new([2; 16]),
                    ik: Zeroizing::new([3; 16]),
                },
            )
            .unwrap()
        };
        let installed_policy = policy();
        let different_policy = policy();
        let mut tracker = RegisterTracker::begin("own", 1, SecAgreeMode::Required).unwrap();
        tracker.phase = RegisterPhase::ChallengeReceived;
        let continuation = SecAgreeContinuation {
            tracker,
            authorization: AuthorizationHeader::from_parts(
                "Authorization",
                "Digest username=\"user\", realm=\"ims\", nonce=\"x\", uri=\"sip:ims.example\", algorithm=AKAv1-MD5, response=\"r\"",
            ),
            context: ProtectedRegisterContext {
                local_port_c: 5064,
                local_port_s: 5063,
                remote_ip: "10.0.0.9".parse().unwrap(),
                remote_port_c: 6064,
                remote_port_s: 6063,
                security_verify: "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc".to_owned(),
            },
            binding: different_policy.binding(),
        };
        let ready = ReadySecAgree {
            policy: installed_policy,
            continuation,
        };
        let mut slot = SecuritySlot::new();
        assert!(matches!(
            slot.install_registration(ready),
            Err(crate::security_slot::SecuritySlotError::BindingInvariant)
        ));
        assert_eq!(slot.installed_generation(), None);
    }

    #[tokio::test]
    async fn auts_stays_on_original_connection_before_security_install() {
        let (client, mut server) = tokio::io::duplex(4096);
        let connection = RegistrationConnection::new(client, "own", 1, SecAgreeMode::Auto).unwrap();
        let (_stop_tx, stop) = watch::channel(false);
        let (events, _rx) = mpsc::channel(4);
        let deadline = Instant::now() + Duration::from_secs(1);
        let offer =
            "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;spi-c=111;spi-s=222;port-c=6064;port-s=6063";

        let remote = async {
            for cseq in [1, 2] {
                let expected = if cseq == 1 {
                    request(1)
                } else {
                    request_with_authorization(
                        2,
                        Some(&AuthorizationHeader::from_parts(
                            "Authorization",
                            "Digest username=\"user\", auts=\"WlpaWlpaWlpaWlpaWlo=\"",
                        )),
                    )
                };
                let mut wire = vec![0; expected.len()];
                server.read_exact(&mut wire).await.unwrap();
                assert_eq!(request_transaction(&wire).unwrap(), ("own", cseq));
                if cseq == 2 {
                    assert!(String::from_utf8_lossy(&wire).contains("auts="));
                    assert!(!String::from_utf8_lossy(&wire).contains("Security-Verify"));
                }
                server
                    .write_all(
                        format!(
                            "SIP/2.0 401 Challenge\r\nCall-ID: own\r\nCSeq: {cseq} REGISTER\r\nWWW-Authenticate: Digest realm=\"ims\",nonce=\"x{cseq}\",algorithm=AKAv1-MD5\r\n{}Content-Length: 0\r\n\r\n",
                            if cseq == 1 {
                                format!("Security-Server: {offer}\r\n")
                            } else {
                                String::new()
                            }
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        };
        let local = async {
            let RegistrationProgress::Challenge(first) = connection
                .advance(&request(1), deadline, stop.clone(), &events)
                .await
                .unwrap()
            else {
                panic!("expected first challenge")
            };
            let answer = AuthenticatedChallenge::from_parts(
                AuthorizationHeader::from_parts(
                    "Authorization",
                    "Digest username=\"user\", auts=\"WlpaWlpaWlpaWlpaWlo=\"",
                ),
                None,
            );
            let ChallengeResolution::Plain(plain) = first.resolve(answer) else {
                panic!("AUTS must remain plain")
            };
            let RegistrationProgress::Challenge(second) = plain
                .advance_with(deadline, stop, &events, |cseq, authorization, security| {
                    assert_eq!(security, PlainSecurityDisposition::RetainOffer);
                    Ok(Zeroizing::new(request_with_authorization(
                        cseq,
                        Some(authorization),
                    )))
                })
                .await
                .unwrap()
            else {
                panic!("expected fresh challenge after AUTS")
            };
            assert!(second.requires_security_install());
            assert_eq!(second.next_cseq().unwrap(), 3);
            let answer = AuthenticatedChallenge::from_parts(
                AuthorizationHeader::from_parts(
                    "Authorization",
                    "Digest username=\"user\", response=\"ok\"",
                ),
                Some(SecurityKeys {
                    ck: Zeroizing::new([2; 16]),
                    ik: Zeroizing::new([3; 16]),
                }),
            );
            let ChallengeResolution::Security(pending) = second.resolve(answer) else {
                panic!("successful AKA must enter security transition")
            };
            let mechanisms = [crate::security_offer::ClientSecurityMechanism {
                algorithm: "hmac-sha-1-96",
                encryption: "aes-cbc",
                protocol: Some("esp"),
                mode: Some("trans"),
            }];
            let prepared = pending
                .prepare(SecAgreeParameters {
                    local_ip: "10.0.0.2".parse().unwrap(),
                    remote_ip: "10.0.0.9".parse().unwrap(),
                    local_endpoints: crate::security_policy::LocalEndpoints {
                        spi_c: 333,
                        spi_s: 444,
                        port_c: 5064,
                        port_s: 5063,
                    },
                    client_mechanisms: &mechanisms,
                    strict: true,
                })
                .unwrap();
            assert_eq!(prepared.ready.context().security_verify(), offer);
            drop(prepared.retire_plain().await.unwrap());
        };
        tokio::join!(local, remote);
    }
    #[tokio::test]
    async fn sends_before_waiting_and_preserves_post_registration_message() {
        let (client, mut server) = tokio::io::duplex(1024);
        let mut connection =
            RegistrationConnection::new(client, "own", 1, SecAgreeMode::Disabled).unwrap();
        assert_eq!(connection.phase(), RegisterPhase::Idle);
        let (_stop, stop) = watch::channel(false);
        let (events, _rx) = mpsc::channel(4);
        let remote = async {
            for cseq in [1, 2] {
                let expected = request(cseq);
                let mut bytes = vec![0; expected.len()];
                server.read_exact(&mut bytes).await.unwrap();
                assert_eq!(bytes, expected);
                let status = if cseq == 1 {
                    "401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"ims\",nonce=\"x\",algorithm=AKAv1-MD5"
                } else {
                    "200 OK"
                };
                let mut reply=format!("SIP/2.0 {status}\r\nCall-ID: own\r\nCSeq: {cseq} REGISTER\r\nContent-Length: 0\r\n\r\n").into_bytes();
                if cseq == 2 {
                    reply.extend(b"INVITE sip:user@ims SIP/2.0\r\nContent-Length: 0\r\n\r\n");
                }
                server.write_all(&reply).await.unwrap();
            }
        };
        let local = async {
            for cseq in [1, 2] {
                connection
                    .exchange(
                        &request(cseq),
                        Instant::now() + Duration::from_secs(1),
                        stop.clone(),
                        &events,
                    )
                    .await
                    .unwrap();
            }
            assert_eq!(connection.phase(), RegisterPhase::Registered);
            let (mut reader, _writer) = connection.into_registered_parts().unwrap();
            let Some(SipStreamItem::Message(frame)) = reader.next().await.unwrap() else {
                panic!("lost buffered request")
            };
            assert!(frame.starts_with(b"INVITE "));
        };
        tokio::join!(local, remote);
    }

    #[tokio::test]
    async fn security_offer_is_not_installation_evidence() {
        for status in [401, 200] {
            let (client, mut server) = tokio::io::duplex(1024);
            let mut connection =
                RegistrationConnection::new(client, "own", 1, SecAgreeMode::Auto).unwrap();
            let (_stop, stop) = watch::channel(false);
            let (events, _rx) = mpsc::channel(1);
            server.write_all(format!("SIP/2.0 {status} Test\r\nCall-ID: own\r\nCSeq: 1 REGISTER\r\nWWW-Authenticate: Digest realm=\"ims\",nonce=\"x\",algorithm=AKAv1-MD5\r\nSecurity-Server: ipsec-3gpp;alg=hmac-sha-1-96\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
            assert!(matches!(
                connection
                    .exchange(
                        &request(1),
                        Instant::now() + Duration::from_secs(1),
                        stop,
                        &events
                    )
                    .await,
                Err(RegisterWaitError::SecurityTransportUnavailable)
            ));
            assert_eq!(connection.phase(), RegisterPhase::Failed);
            assert!(connection.into_registered_parts().is_err());
        }
    }

    #[tokio::test]
    async fn unsupported_selected_suite_keeps_exact_retirement_and_never_sends_plain_auth() {
        use crate::aka_authorization::AuthorizationRequest;
        use crate::aka_exchange::{SimAnswer, answer_challenge};
        let (client, server) = tokio::io::duplex(4096);
        let connection =
            RegistrationConnection::new(client, "own", 1, SecAgreeMode::Required).unwrap();
        let (_stop, stop) = watch::channel(false);
        let (events, _rx) = mpsc::channel(4);
        let deadline = Instant::now() + Duration::from_secs(1);
        let remote = async {
            let (read, mut write) = tokio::io::split(server);
            let mut reader = SipStreamReader::new(read);
            assert!(matches!(
                reader.next().await.unwrap(),
                Some(SipStreamItem::Message(_))
            ));
            write.write_all(format!("SIP/2.0 401 Challenge\r\nCall-ID: own\r\nCSeq: 1 REGISTER\r\nWWW-Authenticate: Digest realm=\"ims.example\",nonce=\"{}\",algorithm=AKAv1-MD5\r\nSecurity-Server: ipsec-3gpp;alg=hmac-sha-256-128;ealg=aes-cbc;spi-c=111;spi-s=222;port-c=6050;port-s=6060\r\nContent-Length: 0\r\n\r\n","01".repeat(32)).as_bytes()).await.unwrap();
            assert!(
                reader.next().await.unwrap().is_none(),
                "unsupported selection submitted plaintext auth or a retry"
            );
        };
        let local = async {
            let RegistrationProgress::Challenge(challenge) = connection
                .advance(&request(1), deadline, stop.clone(), &events)
                .await
                .unwrap()
            else {
                panic!("expected AKA challenge")
            };
            let answer = answer_challenge(
                challenge.response(),
                &AuthorizationRequest {
                    username: "user",
                    method: "REGISTER",
                    uri: "sip:ims.example",
                    body: b"",
                    cnonce: "fixed",
                    nonce_count: 1,
                },
                deadline,
                stop,
                |rand, autn| async move {
                    assert_eq!(*rand, [1; 16]);
                    assert_eq!(*autn, [1; 16]);
                    Ok::<_, std::convert::Infallible>(SimAnswer::Success {
                        res: Zeroizing::new(vec![1; 8]),
                        ck: Zeroizing::new([2; 16]),
                        ik: Zeroizing::new([3; 16]),
                    })
                },
            )
            .await
            .unwrap();
            let ChallengeResolution::Security(pending) = challenge.resolve(answer) else {
                panic!("missing actual security transition")
            };
            let mechanisms = [ClientSecurityMechanism {
                algorithm: "hmac-sha-256-128",
                encryption: "aes-cbc",
                protocol: Some("esp"),
                mode: Some("trans"),
            }];
            let result = pending.prepare_with_retirement(SecAgreeParameters {
                local_ip: "10.0.0.2".parse().unwrap(),
                remote_ip: "10.0.0.9".parse().unwrap(),
                local_endpoints: LocalEndpoints {
                    spi_c: 333,
                    spi_s: 444,
                    port_c: 5050,
                    port_s: 5060,
                },
                client_mechanisms: &mechanisms,
                strict: true,
            });
            let Err((error, retired)) = result else {
                panic!("unsupported transform falsely prepared")
            };
            assert!(matches!(
                error,
                SecAgreePrepareError::Policy(
                    crate::security_policy::PolicyError::UnsupportedTransform
                )
            ));
            retired.retire().await.unwrap();
        };
        tokio::join!(local, remote);
    }

    #[tokio::test]
    async fn partial_write_timeout_retires_stream() {
        let (client, mut server) = tokio::io::duplex(8);
        let mut connection =
            RegistrationConnection::new(client, "own", 1, SecAgreeMode::Disabled).unwrap();
        let (_stop, stop) = watch::channel(false);
        let (events, _rx) = mpsc::channel(1);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                connection.exchange(
                    &request(1),
                    Instant::now() + Duration::from_secs(10),
                    stop,
                    &events
                )
            )
            .await
            .is_err()
        );
        assert_eq!(connection.phase(), RegisterPhase::Failed);
        let mut written = Vec::new();
        server.read_to_end(&mut written).await.unwrap();
        assert_eq!(written.len(), 8);
        assert!(connection.into_registered_parts().is_err());
    }
}
