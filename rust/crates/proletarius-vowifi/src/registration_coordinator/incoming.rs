//! Port-S observations, distinct from REGISTER and primary-flow health.
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IncomingReachability {
    #[default]
    Unverified,
    Observed,
}
impl IncomingReachability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Observed => "observed",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncomingWriteError {
    Timeout,
    ConnectionClosed,
    WriteFailed,
}
impl IncomingWriteError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::ConnectionClosed => "connection_closed",
            Self::WriteFailed => "write_failed",
        }
    }
    pub(super) fn from_error(error: &std::io::Error) -> Self {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::TimedOut => Self::Timeout,
            ErrorKind::BrokenPipe
            | ErrorKind::NotConnected
            | ErrorKind::UnexpectedEof
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset => Self::ConnectionClosed,
            _ => Self::WriteFailed,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IncomingDiagnostics {
    pub listener_started: bool,
    pub listener_closing: bool,
    pub accept_count: u64,
    pub active_connections: u32,
    pub closing_connections: u32,
    pub closed_connections: u64,
    pub last_sip_at: Option<SystemTime>,
    pub last_response_write_error: Option<IncomingWriteError>,
    pub reachability: IncomingReachability,
}
pub type IncomingObserver = std::sync::Arc<dyn Fn(IncomingDiagnostics) + Send + Sync>;
