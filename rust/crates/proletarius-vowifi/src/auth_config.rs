//! First IKE_AUTH frames from effective carrier settings, without I/O.
use proletarius_ike::{PayloadFrame, decode_sa_proposals};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestedIpStack {
    Ipv4,
    Ipv6,
    Dual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthConfigError {
    Identity,
    Apn,
    ChildProposal,
}

/// Caller retains the selected identity (including custom carrier identity)
/// and proposal order. No silent fallback or merge with default proposals.
pub struct InitialAuthParameters<'a> {
    pub identity: &'a [u8],
    pub apn: &'a str,
    pub child_proposals: &'a [u8],
    pub ip_stack: RequestedIpStack,
    /// Stable, carrier-approved IMEI from the existing session identity store.
    /// An absent or malformed optional value emits no device notification.
    pub device_identity_imei: Option<&'a str>,
    /// Explicit because INITIAL_CONTACT can invalidate another active session
    /// for the same identity; do not enable merely because a reader was found.
    pub initial_contact: bool,
}

pub fn build_initial_auth_frames(
    config: InitialAuthParameters<'_>,
) -> Result<Vec<PayloadFrame>, AuthConfigError> {
    if config.identity.is_empty()
        || config.identity.len() > 2048
        || config.identity.iter().any(|b| b.is_ascii_control())
    {
        return Err(AuthConfigError::Identity);
    }
    if config.apn.is_empty()
        || config.apn.len() > 253
        || config.apn.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(AuthConfigError::Apn);
    }
    let offers =
        decode_sa_proposals(config.child_proposals).map_err(|_| AuthConfigError::ChildProposal)?;
    if offers
        .iter()
        .any(|p| p.protocol != 3 || p.spi.len() != 4 || p.spi == [0; 4] || p.spi != offers[0].spi)
    {
        return Err(AuthConfigError::ChildProposal);
    }
    let frame = |kind, body| PayloadFrame {
        kind,
        critical: false,
        body,
    };
    let mut idi = vec![3, 0, 0, 0];
    idi.extend_from_slice(config.identity);
    let mut idr = vec![2, 0, 0, 0];
    idr.extend_from_slice(config.apn.as_bytes());
    let mut cp = vec![1, 0, 0, 0];
    let mut attributes = |kind: u16, value: &[u8]| {
        cp.extend_from_slice(&kind.to_be_bytes());
        cp.extend_from_slice(&(value.len() as u16).to_be_bytes());
        cp.extend_from_slice(value);
    };
    let ipv4 = config.ip_stack != RequestedIpStack::Ipv6;
    let ipv6 = config.ip_stack != RequestedIpStack::Ipv4;
    if ipv4 {
        for kind in [1, 3, 20] {
            attributes(kind, &[]);
        }
    }
    if ipv6 {
        // TS 24.302 section 7.2.2.1: initial attach uses a zero-length
        // address request. Supplying an address signals a handover instead.
        attributes(8, &[]);
        for kind in [10, 21, 16390] {
            attributes(kind, &[]);
        }
    }
    let mut ts = vec![u8::from(ipv4) + u8::from(ipv6), 0, 0, 0];
    for (enabled, kind, len) in [(ipv4, 7, 4), (ipv6, 8, 16)] {
        if !enabled {
            continue;
        }
        ts.extend_from_slice(&[kind, 0]);
        ts.extend_from_slice(&((8 + 2 * len) as u16).to_be_bytes());
        ts.extend_from_slice(&[0, 0, 255, 255]);
        ts.extend(std::iter::repeat_n(0, len));
        ts.extend(std::iter::repeat_n(255, len));
    }
    let mut frames = vec![
        frame(35, idi),
        frame(36, idr),
        frame(47, cp),
        frame(33, config.child_proposals.to_vec()),
        frame(44, ts.clone()),
        frame(45, ts),
        frame(41, vec![0, 0, 0x40, 0x21]),
    ];
    if config.initial_contact {
        frames.push(frame(41, vec![0, 0, 0x40, 0]));
    }
    if let Some(imei) = config.device_identity_imei.and_then(encode_device_identity) {
        frames.push(frame(41, imei));
    }
    // Do not advertise MOBIKE, fragmentation, or session resumption before
    // the corresponding Rust control handlers are implemented and verified.
    Ok(frames)
}

/// 3GPP TS 24.302 8.2.9.2: Notify 41101, identity length 9,
/// identity type 1 and fifteen low-nibble-first TBCD digits. The deployed Go
/// encoder checks only ASCII digits and length; the session owner validates
/// the persisted/configured IMEI before it reaches this optional encoder.
fn encode_device_identity(imei: &str) -> Option<Vec<u8>> {
    let digits = imei.as_bytes();
    if digits.len() != 15 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut body = vec![0, 0, 0xa0, 0x8d, 0, 9, 1];
    for pair in digits.chunks(2) {
        let low = pair[0] - b'0';
        let high = pair.get(1).map_or(0x0f, |digit| *digit - b'0');
        body.push((high << 4) | low);
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requested_families_and_identity_are_preserved() {
        let proposal = [
            0, 0, 0, 24, 1, 3, 4, 1, 0, 0, 0, 1, 0, 0, 0, 12, 1, 0, 0, 20, 0x80, 14, 0, 128,
        ];
        for stack in [
            RequestedIpStack::Ipv4,
            RequestedIpStack::Ipv6,
            RequestedIpStack::Dual,
        ] {
            let frames = build_initial_auth_frames(InitialAuthParameters {
                identity: b"custom@example",
                apn: "ims",
                child_proposals: &proposal,
                ip_stack: stack,
                device_identity_imei: None,
                initial_contact: false,
            })
            .unwrap();
            assert_eq!(&frames[0].body[4..], b"custom@example");
            assert_eq!(&frames[1].body[4..], b"ims");
            assert_eq!(frames[3].body, proposal);
            let selectors = crate::validate_child_traffic_selectors(&frames, &frames).unwrap();
            assert_eq!(
                selectors.initiator.len(),
                if stack == RequestedIpStack::Dual {
                    2
                } else {
                    1
                }
            );
            assert_eq!(frames.iter().filter(|f| f.kind == 41).count(), 1);
            let mut kinds = vec![];
            let mut cp = &frames[2].body[4..];
            while !cp.is_empty() {
                kinds.push(u16::from_be_bytes([cp[0], cp[1]]));
                let n = u16::from_be_bytes([cp[2], cp[3]]) as usize;
                if matches!(kinds.last(), Some(1 | 8)) {
                    assert_eq!(n, 0, "initial attach must not indicate a handover address");
                }
                cp = &cp[4 + n..];
            }
            assert_eq!(kinds.contains(&1), stack != RequestedIpStack::Ipv6);
            assert_eq!(kinds.contains(&8), stack != RequestedIpStack::Ipv4);
            assert_eq!(kinds.contains(&20), stack != RequestedIpStack::Ipv6);
            assert_eq!(kinds.contains(&21), stack != RequestedIpStack::Ipv4);
        }
    }

    #[test]
    fn ipv6_initial_attach_configuration_wire_format() {
        let proposal = [
            0, 0, 0, 24, 1, 3, 4, 1, 0, 0, 0, 1, 0, 0, 0, 12, 1, 0, 0, 20, 0x80, 14, 0, 128,
        ];
        let frames = build_initial_auth_frames(InitialAuthParameters {
            identity: b"custom@example",
            apn: "ims",
            child_proposals: &proposal,
            ip_stack: RequestedIpStack::Ipv6,
            device_identity_imei: None,
            initial_contact: false,
        })
        .unwrap();
        assert_eq!(frames[2].kind, 47);
        assert_eq!(
            frames[2].body,
            [
                1, 0, 0, 0, 0, 8, 0, 0, 0, 10, 0, 0, 0, 21, 0, 0, 0x40, 6, 0, 0
            ]
        );
    }

    #[test]
    fn initial_auth_device_identity_matches_deployed_go_fixture() {
        let proposal = [
            0, 0, 0, 24, 1, 3, 4, 1, 0, 0, 0, 1, 0, 0, 0, 12, 1, 0, 0, 20, 0x80, 14, 0, 128,
        ];
        let frames = build_initial_auth_frames(InitialAuthParameters {
            identity: b"reader@ims.example",
            apn: "ims",
            child_proposals: &proposal,
            ip_stack: RequestedIpStack::Ipv4,
            device_identity_imei: Some("351234567890124"),
            initial_contact: true,
        })
        .unwrap();
        let device_notifies: Vec<_> = frames
            .iter()
            .filter(|frame| frame.kind == 41 && frame.body.get(2..4) == Some(&[0xa0, 0x8d]))
            .collect();
        assert_eq!(device_notifies.len(), 1);
        // Go pkg/ikev2/device_identity_test.go: protocol=0, SPI size=0,
        // notify=41101, length=9, type=IMEI, TBCD digits.
        assert_eq!(
            device_notifies[0].body,
            [
                0, 0, 0xa0, 0x8d, 0, 9, 1, 0x53, 0x21, 0x43, 0x65, 0x87, 0x09, 0x21, 0xf4
            ]
        );
        assert_eq!(frames.iter().filter(|frame| frame.kind == 41).count(), 3);
    }

    #[test]
    fn optional_device_identity_omits_missing_or_malformed_imei() {
        let proposal = [
            0, 0, 0, 24, 1, 3, 4, 1, 0, 0, 0, 1, 0, 0, 0, 12, 1, 0, 0, 20, 0x80, 14, 0, 128,
        ];
        for imei in [
            None,
            Some(""),
            Some("35123456789012"),
            Some("3512345678901245"),
            Some("35123456789012x"),
            Some("３５１２３４５６７８９０１２４"),
        ] {
            let frames = build_initial_auth_frames(InitialAuthParameters {
                identity: b"reader@ims.example",
                apn: "ims",
                child_proposals: &proposal,
                ip_stack: RequestedIpStack::Ipv4,
                device_identity_imei: imei,
                initial_contact: false,
            })
            .unwrap();
            assert_eq!(frames.iter().filter(|frame| frame.kind == 41).count(), 1);
        }
    }
}
