//! Decodes authenticated CFG_REPLY only; never changes host DNS or routes.
use crate::TrafficSelector;
use proletarius_ike::PayloadFrame;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TunnelConfiguration {
    pub ipv4_addresses: Vec<Ipv4Addr>,
    pub ipv6_addresses: Vec<(Ipv6Addr, u8)>,
    pub dns_servers: Vec<IpAddr>,
    pub pcscf_servers: Vec<IpAddr>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TunnelConfigError {
    MissingOrDuplicate,
    Malformed,
    MissingAddress,
    AddressOutsideSelectors,
}
impl std::fmt::Display for TunnelConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MissingOrDuplicate => "missing or duplicate IKE configuration reply",
            Self::Malformed => "malformed IKE configuration reply",
            Self::MissingAddress => "IKE configuration did not assign an internal address",
            Self::AddressOutsideSelectors => "assigned IKE address is outside negotiated TSi",
        })
    }
}
impl std::error::Error for TunnelConfigError {}

pub fn parse_tunnel_configuration(
    frames: &[PayloadFrame],
    required: bool,
    tsi: &[TrafficSelector],
) -> Result<Option<TunnelConfiguration>, TunnelConfigError> {
    use TunnelConfigError::*;
    let mut payloads = frames.iter().filter(|f| f.kind == 47);
    let Some(frame) = payloads.next() else {
        return if required {
            Err(MissingOrDuplicate)
        } else {
            Ok(None)
        };
    };
    if payloads.next().is_some() {
        return Err(MissingOrDuplicate);
    }
    let body = &frame.body;
    if body.len() < 4 || body[0] != 2 || body.len() > 65535 {
        return Err(Malformed);
    }
    let mut config = TunnelConfiguration::default();
    let mut offset = 4;
    while offset < body.len() {
        let header = body.get(offset..offset + 4).ok_or(Malformed)?;
        let kind = u16::from_be_bytes([header[0], header[1]]) & 0x7fff;
        let size = usize::from(u16::from_be_bytes([header[2], header[3]]));
        offset += 4;
        let value = body.get(offset..offset + size).ok_or(Malformed)?;
        offset += size;
        match kind {
            1 | 3 | 20 => {
                let ip = Ipv4Addr::from(<[u8; 4]>::try_from(value).map_err(|_| Malformed)?);
                // Some ePDGs return a zero DNS/P-CSCF placeholder alongside
                // usable servers. Ignore only that optional placeholder;
                // the assigned internal address must still be valid.
                if kind != 1 && ip.is_unspecified() {
                    continue;
                }
                if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
                    return Err(Malformed);
                }
                match kind {
                    1 => config.ipv4_addresses.push(ip),
                    3 => config.dns_servers.push(ip.into()),
                    _ => config.pcscf_servers.push(ip.into()),
                }
            }
            8 => {
                if value.len() != 17 || value[16] > 128 {
                    return Err(Malformed);
                }
                let ip = Ipv6Addr::from(<[u8; 16]>::try_from(&value[..16]).unwrap());
                if ip.is_unspecified() || ip.is_multicast() {
                    return Err(Malformed);
                }
                config.ipv6_addresses.push((ip, value[16]));
            }
            10 | 21 | 16390 => {
                let ip = Ipv6Addr::from(<[u8; 16]>::try_from(value).map_err(|_| Malformed)?);
                if ip.is_unspecified() || ip.is_multicast() {
                    return Err(Malformed);
                }
                if kind == 10 {
                    config.dns_servers.push(ip.into());
                } else {
                    config.pcscf_servers.push(ip.into());
                }
            }
            _ => {} // Unknown configuration attributes are not host settings.
        }
    }
    if config.ipv4_addresses.is_empty() && config.ipv6_addresses.is_empty() {
        return Err(MissingAddress);
    }
    let addresses = config
        .ipv4_addresses
        .iter()
        .copied()
        .map(IpAddr::V4)
        .chain(config.ipv6_addresses.iter().map(|(ip, _)| IpAddr::V6(*ip)));
    for ip in addresses {
        if !tsi.iter().any(|s| {
            s.start_address.is_ipv4() == ip.is_ipv4()
                && s.start_address <= ip
                && s.end_address >= ip
        }) {
            return Err(AddressOutsideSelectors);
        }
    }
    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ipv6_requires_valid_prefix_and_preserves_pcscf() {
        let ip: Ipv6Addr = "2001:db8::2".parse().unwrap();
        let tsi = vec![TrafficSelector {
            protocol: 0,
            start_port: 0,
            end_port: 65535,
            start_address: Ipv6Addr::UNSPECIFIED.into(),
            end_address: Ipv6Addr::from([255; 16]).into(),
        }];
        let mut body = vec![2, 0, 0, 0, 0, 8, 0, 17];
        body.extend_from_slice(&ip.octets());
        body.push(64);
        body.extend_from_slice(&[0, 21, 0, 16]);
        body.extend_from_slice(&ip.octets());
        let mut frame = PayloadFrame {
            kind: 47,
            critical: false,
            body,
        };
        let config = parse_tunnel_configuration(std::slice::from_ref(&frame), true, &tsi)
            .unwrap()
            .unwrap();
        assert_eq!(config.ipv6_addresses, vec![(ip, 64)]);
        assert_eq!(config.pcscf_servers, vec![IpAddr::V6(ip)]);
        frame.body[24] = 129;
        assert!(parse_tunnel_configuration(&[frame], true, &tsi).is_err());
    }
    #[test]
    fn checks_configuration_addresses_dns_pcscf_and_lengths() {
        let tsi = vec![TrafficSelector {
            protocol: 0,
            start_port: 0,
            end_port: 65535,
            start_address: "10.0.0.0".parse().unwrap(),
            end_address: "10.255.255.255".parse().unwrap(),
        }];
        let body = vec![
            2, 0, 0, 0, 0, 1, 0, 4, 10, 0, 0, 2, 0, 3, 0, 4, 10, 0, 0, 53, 0, 20, 0, 4, 10, 0, 0, 9,
        ];
        let frame = PayloadFrame {
            kind: 47,
            critical: false,
            body,
        };
        let config = parse_tunnel_configuration(std::slice::from_ref(&frame), true, &tsi)
            .unwrap()
            .unwrap();
        assert_eq!(config.ipv4_addresses[0], Ipv4Addr::new(10, 0, 0, 2));
        assert_eq!(config.dns_servers.len(), 1);
        assert_eq!(config.pcscf_servers.len(), 1);
        assert!(parse_tunnel_configuration(&[], true, &tsi).is_err());
        assert!(
            parse_tunnel_configuration(&[], false, &tsi)
                .unwrap()
                .is_none()
        );
        assert!(parse_tunnel_configuration(&[frame.clone(), frame.clone()], true, &tsi).is_err());
        assert!(parse_tunnel_configuration(std::slice::from_ref(&frame), true, &[]).is_err());
        let mut truncated = frame;
        truncated.body.pop();
        assert!(parse_tunnel_configuration(&[truncated], true, &tsi).is_err());
    }
    #[test]
    fn zero_optional_ipv4_server_is_ignored_but_zero_internal_address_is_rejected() {
        let tsi = vec![TrafficSelector {
            protocol: 0,
            start_port: 0,
            end_port: 65535,
            start_address: "10.0.0.0".parse().unwrap(),
            end_address: "10.255.255.255".parse().unwrap(),
        }];
        let mut body = vec![
            2, 0, 0, 0, 0, 1, 0, 4, 10, 0, 0, 2, 0, 3, 0, 4, 0, 0, 0, 0, 0, 20, 0, 4, 0, 0, 0, 0,
            0, 20, 0, 4, 10, 0, 0, 9,
        ];
        let mut frame = PayloadFrame {
            kind: 47,
            critical: false,
            body: body.clone(),
        };
        let config = parse_tunnel_configuration(&[frame.clone()], true, &tsi)
            .unwrap()
            .unwrap();
        assert_eq!(config.ipv4_addresses, vec![Ipv4Addr::new(10, 0, 0, 2)]);
        assert!(config.dns_servers.is_empty());
        assert_eq!(
            config.pcscf_servers,
            vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9))]
        );
        body[8..12].fill(0);
        frame.body = body;
        assert_eq!(
            parse_tunnel_configuration(&[frame], true, &tsi),
            Err(TunnelConfigError::Malformed)
        );
    }
}
