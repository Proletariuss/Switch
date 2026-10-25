//! One immutable CREATE_CHILD_SA attempt. Nonces/SPI/DH are allocated once;
//! retries transmit the exact protected packet owned by the IKE runtime.
use crate::sa_init::ModpKeyPair;
use crate::{ChildTrafficSelectors, IkeAuthError, InitialChildSaSelection};
use proletarius_ike::{
    ChildSaKeyMaterial, PayloadFrame, SaProposal, SaTransform, decode_sa_proposals,
};
use zeroize::Zeroizing;

pub(crate) struct Attempt {
    pub frames: Vec<PayloadFrame>,
    pub offer: Vec<u8>,
    pub nonce: Zeroizing<Vec<u8>>,
    pub dh: Option<ModpKeyPair>,
    pub pfs: u16,
}
pub(crate) struct Material {
    pub selection: InitialChildSaSelection,
    pub selectors: ChildTrafficSelectors,
    pub selector_frames: Vec<PayloadFrame>,
    pub keys: ChildSaKeyMaterial,
}
fn frame(kind: u8, body: Vec<u8>) -> PayloadFrame {
    PayloadFrame {
        kind,
        critical: false,
        body,
    }
}
fn unique(frames: &[PayloadFrame], kind: u8) -> Result<&PayloadFrame, IkeAuthError> {
    let mut matched = frames.iter().filter(|v| v.kind == kind);
    let value = matched.next().ok_or(IkeAuthError::UnexpectedPacket)?;
    if matched.next().is_some() {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    Ok(value)
}

pub(crate) fn encode(proposals: &[SaProposal]) -> Result<Vec<u8>, IkeAuthError> {
    let mut result = Vec::new();
    for (index, p) in proposals.iter().enumerate() {
        let mut body = p.spi.clone();
        for (idx, t) in p.transforms.iter().enumerate() {
            let mut attributes = Vec::new();
            for a in &t.attributes {
                if a.value.len() == 2 {
                    attributes.extend_from_slice(&(a.kind | 0x8000).to_be_bytes());
                    attributes.extend_from_slice(&a.value);
                } else {
                    attributes.extend_from_slice(&a.kind.to_be_bytes());
                    attributes.extend_from_slice(&(a.value.len() as u16).to_be_bytes());
                    attributes.extend_from_slice(&a.value);
                }
            }
            let length =
                u16::try_from(8 + attributes.len()).map_err(|_| IkeAuthError::UnexpectedPacket)?;
            body.extend_from_slice(&[if idx + 1 == p.transforms.len() { 0 } else { 3 }, 0]);
            body.extend_from_slice(&length.to_be_bytes());
            body.extend_from_slice(&[t.kind, 0]);
            body.extend_from_slice(&t.id.to_be_bytes());
            body.extend_from_slice(&attributes);
        }
        let length = u16::try_from(8 + body.len()).map_err(|_| IkeAuthError::UnexpectedPacket)?;
        result.extend_from_slice(&[if index + 1 == proposals.len() { 0 } else { 2 }, 0]);
        result.extend_from_slice(&length.to_be_bytes());
        result.extend_from_slice(&[
            p.number,
            p.protocol,
            p.spi.len() as u8,
            p.transforms.len() as u8,
        ]);
        result.extend(body);
    }
    Ok(result)
}
pub(crate) fn begin(
    base_offer: &[u8],
    selectors: &[PayloadFrame],
    old_local_spi: u32,
    pfs: u16,
) -> Result<Attempt, IkeAuthError> {
    if !matches!(pfs, 0 | 14) {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let mut random = [0; 36];
    getrandom::fill(&mut random).map_err(|_| IkeAuthError::UnexpectedPacket)?;
    let spi = &random[..4];
    if spi == [0; 4] || spi == old_local_spi.to_be_bytes() {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let mut proposals =
        decode_sa_proposals(base_offer).map_err(|_| IkeAuthError::UnexpectedPacket)?;
    for proposal in &mut proposals {
        proposal.spi = spi.to_vec();
        proposal.transforms.retain(|v| v.kind != 4);
        if pfs != 0 {
            proposal.transforms.push(SaTransform {
                kind: 4,
                id: pfs,
                attributes: Vec::new(),
            });
        }
    }
    let offer = encode(&proposals)?;
    let nonce = Zeroizing::new(random[4..].to_vec());
    let dh = if pfs != 0 {
        Some(ModpKeyPair::generate(pfs).map_err(|_| IkeAuthError::UnexpectedPacket)?)
    } else {
        None
    };
    let mut frames = vec![frame(33, offer.clone()), frame(40, nonce.to_vec())];
    if let Some(dh) = &dh {
        let mut ke = pfs.to_be_bytes().to_vec();
        ke.extend([0, 0]);
        ke.extend_from_slice(dh.public_key());
        frames.push(frame(34, ke));
    }
    let mut notify = vec![3, 4, 0x40, 0x09];
    notify.extend_from_slice(&old_local_spi.to_be_bytes());
    frames.push(frame(41, notify));
    for kind in [44, 45] {
        frames.push(unique(selectors, kind)?.clone());
    }
    Ok(Attempt {
        frames,
        offer,
        nonce,
        dh,
        pfs,
    })
}
pub(crate) fn finish(
    attempt: &Attempt,
    response: &[PayloadFrame],
    prf: u16,
    sk_d: &[u8],
    old: &InitialChildSaSelection,
) -> Result<Material, IkeAuthError> {
    for frame in response.iter().filter(|v| v.kind == 41) {
        if frame.body.len() < 4 {
            return Err(IkeAuthError::MalformedNotify);
        }
        let kind = u16::from_be_bytes([frame.body[2], frame.body[3]]);
        if kind < 16384 {
            return Err(IkeAuthError::PeerNotify(kind));
        }
    }
    let selected = unique(response, 33)?;
    let nr = &unique(response, 40)?.body;
    if !(16..=256).contains(&nr.len()) {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let offers = decode_sa_proposals(&attempt.offer).map_err(|_| IkeAuthError::UnexpectedPacket)?;
    let choices =
        decode_sa_proposals(&selected.body).map_err(|_| IkeAuthError::UnexpectedPacket)?;
    if choices.len() != 1 {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let dh: Vec<_> = choices[0]
        .transforms
        .iter()
        .filter(|v| v.kind == 4)
        .collect();
    let shared = if attempt.pfs == 0 {
        if !dh.is_empty() || response.iter().any(|v| v.kind == 34) {
            return Err(IkeAuthError::UnexpectedPacket);
        }
        Zeroizing::new(Vec::new())
    } else {
        if dh.len() != 1 || dh[0].id != attempt.pfs || !dh[0].attributes.is_empty() {
            return Err(IkeAuthError::UnexpectedPacket);
        }
        let ke = &unique(response, 34)?.body;
        if ke.len() < 5 || u16::from_be_bytes([ke[0], ke[1]]) != attempt.pfs || ke[2..4] != [0, 0] {
            return Err(IkeAuthError::UnexpectedPacket);
        }
        attempt
            .dh
            .as_ref()
            .ok_or(IkeAuthError::UnexpectedPacket)?
            .shared_secret(&ke[4..])
            .map_err(|_| IkeAuthError::UnexpectedPacket)?
    };
    let strip = |mut proposals: Vec<SaProposal>| {
        for p in &mut proposals {
            p.transforms.retain(|v| v.kind != 4);
        }
        encode(&proposals)
    };
    let selection = crate::validate_initial_child_sa(&strip(offers)?, &strip(choices)?)
        .map_err(IkeAuthError::ChildSa)?;
    if selection.local_spi == old.local_spi || selection.remote_spi == old.remote_spi {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let selectors = crate::validate_child_traffic_selectors(&attempt.frames, response)
        .map_err(IkeAuthError::TrafficSelectors)?;
    let keys = proletarius_ike::derive_rekey_child_sa_keys(
        prf,
        sk_d,
        &shared,
        &attempt.nonce,
        nr,
        selection.encryption_key_len,
        selection.integrity_key_len,
    )
    .map_err(IkeAuthError::KeyDerivation)?;
    let selector_frames = [44, 45]
        .into_iter()
        .map(|kind| unique(response, kind).cloned())
        .collect::<Result<_, _>>()?;
    Ok(Material {
        selection,
        selectors,
        selector_frames,
        keys,
    })
}

/// Peer exchange uses peer Ni first, and its initiator direction is our inbound
/// direction. Validate against the current suite and selectors before responding.
pub(crate) fn respond(
    request: &[PayloadFrame],
    base_offer: &[u8],
    selectors: &[PayloadFrame],
    old: &InitialChildSaSelection,
    pfs: u16,
    prf: u16,
    sk_d: &[u8],
) -> Result<(Vec<PayloadFrame>, Material), IkeAuthError> {
    let notify = unique(request, 41)?;
    let mut expected = vec![3, 4, 0x40, 0x09];
    expected.extend_from_slice(&old.remote_spi.to_be_bytes());
    if notify.body != expected {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let peer_nonce = &unique(request, 40)?.body;
    if !(16..=256).contains(&peer_nonce.len()) {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let requested = decode_sa_proposals(&unique(request, 33)?.body)
        .map_err(|_| IkeAuthError::UnexpectedPacket)?;
    let allowed = decode_sa_proposals(base_offer).map_err(|_| IkeAuthError::UnexpectedPacket)?;
    let chosen = requested
        .iter()
        .find(|candidate| {
            allowed.iter().any(|policy| {
                let candidate_transforms: Vec<_> = candidate
                    .transforms
                    .iter()
                    .filter(|v| v.kind != 4)
                    .collect();
                let policy_transforms: Vec<_> =
                    policy.transforms.iter().filter(|v| v.kind != 4).collect();
                candidate.protocol == 3 && candidate_transforms == policy_transforms
            })
        })
        .ok_or(IkeAuthError::UnexpectedPacket)?
        .clone();
    let mut offered_local = chosen.clone();
    offered_local.spi = old.local_spi.to_be_bytes().to_vec();
    let dh: Vec<_> = chosen.transforms.iter().filter(|v| v.kind == 4).collect();
    if (pfs == 0 && !dh.is_empty())
        || (pfs != 0 && (dh.len() != 1 || dh[0].id != pfs || !dh[0].attributes.is_empty()))
    {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let mut ephemeral = begin(&encode(&[offered_local])?, selectors, old.local_spi, pfs)?;
    let peer_spi = u32::from_be_bytes(
        chosen
            .spi
            .as_slice()
            .try_into()
            .map_err(|_| IkeAuthError::UnexpectedPacket)?,
    );
    if peer_spi == 0 || peer_spi == old.remote_spi {
        return Err(IkeAuthError::UnexpectedPacket);
    }
    let mut policy = chosen.clone();
    policy.spi = ephemeral.frames[0].body[8..12].to_vec();
    let response_sa = encode(&[policy])?;
    // finish() validates local/remote SPIs and narrows selectors in our direction.
    let mut normalized_request = request.to_vec();
    normalized_request
        .iter_mut()
        .find(|v| v.kind == 33)
        .ok_or(IkeAuthError::UnexpectedPacket)?
        .body = encode(&[chosen])?;
    for item in &mut normalized_request {
        if item.kind == 44 {
            item.kind = 45;
        } else if item.kind == 45 {
            item.kind = 44;
        }
    }
    let mut response = vec![frame(33, response_sa), frame(40, ephemeral.nonce.to_vec())];
    response.extend(ephemeral.frames.iter().filter(|v| v.kind == 34).cloned());
    response.extend(
        normalized_request
            .iter()
            .filter(|v| matches!(v.kind, 44 | 45))
            .cloned(),
    );
    // Store our offered SPI plus peer transform/number; finish validates the same suite.
    ephemeral.offer = response[0].body.clone();
    ephemeral.frames = selectors.to_vec();
    let our_nonce = std::mem::replace(&mut ephemeral.nonce, Zeroizing::new(peer_nonce.clone()));
    let mut peer_response = normalized_request;
    for item in &mut peer_response {
        if item.kind == 40 {
            item.body = our_nonce.to_vec();
        }
    }
    let mut material = finish(&ephemeral, &peer_response, prf, sk_d, old)?;
    // Its KE belongs to the peer, while our response advertises our generated KE.
    std::mem::swap(
        &mut material.keys.initiator_encryption,
        &mut material.keys.responder_encryption,
    );
    std::mem::swap(
        &mut material.keys.initiator_integrity,
        &mut material.keys.responder_integrity,
    );
    // RESPONSE selectors remain in the peer exchange's original orientation.
    for item in &mut response {
        if item.kind == 44 {
            item.kind = 45;
        } else if item.kind == 45 {
            item.kind = 44;
        }
    }
    Ok((response, material))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Vec<u8>, Vec<PayloadFrame>, InitialChildSaSelection) {
        let offer = proletarius_ike::build_native_proposals(
            &["aes128-sha256-prfsha256-modp2048".into()],
            proletarius_ike::ProposalProtocol::Esp,
            &1u32.to_be_bytes(),
        );
        // ESP syntax excludes a PRF/DH during initial authentication.
        let offer = offer.unwrap_or_else(|_| {
            proletarius_ike::build_native_proposals(
                &["aes128-sha256".into()],
                proletarius_ike::ProposalProtocol::Esp,
                &1u32.to_be_bytes(),
            )
            .unwrap()
        });
        let frames = crate::build_initial_auth_frames(crate::InitialAuthParameters {
            identity: b"reader@ims.example",
            apn: "ims",
            child_proposals: &offer,
            ip_stack: crate::RequestedIpStack::Ipv4,
            device_identity_imei: None,
            initial_contact: false,
        })
        .unwrap();
        let mut peer = decode_sa_proposals(&offer).unwrap();
        peer[0].spi = 2u32.to_be_bytes().to_vec();
        let old = crate::validate_initial_child_sa(&offer, &encode(&peer).unwrap()).unwrap();
        (
            offer,
            frames
                .into_iter()
                .filter(|v| matches!(v.kind, 44 | 45))
                .collect(),
            old,
        )
    }
    #[test]
    fn pfs_rekey_derives_new_keys_and_rejects_ke_downgrade_and_old_spi() {
        let (offer, selectors, old) = fixture();
        let attempt = begin(&offer, &selectors, old.local_spi, 14).unwrap();
        let peer = ModpKeyPair::generate(14).unwrap();
        let mut selected = decode_sa_proposals(&attempt.offer).unwrap();
        selected.truncate(1);
        selected[0].spi = 3u32.to_be_bytes().to_vec();
        let mut ke = 14u16.to_be_bytes().to_vec();
        ke.extend([0, 0]);
        ke.extend_from_slice(peer.public_key());
        let mut response = vec![
            frame(33, encode(&selected).unwrap()),
            frame(40, vec![7; 32]),
            frame(34, ke),
        ];
        response.extend(selectors);
        let result = finish(&attempt, &response, 5, &[9; 32], &old).unwrap();
        assert_eq!(result.selection.remote_spi, 3);
        assert_ne!(result.selection.local_spi, old.local_spi);
        let shared = peer
            .shared_secret(attempt.dh.as_ref().unwrap().public_key())
            .unwrap();
        let expected = proletarius_ike::derive_rekey_child_sa_keys(
            5,
            &[9; 32],
            &shared,
            &attempt.nonce,
            &[7; 32],
            result.selection.encryption_key_len,
            result.selection.integrity_key_len,
        )
        .unwrap();
        assert_eq!(
            result.keys.initiator_encryption,
            expected.initiator_encryption
        );
        let without_ke: Vec<_> = response.iter().filter(|v| v.kind != 34).cloned().collect();
        assert!(finish(&attempt, &without_ke, 5, &[9; 32], &old).is_err());
        selected[0].spi = old.remote_spi.to_be_bytes().to_vec();
        response[0].body = encode(&selected).unwrap();
        assert!(finish(&attempt, &response, 5, &[9; 32], &old).is_err());
    }
    #[test]
    fn no_pfs_rekey_rejects_unrequested_ke_and_expanded_selectors() {
        let (offer, mut selectors, old) = fixture();
        selectors.iter_mut().find(|v| v.kind == 44).unwrap().body[5] = 17;
        let attempt = begin(&offer, &selectors, old.local_spi, 0).unwrap();
        let mut selected = decode_sa_proposals(&attempt.offer).unwrap();
        selected.truncate(1);
        selected[0].spi = 3u32.to_be_bytes().to_vec();
        let mut response = vec![
            frame(33, encode(&selected).unwrap()),
            frame(40, vec![7; 32]),
        ];
        response.extend(selectors);
        assert!(finish(&attempt, &response, 5, &[9; 32], &old).is_ok());
        response.push(frame(34, vec![0, 14, 0, 0, 1]));
        assert!(finish(&attempt, &response, 5, &[9; 32], &old).is_err());
        response.pop();
        assert!(finish(&attempt, &response, 5, &[9; 32], &old).is_ok());
        response.iter_mut().find(|v| v.kind == 44).unwrap().body[5] = 0;
        assert!(finish(&attempt, &response, 5, &[9; 32], &old).is_err());
    }
}
