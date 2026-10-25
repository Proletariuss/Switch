use proletarius_ike::PayloadFrame;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrafficSelector {
    pub protocol: u8,
    pub start_port: u16,
    pub end_port: u16,
    pub start_address: IpAddr,
    pub end_address: IpAddr,
}

#[derive(Debug)]
pub struct ChildTrafficSelectors {
    pub initiator: Vec<TrafficSelector>,
    pub responder: Vec<TrafficSelector>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrafficSelectorError {
    MissingOrDuplicate,
    Malformed,
    UnsupportedType,
    ExpandedRange,
}
impl std::fmt::Display for TrafficSelectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MissingOrDuplicate => "IKE_AUTH requires one TSi and one TSr",
            Self::Malformed => "malformed IKE traffic selector",
            Self::UnsupportedType => "unsupported IKE traffic selector type",
            Self::ExpandedRange => "IKE peer traffic selector exceeds the requested range",
        })
    }
}
impl std::error::Error for TrafficSelectorError {}

fn decode(body: &[u8]) -> Result<Vec<TrafficSelector>, TrafficSelectorError> {
    use TrafficSelectorError::*;
    if body.len() < 4 || body[0] == 0 {
        return Err(Malformed);
    }
    let mut offset = 4;
    let mut result = Vec::with_capacity(usize::from(body[0]));
    for _ in 0..body[0] {
        let header = body.get(offset..offset + 8).ok_or(Malformed)?;
        let width = match header[0] {
            7 => 4,
            8 => 16,
            _ => return Err(UnsupportedType),
        };
        let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
        if length != 8 + 2 * width {
            return Err(Malformed);
        }
        let bytes = body.get(offset..offset + length).ok_or(Malformed)?;
        let address = |slice: &[u8]| -> IpAddr {
            if width == 4 {
                Ipv4Addr::from(<[u8; 4]>::try_from(slice).unwrap()).into()
            } else {
                Ipv6Addr::from(<[u8; 16]>::try_from(slice).unwrap()).into()
            }
        };
        let entry = TrafficSelector {
            protocol: header[1],
            start_port: u16::from_be_bytes([header[4], header[5]]),
            end_port: u16::from_be_bytes([header[6], header[7]]),
            start_address: address(&bytes[8..8 + width]),
            end_address: address(&bytes[8 + width..]),
        };
        if entry.start_port > entry.end_port || entry.start_address > entry.end_address {
            return Err(Malformed);
        }
        result.push(entry);
        offset += length;
    }
    if offset != body.len() {
        return Err(Malformed);
    }
    Ok(result)
}

pub fn validate_child_traffic_selectors(
    offered: &[PayloadFrame],
    selected: &[PayloadFrame],
) -> Result<ChildTrafficSelectors, TrafficSelectorError> {
    let extract = |frames: &[PayloadFrame], kind| {
        let mut matches = frames.iter().filter(|f| f.kind == kind);
        let frame = matches
            .next()
            .ok_or(TrafficSelectorError::MissingOrDuplicate)?;
        if matches.next().is_some() {
            return Err(TrafficSelectorError::MissingOrDuplicate);
        }
        decode(&frame.body)
    };
    let validate = |kind| {
        let offers = extract(offered, kind)?;
        let choices = extract(selected, kind)?;
        for choice in &choices {
            if !offers.iter().any(|offer| {
                offer.start_address.is_ipv4() == choice.start_address.is_ipv4()
                    && (offer.protocol == 0 || offer.protocol == choice.protocol)
                    && offer.start_port <= choice.start_port
                    && offer.end_port >= choice.end_port
                    && offer.start_address <= choice.start_address
                    && offer.end_address >= choice.end_address
            }) {
                return Err(TrafficSelectorError::ExpandedRange);
            }
        }
        Ok(choices)
    };
    Ok(ChildTrafficSelectors {
        initiator: validate(44)?,
        responder: validate(45)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frames(ipv6: bool, protocol: u8, start: u8, end: u8) -> Vec<PayloadFrame> {
        let width = if ipv6 { 16 } else { 4 };
        let mut body = vec![
            1,
            0,
            0,
            0,
            if ipv6 { 8 } else { 7 },
            protocol,
            0,
            (8 + 2 * width) as u8,
            0,
            0,
            255,
            255,
        ];
        body.extend(vec![start; width]);
        body.extend(vec![end; width]);
        [44, 45]
            .into_iter()
            .map(|kind| PayloadFrame {
                kind,
                critical: false,
                body: body.clone(),
            })
            .collect()
    }
    #[test]
    fn narrowing_checks_both_families_protocols_and_ranges() {
        for ipv6 in [false, true] {
            let offers = frames(ipv6, 0, 0, 255);
            let choices = frames(ipv6, 17, 10, 20);
            assert!(validate_child_traffic_selectors(&offers, &choices).is_ok());
            assert_eq!(
                validate_child_traffic_selectors(&choices, &offers).unwrap_err(),
                TrafficSelectorError::ExpandedRange
            );
            assert!(validate_child_traffic_selectors(&choices, &frames(ipv6, 6, 10, 20)).is_err());
            assert!(validate_child_traffic_selectors(&offers, &frames(!ipv6, 0, 10, 20)).is_err());
            let mut bad = choices.clone();
            bad[0].body.pop();
            assert!(validate_child_traffic_selectors(&offers, &bad).is_err());
            assert!(validate_child_traffic_selectors(&offers, &choices[..1]).is_err());
            let mut ports = choices.clone();
            ports[0].body[10..12].copy_from_slice(&5060_u16.to_be_bytes());
            assert!(validate_child_traffic_selectors(&offers, &ports).is_ok());
            assert!(validate_child_traffic_selectors(&ports, &choices).is_err());
            let mut duplicate = choices.clone();
            duplicate.push(choices[0].clone());
            assert!(validate_child_traffic_selectors(&offers, &duplicate).is_err());
        }
    }
}
