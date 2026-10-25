//! Bounded IKE certificate/signature parsing. Trust evaluation is supplied by
//! the platform owner; parsing alone can never authenticate a responder.
use crate::{IkeHeader, PayloadFrame, decode_payload_chain};

pub const MAX_CERTIFICATES: usize = 8;
pub const MAX_CERTIFICATE_BYTES: usize = 32 * 1024;
pub const MAX_CHAIN_BYTES: usize = 128 * 1024;
pub const SIGNATURE_HASH_ALGORITHMS: u16 = 16431;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertificateAuthError {
    MissingIdentity,
    MissingAuthentication,
    DuplicateAuthentication,
    MissingCertificate,
    InvalidCertificate,
    DuplicateCertificate,
    ChainTooLarge,
    InvalidAlgorithm,
    UnsupportedAlgorithm,
    UnnegotiatedAlgorithm,
    InvalidTranscript,
    UntrustedCertificate,
    InvalidSignature,
    VerificationUnavailable,
}
impl std::fmt::Display for CertificateAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MissingIdentity => "IKE certificate authentication lacks responder identity",
            Self::MissingAuthentication => "IKE certificate authentication lacks AUTH",
            Self::DuplicateAuthentication => "IKE certificate authentication repeats AUTH",
            Self::MissingCertificate => "IKE certificate authentication lacks certificate",
            Self::InvalidCertificate => "IKE certificate encoding is invalid",
            Self::DuplicateCertificate => "IKE certificate chain repeats a certificate",
            Self::ChainTooLarge => "IKE certificate chain exceeds its limit",
            Self::InvalidAlgorithm => "IKE signature AlgorithmIdentifier is invalid",
            Self::UnsupportedAlgorithm => "IKE signature algorithm is unsupported",
            Self::UnnegotiatedAlgorithm => "IKE signature hash was not negotiated",
            Self::InvalidTranscript => "IKE certificate transcript is invalid",
            Self::UntrustedCertificate => "IKE certificate trust or name validation failed",
            Self::InvalidSignature => "IKE responder signature validation failed",
            Self::VerificationUnavailable => "IKE certificate verification did not complete",
        })
    }
}
impl std::error::Error for CertificateAuthError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureAlgorithm {
    RsaPkcs1Sha1,
    RsaPkcs1Sha256,
    RsaPkcs1Sha384,
    RsaPkcs1Sha512,
    EcdsaSha256,
    EcdsaSha384,
}
impl SignatureAlgorithm {
    pub fn hash_id(self) -> u16 {
        match self {
            Self::RsaPkcs1Sha1 => 1,
            Self::RsaPkcs1Sha256 | Self::EcdsaSha256 => 2,
            Self::RsaPkcs1Sha384 | Self::EcdsaSha384 => 3,
            Self::RsaPkcs1Sha512 => 4,
        }
    }
}

pub struct CertificateSignature<'a> {
    pub algorithm: SignatureAlgorithm,
    pub signature: &'a [u8],
}

/// CERT encoding 4 contains exactly one DER certificate. Chain order and the
/// leaf remain exactly as supplied; the trust owner must verify that leaf.
pub fn certificate_chain(frames: &[PayloadFrame]) -> Result<Vec<&[u8]>, CertificateAuthError> {
    let mut result = Vec::new();
    let mut total = 0;
    for frame in frames.iter().filter(|f| f.kind == 37) {
        if result.len() == MAX_CERTIFICATES || frame.body.len() > MAX_CERTIFICATE_BYTES + 1 {
            return Err(CertificateAuthError::ChainTooLarge);
        }
        let Some((&4, der)) = frame.body.split_first() else {
            return Err(CertificateAuthError::InvalidCertificate);
        };
        if !single_der_sequence(der) {
            return Err(CertificateAuthError::InvalidCertificate);
        }
        total += der.len();
        if total > MAX_CHAIN_BYTES {
            return Err(CertificateAuthError::ChainTooLarge);
        }
        if result.contains(&der) {
            return Err(CertificateAuthError::DuplicateCertificate);
        }
        result.push(der);
    }
    if result.is_empty() {
        return Err(CertificateAuthError::MissingCertificate);
    }
    Ok(result)
}

fn single_der_sequence(der: &[u8]) -> bool {
    if der.first() != Some(&0x30) || der.len() < 3 {
        return false;
    }
    let initial = der[1];
    let (header, length) = if initial < 0x80 {
        (2, usize::from(initial))
    } else {
        let count = usize::from(initial & 0x7f);
        if count == 0 || count > 3 || der.len() < count + 2 || der[2] == 0 {
            return false;
        }
        let length = der[2..2 + count]
            .iter()
            .fold(0_usize, |v, b| (v << 8) | usize::from(*b));
        if length < 128 || (count > 1 && length < (1_usize << (8 * (count - 1)))) {
            return false;
        }
        (2 + count, length)
    };
    length > 0 && header + length == der.len()
}

/// Method 1 retains the explicit legacy RSA/SHA1 wire scheme. Method 14
/// accepts only complete canonical AlgorithmIdentifiers supported by the
/// verifier. PSS and unknown parameters never become a guessed RSA variant.
pub fn certificate_signature<'a>(
    frames: &'a [PayloadFrame],
    initial_request: &[u8],
    initial_response: &[u8],
) -> Result<CertificateSignature<'a>, CertificateAuthError> {
    let mut auths = frames.iter().filter(|f| f.kind == 39);
    let auth = auths
        .next()
        .ok_or(CertificateAuthError::MissingAuthentication)?;
    if auths.next().is_some() {
        return Err(CertificateAuthError::DuplicateAuthentication);
    }
    let method = *auth
        .body
        .first()
        .ok_or(CertificateAuthError::InvalidAlgorithm)?;
    if auth.body.len() < 5 {
        return Err(CertificateAuthError::InvalidAlgorithm);
    }
    let data = &auth.body[4..]; // Reserved AUTH bytes are ignored on receipt.
    let (algorithm, signature) = match method {
        1 => (SignatureAlgorithm::RsaPkcs1Sha1, data),
        14 => {
            let length = usize::from(data[0]);
            if length == 0 || data.len() <= length + 1 {
                return Err(CertificateAuthError::InvalidAlgorithm);
            }
            let der = &data[1..length + 1];
            if !single_der_sequence(der) {
                return Err(CertificateAuthError::InvalidAlgorithm);
            }
            let algorithm = parse_algorithm(der)?;
            let offered = signature_hashes(initial_request)?;
            if offered.is_empty()
                || signature_hashes(initial_response)?.is_empty()
                || !offered.contains(&algorithm.hash_id())
            {
                return Err(CertificateAuthError::UnnegotiatedAlgorithm);
            }
            (algorithm, &data[length + 1..])
        }
        _ => return Err(CertificateAuthError::UnsupportedAlgorithm),
    };
    if signature.is_empty() || signature.len() > 8192 {
        return Err(CertificateAuthError::InvalidAlgorithm);
    }
    Ok(CertificateSignature {
        algorithm,
        signature,
    })
}

fn parse_algorithm(der: &[u8]) -> Result<SignatureAlgorithm, CertificateAuthError> {
    use SignatureAlgorithm::*;
    // RFC 7427 AlgorithmIdentifier: RSA signature OIDs with absent
    // or NULL parameters, and ECDSA signature OIDs with absent parameters.
    for (last, algorithm) in [
        (11, RsaPkcs1Sha256),
        (12, RsaPkcs1Sha384),
        (13, RsaPkcs1Sha512),
    ] {
        let mut null = vec![
            0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, last, 0x05,
            0x00,
        ];
        if der == null {
            return Ok(algorithm);
        }
        null.truncate(13);
        null[1] = 0x0b;
        if der == null {
            return Ok(algorithm);
        }
    }
    for (last, algorithm) in [(2, EcdsaSha256), (3, EcdsaSha384)] {
        if der
            == [
                0x30, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, last,
            ]
        {
            return Ok(algorithm);
        }
    }
    Err(CertificateAuthError::UnsupportedAlgorithm)
}

pub fn signature_hashes(packet: &[u8]) -> Result<Vec<u16>, CertificateAuthError> {
    let (header, body) =
        IkeHeader::decode(packet).map_err(|_| CertificateAuthError::InvalidTranscript)?;
    if header.exchange != crate::ExchangeType::IkeSaInit || header.message_id != 0 {
        return Err(CertificateAuthError::InvalidTranscript);
    }
    let frames = decode_payload_chain(header.next_payload, body)
        .map_err(|_| CertificateAuthError::InvalidTranscript)?;
    let mut result = None;
    for frame in frames.iter().filter(|f| f.kind == 41) {
        if frame.body.len() < 4 {
            return Err(CertificateAuthError::InvalidTranscript);
        }
        if u16::from_be_bytes([frame.body[2], frame.body[3]]) != SIGNATURE_HASH_ALGORITHMS {
            continue;
        }
        if result.is_some()
            || frame.body[..2] != [0, 0]
            || frame.body.len() < 6
            || (frame.body.len() - 4) % 2 != 0
            || frame.body.len() > 36
        {
            return Err(CertificateAuthError::InvalidTranscript);
        }
        result = Some(
            frame.body[4..]
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect(),
        );
    }
    Ok(result.unwrap_or_default())
}

pub fn signature_hash_offer() -> PayloadFrame {
    PayloadFrame {
        kind: 41,
        critical: false,
        body: vec![0, 0, 0x40, 0x2f, 0, 2, 0, 3, 0, 4],
    }
}

#[cfg(test)]
mod tests;
