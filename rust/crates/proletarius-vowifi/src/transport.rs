use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use proletarius_ike::{IkeHeader, IkeProtocolError, NON_ESP_MARKER};
use tokio::net::UdpSocket;

use crate::{ProxyMode, RelayError, Socks5UdpRelay, encode_udp_domain};

const MAX_DATAGRAM_BYTES: usize = 65_535;

fn ike_wire_payload(port: u16, payload: &[u8]) -> Vec<u8> {
    if port == 4500 {
        proletarius_ike::encode_natt(payload)
    } else {
        payload.to_vec()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EpdgPacket {
    Ike(Vec<u8>),
    Esp(Vec<u8>),
    Keepalive,
}

fn classify_packet(port: u16, packet: Vec<u8>) -> Result<EpdgPacket, DatagramTransportError> {
    if port == 4500 {
        if packet == [0xff] {
            return Ok(EpdgPacket::Keepalive);
        }
        if packet.starts_with(&NON_ESP_MARKER) {
            let ike = packet[NON_ESP_MARKER.len()..].to_vec();
            IkeHeader::decode(&ike).map_err(DatagramTransportError::InvalidIke)?;
            return Ok(EpdgPacket::Ike(ike));
        }
        if packet.len() < 8 {
            return Err(DatagramTransportError::InvalidEsp);
        }
        return Ok(EpdgPacket::Esp(packet));
    }
    IkeHeader::decode(&packet).map_err(DatagramTransportError::InvalidIke)?;
    Ok(EpdgPacket::Ike(packet))
}

#[derive(Debug)]
pub enum DatagramTransportError {
    MissingProxy,
    InvalidPort,
    EspRequiresNatT,
    InvalidEsp,
    InvalidIke(IkeProtocolError),
    Io(io::Error),
    Relay(RelayError),
}

impl fmt::Display for DatagramTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProxy => formatter.write_str("SOCKS5 mode requires a proxy endpoint"),
            Self::InvalidPort => formatter.write_str("ePDG port must be non-zero"),
            Self::EspRequiresNatT => formatter.write_str("ESP requires NAT-T port 4500"),
            Self::InvalidEsp => formatter.write_str("ESP-in-UDP packet is shorter than its header"),
            Self::InvalidIke(error) => write!(formatter, "invalid IKE packet: {error}"),
            Self::Io(error) => error.fmt(formatter),
            Self::Relay(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DatagramTransportError {}

impl From<io::Error> for DatagramTransportError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<RelayError> for DatagramTransportError {
    fn from(value: RelayError) -> Self {
        Self::Relay(value)
    }
}

/// A single ePDG UDP channel. IKE :500 transitions to NAT-T/ESP :4500 on the
/// same socket/association, preserving the local UDP mapping. Transparent
/// modes leave interface selection to macOS so Network Extensions can capture
/// the socket; the SOCKS mode never resolves the ePDG hostname locally.
pub enum EpdgDatagramTransport {
    PinnedNatT(Box<EpdgDatagramTransport>),
    Transparent(UdpSocket),
    Socks5 {
        relay: Socks5UdpRelay,
        epdg_host: String,
        epdg_port: u16,
    },
}

/// Evidence used to build the two IKE_SA_INIT NAT detection notifications.
///
/// A transparent socket exposes its exact numeric outer tuple. A SOCKS5
/// domain relay intentionally hides the numeric ePDG address from this
/// process, but the relay still changes the outer path and therefore requires
/// NAT traversal. `ForceNat` keeps that semantic explicit without resolving
/// the ePDG hostname locally or bypassing the proxy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NatDetectionEvidence {
    Exact {
        source: SocketAddr,
        destination: SocketAddr,
    },
    ForceNat,
}

impl EpdgDatagramTransport {
    pub(crate) fn nat_detection_evidence(
        &self,
    ) -> Result<NatDetectionEvidence, DatagramTransportError> {
        match self {
            Self::PinnedNatT(inner) => inner.nat_detection_evidence(),
            Self::Transparent(socket) => {
                let local = socket.local_addr()?;
                let peer = socket.peer_addr()?;
                // Match the retained Go sender: hash even an unspecified
                // bound source address as reported, so the peer detects NAT.
                // Never open a second socket to infer a different address.
                Ok(NatDetectionEvidence::Exact {
                    source: local,
                    destination: peer,
                })
            }
            Self::Socks5 { .. } => Ok(NatDetectionEvidence::ForceNat),
        }
    }

    fn epdg_port(&self) -> Result<u16, DatagramTransportError> {
        match self {
            Self::PinnedNatT(inner) => inner.epdg_port(),
            Self::Transparent(socket) => Ok(socket.peer_addr()?.port()),
            Self::Socks5 { epdg_port, .. } => Ok(*epdg_port),
        }
    }

    pub async fn open(
        mode: ProxyMode,
        epdg_host: &str,
        epdg_port: u16,
        proxy: Option<SocketAddr>,
    ) -> Result<Self, DatagramTransportError> {
        Self::open_with_local_port(mode, epdg_host, epdg_port, proxy, 0).await
    }
    pub async fn open_with_local_port(
        mode: ProxyMode,
        epdg_host: &str,
        epdg_port: u16,
        proxy: Option<SocketAddr>,
        local_port: u16,
    ) -> Result<Self, DatagramTransportError> {
        if epdg_port == 0 {
            return Err(DatagramTransportError::InvalidPort);
        }
        match mode {
            ProxyMode::Socks5UdpDomain => {
                // Validate the domain without asking the local resolver for an
                // IP. The same codec is used for each later UDP datagram.
                encode_udp_domain(epdg_host, epdg_port, &[]).map_err(RelayError::Codec)?;
                let proxy = proxy.ok_or(DatagramTransportError::MissingProxy)?;
                let relay = Socks5UdpRelay::associate_with_local_port(proxy, local_port).await?;
                Ok(Self::Socks5 {
                    relay,
                    epdg_host: epdg_host.to_owned(),
                    epdg_port,
                })
            }
            ProxyMode::EnhancedFakeIp(address)
            | ProxyMode::EnhancedUtun { address, .. }
            | ProxyMode::Direct(address) => {
                let bind = match address {
                    IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), local_port),
                    IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), local_port),
                };
                let socket = UdpSocket::bind(bind).await?;
                socket.connect(SocketAddr::new(address, epdg_port)).await?;
                Ok(Self::Transparent(socket))
            }
        }
    }

    pub async fn send(&self, payload: &[u8]) -> Result<usize, DatagramTransportError> {
        match self {
            Self::PinnedNatT(inner) => Box::pin(inner.send(payload)).await,
            Self::Transparent(socket) => Ok(socket.send(payload).await?),
            Self::Socks5 {
                relay,
                epdg_host,
                epdg_port,
            } => {
                relay.send_domain(epdg_host, *epdg_port, payload).await?;
                Ok(payload.len())
            }
        }
    }

    pub async fn send_ike(&self, payload: &[u8]) -> Result<(), DatagramTransportError> {
        self.send(&ike_wire_payload(self.epdg_port()?, payload))
            .await?;
        Ok(())
    }

    pub async fn send_esp(&self, payload: &[u8]) -> Result<(), DatagramTransportError> {
        if self.epdg_port()? != 4500 && !matches!(self, Self::PinnedNatT(_)) {
            return Err(DatagramTransportError::EspRequiresNatT);
        }
        self.send(payload).await?;
        Ok(())
    }

    pub async fn send_keepalive(&self) -> Result<(), DatagramTransportError> {
        if self.epdg_port()? != 4500 && !matches!(self, Self::PinnedNatT(_)) {
            return Err(DatagramTransportError::EspRequiresNatT);
        }
        self.send(&[0xff]).await?;
        Ok(())
    }

    /// Move an existing IKE channel to NAT-T without replacing its local UDP
    /// socket or SOCKS5 UDP ASSOCIATE. The caller must serialize this with
    /// send/receive so responses are interpreted against the intended port.
    pub async fn switch_port(&mut self, port: u16) -> Result<(), DatagramTransportError> {
        if port == 0 {
            return Err(DatagramTransportError::InvalidPort);
        }
        match self {
            Self::PinnedNatT(inner) => return Box::pin(inner.switch_port(port)).await,
            Self::Transparent(socket) => {
                let address = socket.peer_addr()?.ip();
                socket.connect(SocketAddr::new(address, port)).await?;
            }
            Self::Socks5 { epdg_port, .. } => *epdg_port = port,
        }
        Ok(())
    }

    pub async fn receive(&self) -> Result<Vec<u8>, DatagramTransportError> {
        match self {
            Self::PinnedNatT(inner) => Box::pin(inner.receive()).await,
            Self::Transparent(socket) => {
                let mut packet = vec![0; MAX_DATAGRAM_BYTES];
                let length = socket.recv(&mut packet).await?;
                packet.truncate(length);
                Ok(packet)
            }
            Self::Socks5 {
                relay, epdg_port, ..
            } => {
                loop {
                    let packet = relay.receive().await?;
                    // A :500 reply can arrive after NAT-T has moved to :4500.
                    // It must neither tear down the session nor switch the
                    // destination back to the obsolete port.
                    if packet.port == *epdg_port {
                        break Ok(packet.payload);
                    }
                }
            }
        }
    }

    pub fn enable_pinned_natt(self) -> Self {
        Self::PinnedNatT(Box::new(self))
    }

    pub async fn receive_packet(&self) -> Result<EpdgPacket, DatagramTransportError> {
        let packet = self.receive().await?;
        if matches!(self, Self::PinnedNatT(_)) && self.epdg_port()? != 4500 {
            if packet == [0xff] {
                return Ok(EpdgPacket::Keepalive);
            }
            if IkeHeader::decode(&packet).is_ok() {
                return Ok(EpdgPacket::Ike(packet));
            }
            if packet.len() < 8 {
                return Err(DatagramTransportError::InvalidEsp);
            }
            return Ok(EpdgPacket::Esp(packet));
        }
        classify_packet(self.epdg_port()?, packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SocksTarget, decode_udp_datagram};
    use proletarius_ike::ExchangeType;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::{Duration, timeout};

    fn sample_ike() -> Vec<u8> {
        IkeHeader {
            initiator_spi: 1,
            responder_spi: 2,
            next_payload: 0,
            version: 0x20,
            exchange: ExchangeType::Informational,
            flags: IkeHeader::RESPONSE_FLAG,
            message_id: 3,
        }
        .encode(&[])
        .unwrap()
    }

    #[test]
    fn natt_receive_classifies_ike_esp_keepalive_without_cross_delivery() {
        let ike = sample_ike();
        assert_eq!(
            classify_packet(500, ike.clone()).unwrap(),
            EpdgPacket::Ike(ike.clone())
        );
        assert_eq!(
            classify_packet(4500, ike_wire_payload(4500, &ike)).unwrap(),
            EpdgPacket::Ike(ike)
        );
        assert_eq!(
            classify_packet(4500, vec![0xff]).unwrap(),
            EpdgPacket::Keepalive
        );
        let esp = vec![1, 0, 0, 0, 0, 0, 0, 1];
        assert_eq!(
            classify_packet(4500, esp.clone()).unwrap(),
            EpdgPacket::Esp(esp)
        );
        assert!(matches!(
            classify_packet(4500, vec![0, 0, 0, 0]),
            Err(DatagramTransportError::InvalidIke(_))
        ));
        assert!(matches!(
            classify_packet(4500, vec![1, 2, 3]),
            Err(DatagramTransportError::InvalidEsp)
        ));
        assert!(matches!(
            classify_packet(500, vec![0xff]),
            Err(DatagramTransportError::InvalidIke(_))
        ));
    }

    #[tokio::test]
    async fn transparent_mode_uses_system_route_and_round_trips() {
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let natt_peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = peer.local_addr().unwrap();
        let mut transport = EpdgDatagramTransport::open(
            ProxyMode::Direct(address.ip()),
            "not-used.invalid",
            address.port(),
            None,
        )
        .await
        .unwrap();
        let EpdgDatagramTransport::Transparent(socket) = &transport else {
            panic!("selected direct mode changed transport type")
        };
        assert_eq!(
            transport.nat_detection_evidence().unwrap(),
            NatDetectionEvidence::Exact {
                source: socket.local_addr().unwrap(),
                destination: socket.peer_addr().unwrap(),
            }
        );
        // macOS chooses a concrete source address once the wildcard-bound
        // socket is connected; that does not imply IP_BOUND_IF was applied.
        let local_port = socket.local_addr().unwrap().port();
        assert_ne!(local_port, 0);
        assert_eq!(transport.send(b"ike").await.unwrap(), 3);
        let mut packet = [0; 16];
        let (length, sender) = peer.recv_from(&mut packet).await.unwrap();
        assert_eq!(&packet[..length], b"ike");
        peer.send_to(b"response", sender).await.unwrap();
        assert_eq!(transport.receive().await.unwrap(), b"response");
        assert!(matches!(
            transport.send_esp(b"esp").await,
            Err(DatagramTransportError::EspRequiresNatT)
        ));
        transport
            .switch_port(natt_peer.local_addr().unwrap().port())
            .await
            .unwrap();
        assert_eq!(transport.send(b"natt").await.unwrap(), 4);
        let (length, sender) = natt_peer.recv_from(&mut packet).await.unwrap();
        assert_eq!(&packet[..length], b"natt");
        assert_eq!(sender.port(), local_port);
        transport.send_ike(b"ike").await.unwrap();
        let (length, _) = natt_peer.recv_from(&mut packet).await.unwrap();
        assert_eq!(&packet[..length], b"ike");
        assert_eq!(ike_wire_payload(500, b"ike"), b"ike");
        assert_eq!(ike_wire_payload(4500, b"ike"), b"\0\0\0\0ike");
    }

    #[tokio::test]
    async fn socks_mode_preserves_epdg_domain_and_rejects_other_ports() {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = tcp.local_addr().unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let udp_address = udp.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut control, _) = tcp.accept().await.unwrap();
            let mut greeting = [0; 3];
            control.read_exact(&mut greeting).await.unwrap();
            control.write_all(&[5, 0]).await.unwrap();
            let mut associate = [0; 10];
            control.read_exact(&mut associate).await.unwrap();
            let mut response = vec![5, 0, 0, 1, 127, 0, 0, 1];
            response.extend_from_slice(&udp_address.port().to_be_bytes());
            control.write_all(&response).await.unwrap();

            let mut packet = [0; 256];
            let (length, sender) = udp.recv_from(&mut packet).await.unwrap();
            let decoded = decode_udp_datagram(&packet[..length]).unwrap();
            assert_eq!(decoded.target, SocksTarget::Domain("epdg.invalid".into()));
            assert_eq!(decoded.port, 500);
            assert_eq!(decoded.payload, b"ike");
            let wrong_port = crate::encode_udp_domain("epdg.invalid", 4500, b"wrong").unwrap();
            udp.send_to(&wrong_port, sender).await.unwrap();
            let good = crate::encode_udp_domain("epdg.invalid", 500, b"response").unwrap();
            udp.send_to(&good, sender).await.unwrap();
            let (length, next_sender) = udp.recv_from(&mut packet).await.unwrap();
            let decoded = decode_udp_datagram(&packet[..length]).unwrap();
            assert_eq!(next_sender, sender);
            assert_eq!(decoded.port, 4500);
            assert_eq!(decoded.payload, b"natt");
            let stale = crate::encode_udp_domain("epdg.invalid", 500, b"late-ike").unwrap();
            udp.send_to(&stale, sender).await.unwrap();
            let good = crate::encode_udp_domain("epdg.invalid", 4500, b"natt-response").unwrap();
            udp.send_to(&good, sender).await.unwrap();
            let ike = ike_wire_payload(4500, &sample_ike());
            for payload in [ike, vec![1, 0, 0, 0, 0, 0, 0, 1], vec![0xff]] {
                let packet = crate::encode_udp_domain("epdg.invalid", 4500, &payload).unwrap();
                udp.send_to(&packet, sender).await.unwrap();
            }
            let (length, next_sender) = udp.recv_from(&mut packet).await.unwrap();
            let decoded = decode_udp_datagram(&packet[..length]).unwrap();
            assert_eq!(next_sender, sender);
            assert_eq!(decoded.port, 4500);
            assert_eq!(decoded.payload, b"\0\0\0\0ike");
            let (length, _) = udp.recv_from(&mut packet).await.unwrap();
            let decoded = decode_udp_datagram(&packet[..length]).unwrap();
            assert_eq!(decoded.payload, b"esp");
            let (length, _) = udp.recv_from(&mut packet).await.unwrap();
            let decoded = decode_udp_datagram(&packet[..length]).unwrap();
            assert_eq!(decoded.payload, [0xff]);
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let mut transport = EpdgDatagramTransport::open(
            ProxyMode::Socks5UdpDomain,
            "epdg.invalid",
            500,
            Some(proxy),
        )
        .await
        .unwrap();
        // SOCKS hides the numeric ePDG tuple, but it is itself an outer-path
        // translation. Force NAT-T without resolving the domain locally.
        assert_eq!(
            transport.nat_detection_evidence().unwrap(),
            NatDetectionEvidence::ForceNat
        );
        assert_eq!(transport.send(b"ike").await.unwrap(), 3);
        assert_eq!(
            timeout(Duration::from_secs(1), transport.receive())
                .await
                .unwrap()
                .unwrap(),
            b"response"
        );
        transport.switch_port(4500).await.unwrap();
        assert_eq!(transport.send(b"natt").await.unwrap(), 4);
        assert_eq!(transport.receive().await.unwrap(), b"natt-response");
        assert_eq!(
            transport.receive_packet().await.unwrap(),
            EpdgPacket::Ike(sample_ike())
        );
        assert_eq!(
            transport.receive_packet().await.unwrap(),
            EpdgPacket::Esp(vec![1, 0, 0, 0, 0, 0, 0, 1])
        );
        assert_eq!(
            transport.receive_packet().await.unwrap(),
            EpdgPacket::Keepalive
        );
        transport.send_ike(b"ike").await.unwrap();
        transport.send_esp(b"esp").await.unwrap();
        transport.send_keepalive().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn socks_mode_never_falls_back_to_direct_when_proxy_is_missing() {
        assert!(matches!(
            EpdgDatagramTransport::open(ProxyMode::Socks5UdpDomain, "epdg.invalid", 4500, None)
                .await,
            Err(DatagramTransportError::MissingProxy)
        ));
    }
}
