//! Initial ESP proposal validation; does not authorize traffic or install an SA.
use proletarius_ike::{SaDecodeError, decode_sa_proposals};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitialChildSaSelection {
    pub local_spi: u32,
    pub remote_spi: u32,
    pub encryption_transform: u16,
    pub integrity_transform: Option<u16>,
    /// Includes salt for AEAD.
    pub encryption_key_len: usize,
    pub integrity_key_len: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChildSaError {
    Decode(SaDecodeError),
    UnofferedProposal,
    InvalidSpi,
    InvalidTransforms,
    UnsupportedAlgorithm,
}

impl std::fmt::Display for ChildSaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(e) => e.fmt(f),
            Self::UnofferedProposal => f.write_str("CHILD SA selected an unoffered proposal"),
            Self::InvalidSpi => f.write_str("ESP requires a nonzero four-byte SPI"),
            Self::InvalidTransforms => f.write_str("invalid initial ESP transform combination"),
            Self::UnsupportedAlgorithm => f.write_str("unsupported initial ESP algorithm"),
        }
    }
}
impl std::error::Error for ChildSaError {}

pub fn validate_initial_child_sa(
    offered: &[u8],
    selected: &[u8],
) -> Result<InitialChildSaSelection, ChildSaError> {
    let offers = decode_sa_proposals(offered).map_err(ChildSaError::Decode)?;
    let choices = decode_sa_proposals(selected).map_err(ChildSaError::Decode)?;
    if choices.len() != 1 {
        return Err(ChildSaError::UnofferedProposal);
    }
    let choice = &choices[0];
    let mut matching = offers
        .iter()
        .filter(|p| p.number == choice.number && p.protocol == 3);
    let offer = matching.next().ok_or(ChildSaError::UnofferedProposal)?;
    if choice.number == 0 || choice.protocol != 3 || matching.next().is_some() {
        return Err(ChildSaError::UnofferedProposal);
    }
    let spi = |bytes: &[u8]| -> Result<u32, ChildSaError> {
        let value = u32::from_be_bytes(bytes.try_into().map_err(|_| ChildSaError::InvalidSpi)?);
        if value == 0 {
            Err(ChildSaError::InvalidSpi)
        } else {
            Ok(value)
        }
    };
    let local_spi = spi(&offer.spi)?;
    let remote_spi = spi(&choice.spi)?;
    let mut transforms = [None; 6];
    for transform in &choice.transforms {
        let kind = usize::from(transform.kind);
        // Initial CHILD SA reuses IKE nonces and has no separate PRF or DH.
        if !matches!(kind, 1 | 3 | 5) || transforms[kind].is_some() {
            return Err(ChildSaError::InvalidTransforms);
        }
        if !offer.transforms.contains(transform) {
            return Err(ChildSaError::UnofferedProposal);
        }
        transforms[kind] = Some(transform);
    }
    // An omitted ESN means no ESN; never silently accept an ESN-only offer.
    if transforms[5].is_some_and(|t| t.id != 0 || !t.attributes.is_empty())
        || (transforms[5].is_none()
            && offer.transforms.iter().any(|t| t.kind == 5)
            && !offer.transforms.iter().any(|t| t.kind == 5 && t.id == 0))
    {
        return Err(ChildSaError::UnsupportedAlgorithm);
    }
    let enc = transforms[1].ok_or(ChildSaError::InvalidTransforms)?;
    if enc.attributes.len() != 1
        || enc.attributes[0].kind != 14
        || enc.attributes[0].value.len() != 2
    {
        return Err(ChildSaError::InvalidTransforms);
    }
    let bits = u16::from_be_bytes(enc.attributes[0].value.as_slice().try_into().unwrap());
    if !matches!(bits, 128 | 192 | 256) {
        return Err(ChildSaError::UnsupportedAlgorithm);
    }
    let integrity = transforms[3];
    if integrity.is_some_and(|t| !t.attributes.is_empty()) {
        return Err(ChildSaError::InvalidTransforms);
    }
    let (salt, integrity_key_len) = match enc.id {
        20 if integrity.is_none_or(|t| t.id == 0) => (4, 0),
        12 => (
            0,
            match integrity.map(|t| t.id) {
                Some(2) => 20,
                Some(12) => 32,
                Some(13) => 48,
                Some(14) => 64,
                _ => return Err(ChildSaError::UnsupportedAlgorithm),
            },
        ),
        _ => return Err(ChildSaError::UnsupportedAlgorithm),
    };
    Ok(InitialChildSaSelection {
        local_spi,
        remote_spi,
        encryption_transform: enc.id,
        integrity_transform: integrity.map(|t| t.id),
        encryption_key_len: usize::from(bits / 8) + salt,
        integrity_key_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn proposal(spi: u32) -> Vec<u8> {
        let mut bytes = vec![0, 0, 0, 32, 1, 3, 4, 2];
        bytes.extend_from_slice(&spi.to_be_bytes());
        bytes.extend_from_slice(&[
            3, 0, 0, 12, 1, 0, 0, 20, 0x80, 14, 0, 128, 0, 0, 0, 8, 5, 0, 0, 0,
        ]);
        bytes
    }
    #[test]
    fn validates_gcm_and_rejects_unoffered_or_invalid_selections() {
        let offered = proposal(0x11223344);
        let selected = proposal(0x55667788);
        let result = validate_initial_child_sa(&offered, &selected).unwrap();
        assert_eq!(
            (result.local_spi, result.remote_spi),
            (0x11223344, 0x55667788)
        );
        assert_eq!(
            (result.encryption_key_len, result.integrity_key_len),
            (20, 0)
        );
        for (offset, value) in [(4, 2), (5, 1), (19, 12), (23, 64), (28, 1), (31, 1)] {
            let mut invalid = selected.clone();
            invalid[offset] = value;
            assert!(validate_initial_child_sa(&offered, &invalid).is_err());
        }
        assert!(validate_initial_child_sa(&offered, &proposal(0)).is_err());
        assert!(validate_initial_child_sa(&proposal(0), &selected).is_err());
        assert!(validate_initial_child_sa(&offered, &selected[..31]).is_err());
    }
    #[test]
    fn validates_cbc_integrity_and_rejects_gcm_with_integrity() {
        let mut offered = proposal(1);
        offered[19] = 12;
        offered[28] = 3;
        offered[31] = 2;
        let mut selected = offered.clone();
        selected[11] = 2;
        let result = validate_initial_child_sa(&offered, &selected).unwrap();
        assert_eq!(
            (result.encryption_key_len, result.integrity_key_len),
            (16, 20)
        );
        offered[19] = 20;
        selected[19] = 20;
        assert!(validate_initial_child_sa(&offered, &selected).is_err());
    }
}
