//! Strict selected-flow IP/TCP transform. Selected IMS traffic is protected or
//! dropped; unrelated traffic is returned explicitly for the tunnel router.
use crate::{
    security_esp::{EspError, Receiver, Sender},
    security_policy::{Flow, SecurityPolicy},
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq)]
pub enum TransportError {
    Packet,
    FlowMismatch,
    Esp(EspError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundDropReason {
    Malformed,
    FlowMismatch,
    Authentication,
    Replay,
    UnprotectedSelectedFlow,
    RetiredSecurityAssociation,
}

pub enum TransportDisposition {
    Protected(Zeroizing<Vec<u8>>),
    Passthrough(Zeroizing<Vec<u8>>),
    Drop(InboundDropReason),
}

impl TransportError {
    pub fn inbound_drop_reason(&self) -> Option<InboundDropReason> {
        match self {
            Self::Packet | Self::Esp(EspError::InvalidPacket) => Some(InboundDropReason::Malformed),
            Self::FlowMismatch | Self::Esp(EspError::InvalidSpi) => {
                Some(InboundDropReason::FlowMismatch)
            }
            Self::Esp(EspError::Authentication) => Some(InboundDropReason::Authentication),
            Self::Esp(EspError::Replay) => Some(InboundDropReason::Replay),
            Self::Esp(
                EspError::Exhausted
                | EspError::Entropy
                | EspError::InvalidKey
                | EspError::UnsupportedTransform,
            ) => None,
        }
    }
}
struct Channel {
    flow: Flow,
    tx: Sender,
    rx: Receiver,
}
pub struct SecurityTransport {
    local: IpAddr,
    remote: IpAddr,
    channels: [Channel; 2],
}

impl SecurityTransport {
    pub fn new(policy: SecurityPolicy) -> Result<Self, TransportError> {
        let make = |flow: Flow| -> Result<Channel, TransportError> {
            let tx = Sender::with_suite(
                flow.outbound_spi,
                policy.suite,
                policy.encryption_key.clone(),
                policy.integrity_key.clone(),
            )
            .map_err(TransportError::Esp)?;
            let rx = Receiver::with_suite(
                flow.inbound_spi,
                policy.suite,
                policy.encryption_key.clone(),
                policy.integrity_key.clone(),
            )
            .map_err(TransportError::Esp)?;
            Ok(Channel { flow, tx, rx })
        };
        Ok(Self {
            local: policy.local_ip,
            remote: policy.remote_ip,
            channels: [make(policy.client_flow)?, make(policy.server_flow)?],
        })
    }
    pub fn outbound(&mut self, packet: &[u8]) -> Result<Zeroizing<Vec<u8>>, TransportError> {
        let ip = parse(packet)?;
        if ip.source != self.local || ip.destination != self.remote || ip.protocol != 6 {
            return Err(TransportError::FlowMismatch);
        }
        let (source, destination) = ports(&packet[ip.header..])?;
        let channel = self
            .channels
            .iter_mut()
            .find(|c| c.flow.local_port == source && c.flow.remote_port == destination)
            .ok_or(TransportError::FlowMismatch)?;
        let esp = channel
            .tx
            .seal(&packet[ip.header..])
            .map_err(TransportError::Esp)?;
        replace(&packet[..ip.header], &esp, 50)
    }

    pub fn route_outbound(
        &mut self,
        packet: Zeroizing<Vec<u8>>,
    ) -> Result<TransportDisposition, TransportError> {
        let ip = parse(&packet)?;
        if ip.source != self.local || ip.destination != self.remote || ip.protocol != 6 {
            return Ok(TransportDisposition::Passthrough(packet));
        }
        let (source, destination) = ports(&packet[ip.header..])?;
        if !self.channels.iter().any(|channel| {
            channel.flow.local_port == source && channel.flow.remote_port == destination
        }) {
            return Ok(TransportDisposition::Passthrough(packet));
        }
        self.outbound(&packet).map(TransportDisposition::Protected)
    }

    pub fn route_inbound(
        &mut self,
        packet: Zeroizing<Vec<u8>>,
    ) -> Result<TransportDisposition, TransportError> {
        let ip = match parse(&packet) {
            Ok(ip) => ip,
            Err(error) => {
                return Ok(TransportDisposition::Drop(
                    error
                        .inbound_drop_reason()
                        .unwrap_or(InboundDropReason::Malformed),
                ));
            }
        };
        if ip.protocol == 50 {
            return match self.inbound(&packet) {
                Ok(packet) => Ok(TransportDisposition::Protected(packet)),
                Err(error) => match error.inbound_drop_reason() {
                    Some(reason) => Ok(TransportDisposition::Drop(reason)),
                    None => Err(error),
                },
            };
        }
        if ip.protocol == 6 && ip.source == self.remote && ip.destination == self.local {
            let (source, destination) = match ports(&packet[ip.header..]) {
                Ok(ports) => ports,
                Err(_) => {
                    return Ok(TransportDisposition::Drop(InboundDropReason::Malformed));
                }
            };
            let selected = self.channels.iter().any(|channel| {
                channel.flow.remote_port == source && channel.flow.local_port == destination
            });
            if selected {
                return Ok(TransportDisposition::Drop(
                    InboundDropReason::UnprotectedSelectedFlow,
                ));
            }
        }
        Ok(TransportDisposition::Passthrough(packet))
    }
    pub fn inbound(&mut self, packet: &[u8]) -> Result<Zeroizing<Vec<u8>>, TransportError> {
        let ip = parse(packet)?;
        if ip.source != self.remote || ip.destination != self.local || ip.protocol != 50 {
            return Err(TransportError::FlowMismatch);
        }
        let esp = &packet[ip.header..];
        let spi = u32::from_be_bytes(
            esp.get(..4)
                .ok_or(TransportError::Packet)?
                .try_into()
                .unwrap(),
        );
        let channel = self
            .channels
            .iter_mut()
            .find(|c| c.flow.inbound_spi == spi)
            .ok_or(TransportError::FlowMismatch)?;
        let plain = channel.rx.open(esp).map_err(TransportError::Esp)?;
        let (source, destination) = ports(&plain)?;
        if source != channel.flow.remote_port || destination != channel.flow.local_port {
            return Err(TransportError::FlowMismatch);
        }
        replace(&packet[..ip.header], &plain, 6)
    }
}
struct Ip {
    header: usize,
    source: IpAddr,
    destination: IpAddr,
    protocol: u8,
}

pub(crate) struct PacketMetadata {
    pub source: IpAddr,
    pub destination: IpAddr,
    pub protocol: u8,
    pub tcp_ports: Option<(u16, u16)>,
    pub esp_spi: Option<u32>,
}

pub(crate) fn packet_metadata(packet: &[u8]) -> Result<PacketMetadata, TransportError> {
    let ip = parse(packet)?;
    let tcp_ports = if ip.protocol == 6 {
        Some(ports(&packet[ip.header..])?)
    } else {
        None
    };
    let esp_spi = if ip.protocol == 50 {
        Some(u32::from_be_bytes(
            packet[ip.header..]
                .get(..4)
                .ok_or(TransportError::Packet)?
                .try_into()
                .unwrap(),
        ))
    } else {
        None
    };
    Ok(PacketMetadata {
        source: ip.source,
        destination: ip.destination,
        protocol: ip.protocol,
        tcp_ports,
        esp_spi,
    })
}
fn parse(packet: &[u8]) -> Result<Ip, TransportError> {
    match packet.first().map(|b| b >> 4) {
        Some(4) if packet.len() >= 20 => {
            let header = usize::from(packet[0] & 15) * 4;
            if header < 20
                || header > packet.len()
                || usize::from(u16::from_be_bytes([packet[2], packet[3]])) != packet.len()
                || u16::from_be_bytes([packet[6], packet[7]]) & 0xbfff != 0
                || checksum(&packet[..header]) != 0
            {
                return Err(TransportError::Packet);
            }
            Ok(Ip {
                header,
                source: Ipv4Addr::from(<[u8; 4]>::try_from(&packet[12..16]).unwrap()).into(),
                destination: Ipv4Addr::from(<[u8; 4]>::try_from(&packet[16..20]).unwrap()).into(),
                protocol: packet[9],
            })
        }
        Some(6)
            if packet.len() >= 40
                && usize::from(u16::from_be_bytes([packet[4], packet[5]])) + 40 == packet.len() =>
        {
            let protocol = packet[6];
            if matches!(protocol, 0 | 43 | 44 | 51 | 60 | 135 | 139 | 140) {
                return Err(TransportError::Packet);
            }
            Ok(Ip {
                header: 40,
                source: Ipv6Addr::from(<[u8; 16]>::try_from(&packet[8..24]).unwrap()).into(),
                destination: Ipv6Addr::from(<[u8; 16]>::try_from(&packet[24..40]).unwrap()).into(),
                protocol,
            })
        }
        _ => Err(TransportError::Packet),
    }
}
fn ports(tcp: &[u8]) -> Result<(u16, u16), TransportError> {
    if tcp.len() < 20 || tcp[12] >> 4 < 5 || usize::from(tcp[12] >> 4) * 4 > tcp.len() {
        return Err(TransportError::Packet);
    }
    Ok((
        u16::from_be_bytes([tcp[0], tcp[1]]),
        u16::from_be_bytes([tcp[2], tcp[3]]),
    ))
}
fn checksum(bytes: &[u8]) -> u16 {
    let (pairs, remainder) = bytes.as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    let mut sum: u32 = pairs
        .iter()
        .map(|pair| u32::from(u16::from_be_bytes([pair[0], pair[1]])))
        .sum();
    while sum >> 16 != 0 {
        sum = (sum & 65535) + (sum >> 16);
    }
    !(sum as u16)
}
fn replace(
    header: &[u8],
    payload: &[u8],
    protocol: u8,
) -> Result<Zeroizing<Vec<u8>>, TransportError> {
    let mut result = Zeroizing::new(header.to_vec());
    if header[0] >> 4 == 4 {
        let len =
            u16::try_from(header.len() + payload.len()).map_err(|_| TransportError::Packet)?;
        result[2..4].copy_from_slice(&len.to_be_bytes());
        result[9] = protocol;
        result[10] = 0;
        result[11] = 0;
        let sum = checksum(&result);
        result[10..12].copy_from_slice(&sum.to_be_bytes());
    } else {
        let len = u16::try_from(payload.len()).map_err(|_| TransportError::Packet)?;
        result[4..6].copy_from_slice(&len.to_be_bytes());
        result[6] = protocol;
    }
    result.extend_from_slice(payload);
    Ok(result)
}

#[cfg(test)]
pub(crate) fn build_ip_packet(
    source: IpAddr,
    destination: IpAddr,
    protocol: u8,
    payload: &[u8],
) -> Result<Zeroizing<Vec<u8>>, TransportError> {
    let header = match (source, destination) {
        (IpAddr::V4(source), IpAddr::V4(destination)) => {
            let mut header = vec![0; 20];
            header[0] = 0x45;
            header[8] = 64;
            header[12..16].copy_from_slice(&source.octets());
            header[16..20].copy_from_slice(&destination.octets());
            header
        }
        (IpAddr::V6(source), IpAddr::V6(destination)) => {
            let mut header = vec![0; 40];
            header[0] = 0x60;
            header[7] = 64;
            header[8..24].copy_from_slice(&source.octets());
            header[24..40].copy_from_slice(&destination.octets());
            header
        }
        _ => return Err(TransportError::Packet),
    };
    replace(&header, payload, protocol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        aka_exchange::SecurityKeys, security_offer::parse_security_server,
        security_policy::LocalEndpoints,
    };
    #[test]
    fn ipv4_ipv6_selected_flow_roundtrips_and_rejects_wrong_address() {
        for encryption in ["aes-cbc", "des-ede3-cbc", "null"] {
            for algorithm in ["hmac-sha-1-96", "hmac-md5-96"] {
                for (a, b) in [("10.0.0.2", "10.0.0.9"), ("fd00::2", "fd00::9")] {
                    let (a, b): (IpAddr, IpAddr) = (a.parse().unwrap(), b.parse().unwrap());
                    let make = |local, remote, c, s, rc, rs| {
                        let offer=parse_security_server(&format!("ipsec-3gpp;alg={algorithm};ealg={encryption};spi-c={rc};spi-s={rs};port-c=5050;port-s=5060")).unwrap().remove(0);
                        SecurityTransport::new(
                            SecurityPolicy::negotiated(
                                local,
                                remote,
                                LocalEndpoints {
                                    spi_c: c,
                                    spi_s: s,
                                    port_c: 5050,
                                    port_s: 5060,
                                },
                                &offer,
                                SecurityKeys {
                                    ck: Zeroizing::new([1; 16]),
                                    ik: Zeroizing::new([2; 16]),
                                },
                            )
                            .unwrap(),
                        )
                        .unwrap()
                    };
                    let mut one = make(a, b, 111, 222, 333, 444);
                    let mut two = make(b, a, 333, 444, 111, 222);
                    let mut header = match (a, b) {
                        (IpAddr::V4(a), IpAddr::V4(b)) => {
                            let mut h = vec![0; 20];
                            h[0] = 0x45;
                            h[8] = 64;
                            h[12..16].copy_from_slice(&a.octets());
                            h[16..20].copy_from_slice(&b.octets());
                            h
                        }
                        (IpAddr::V6(a), IpAddr::V6(b)) => {
                            let mut h = vec![0; 40];
                            h[0] = 0x60;
                            h[7] = 64;
                            h[8..24].copy_from_slice(&a.octets());
                            h[24..40].copy_from_slice(&b.octets());
                            h
                        }
                        _ => unreachable!(),
                    };
                    let mut tcp = vec![0; 20];
                    tcp[..2].copy_from_slice(&5050u16.to_be_bytes());
                    tcp[2..4].copy_from_slice(&5060u16.to_be_bytes());
                    tcp[12] = 0x50;
                    let original = replace(&header, &tcp, 6).unwrap();
                    if a.is_ipv6() {
                        let mut extension = vec![6, 0, 0, 0, 0, 0, 0, 0];
                        extension.extend_from_slice(&tcp);
                        let packet = replace(&header, &extension, 0).unwrap();
                        assert!(matches!(
                            one.route_outbound(packet),
                            Err(TransportError::Packet)
                        ));
                    }
                    let encrypted = one.outbound(&original).unwrap();
                    assert_eq!(two.inbound(&encrypted).unwrap(), original);
                    assert!(two.inbound(&encrypted).is_err());
                    assert!(one.inbound(&encrypted).is_err());
                    // Reverse SIP response uses the server flow at the peer.
                    if header.len() == 20 {
                        header[12..20].rotate_left(4);
                    } else {
                        header[8..40].rotate_left(16);
                    }
                    tcp[..4].rotate_left(2);
                    let reply = replace(&header, &tcp, 6).unwrap();
                    let truncated = replace(&header, &[0; 10], 6).unwrap();
                    assert!(matches!(
                        one.route_inbound(truncated),
                        Ok(TransportDisposition::Drop(InboundDropReason::Malformed))
                    ));
                    if a.is_ipv6() {
                        let mut extension = vec![6, 0, 0, 0, 0, 0, 0, 0];
                        extension.extend_from_slice(&tcp);
                        let packet = replace(&header, &extension, 0).unwrap();
                        assert!(matches!(
                            one.route_inbound(packet),
                            Ok(TransportDisposition::Drop(InboundDropReason::Malformed))
                        ));
                    }
                    let encrypted = two.outbound(&reply).unwrap();
                    assert_eq!(one.inbound(&encrypted).unwrap(), reply);

                    let mut udp = vec![0; 8];
                    udp[..2].copy_from_slice(&5050u16.to_be_bytes());
                    udp[2..4].copy_from_slice(&5060u16.to_be_bytes());
                    let plain_udp =
                        replace(&original[..if a.is_ipv4() { 20 } else { 40 }], &udp, 17).unwrap();
                    assert!(matches!(
                        one.route_outbound(plain_udp),
                        Ok(TransportDisposition::Passthrough(_))
                    ));
                    assert!(matches!(
                        one.route_inbound(reply.clone()),
                        Ok(TransportDisposition::Drop(
                            InboundDropReason::UnprotectedSelectedFlow
                        ))
                    ));

                    let next = two.outbound(&reply).unwrap();
                    let mut unknown_spi = next.clone();
                    let offset = if a.is_ipv4() { 20 } else { 40 };
                    unknown_spi[offset..offset + 4].copy_from_slice(&999u32.to_be_bytes());
                    assert!(matches!(
                        one.route_inbound(unknown_spi),
                        Ok(TransportDisposition::Drop(InboundDropReason::FlowMismatch))
                    ));
                    let mut tampered = next.clone();
                    *tampered.last_mut().unwrap() ^= 1;
                    assert!(matches!(
                        one.route_inbound(tampered),
                        Ok(TransportDisposition::Drop(
                            InboundDropReason::Authentication
                        ))
                    ));
                    assert!(matches!(
                        one.route_inbound(next.clone()),
                        Ok(TransportDisposition::Protected(_))
                    ));
                    assert!(matches!(
                        one.route_inbound(next),
                        Ok(TransportDisposition::Drop(InboundDropReason::Replay))
                    ));
                }
            }
        }
    }
}
