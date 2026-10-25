//! Authenticated IKE SA rekey material. The old SA remains authoritative until
//! its exact DELETE outcome is confirmed; CHILD SA counters are never reset.
use crate::{IkeAuthError, sa_init::ModpKeyPair};
use proletarius_ike::{IkeSaKeyMaterial, PayloadFrame, SaProposal, decode_sa_proposals};
use zeroize::Zeroizing;
pub(crate) struct Attempt {
    pub frames: Vec<PayloadFrame>,
    pub offer: SaProposal,
    pub nonce: Zeroizing<Vec<u8>>,
    pub dh: ModpKeyPair,
}
pub(crate) struct Material {
    pub spi_i: u64,
    pub spi_r: u64,
    pub keys: IkeSaKeyMaterial,
    pub offer: Vec<u8>,
}
fn unique(frames: &[PayloadFrame], kind: u8) -> Result<&PayloadFrame, IkeAuthError> {
    let mut found = frames.iter().filter(|v| v.kind == kind);
    let value = found.next().ok_or(IkeAuthError::UnexpectedPacket)?;
    if found.next().is_some() {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    Ok(value)
}
fn frame(kind: u8, body: Vec<u8>) -> PayloadFrame {
    PayloadFrame {
        kind,
        critical: false,
        body,
    }
}
pub(crate) fn begin(base: &[u8], old_spi: u64) -> Result<Attempt, IkeAuthError> {
    let mut offers = decode_sa_proposals(base).map_err(|_| IkeAuthError::UnexpectedPacket)?;
    if offers.len() != 1 || offers[0].protocol != 1 {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let mut random = [0; 40];
    getrandom::fill(&mut random).map_err(|_| IkeAuthError::UnexpectedPacket)?;
    let spi = u64::from_be_bytes(
        random[..8]
            .try_into()
            .map_err(|_| IkeAuthError::UnexpectedPacket)?,
    );
    if spi == 0 || spi == old_spi {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let mut offer = offers.remove(0);
    offer.spi = random[..8].to_vec();
    let group = offer
        .transforms
        .iter()
        .find(|v| v.kind == 4)
        .ok_or(IkeAuthError::UnexpectedPacket)?
        .id;
    let dh = ModpKeyPair::generate(group).map_err(|_| IkeAuthError::UnexpectedPacket)?;
    let mut ke = group.to_be_bytes().to_vec();
    ke.extend([0, 0]);
    ke.extend_from_slice(dh.public_key());
    let nonce = Zeroizing::new(random[8..].to_vec());
    let frames = vec![
        frame(33, crate::child_rekey::encode(&[offer.clone()])?),
        frame(40, nonce.to_vec()),
        frame(34, ke),
    ];
    Ok(Attempt {
        frames,
        offer,
        nonce,
        dh,
    })
}
fn choice(
    attempt: &Attempt,
    response: &[PayloadFrame],
    old_peer_spi: u64,
) -> Result<SaProposal, IkeAuthError> {
    let mut values = decode_sa_proposals(&unique(response, 33)?.body)
        .map_err(|_| IkeAuthError::UnexpectedPacket)?;
    if values.len() != 1 {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let selected = values.remove(0);
    if selected.protocol != 1
        || selected.number != attempt.offer.number
        || selected.transforms != attempt.offer.transforms
        || selected.spi.len() != 8
    {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let spi = u64::from_be_bytes(
        selected
            .spi
            .as_slice()
            .try_into()
            .map_err(|_| IkeAuthError::UnexpectedPacket)?,
    );
    if spi == 0 || spi == old_peer_spi {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    Ok(selected)
}
pub(crate) fn finish(
    attempt: &Attempt,
    response: &[PayloadFrame],
    old_sk_d: &[u8],
    old_peer_spi: u64,
    enc_len: usize,
    integ_len: usize,
) -> Result<Material, IkeAuthError> {
    let selected = choice(attempt, response, old_peer_spi)?;
    let nr = &unique(response, 40)?.body;
    if !(16..=256).contains(&nr.len()) || !(16..=256).contains(&attempt.nonce.len()) {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let ke = &unique(response, 34)?.body;
    let group = attempt
        .offer
        .transforms
        .iter()
        .find(|v| v.kind == 4)
        .ok_or(IkeAuthError::UnexpectedPacket)?
        .id;
    if ke.len() < 5 || ke[..2] != group.to_be_bytes() || ke[2..4] != [0, 0] {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let shared = attempt
        .dh
        .shared_secret(&ke[4..])
        .map_err(|_| IkeAuthError::UnexpectedPacket)?;
    let spi_i = u64::from_be_bytes(
        attempt
            .offer
            .spi
            .as_slice()
            .try_into()
            .map_err(|_| IkeAuthError::UnexpectedPacket)?,
    );
    let spi_r = u64::from_be_bytes(
        selected
            .spi
            .as_slice()
            .try_into()
            .map_err(|_| IkeAuthError::UnexpectedPacket)?,
    );
    let prf = selected
        .transforms
        .iter()
        .find(|v| v.kind == 2)
        .ok_or(IkeAuthError::UnexpectedPacket)?
        .id;
    let keys = proletarius_ike::derive_rekey_ike_sa_key_material(
        prf,
        old_sk_d,
        &attempt.nonce,
        nr,
        &shared,
        spi_i,
        spi_r,
        enc_len,
        integ_len,
    )
    .map_err(IkeAuthError::KeyDerivation)?;
    Ok(Material {
        spi_i,
        spi_r,
        keys,
        offer: crate::child_rekey::encode(&[selected])?,
    })
}
pub(crate) fn respond(
    base: &[u8],
    request: &[PayloadFrame],
    old_sk_d: &[u8],
    old_local_spi: u64,
    old_peer_spi: u64,
    enc_len: usize,
    integ_len: usize,
) -> Result<(Vec<PayloadFrame>, Material), IkeAuthError> {
    let mut attempt = begin(base, old_local_spi)?;
    // Preserve the peer's proposal number, while requiring our current exact suite.
    let requested = decode_sa_proposals(&unique(request, 33)?.body)
        .map_err(|_| IkeAuthError::UnexpectedPacket)?;
    if requested.len() != 1 {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    attempt.offer.number = requested[0].number;
    let selected = choice(&attempt, request, old_peer_spi)?;
    let peer_nonce = Zeroizing::new(unique(request, 40)?.body.clone());
    let our_nonce = std::mem::replace(&mut attempt.nonce, peer_nonce);
    let our_spi = attempt.offer.spi.clone();
    attempt.offer.spi = selected.spi.clone();
    let mut normalized = request.to_vec();
    let mut local_selected = selected;
    local_selected.spi = our_spi;
    normalized
        .iter_mut()
        .find(|v| v.kind == 33)
        .ok_or(IkeAuthError::UnexpectedPacket)?
        .body = crate::child_rekey::encode(&[local_selected.clone()])?;
    normalized
        .iter_mut()
        .find(|v| v.kind == 40)
        .ok_or(IkeAuthError::UnexpectedPacket)?
        .body = our_nonce.to_vec();
    let material = finish(
        &attempt,
        &normalized,
        old_sk_d,
        old_local_spi,
        enc_len,
        integ_len,
    )?;
    let response = vec![
        frame(33, crate::child_rekey::encode(&[local_selected])?),
        frame(40, our_nonce.to_vec()),
        attempt.frames[2].clone(),
    ];
    Ok((response, material))
}

/// Go records the last authenticated RFC4478 lifetime (positive values enable
/// dynamic 80%/87.5% whole-second cadence); absence is not a zero lifetime.
pub(crate) fn auth_lifetime(frames: &[PayloadFrame]) -> Result<Option<u32>, IkeAuthError> {
    let mut value = None;
    for frame in frames.iter().filter(|v| v.kind == 41) {
        if frame.body.len() < 4 {
            return Err(IkeAuthError::MalformedNotify);
        }
        if frame.body[2..4] == 16403u16.to_be_bytes() {
            if frame.body[0..2] != [0, 0] || frame.body.len() < 8 {
                return Err(IkeAuthError::MalformedNotify);
            }
            value = Some(u32::from_be_bytes(
                frame.body[4..8]
                    .try_into()
                    .map_err(|_| IkeAuthError::MalformedNotify)?,
            ));
        }
    }
    Ok(value)
}
/// An authenticated lifetime is an absolute authentication boundary. Rekey
/// derives keys without repeating EAP and must never renew this timestamp.
#[derive(Clone, Copy)]
pub(crate) struct AuthenticationLifetime {
    pub seconds: u32,
    pub deadline: tokio::time::Instant,
}
impl AuthenticationLifetime {
    pub fn received(seconds: u32) -> Self {
        Self {
            seconds,
            deadline: tokio::time::Instant::now()
                + std::time::Duration::from_secs(u64::from(seconds)),
        }
    }
}
pub(crate) fn earlier_deadline(
    current: Option<tokio::time::Instant>,
    authenticated: Option<AuthenticationLifetime>,
) -> Option<tokio::time::Instant> {
    match (current, authenticated) {
        (Some(a), Some(b)) => Some(a.min(b.deadline)),
        (None, Some(b)) => Some(b.deadline),
        (value, None) => value,
    }
}

pub(crate) fn intervals(
    lifetime: u32,
    options: &crate::NativeTunnelOptions,
) -> (Option<std::time::Duration>, Option<std::time::Duration>) {
    use std::time::Duration;
    if lifetime > 0
        && (options.ike_rekey_interval.is_some() || options.child_rekey_interval.is_some())
    {
        (
            options
                .ike_rekey_interval
                .map(|_| Duration::from_secs((f64::from(lifetime) * 0.8) as u64)),
            options
                .child_rekey_interval
                .map(|_| Duration::from_secs((f64::from(lifetime) * 0.875) as u64)),
        )
    } else {
        (options.ike_rekey_interval, options.child_rekey_interval)
    }
}
pub(crate) fn deadline(
    interval: Option<std::time::Duration>,
    jitter: std::time::Duration,
) -> Result<Option<tokio::time::Instant>, IkeAuthError> {
    interval
        .map(|v| {
            let mut bytes = [0; 8];
            let max = jitter.as_secs();
            if max > 0 {
                getrandom::fill(&mut bytes).map_err(|_| IkeAuthError::UnexpectedPacket)?;
            }
            let advance = if max == 0 {
                0
            } else {
                u64::from_be_bytes(bytes) % max
            };
            tokio::time::Instant::now()
                .checked_add(v.saturating_sub(std::time::Duration::from_secs(advance)))
                .ok_or(IkeAuthError::InvalidAttempts)
        })
        .transpose()
}

/// Go's planned reauthentication is one shot, delayed by a sub-10% jitter.
/// It is independent of rekey timer resets and uses sub-second precision.
pub(crate) fn reauth_deadline(
    interval: Option<std::time::Duration>,
    jitter: bool,
) -> Result<Option<tokio::time::Instant>, IkeAuthError> {
    interval
        .map(|value| {
            let mut bytes = [0; 8];
            let bound = (value / 10).as_nanos();
            if jitter && bound > 0 {
                getrandom::fill(&mut bytes).map_err(|_| IkeAuthError::UnexpectedPacket)?;
            }
            let extra = if jitter && bound > 0 {
                std::time::Duration::from_nanos(
                    (u128::from(u64::from_be_bytes(bytes)) % bound)
                        .try_into()
                        .map_err(|_| IkeAuthError::InvalidAttempts)?,
                )
            } else {
                std::time::Duration::ZERO
            };
            tokio::time::Instant::now()
                .checked_add(
                    value
                        .checked_add(extra)
                        .ok_or(IkeAuthError::InvalidAttempts)?,
                )
                .ok_or(IkeAuthError::InvalidAttempts)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn auth_lifetime_cadence_matches_go_truncated_seconds_and_zero_fallback() {
        let options = crate::NativeTunnelOptions {
            ike_rekey_interval: Some(Duration::from_secs(1500)),
            child_rekey_interval: Some(Duration::from_secs(1800)),
            ..Default::default()
        };
        for (lifetime, ike, child) in [
            (0, 1500, 1800),
            (1, 0, 0),
            (2, 1, 1),
            (8, 6, 7),
            (3600, 2880, 3150),
            (u32::MAX, 3435973836, 3758096383),
        ] {
            assert_eq!(
                intervals(lifetime, &options),
                (
                    Some(Duration::from_secs(ike)),
                    Some(Duration::from_secs(child))
                )
            );
        }
        // Legacy callers with both schedulers disabled stay disabled.
        assert_eq!(intervals(3600, &Default::default()), (None, None));
    }
    #[test]
    fn authenticated_lifetime_notify_rejects_malformed_binding_and_uses_last_value() {
        let notice = |seconds: u32| {
            frame(
                41,
                [
                    vec![0, 0],
                    16403u16.to_be_bytes().to_vec(),
                    seconds.to_be_bytes().to_vec(),
                ]
                .concat(),
            )
        };
        assert_eq!(auth_lifetime(&[notice(10), notice(20)]).unwrap(), Some(20));
        assert_eq!(auth_lifetime(&[notice(0)]).unwrap(), Some(0));
        assert_eq!(
            auth_lifetime(&[frame(41, vec![0, 0, 0, 24])]).unwrap(),
            None
        );
        for body in [
            vec![0, 0, 0x40, 0x13],
            vec![1, 0, 0x40, 0x13, 0, 0, 0, 2],
            vec![0, 4, 0x40, 0x13, 0, 0, 0, 2],
            vec![0],
        ] {
            assert!(matches!(
                auth_lifetime(&[frame(41, body)]),
                Err(IkeAuthError::MalformedNotify)
            ));
        }
    }
    #[tokio::test]
    async fn authentication_deadline_is_absolute_earliest_and_zero_is_immediate() {
        let first = AuthenticationLifetime::received(60);
        let local = first.deadline - std::time::Duration::from_secs(20);
        assert_eq!(earlier_deadline(Some(local), Some(first)), Some(local));
        assert_eq!(earlier_deadline(None, Some(first)), Some(first.deadline));
        let later = AuthenticationLifetime::received(120);
        assert_eq!(
            earlier_deadline(Some(first.deadline), Some(later)),
            Some(first.deadline)
        );
        let zero = AuthenticationLifetime::received(0);
        assert!(
            earlier_deadline(Some(first.deadline), Some(zero)).unwrap()
                <= tokio::time::Instant::now()
        );
        assert_eq!(
            earlier_deadline(Some(first.deadline), None),
            Some(first.deadline)
        );
    }
    #[tokio::test]
    async fn jitter_reseeds_each_deadline_in_go_whole_second_advance_window() {
        let mut advances = std::collections::BTreeSet::new();
        for _ in 0..64 {
            let before = tokio::time::Instant::now();
            let due = deadline(Some(Duration::from_secs(1500)), Duration::from_secs(30))
                .unwrap()
                .unwrap();
            let after = tokio::time::Instant::now();
            assert!(due >= before + Duration::from_secs(1471));
            assert!(due <= after + Duration::from_secs(1500));
            // Allow function execution time; advance remains a whole number of seconds.
            advances.insert((due - before).as_secs());
        }
        assert!(advances.len() > 1);
        assert!(deadline(None, Duration::from_secs(30)).unwrap().is_none());
        let before = tokio::time::Instant::now();
        let due = deadline(Some(Duration::from_millis(1)), Duration::from_secs(30))
            .unwrap()
            .unwrap();
        assert!(due >= before);
    }
    #[tokio::test]
    async fn planned_reauth_jitter_is_positive_sub_ten_percent_and_one_shot() {
        for _ in 0..32 {
            let before = tokio::time::Instant::now();
            let due = reauth_deadline(Some(Duration::from_secs(10)), true)
                .unwrap()
                .unwrap();
            let after = tokio::time::Instant::now();
            assert!(due >= before + Duration::from_secs(10));
            assert!(due < after + Duration::from_secs(11));
        }
        assert!(reauth_deadline(None, true).unwrap().is_none());
        assert!(reauth_deadline(Some(Duration::MAX), true).is_err());
        assert!(deadline(Some(Duration::MAX), Duration::ZERO).is_err());
    }
}
