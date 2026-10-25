//! RFC 3310 AKAv1-MD5 math. No SIM IO, algorithm downgrade or secret logging.
use base64::{Engine, engine::general_purpose::STANDARD};
use md5::{Digest, Md5};
use zeroize::Zeroizing;

pub enum AkaPassword<'a> {
    Res(&'a [u8]),
    /// Raw RES bytes from the current verified EAP_DIRECT exchange. The
    /// legacy REGISTER path accepted 1..=16 bytes; keep that scoped here.
    VerifiedEapRes(&'a [u8]),
    SynchronizationFailure(&'a [u8; 14]),
}
pub enum DigestQop<'a> {
    None,
    Auth {
        cnonce: &'a str,
        count: u32,
    },
    AuthInt {
        cnonce: &'a str,
        count: u32,
        body: &'a [u8],
    },
}
pub struct AkaDigestInput<'a> {
    pub algorithm: &'a str,
    pub username: &'a str,
    pub realm: &'a str,
    pub nonce: &'a str,
    pub method: &'a str,
    pub uri: &'a str,
    pub qop: DigestQop<'a>,
}
pub struct AkaDigestResult {
    pub response: Zeroizing<String>,
    pub auts: Option<Zeroizing<String>>,
}
#[derive(Debug, Eq, PartialEq)]
pub enum DigestError {
    UnsupportedAlgorithm,
    InvalidInput,
    InvalidNonce,
    InvalidRes,
    InvalidQop,
}

fn hash(parts: &[&[u8]]) -> Zeroizing<String> {
    let mut digest = Md5::new();
    for part in parts {
        digest.update(part);
    }
    Zeroizing::new(format!("{:x}", digest.finalize()))
}

pub fn compute_aka_digest(
    input: &AkaDigestInput<'_>,
    password: AkaPassword<'_>,
) -> Result<AkaDigestResult, DigestError> {
    if !input.algorithm.eq_ignore_ascii_case("AKAv1-MD5") {
        return Err(DigestError::UnsupportedAlgorithm);
    }
    for value in [
        input.username,
        input.realm,
        input.nonce,
        input.method,
        input.uri,
    ] {
        if value.is_empty() || value.len() > 4096 || value.bytes().any(|b| b.is_ascii_control()) {
            return Err(DigestError::InvalidInput);
        }
    }
    crate::aka_nonce::decode_aka_nonce(input.nonce).map_err(|_| DigestError::InvalidNonce)?;
    let (res, auts) = match password {
        AkaPassword::Res(res) => {
            if !(4..=16).contains(&res.len()) {
                return Err(DigestError::InvalidRes);
            }
            (res, None)
        }
        AkaPassword::VerifiedEapRes(res) => {
            if !(1..=16).contains(&res.len()) {
                return Err(DigestError::InvalidRes);
            }
            (res, None)
        }
        AkaPassword::SynchronizationFailure(auts) => {
            (b"".as_slice(), Some(Zeroizing::new(STANDARD.encode(auts))))
        }
    };
    // RES is the original octet string, NOT hex/base64 text (including NUL).
    let ha1 = hash(&[
        input.username.as_bytes(),
        b":",
        input.realm.as_bytes(),
        b":",
        res,
    ]);
    let ha2 = match &input.qop {
        DigestQop::AuthInt { body, .. } => {
            if body.len() > 65_536 {
                return Err(DigestError::InvalidInput);
            }
            let entity = hash(&[body]);
            hash(&[
                input.method.as_bytes(),
                b":",
                input.uri.as_bytes(),
                b":",
                entity.as_bytes(),
            ])
        }
        _ => hash(&[input.method.as_bytes(), b":", input.uri.as_bytes()]),
    };
    let response = match &input.qop {
        DigestQop::None => hash(&[
            ha1.as_bytes(),
            b":",
            input.nonce.as_bytes(),
            b":",
            ha2.as_bytes(),
        ]),
        DigestQop::Auth { cnonce, count } | DigestQop::AuthInt { cnonce, count, .. } => {
            if *count == 0
                || cnonce.is_empty()
                || cnonce.len() > 256
                || !cnonce.bytes().all(|b| b.is_ascii_graphic())
            {
                return Err(DigestError::InvalidQop);
            }
            let count = format!("{count:08x}");
            let qop = if matches!(input.qop, DigestQop::Auth { .. }) {
                b"auth".as_slice()
            } else {
                b"auth-int".as_slice()
            };
            hash(&[
                ha1.as_bytes(),
                b":",
                input.nonce.as_bytes(),
                b":",
                count.as_bytes(),
                b":",
                cnonce.as_bytes(),
                b":",
                qop,
                b":",
                ha2.as_bytes(),
            ])
        }
    };
    Ok(AkaDigestResult { response, auts })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_res_and_sync_failure_match_independent_digest_fixture() {
        let nonce = "01".repeat(32);
        let mut input = AkaDigestInput {
            algorithm: "AKAv1-MD5",
            username: "user@example.test",
            realm: "ims.example.test",
            nonce: &nonce,
            method: "REGISTER",
            uri: "sip:ims.example.test",
            qop: DigestQop::None,
        };
        let result =
            compute_aka_digest(&input, AkaPassword::Res(&[0xde, 0xad, 0, 0xbe, 0xef])).unwrap();
        // Fixture is checked against the deployed Go digest implementation.
        assert_eq!(result.response.as_str(), "daa545a7774d3ac56e434f9bdd42beea");
        assert!(result.auts.is_none());
        let sync =
            compute_aka_digest(&input, AkaPassword::SynchronizationFailure(&[0x5a; 14])).unwrap();
        assert_eq!(sync.auts.unwrap().as_str(), "WlpaWlpaWlpaWlpaWlo=");
        assert_ne!(result.response, sync.response);
        for (qop, res, sync) in [
            (
                DigestQop::None,
                "daa545a7774d3ac56e434f9bdd42beea",
                "1add6ee096dfebc41cb5acd8bddda4b4",
            ),
            (
                DigestQop::Auth {
                    cnonce: "fixed-client-nonce",
                    count: 1,
                },
                "ec22021f8bb98a4b59c216988c2a1fe6",
                "8a308c6d561c8a53ebba45c351d8033b",
            ),
            (
                DigestQop::AuthInt {
                    cnonce: "fixed-client-nonce",
                    count: 1,
                    body: b"body\0data",
                },
                "fa9dd8b3612bd63fafaaa84202325a99",
                "37182df5cdf94b56b34616a5ceca9e28",
            ),
        ] {
            input.qop = qop;
            assert_eq!(
                compute_aka_digest(&input, AkaPassword::Res(&[0xde, 0xad, 0, 0xbe, 0xef]))
                    .unwrap()
                    .response
                    .as_str(),
                res
            );
            assert_eq!(
                compute_aka_digest(&input, AkaPassword::SynchronizationFailure(&[0x5a; 14]))
                    .unwrap()
                    .response
                    .as_str(),
                sync
            );
        }
        input.algorithm = "MD5";
        assert!(matches!(
            compute_aka_digest(&input, AkaPassword::Res(&[1; 8])),
            Err(DigestError::UnsupportedAlgorithm)
        ));
        input.algorithm = "AKAv2-MD5";
        assert!(matches!(
            compute_aka_digest(&input, AkaPassword::Res(&[1; 8])),
            Err(DigestError::UnsupportedAlgorithm)
        ));
    }
}
