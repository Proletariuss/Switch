//! IKE_SA_INIT NAT detection, RFC 7296 section 2.23.
//! The socket owner supplies the addresses; this codec never resolves a name.
use std::net::{IpAddr, SocketAddr};

use sha1::{Digest, Sha1};

use crate::PayloadFrame;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NatDetectionKind {
    Source,
    Destination,
}

impl NatDetectionKind {
    fn notify_type(self) -> u16 {
        match self {
            Self::Source => 16_388,
            Self::Destination => 16_389,
        }
    }
}

/// The retained swu-go writer uses IKE Protocol ID 1 and no Notify SPI.
/// RFC 7296 section 3.10 requires Protocol ID 0 with an empty SPI; retaining
/// the Go byte here deliberately isolates NAT Notify presence in the A/B test.
/// Revisit this nonconformance separately after peer interoperability is known.
/// `responder_spi` is zero in IKE_SA_INIT requests. A nonzero value is useful
/// when checking a peer's response against the known outer socket tuple.
pub fn nat_detection_notify(
    kind: NatDetectionKind,
    initiator_spi: u64,
    responder_spi: u64,
    endpoint: SocketAddr,
) -> PayloadFrame {
    let mut hash = Sha1::new();
    hash.update(initiator_spi.to_be_bytes());
    hash.update(responder_spi.to_be_bytes());
    match endpoint.ip() {
        IpAddr::V4(ip) => hash.update(ip.octets()),
        IpAddr::V6(ip) => hash.update(ip.octets()),
    }
    hash.update(endpoint.port().to_be_bytes());

    let mut body = Vec::with_capacity(24);
    body.extend_from_slice(&[1, 0]); // Protocol ID IKE, Notify SPI size zero.
    body.extend_from_slice(&kind.notify_type().to_be_bytes());
    body.extend_from_slice(&hash.finalize());
    PayloadFrame {
        kind: 41,
        critical: false,
        body,
    }
}
