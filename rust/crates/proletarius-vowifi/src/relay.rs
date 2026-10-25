use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket, lookup_host};
use tokio::time::timeout;

use crate::{
    NO_AUTH_GREETING, SocksCodecError, SocksDatagram, SocksTarget, UDP_ASSOCIATE_REQUEST,
    decode_udp_datagram, encode_udp_domain, parse_no_auth_response, parse_udp_associate_response,
};

const ASSOCIATE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_UDP_DATAGRAM: usize = 65_535;

#[derive(Debug)]
pub enum RelayError {
    Io(io::Error),
    Codec(SocksCodecError),
    AssociateTimeout,
    RelayAddressUnavailable,
    ControlClosed,
}

impl fmt::Display for RelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "SOCKS5 relay I/O: {error}"),
            Self::Codec(error) => error.fmt(formatter),
            Self::AssociateTimeout => formatter.write_str("SOCKS5 UDP ASSOCIATE timed out"),
            Self::RelayAddressUnavailable => {
                formatter.write_str("SOCKS5 proxy returned no usable UDP relay address")
            }
            Self::ControlClosed => {
                formatter.write_str("SOCKS5 UDP association control connection closed")
            }
        }
    }
}

impl std::error::Error for RelayError {}

impl From<io::Error> for RelayError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<SocksCodecError> for RelayError {
    fn from(value: SocksCodecError) -> Self {
        Self::Codec(value)
    }
}

/// Owns the TCP control connection and connected UDP relay socket.
///
/// ePDG destinations are sent only with ATYP=DOMAIN. This type never resolves
/// the ePDG hostname locally and has no direct-UDP fallback.
pub struct Socks5UdpRelay {
    _control: TcpStream,
    udp: UdpSocket,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Socks5Probe {
    pub handshake_latency_ms: u64,
    pub udp_associate: bool,
}

/// Probes the configured SOCKS5 endpoint without resolving or contacting an
/// ePDG. The UDP result is based on a real UDP ASSOCIATE negotiation, not on
/// the TCP port merely accepting a connection.
pub async fn probe_socks5(proxy: SocketAddr) -> Result<Socks5Probe, RelayError> {
    let started = Instant::now();
    timeout(ASSOCIATE_TIMEOUT, probe_no_auth(proxy))
        .await
        .map_err(|_| RelayError::AssociateTimeout)??;
    let handshake_latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let udp_associate = Socks5UdpRelay::associate(proxy).await.is_ok();
    Ok(Socks5Probe {
        handshake_latency_ms,
        udp_associate,
    })
}

async fn probe_no_auth(proxy: SocketAddr) -> Result<(), RelayError> {
    let mut control = TcpStream::connect(proxy).await?;
    control.write_all(&NO_AUTH_GREETING).await?;
    let mut method = [0_u8; 2];
    control.read_exact(&mut method).await?;
    parse_no_auth_response(&method)?;
    Ok(())
}

impl Socks5UdpRelay {
    pub async fn associate(proxy: SocketAddr) -> Result<Self, RelayError> {
        Self::associate_with_local_port(proxy, 0).await
    }
    pub async fn associate_with_local_port(
        proxy: SocketAddr,
        local_port: u16,
    ) -> Result<Self, RelayError> {
        timeout(ASSOCIATE_TIMEOUT, Self::associate_inner(proxy, local_port))
            .await
            .map_err(|_| RelayError::AssociateTimeout)?
    }

    async fn associate_inner(proxy: SocketAddr, local_port: u16) -> Result<Self, RelayError> {
        let mut control = TcpStream::connect(proxy).await?;
        control.write_all(&NO_AUTH_GREETING).await?;
        let mut method = [0_u8; 2];
        control.read_exact(&mut method).await?;
        parse_no_auth_response(&method)?;

        control.write_all(&UDP_ASSOCIATE_REQUEST).await?;
        let response = read_associate_response(&mut control).await?;
        let (target, mut port) = parse_udp_associate_response(&response)?;
        if port == 0 {
            port = proxy.port();
        }
        let relay = resolve_relay_target(target, port, proxy).await?;
        let bind_address = match relay {
            SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), local_port),
            SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), local_port),
        };
        let udp = UdpSocket::bind(bind_address).await?;
        udp.connect(relay).await?;
        Ok(Self {
            _control: control,
            udp,
        })
    }

    pub async fn send_domain(
        &self,
        epdg_host: &str,
        port: u16,
        payload: &[u8],
    ) -> Result<(), RelayError> {
        let packet = encode_udp_domain(epdg_host, port, payload)?;
        self.udp.send(&packet).await?;
        Ok(())
    }

    pub async fn receive(&self) -> Result<SocksDatagram, RelayError> {
        let mut packet = vec![0_u8; MAX_UDP_DATAGRAM];
        let length = loop {
            tokio::select! {
                result = self.udp.recv(&mut packet) => break result?,
                result = self._control.readable() => {
                    result?;
                    let mut byte = [0_u8; 1];
                    match self._control.try_read(&mut byte) {
                        Ok(0) => return Err(RelayError::ControlClosed),
                        Ok(_) => return Err(RelayError::Io(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected SOCKS5 control data after UDP association",
                        ))),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                        Err(error) => return Err(RelayError::Io(error)),
                    }
                }
            }
        };
        packet.truncate(length);
        decode_udp_datagram(&packet).map_err(Into::into)
    }
}

async fn read_associate_response(control: &mut TcpStream) -> Result<Vec<u8>, RelayError> {
    let mut header = [0_u8; 4];
    control.read_exact(&mut header).await?;
    let mut response = header.to_vec();
    match header[3] {
        0x01 => read_append(control, &mut response, 6).await?,
        0x04 => read_append(control, &mut response, 18).await?,
        0x03 => {
            let mut length = [0_u8; 1];
            control.read_exact(&mut length).await?;
            response.push(length[0]);
            read_append(control, &mut response, usize::from(length[0]) + 2).await?;
        }
        _ => return Err(SocksCodecError::AddressTypeUnsupported(header[3]).into()),
    }
    Ok(response)
}

async fn read_append(
    control: &mut TcpStream,
    response: &mut Vec<u8>,
    length: usize,
) -> Result<(), io::Error> {
    let start = response.len();
    response.resize(start + length, 0);
    control.read_exact(&mut response[start..]).await?;
    Ok(())
}

async fn resolve_relay_target(
    target: SocksTarget,
    port: u16,
    proxy: SocketAddr,
) -> Result<SocketAddr, RelayError> {
    match target {
        SocksTarget::Ip(address) => {
            let address = if address.is_unspecified() {
                proxy.ip()
            } else {
                address
            };
            Ok(SocketAddr::new(address, port))
        }
        SocksTarget::Domain(domain) => lookup_host((domain.as_str(), port))
            .await?
            .find(|candidate| candidate.is_ipv4() == proxy.is_ipv4())
            .ok_or(RelayError::RelayAddressUnavailable),
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::{TcpListener, UdpSocket};

    use super::*;

    #[tokio::test]
    async fn associate_sends_epdg_as_domain_and_receives_response() {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tcp_address = tcp.local_addr().unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let udp_address = udp.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = tcp.accept().await.unwrap();
            let mut greeting = [0_u8; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, NO_AUTH_GREETING);
            stream.write_all(&[5, 0]).await.unwrap();
            let mut request = [0_u8; 10];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(request, UDP_ASSOCIATE_REQUEST);
            let mut response = vec![5, 0, 0, 1, 127, 0, 0, 1];
            response.extend_from_slice(&udp_address.port().to_be_bytes());
            stream.write_all(&response).await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        let relay = Socks5UdpRelay::associate(tcp_address).await.unwrap();
        relay
            .send_domain("epdg.example", 4500, b"ike")
            .await
            .unwrap();
        let mut packet = [0_u8; 256];
        let (length, sender) = udp.recv_from(&mut packet).await.unwrap();
        let decoded = decode_udp_datagram(&packet[..length]).unwrap();
        assert_eq!(decoded.target, SocksTarget::Domain("epdg.example".into()));
        assert_eq!(decoded.port, 4500);
        assert_eq!(decoded.payload, b"ike");
        udp.send_to(&packet[..length], sender).await.unwrap();
        assert_eq!(relay.receive().await.unwrap(), decoded);
        server.abort();
    }

    #[tokio::test]
    async fn association_rejection_is_not_retried_as_direct_udp() {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = tcp.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = tcp.accept().await.unwrap();
            let mut greeting = [0_u8; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            stream.write_all(&[5, 0]).await.unwrap();
            let mut request = [0_u8; 10];
            stream.read_exact(&mut request).await.unwrap();
            stream
                .write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });
        let error = Socks5UdpRelay::associate(address).await.err().unwrap();
        assert!(matches!(
            error,
            RelayError::Codec(SocksCodecError::AssociateRejected(7))
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn closed_control_connection_ends_udp_association() {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = tcp.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = tcp.accept().await.unwrap();
            let mut greeting = [0_u8; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            stream.write_all(&[5, 0]).await.unwrap();
            let mut request = [0_u8; 10];
            stream.read_exact(&mut request).await.unwrap();
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0x12, 0x34])
                .await
                .unwrap();
        });
        let relay = Socks5UdpRelay::associate(address).await.unwrap();
        server.await.unwrap();
        let result = timeout(Duration::from_secs(1), relay.receive())
            .await
            .unwrap();
        assert!(matches!(result, Err(RelayError::ControlClosed)));
    }

    #[tokio::test]
    async fn probe_requires_a_real_udp_associate_response() {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = tcp.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for connection in 0..2 {
                let (mut stream, _) = tcp.accept().await.unwrap();
                let mut greeting = [0_u8; 3];
                stream.read_exact(&mut greeting).await.unwrap();
                stream.write_all(&[5, 0]).await.unwrap();
                if connection == 1 {
                    let mut request = [0_u8; 10];
                    stream.read_exact(&mut request).await.unwrap();
                    stream
                        .write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0])
                        .await
                        .unwrap();
                }
            }
        });
        let result = probe_socks5(address).await.unwrap();
        assert!(!result.udp_associate);
        server.await.unwrap();
    }
}
