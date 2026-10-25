use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayloadFrame {
    pub kind: u8,
    pub critical: bool,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PayloadChainError {
    TooLong,
    TruncatedHeader,
    InvalidLength,
    TruncatedBody,
    TrailingBytes,
}

impl fmt::Display for PayloadChainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong => formatter.write_str("IKE payload exceeds 65535 bytes"),
            Self::TruncatedHeader => formatter.write_str("IKE payload header is truncated"),
            Self::InvalidLength => formatter.write_str("IKE payload length is smaller than header"),
            Self::TruncatedBody => formatter.write_str("IKE payload body is truncated"),
            Self::TrailingBytes => formatter.write_str("IKE payload chain has trailing bytes"),
        }
    }
}

impl std::error::Error for PayloadChainError {}

/// Encodes the RFC 7296 generic payload headers. The first type is carried
/// in the IKE header, while each payload header names the following type.
pub fn encode_payload_chain(frames: &[PayloadFrame]) -> Result<(u8, Vec<u8>), PayloadChainError> {
    let mut encoded = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let length = frame
            .body
            .len()
            .checked_add(4)
            .ok_or(PayloadChainError::TooLong)?;
        let length = u16::try_from(length).map_err(|_| PayloadChainError::TooLong)?;
        encoded.push(frames.get(index + 1).map_or(0, |next| next.kind));
        encoded.push(if frame.critical { 0x80 } else { 0 });
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(&frame.body);
    }
    Ok((frames.first().map_or(0, |first| first.kind), encoded))
}

pub fn decode_payload_chain(
    first: u8,
    bytes: &[u8],
) -> Result<Vec<PayloadFrame>, PayloadChainError> {
    let mut frames = Vec::new();
    let mut kind = first;
    let mut offset = 0;
    while kind != 0 {
        let header = bytes
            .get(offset..offset + 4)
            .ok_or(PayloadChainError::TruncatedHeader)?;
        let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
        if length < 4 {
            return Err(PayloadChainError::InvalidLength);
        }
        let end = offset
            .checked_add(length)
            .ok_or(PayloadChainError::TruncatedBody)?;
        let body = bytes
            .get(offset + 4..end)
            .ok_or(PayloadChainError::TruncatedBody)?;
        frames.push(PayloadFrame {
            kind,
            critical: header[1] & 0x80 != 0,
            body: body.to_vec(),
        });
        kind = header[0];
        offset = end;
    }
    if offset != bytes.len() {
        return Err(PayloadChainError::TrailingBytes);
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sa_ke_nonce_chain_matches_go_wire_headers() {
        let frames = [
            PayloadFrame {
                kind: 33,
                critical: false,
                body: vec![1, 2],
            },
            PayloadFrame {
                kind: 34,
                critical: false,
                body: vec![0, 14, 0, 0, 3],
            },
            PayloadFrame {
                kind: 40,
                critical: false,
                body: vec![4, 5],
            },
        ];
        let (first, wire) = encode_payload_chain(&frames).unwrap();
        assert_eq!(first, 33);
        assert_eq!(
            wire,
            [
                34, 0, 0, 6, 1, 2, 40, 0, 0, 9, 0, 14, 0, 0, 3, 0, 0, 0, 6, 4, 5
            ]
        );
        assert_eq!(decode_payload_chain(first, &wire).unwrap(), frames);
    }

    #[test]
    fn rejects_truncated_or_inconsistent_payload_chain() {
        assert_eq!(
            decode_payload_chain(0, &[1]),
            Err(PayloadChainError::TrailingBytes)
        );
        assert_eq!(
            decode_payload_chain(33, &[0, 0, 0]),
            Err(PayloadChainError::TruncatedHeader)
        );
        assert_eq!(
            decode_payload_chain(33, &[0, 0, 0, 3]),
            Err(PayloadChainError::InvalidLength)
        );
        assert_eq!(
            decode_payload_chain(33, &[0, 0, 0, 5]),
            Err(PayloadChainError::TruncatedBody)
        );
        assert_eq!(
            decode_payload_chain(33, &[0, 0, 0, 4, 1]),
            Err(PayloadChainError::TrailingBytes)
        );
        assert_eq!(
            encode_payload_chain(&[PayloadFrame {
                kind: 40,
                critical: false,
                body: vec![0; 65532]
            }]),
            Err(PayloadChainError::TooLong)
        );
    }
}
