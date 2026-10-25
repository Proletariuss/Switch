//! Retained, session-local OPTIONS transaction and scheduling state.
use super::*;
use crate::registration_coordinator::RegisteredCallContext;
use proletarius_ims::options_heartbeat::{self, Decision};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Read-only health of one registered owner, shared by its command admission.
#[derive(Clone)]
pub struct HeartbeatHealth(Arc<AtomicBool>);
impl HeartbeatHealth {
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
    pub fn is_healthy(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub(crate) fn set(&self, healthy: bool) {
        self.0.store(healthy, Ordering::Release);
    }
}
pub type HeartbeatObserver = Arc<dyn Fn(bool) + Send + Sync>;

pub(crate) struct Configuration {
    interval: Duration,
    target: SocketAddr,
    local_ip: std::net::IpAddr,
    port_c: u16,
    identity: Zeroizing<String>,
    user_agent: String,
    security_verify: Option<Zeroizing<String>>,
}
impl Configuration {
    pub(crate) fn validate_interval(seconds: u64) -> io::Result<Duration> {
        if !(1..=86_400).contains(&seconds) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid IMS OPTIONS heartbeat interval",
            ));
        }
        Ok(Duration::from_secs(seconds))
    }
    pub(crate) fn freeze(context: &RegisteredCallContext, interval: Duration) -> Self {
        Self {
            interval,
            target: context.peer(),
            local_ip: context.local_ip(),
            port_c: context.port_c(),
            identity: Zeroizing::new(context.public_identity().to_owned()),
            user_agent: context.user_agent().to_owned(),
            security_verify: context
                .security_verify()
                .map(|v| Zeroizing::new(v.to_owned())),
        }
    }
    fn request(&self, cseq: u32) -> io::Result<Request> {
        if cseq == 0 || cseq >= (1 << 31) {
            return Err(failed("IMS OPTIONS CSeq exhausted"));
        }
        let mut random = [0; 32];
        getrandom::fill(&mut random).map_err(|_| failed("IMS OPTIONS entropy unavailable"))?;
        let hex = |value: &[u8]| value.iter().map(|n| format!("{n:02x}")).collect::<String>();
        let branch = format!("z9hG4bK{}", hex(&random[..8]));
        let tag = hex(&random[8..16]);
        let call_id = hex(&random[16..]);
        let target = self.target.to_string();
        let via = match self.local_ip {
            std::net::IpAddr::V4(ip) => format!("{ip}:{}", self.port_c),
            std::net::IpAddr::V6(ip) => format!("[{ip}]:{}", self.port_c),
        };
        let mut headers = Headers::default();
        for (name, value) in [
            ("Via", format!("SIP/2.0/TCP {via};branch={branch}")),
            ("From", format!("<{}>;tag={tag}", self.identity.as_str())),
            ("To", format!("<sip:{target}>")),
            ("Call-ID", call_id),
            ("CSeq", format!("{cseq} OPTIONS")),
            ("Max-Forwards", "70".into()),
            ("Route", format!("<sip:{target};lr>")),
            (
                "P-Preferred-Identity",
                format!("<{}>", self.identity.as_str()),
            ),
            ("User-Agent", self.user_agent.clone()),
        ] {
            headers
                .push(Header::new(name, value).map_err(|_| failed("invalid IMS OPTIONS header"))?);
        }
        if let Some(verify) = &self.security_verify {
            headers.push(
                Header::new("Security-Verify", verify.as_str())
                    .map_err(|_| failed("invalid IMS OPTIONS security header"))?,
            );
        }
        Ok(Request {
            method: Method::OPTIONS,
            uri: Uri::parse(&format!("sip:{target}"))
                .map_err(|_| failed("invalid IMS OPTIONS target"))?,
            data: MessageData::new("SIP/2.0", headers, vec![]),
        })
    }
}

pub(super) enum Phase {
    Ready(Instant),
    Writing {
        ticket: ResponseTicket,
        confirmation: bool,
    },
    Pending {
        ticket: ResponseTicket,
        confirmation: bool,
        deadline: Instant,
    },
    ConfirmAt(Instant),
    Failed,
}
pub(super) struct Heartbeat {
    config: Configuration,
    pub(super) phase: Phase,
    next_primary: Instant,
    cseq: u32,
    pub(super) response_timeout: Duration,
    pub(super) confirm_delay: Duration,
    notices: VecDeque<bool>,
}
impl Heartbeat {
    pub(super) fn new(config: Configuration) -> Self {
        let first = Instant::now() + config.interval;
        Self {
            config,
            phase: Phase::Ready(first),
            next_primary: first,
            cseq: 1,
            response_timeout: options_heartbeat::RESPONSE_TIMEOUT,
            confirm_delay: options_heartbeat::CONFIRM_DELAY,
            notices: VecDeque::new(),
        }
    }
    pub(super) fn deadline(&self) -> Option<Instant> {
        match &self.phase {
            Phase::Ready(at) | Phase::ConfirmAt(at) => Some(*at),
            Phase::Pending { deadline, .. } => Some(*deadline),
            Phase::Writing { .. } => Some(Instant::now()),
            Phase::Failed => None,
        }
    }
    pub(super) fn notice(&mut self) -> Option<bool> {
        self.notices.pop_front()
    }
    pub(super) fn owns(&self, ticket: ResponseTicket) -> bool {
        match self.phase {
            Phase::Writing { ticket: own, .. } | Phase::Pending { ticket: own, .. } => {
                ticket == own
            }
            _ => false,
        }
    }
    pub(super) fn complete(&mut self, status: u16, now: Instant) -> Option<bool> {
        let confirmation = match self.phase {
            Phase::Pending { confirmation, .. } | Phase::Writing { confirmation, .. } => {
                confirmation
            }
            _ => return None,
        };
        match options_heartbeat::decide(confirmation, status) {
            Decision::Continue => {
                self.cseq += 1;
                self.phase = Phase::Ready(self.next_primary);
                None
            }
            Decision::ConfirmAfterDelay => {
                self.phase = Phase::ConfirmAt(now + self.confirm_delay);
                self.push(false);
                Some(false)
            }
            Decision::Recovered => {
                self.cseq += 1;
                self.phase = Phase::Ready(self.next_primary);
                self.push(true);
                Some(true)
            }
            Decision::Teardown => {
                self.phase = Phase::Failed;
                None
            }
        }
    }
    fn push(&mut self, healthy: bool) {
        // A stalled event consumer must not retain unbounded history. Fail the
        // owner rather than silently dropping a liveness transition.
        if self.notices.len() == 32 {
            self.phase = Phase::Failed;
        } else {
            self.notices.push_back(healthy);
        }
    }
    pub(super) fn start(&mut self, now: Instant) -> io::Result<Option<(Request, bool)>> {
        let confirmation = match self.phase {
            Phase::Ready(at) if at <= now => false,
            Phase::ConfirmAt(at) if at <= now => true,
            _ => return Ok(None),
        };
        if !confirmation {
            let next = self.next_primary + self.config.interval;
            self.next_primary = if next <= now {
                now + self.config.interval
            } else {
                next
            };
        }
        self.config
            .request(self.cseq)
            .map(|request| Some((request, confirmation)))
    }
    pub(super) fn write_pending(&self) -> bool {
        matches!(self.phase, Phase::Writing { .. })
    }
    pub(super) fn written(&mut self, now: Instant) {
        if let Phase::Writing {
            ticket,
            confirmation,
            ..
        } = self.phase
        {
            self.phase = Phase::Pending {
                ticket,
                confirmation,
                deadline: now + self.response_timeout,
            };
        }
    }
    pub(super) fn timeout(&self) -> Duration {
        self.response_timeout
    }
}

/// Stable typed failure for actor lifecycle classification; contains no URI,
/// subscriber identity, SIP contents, keys or raw transport diagnostics.
#[derive(Debug)]
pub struct HeartbeatFailure(&'static str);
impl std::fmt::Display for HeartbeatFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for HeartbeatFailure {}
pub(super) fn failed(code: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, HeartbeatFailure(code))
}
