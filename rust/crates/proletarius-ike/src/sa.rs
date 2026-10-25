use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaAttribute {
    pub kind: u16,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaTransform {
    pub kind: u8,
    pub id: u16,
    pub attributes: Vec<SaAttribute>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaProposal {
    pub number: u8,
    pub protocol: u8,
    pub spi: Vec<u8>,
    pub transforms: Vec<SaTransform>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaDecodeError {
    Empty,
    Truncated,
    InvalidLength,
    InvalidChain,
}

impl fmt::Display for SaDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("IKE SA contains no proposal"),
            Self::Truncated => formatter.write_str("IKE SA proposal or transform is truncated"),
            Self::InvalidLength => {
                formatter.write_str("IKE SA proposal or transform has invalid length")
            }
            Self::InvalidChain => {
                formatter.write_str("IKE SA proposal or transform chain is invalid")
            }
        }
    }
}

impl std::error::Error for SaDecodeError {}

pub fn decode_sa_proposals(bytes: &[u8]) -> Result<Vec<SaProposal>, SaDecodeError> {
    if bytes.is_empty() {
        return Err(SaDecodeError::Empty);
    }
    let mut proposals = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let header = bytes
            .get(offset..offset + 8)
            .ok_or(SaDecodeError::Truncated)?;
        let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
        if length < 8 {
            return Err(SaDecodeError::InvalidLength);
        }
        let proposal = bytes
            .get(offset..offset + length)
            .ok_or(SaDecodeError::Truncated)?;
        let spi_end = 8 + usize::from(header[6]);
        let spi = proposal
            .get(8..spi_end)
            .ok_or(SaDecodeError::Truncated)?
            .to_vec();
        let count = usize::from(header[7]);
        if count == 0 {
            return Err(SaDecodeError::InvalidChain);
        }
        let mut transforms = Vec::with_capacity(count);
        let mut cursor = spi_end;
        for index in 0..count {
            let transform_header = proposal
                .get(cursor..cursor + 8)
                .ok_or(SaDecodeError::Truncated)?;
            let transform_length = usize::from(u16::from_be_bytes([
                transform_header[2],
                transform_header[3],
            ]));
            if transform_length < 8 {
                return Err(SaDecodeError::InvalidLength);
            }
            let transform = proposal
                .get(cursor..cursor + transform_length)
                .ok_or(SaDecodeError::Truncated)?;
            let expected_last = index + 1 == count;
            if transform_header[0] != if expected_last { 0 } else { 3 } {
                return Err(SaDecodeError::InvalidChain);
            }
            let mut attributes = Vec::new();
            let mut attr_offset = 8;
            while attr_offset < transform.len() {
                let attr = transform
                    .get(attr_offset..attr_offset + 4)
                    .ok_or(SaDecodeError::Truncated)?;
                let raw_kind = u16::from_be_bytes([attr[0], attr[1]]);
                let value_length = if raw_kind & 0x8000 != 0 {
                    2
                } else {
                    usize::from(u16::from_be_bytes([attr[2], attr[3]]))
                };
                let (value, consumed) = if raw_kind & 0x8000 != 0 {
                    (attr[2..4].to_vec(), 4)
                } else {
                    (
                        transform
                            .get(attr_offset + 4..attr_offset + 4 + value_length)
                            .ok_or(SaDecodeError::Truncated)?
                            .to_vec(),
                        4 + value_length,
                    )
                };
                attributes.push(SaAttribute {
                    kind: raw_kind & 0x7fff,
                    value,
                });
                attr_offset += consumed;
            }
            transforms.push(SaTransform {
                kind: transform_header[4],
                id: u16::from_be_bytes([transform_header[6], transform_header[7]]),
                attributes,
            });
            cursor += transform_length;
        }
        if cursor != proposal.len() {
            return Err(SaDecodeError::InvalidChain);
        }
        offset += length;
        if header[0] != if offset == bytes.len() { 0 } else { 2 } {
            return Err(SaDecodeError::InvalidChain);
        }
        proposals.push(SaProposal {
            number: header[4],
            protocol: header[5],
            spi,
            transforms,
        });
    }
    Ok(proposals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_go_aes_gcm_proposal_and_rejects_bad_lengths() {
        // Go EncryptedPayloadSA: proposal 1, IKE, AES-GCM-16/128, PRF SHA2-256, MODP2048.
        let wire = [
            0, 0, 0, 36, 1, 1, 0, 3, 3, 0, 0, 12, 1, 0, 0, 20, 0x80, 14, 0, 128, 3, 0, 0, 8, 2, 0,
            0, 5, 0, 0, 0, 8, 4, 0, 0, 14,
        ];
        let proposal = decode_sa_proposals(&wire).unwrap();
        assert_eq!(proposal.len(), 1);
        assert_eq!(proposal[0].number, 1);
        assert_eq!(proposal[0].transforms[0].id, 20);
        assert_eq!(proposal[0].transforms[0].attributes[0].value, [0, 128]);
        assert_eq!(proposal[0].transforms[2].id, 14);
        let mut invalid = wire;
        invalid[2..4].copy_from_slice(&0_u16.to_be_bytes());
        assert_eq!(
            decode_sa_proposals(&invalid),
            Err(SaDecodeError::InvalidLength)
        );
        invalid = wire;
        invalid[7] = 4;
        assert_eq!(
            decode_sa_proposals(&invalid),
            Err(SaDecodeError::InvalidChain)
        );
        invalid = wire;
        invalid[8] = 0;
        assert_eq!(
            decode_sa_proposals(&invalid),
            Err(SaDecodeError::InvalidChain)
        );
    }
}
