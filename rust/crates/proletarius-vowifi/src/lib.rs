#![forbid(unsafe_code)]

pub mod carrier;
pub mod carrier_overrides;
pub mod carrier_runtime_profile;
pub mod carrier_session_config;
pub mod ims_network_runtime;
pub mod ims_profile;
pub mod message_routing;
pub mod netstack_device;
pub mod netstack_runtime;
pub mod netstack_tcp;
pub mod netstack_udp;
pub mod registration_coordinator;
pub mod security_runtime;
mod sip_response_registry;
pub mod sip_session_runtime;
pub mod sms_transaction;

use std::fmt;
use std::net::IpAddr;

mod auth;
mod auth_config;
pub use auth_config::{
    AuthConfigError, InitialAuthParameters, RequestedIpStack, build_initial_auth_frames,
};
mod startup;
#[cfg(test)]
mod startup_loopback;
pub use startup::{
    NativeConfigError, NativeNetworkError, NativeTunnel, NativeTunnelOptions, NativeTunnelSettings,
    TunnelStartupError, start_configured_native_tunnel,
    start_configured_native_tunnel_with_options,
};
mod child_data;
pub use child_data::{
    ChildDataError, ChildDataPlane, PolicyRejectReason, TunnelCommand, TunnelEvent, TunnelRunStats,
    TunnelTrafficProbe, TunnelTrafficSnapshot,
};
mod tunnel_config;
pub use tunnel_config::{TunnelConfigError, TunnelConfiguration, parse_tunnel_configuration};
mod traffic_selectors;
pub use traffic_selectors::{
    ChildTrafficSelectors, TrafficSelector, TrafficSelectorError, validate_child_traffic_selectors,
};
mod child_rekey;
mod child_sa;
mod ike_rekey;
pub use child_sa::{ChildSaError, InitialChildSaSelection, validate_initial_child_sa};
mod eap;
mod eap_runtime;
mod peer_certificate;
pub use peer_certificate::{
    CertificateAuthRequest, CertificateVerifyFuture, PeerCertificatePolicy,
    PeerCertificateVerifier, valid_peer_name,
};
mod relay;
mod sa_init;
mod transport;

pub use auth::{IkeAuthError, IkeAuthResponse, IkeAuthSession, PendingChildSa};
pub use eap::{
    EapAkaExchangeError, EapAkaExchangeResult, EapDirectAuthMaterial, Ikev2EapConversation,
    Ikev2EapError, SimAkaResult, eap_request_from_ike, eap_response_frame,
};
pub use eap_runtime::{EapHandshakeError, EapHandshakeResult};
pub use relay::{RelayError, Socks5Probe, Socks5UdpRelay, probe_socks5};
pub use sa_init::{
    SaInitError, SaInitKeySchedule, SaInitResponse, perform_sa_init, perform_sa_init_configured,
    perform_sa_init_group14, perform_sa_init_payloads,
};
pub use transport::{DatagramTransportError, EpdgDatagramTransport, EpdgPacket};

pub const PROXY_CONTRACT: &str = "v0.16-proxy-preferred-direct";

/// Returns the delay before rebuilding an authenticated IMS session. The
/// network-granted Contact lifetime is authoritative; refresh keeps a bounded
/// safety window without reconnecting so early that it creates registration
/// churn.
pub fn registration_renewal_delay_seconds(expires_seconds: u64) -> Option<u64> {
    if expires_seconds == 0 {
        return None;
    }
    let safety = (expires_seconds / 4).clamp(60, 300);
    Some(expires_seconds.saturating_sub(safety).max(30))
}

/// Bounded exponential delay for rebuilding the whole reader VoWiFi session.
/// Attempt one waits two seconds and the delay is capped at sixteen seconds.
pub fn reconnect_delay_seconds(attempt: u32) -> u64 {
    1_u64 << attempt.clamp(1, 4)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteEvidence {
    pub address: IpAddr,
    pub interface: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProxyEvidence {
    pub proxy_is_loopback: bool,
    pub epdg_addresses: Vec<IpAddr>,
    pub routes: Vec<RouteEvidence>,
    pub socks5_udp_associate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyMode {
    EnhancedFakeIp(IpAddr),
    EnhancedUtun { address: IpAddr, interface: String },
    Socks5UdpDomain,
    Direct(IpAddr),
}

impl ProxyMode {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::EnhancedFakeIp(_) => "enhanced-mode-fake-ip",
            Self::EnhancedUtun { .. } => "enhanced-mode-utun",
            Self::Socks5UdpDomain => "socks5-udp-domain",
            Self::Direct(_) => "direct",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyPolicyError {
    Socks5UdpUnavailable,
    UnverifiedPublicRoute,
}

impl fmt::Display for ProxyPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socks5UdpUnavailable => formatter.write_str(
                "SOCKS5 UDP ASSOCIATE is unavailable and no real ePDG address is available",
            ),
            Self::UnverifiedPublicRoute => {
                formatter.write_str("only fake-IP/unusable ePDG addresses are available")
            }
        }
    }
}

impl std::error::Error for ProxyPolicyError {}

/// Selects proxy transport first, then a real-address direct path. A fake-IP
/// is never treated as a direct destination.
pub fn select_proxy_mode(evidence: &ProxyEvidence) -> Result<ProxyMode, ProxyPolicyError> {
    if evidence.proxy_is_loopback
        && let Some(address) = evidence
            .routes
            .iter()
            .find(|route| {
                is_fake_ip(route.address)
                    && evidence.epdg_addresses.contains(&route.address)
                    && route
                        .interface
                        .as_deref()
                        .is_some_and(|interface| interface.starts_with("utun"))
            })
            .map(|route| route.address)
    {
        return Ok(ProxyMode::EnhancedFakeIp(address));
    }

    for route in &evidence.routes {
        let Some(interface) = route.interface.as_deref() else {
            continue;
        };
        if interface.starts_with("utun") && evidence.epdg_addresses.contains(&route.address) {
            return Ok(ProxyMode::EnhancedUtun {
                address: route.address,
                interface: interface.to_owned(),
            });
        }
    }

    if evidence.socks5_udp_associate {
        return Ok(ProxyMode::Socks5UdpDomain);
    }

    if let Some(address) = evidence
        .epdg_addresses
        .iter()
        .copied()
        .find(|address| is_public_direct_ip(*address))
    {
        return Ok(ProxyMode::Direct(address));
    }
    Err(ProxyPolicyError::UnverifiedPublicRoute)
}

pub fn is_fake_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let octets = address.octets();
            octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)
        }
        IpAddr::V6(_) => false,
    }
}

/// Returns whether an ePDG address is safe to use as a direct destination.
///
/// Direct mode is an opt-out of the configured proxy path, so it must never
/// turn a DNS answer for a loopback, private, link-local, documentation, or
/// multicast address into host-network traffic. IPv6 direct mode is restricted
/// to global-unicast space rather than treating every non-IPv4-fake address as
/// public.
pub fn is_public_direct_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [first, second, third, _] = address.octets();
            !matches!(
                (first, second, third),
                // Unspecified, RFC 1918, shared, loopback, and link-local.
                (0, _, _)
                    | (10, _, _)
                    | (100, 64..=127, _)
                    | (127, _, _)
                    | (169, 254, _)
                    | (172, 16..=31, _)
                    | (192, 168, _)
                    // IANA special-purpose and documentation blocks.
                    | (192, 0, _)
                    | (192, 31, 196)
                    | (192, 52, 193)
                    | (192, 88, 99)
                    | (192, 175, 48)
                    | (198, 18..=19, _)
                    | (198, 51, 100)
                    | (203, 0, 113)
                    // Multicast and reserved space, including limited broadcast.
                    | (224..=255, _, _)
            )
        }
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return is_public_direct_ip(IpAddr::V4(mapped));
            }
            let segments = address.segments();
            // Global-unicast IPv6 is 2000::/3. Exclude IETF protocol,
            // transition, benchmarking, and documentation allocations within
            // that range as well.
            (segments[0] & 0xe000) == 0x2000
                && !((segments[0] == 0x2001 && segments[1] == 0x0000)
                    || (segments[0] == 0x2001 && segments[1] == 0x0002)
                    || (segments[0] == 0x2001 && segments[1] == 0x0003)
                    || (segments[0] == 0x2001 && segments[1] == 0x0004 && segments[2] == 0x0112)
                    || (segments[0] == 0x2001
                        && matches!(segments[1] & 0xfff0, 0x0010 | 0x0020 | 0x0030))
                    || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                    || segments[0] == 0x2002
                    || (segments[0] == 0x2620 && segments[1] == 0x004f && segments[2] == 0x8000)
                    || (segments[0] == 0x3fff && (segments[1] & 0xf000) == 0))
        }
    }
}

pub const NO_AUTH_GREETING: [u8; 3] = [0x05, 0x01, 0x00];
pub const UDP_ASSOCIATE_REQUEST: [u8; 10] = [0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SocksTarget {
    Ip(IpAddr),
    Domain(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SocksDatagram {
    pub target: SocksTarget,
    pub port: u16,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SocksCodecError {
    Truncated,
    InvalidVersion(u8),
    AuthenticationRejected(u8),
    AssociateRejected(u8),
    ReservedField,
    FragmentationUnsupported(u8),
    AddressTypeUnsupported(u8),
    EmptyDomain,
    DomainTooLong,
    InvalidDomain,
    InvalidPort,
}

impl fmt::Display for SocksCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SOCKS5 codec error: {self:?}")
    }
}

impl std::error::Error for SocksCodecError {}

pub fn parse_no_auth_response(response: &[u8]) -> Result<(), SocksCodecError> {
    if response.len() < 2 {
        return Err(SocksCodecError::Truncated);
    }
    if response[0] != 0x05 {
        return Err(SocksCodecError::InvalidVersion(response[0]));
    }
    if response[1] != 0x00 {
        return Err(SocksCodecError::AuthenticationRejected(response[1]));
    }
    Ok(())
}

pub fn parse_udp_associate_response(
    response: &[u8],
) -> Result<(SocksTarget, u16), SocksCodecError> {
    if response.len() < 4 {
        return Err(SocksCodecError::Truncated);
    }
    if response[0] != 0x05 {
        return Err(SocksCodecError::InvalidVersion(response[0]));
    }
    if response[1] != 0x00 {
        return Err(SocksCodecError::AssociateRejected(response[1]));
    }
    if response[2] != 0x00 {
        return Err(SocksCodecError::ReservedField);
    }
    let (target, port_offset) = decode_target(response, 3)?;
    let port = read_port(response, port_offset)?;
    Ok((target, port))
}

pub fn encode_udp_domain(
    domain: &str,
    port: u16,
    payload: &[u8],
) -> Result<Vec<u8>, SocksCodecError> {
    let domain = domain.trim();
    if domain.is_empty() {
        return Err(SocksCodecError::EmptyDomain);
    }
    if !domain.is_ascii() {
        return Err(SocksCodecError::InvalidDomain);
    }
    if domain.len() > u8::MAX as usize {
        return Err(SocksCodecError::DomainTooLong);
    }
    if port == 0 {
        return Err(SocksCodecError::InvalidPort);
    }
    let mut packet = Vec::with_capacity(7 + domain.len() + payload.len());
    packet.extend_from_slice(&[0, 0, 0, 0x03, domain.len() as u8]);
    packet.extend_from_slice(domain.as_bytes());
    packet.extend_from_slice(&port.to_be_bytes());
    packet.extend_from_slice(payload);
    Ok(packet)
}

pub fn decode_udp_datagram(packet: &[u8]) -> Result<SocksDatagram, SocksCodecError> {
    if packet.len() < 4 {
        return Err(SocksCodecError::Truncated);
    }
    if packet[0] != 0 || packet[1] != 0 {
        return Err(SocksCodecError::ReservedField);
    }
    if packet[2] != 0 {
        return Err(SocksCodecError::FragmentationUnsupported(packet[2]));
    }
    let (target, port_offset) = decode_target(packet, 3)?;
    let port = read_port(packet, port_offset)?;
    Ok(SocksDatagram {
        target,
        port,
        payload: packet[port_offset + 2..].to_vec(),
    })
}

fn decode_target(
    packet: &[u8],
    atyp_offset: usize,
) -> Result<(SocksTarget, usize), SocksCodecError> {
    let atyp = *packet.get(atyp_offset).ok_or(SocksCodecError::Truncated)?;
    let address_offset = atyp_offset + 1;
    match atyp {
        0x01 => {
            let octets: [u8; 4] = packet
                .get(address_offset..address_offset + 4)
                .ok_or(SocksCodecError::Truncated)?
                .try_into()
                .map_err(|_| SocksCodecError::Truncated)?;
            Ok((SocksTarget::Ip(octets.into()), address_offset + 4))
        }
        0x03 => {
            let length = *packet
                .get(address_offset)
                .ok_or(SocksCodecError::Truncated)? as usize;
            if length == 0 {
                return Err(SocksCodecError::EmptyDomain);
            }
            let bytes = packet
                .get(address_offset + 1..address_offset + 1 + length)
                .ok_or(SocksCodecError::Truncated)?;
            let domain = std::str::from_utf8(bytes)
                .map_err(|_| SocksCodecError::InvalidDomain)?
                .to_owned();
            Ok((SocksTarget::Domain(domain), address_offset + 1 + length))
        }
        0x04 => {
            let octets: [u8; 16] = packet
                .get(address_offset..address_offset + 16)
                .ok_or(SocksCodecError::Truncated)?
                .try_into()
                .map_err(|_| SocksCodecError::Truncated)?;
            Ok((SocksTarget::Ip(octets.into()), address_offset + 16))
        }
        other => Err(SocksCodecError::AddressTypeUnsupported(other)),
    }
}

fn read_port(packet: &[u8], offset: usize) -> Result<u16, SocksCodecError> {
    let bytes: [u8; 2] = packet
        .get(offset..offset + 2)
        .ok_or(SocksCodecError::Truncated)?
        .try_into()
        .map_err(|_| SocksCodecError::Truncated)?;
    Ok(u16::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn lifecycle_delays_match_live_session_contract() {
        assert_eq!(registration_renewal_delay_seconds(0), None);
        assert_eq!(registration_renewal_delay_seconds(120), Some(60));
        assert_eq!(registration_renewal_delay_seconds(600), Some(450));
        assert_eq!(registration_renewal_delay_seconds(2_400), Some(2_100));
        assert_eq!(registration_renewal_delay_seconds(3_600), Some(3_300));
        assert_eq!(reconnect_delay_seconds(0), 2);
        assert_eq!(reconnect_delay_seconds(1), 2);
        assert_eq!(reconnect_delay_seconds(2), 4);
        assert_eq!(reconnect_delay_seconds(9), 16);
    }

    #[test]
    fn fake_ip_range_is_exact() {
        assert!(is_fake_ip("198.18.0.1".parse().unwrap()));
        assert!(is_fake_ip("198.19.255.254".parse().unwrap()));
        assert!(!is_fake_ip("198.20.0.1".parse().unwrap()));
        assert!(!is_fake_ip("10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn direct_mode_requires_a_globally_routable_address() {
        for address in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.168.1.1",
            "192.0.2.1",
            "192.31.196.1",
            "192.52.193.1",
            "192.175.48.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "::",
            "::1",
            "::ffff:10.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2001::1",
            "2001:2::1",
            "2001:3::1",
            "2001:4:112::1",
            "2001:10::1",
            "2001:20::1",
            "2001:30::1",
            "2002::1",
            "2620:4f:8000::1",
            "3fff::1",
            "ff02::1",
        ] {
            assert!(
                !is_public_direct_ip(address.parse().unwrap()),
                "{address} must not become a direct ePDG destination"
            );
        }
        for address in ["87.194.8.8", "2606:4700:4700::1111"] {
            assert!(
                is_public_direct_ip(address.parse().unwrap()),
                "{address} should remain eligible for direct mode"
            );
        }

        let blocked = ProxyEvidence {
            epdg_addresses: vec!["10.0.0.1".parse().unwrap(), "::1".parse().unwrap()],
            ..ProxyEvidence::default()
        };
        assert_eq!(
            select_proxy_mode(&blocked),
            Err(ProxyPolicyError::UnverifiedPublicRoute)
        );
    }

    #[test]
    fn policy_prefers_fake_ip_then_verified_utun() {
        let public: IpAddr = "87.194.8.8".parse().unwrap();
        let fake: IpAddr = "198.18.0.14".parse().unwrap();
        let evidence = ProxyEvidence {
            proxy_is_loopback: true,
            epdg_addresses: vec![public, fake],
            routes: vec![
                RouteEvidence {
                    address: public,
                    interface: Some("utun8".into()),
                },
                RouteEvidence {
                    address: fake,
                    interface: Some("utun8".into()),
                },
            ],
            socks5_udp_associate: true,
        };
        assert_eq!(
            select_proxy_mode(&evidence),
            Ok(ProxyMode::EnhancedFakeIp(fake))
        );

        let evidence = ProxyEvidence {
            epdg_addresses: vec![public],
            ..evidence
        };
        assert_eq!(
            select_proxy_mode(&evidence),
            Ok(ProxyMode::EnhancedUtun {
                address: public,
                interface: "utun8".into(),
            })
        );
    }

    #[test]
    fn fake_ip_selection_uses_only_the_address_with_verified_utun_route() {
        let stale: IpAddr = "198.18.0.14".parse().unwrap();
        let routed: IpAddr = "198.18.0.15".parse().unwrap();
        let evidence = ProxyEvidence {
            proxy_is_loopback: true,
            epdg_addresses: vec![stale, routed],
            routes: vec![
                RouteEvidence {
                    address: stale,
                    interface: Some("en0".into()),
                },
                RouteEvidence {
                    address: routed,
                    interface: Some("utun8".into()),
                },
            ],
            socks5_udp_associate: false,
        };
        assert_eq!(
            select_proxy_mode(&evidence),
            Ok(ProxyMode::EnhancedFakeIp(routed))
        );
    }

    #[test]
    fn policy_uses_proxy_side_domain_then_direct_udp() {
        let public: IpAddr = "87.194.8.8".parse().unwrap();
        let fallback = ProxyEvidence {
            proxy_is_loopback: true,
            epdg_addresses: vec![public],
            routes: vec![RouteEvidence {
                address: public,
                interface: Some("en0".into()),
            }],
            socks5_udp_associate: true,
        };
        assert_eq!(select_proxy_mode(&fallback), Ok(ProxyMode::Socks5UdpDomain));

        let rejected = ProxyEvidence {
            socks5_udp_associate: false,
            ..fallback
        };
        assert_eq!(select_proxy_mode(&rejected), Ok(ProxyMode::Direct(public)));
    }

    #[test]
    fn unavailable_remote_proxy_can_use_real_address() {
        let evidence = ProxyEvidence {
            proxy_is_loopback: false,
            socks5_udp_associate: false,
            epdg_addresses: vec!["87.194.8.8".parse().unwrap()],
            ..ProxyEvidence::default()
        };
        assert_eq!(
            select_proxy_mode(&evidence),
            Ok(ProxyMode::Direct("87.194.8.8".parse().unwrap()))
        );
    }

    #[test]
    fn domain_datagram_round_trips_without_local_resolution() {
        let packet = encode_udp_domain("epdg.example", 4500, b"ike").unwrap();
        assert_eq!(packet[3], 0x03);
        assert_eq!(
            decode_udp_datagram(&packet).unwrap(),
            SocksDatagram {
                target: SocksTarget::Domain("epdg.example".into()),
                port: 4500,
                payload: b"ike".to_vec(),
            }
        );
    }

    #[test]
    fn datagram_decoder_supports_ipv4_and_ipv6() {
        let ipv4 = [0, 0, 0, 1, 203, 0, 113, 7, 0x01, 0xbb, 9];
        assert_eq!(
            decode_udp_datagram(&ipv4).unwrap().target,
            SocksTarget::Ip(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)))
        );
        let mut ipv6 = vec![0, 0, 0, 4];
        ipv6.extend_from_slice(
            &"2001:db8::1"
                .parse::<std::net::Ipv6Addr>()
                .unwrap()
                .octets(),
        );
        ipv6.extend_from_slice(&[0x11, 0x94, 7]);
        assert_eq!(decode_udp_datagram(&ipv6).unwrap().port, 4500);
    }

    #[test]
    fn malformed_or_fragmented_datagrams_are_rejected() {
        assert_eq!(
            decode_udp_datagram(&[0, 0, 1, 1, 127, 0, 0, 1, 0, 80]),
            Err(SocksCodecError::FragmentationUnsupported(1))
        );
        assert_eq!(
            decode_udp_datagram(&[0, 0, 0, 3, 8, b'e']),
            Err(SocksCodecError::Truncated)
        );
        assert_eq!(
            encode_udp_domain("", 4500, b""),
            Err(SocksCodecError::EmptyDomain)
        );
    }

    #[test]
    fn handshake_responses_are_strictly_parsed() {
        parse_no_auth_response(&[5, 0]).unwrap();
        assert_eq!(
            parse_no_auth_response(&[5, 2]),
            Err(SocksCodecError::AuthenticationRejected(2))
        );
        assert_eq!(
            parse_udp_associate_response(&[5, 0, 0, 1, 127, 0, 0, 1, 0x1e, 0x61]).unwrap(),
            (SocksTarget::Ip("127.0.0.1".parse().unwrap()), 7777)
        );
    }
}
