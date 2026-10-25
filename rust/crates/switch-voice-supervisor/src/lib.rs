//! A finite authenticated control protocol and a single unreaped-child owner.
//! The standalone server is single threaded. A terminal response proves only
//! this owned child was reaped; it does not prove DSP/audio/device cleanup.
#![cfg(unix)]
#![deny(unsafe_op_in_unsafe_fn)]

mod filesystem;
mod owner;
mod protocol;
mod server;
pub use protocol::{
    Command, Exit, Response, State, encode_request, encode_response, verify_response,
};
pub use server::{ClientConfig, ServeConfig, client, serve};

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Error {
    Protocol,
    Authentication,
    UnsafePath,
    Io,
    Child,
    Active,
}
impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::Protocol => "supervisor_protocol_invalid",
            Self::Authentication => "supervisor_authentication_failed",
            Self::UnsafePath => "supervisor_namespace_unconfirmed",
            Self::Io => "supervisor_io_unconfirmed",
            Self::Child => "supervisor_child_unconfirmed",
            Self::Active => "supervisor_child_active",
        }
    }
}
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}
