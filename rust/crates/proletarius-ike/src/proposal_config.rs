//! Explicit capability-limited native proposals. Never drops unsupported
//! entries or substitutes algorithms from a built-in compatibility list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalProtocol {
    Ike,
    Esp,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ProposalConfigError {
    InvalidCount,
    InvalidSpi,
    InvalidProposal(usize),
    UnsupportedToken { proposal: usize, token: String },
}

/// Compatibility wrapper for the first native data-plane milestone. New
/// callers should use [`build_native_proposals`]; this entry deliberately
/// keeps rejecting CBC so an existing GCM-only caller cannot widen its
/// cryptographic contract by accident.
pub fn build_gcm_proposals(
    configured: &[String],
    protocol: ProposalProtocol,
    spi: &[u8],
) -> Result<Vec<u8>, ProposalConfigError> {
    build_proposals(configured, protocol, spi, false)
}

/// Encodes exactly the configured, runtime-supported IKE/ESP proposals.
///
/// Supported suites are AES-128/256-GCM-16 with the legacy GCM PRF set, and
/// AES-128/256-CBC with HMAC-SHA1-96 or HMAC-SHA2-256/512 plus matching PRFs.
/// IKE may use MODP-1024 only when an exact carrier profile requests group 2;
/// otherwise MODP-2048 remains the supported group. ESP is without ESN. A configured
/// list is authoritative: unsupported entries fail the complete operation
/// rather than being removed or replaced by defaults.
pub fn build_native_proposals(
    configured: &[String],
    protocol: ProposalProtocol,
    spi: &[u8],
) -> Result<Vec<u8>, ProposalConfigError> {
    build_proposals(configured, protocol, spi, true)
}

fn build_proposals(
    configured: &[String],
    protocol: ProposalProtocol,
    spi: &[u8],
    allow_cbc: bool,
) -> Result<Vec<u8>, ProposalConfigError> {
    if configured.is_empty() || configured.len() > 255 {
        return Err(ProposalConfigError::InvalidCount);
    }
    if match protocol {
        ProposalProtocol::Ike => !spi.is_empty(),
        ProposalProtocol::Esp => spi.len() != 4 || spi == [0; 4],
    } {
        return Err(ProposalConfigError::InvalidSpi);
    }
    let mut wire = Vec::new();
    for (index, raw) in configured.iter().enumerate() {
        let mut encryption = None;
        let mut integrity = None;
        let mut prf = None;
        let mut gcm_only_prf = None;
        let mut dh = None;
        let mut noesn = false;
        for token in raw.trim().to_ascii_lowercase().split('-').map(str::trim) {
            let duplicate = match token {
                "aes128gcm16" | "aes256gcm16" => encryption
                    .replace((20, if token == "aes128gcm16" { 128u16 } else { 256 }, true))
                    .is_some(),
                "aes128" | "aes256" if allow_cbc => encryption
                    .replace((12, if token == "aes128" { 128u16 } else { 256 }, false))
                    .is_some(),
                "sha256" | "sha2_256" if allow_cbc => integrity.replace(12).is_some(),
                "sha512" | "sha2_512" if allow_cbc => integrity.replace(14).is_some(),
                "sha1" if allow_cbc => integrity.replace(2).is_some(),
                "prfsha1" | "prfsha256" | "prfsha2_256" | "prfsha384" | "prfsha2_384"
                | "prfsha512" | "prfsha2_512"
                    if protocol == ProposalProtocol::Ike =>
                {
                    let id = match token {
                        "prfsha1" => 2,
                        "prfsha256" | "prfsha2_256" => 5,
                        "prfsha384" | "prfsha2_384" => 6,
                        "prfsha512" | "prfsha2_512" => 7,
                        _ => unreachable!("the match guard admits only known PRF tokens"),
                    };
                    // PRF-SHA384 remains limited to the established GCM
                    // contract. SHA1 CBC suites are enabled only when explicitly
                    // present in the exact carrier proposal.
                    if id == 6 {
                        gcm_only_prf = Some(token.to_owned());
                    }
                    prf.replace(id).is_some()
                }
                "modp1024" | "modp2048" if protocol == ProposalProtocol::Ike => {
                    let group = if token == "modp1024" { 2 } else { 14 };
                    dh.replace(group).is_some()
                }
                "noesn" if protocol == ProposalProtocol::Esp => {
                    let old = noesn;
                    noesn = true;
                    old
                }
                other => {
                    return Err(ProposalConfigError::UnsupportedToken {
                        proposal: index + 1,
                        token: other.into(),
                    });
                }
            };
            if duplicate {
                return Err(ProposalConfigError::InvalidProposal(index + 1));
            }
        }
        let (encryption_id, bits, aead) =
            encryption.ok_or(ProposalConfigError::InvalidProposal(index + 1))?;
        if aead && integrity.is_some() || !aead && integrity.is_none() {
            return Err(ProposalConfigError::InvalidProposal(index + 1));
        }
        if !aead && let Some(token) = gcm_only_prf {
            return Err(ProposalConfigError::UnsupportedToken {
                proposal: index + 1,
                token,
            });
        }
        let dh_group = if protocol == ProposalProtocol::Ike {
            let Some(group) = dh else {
                return Err(ProposalConfigError::InvalidProposal(index + 1));
            };
            if prf.is_none() {
                prf = match integrity {
                    Some(2) => Some(2),
                    Some(12) => Some(5),
                    Some(14) => Some(7),
                    _ => None,
                };
            }
            if prf.is_none() {
                return Err(ProposalConfigError::InvalidProposal(index + 1));
            }
            Some(group)
        } else {
            None
        };
        let mut transforms = vec![vec![
            3,
            0,
            0,
            12,
            1,
            0,
            0,
            encryption_id,
            0x80,
            14,
            (bits >> 8) as u8,
            bits as u8,
        ]];
        if let Some(integrity) = integrity {
            transforms.push(vec![3, 0, 0, 8, 3, 0, 0, integrity]);
        }
        if protocol == ProposalProtocol::Ike {
            transforms.push(vec![3, 0, 0, 8, 2, 0, 0, prf.unwrap()]);
            transforms.push(vec![0, 0, 0, 8, 4, 0, 0, dh_group.unwrap()]);
        } else {
            transforms.push(vec![0, 0, 0, 8, 5, 0, 0, 0]);
        }
        let len = 8 + spi.len() + transforms.iter().map(Vec::len).sum::<usize>();
        wire.extend_from_slice(&[
            if index + 1 == configured.len() { 0 } else { 2 },
            0,
            (len >> 8) as u8,
            len as u8,
            (index + 1) as u8,
            if protocol == ProposalProtocol::Ike {
                1
            } else {
                3
            },
            spi.len() as u8,
            transforms.len() as u8,
        ]);
        wire.extend_from_slice(spi);
        for transform in transforms {
            wire.extend(transform);
        }
    }
    Ok(wire)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_ike_matches_go_vector_and_keeps_order() {
        let input = vec!["aes128gcm16-prfsha256-modp2048".into()];
        assert_eq!(
            build_gcm_proposals(&input, ProposalProtocol::Ike, &[]).unwrap(),
            [
                0, 0, 0, 36, 1, 1, 0, 3, 3, 0, 0, 12, 1, 0, 0, 20, 128, 14, 0, 128, 3, 0, 0, 8, 2,
                0, 0, 5, 0, 0, 0, 8, 4, 0, 0, 14
            ]
        );
        let inputs = vec!["aes256gcm16-prfsha512-modp2048".into(), input[0].clone()];
        let wire = build_gcm_proposals(&inputs, ProposalProtocol::Ike, &[]).unwrap();
        let proposals = crate::decode_sa_proposals(&wire).unwrap();
        assert_eq!(proposals.len(), 2);
        assert_eq!(proposals[0].number, 1);
        assert_eq!(proposals[1].number, 2);
        assert_eq!(proposals[0].transforms[0].attributes[0].value, [1, 0]);
        assert_eq!(proposals[1].transforms[0].attributes[0].value, [0, 128]);
        for invalid in [
            "aes128-sha256-modp2048",
            "aes192gcm16-prfsha256-modp2048",
            "aes128gcm16-modp2048",
            "aes128gcm16-prfsha256-modp2048-modp2048",
        ] {
            assert!(build_gcm_proposals(&[invalid.into()], ProposalProtocol::Ike, &[]).is_err());
        }
        assert!(
            build_gcm_proposals(
                &["aes128gcm16-esn".into()],
                ProposalProtocol::Esp,
                &[0, 0, 0, 1]
            )
            .is_err()
        );
        assert!(build_gcm_proposals(&[], ProposalProtocol::Ike, &[]).is_err());
    }

    #[test]
    fn native_gcm_preserves_legacy_sha1_and_sha384_prfs_exactly() {
        let cases = [
            (
                "aes128gcm16-prfsha1-modp2048",
                [
                    0, 0, 0, 36, 1, 1, 0, 3, 3, 0, 0, 12, 1, 0, 0, 20, 128, 14, 0, 128, 3, 0, 0, 8,
                    2, 0, 0, 2, 0, 0, 0, 8, 4, 0, 0, 14,
                ],
            ),
            (
                "aes256gcm16-prfsha384-modp2048",
                [
                    0, 0, 0, 36, 1, 1, 0, 3, 3, 0, 0, 12, 1, 0, 0, 20, 128, 14, 1, 0, 3, 0, 0, 8,
                    2, 0, 0, 6, 0, 0, 0, 8, 4, 0, 0, 14,
                ],
            ),
        ];
        for (configured, expected) in cases {
            let configured = vec![configured.to_owned()];
            let compatibility =
                build_gcm_proposals(&configured, ProposalProtocol::Ike, &[]).unwrap();
            let native = build_native_proposals(&configured, ProposalProtocol::Ike, &[]).unwrap();
            assert_eq!(native, expected, "wrong wire encoding for {configured:?}");
            assert_eq!(
                native, compatibility,
                "native path regressed {configured:?}"
            );
        }
    }

    #[test]
    fn native_cbc_sha2_proposals_match_go_wire_format() {
        assert_eq!(
            build_native_proposals(
                &["aes128-sha256-modp2048".into()],
                ProposalProtocol::Ike,
                &[],
            )
            .unwrap(),
            [
                0, 0, 0, 44, 1, 1, 0, 4, 3, 0, 0, 12, 1, 0, 0, 12, 128, 14, 0, 128, 3, 0, 0, 8, 3,
                0, 0, 12, 3, 0, 0, 8, 2, 0, 0, 5, 0, 0, 0, 8, 4, 0, 0, 14,
            ]
        );
        assert_eq!(
            build_native_proposals(
                &["aes256-sha512".into()],
                ProposalProtocol::Esp,
                &[0x11, 0x22, 0x33, 0x44],
            )
            .unwrap(),
            [
                0, 0, 0, 40, 1, 3, 4, 3, 0x11, 0x22, 0x33, 0x44, 3, 0, 0, 12, 1, 0, 0, 12, 128, 14,
                1, 0, 3, 0, 0, 8, 3, 0, 0, 14, 0, 0, 0, 8, 5, 0, 0, 0,
            ]
        );
    }

    #[test]
    fn exact_group2_sha1_carrier_suite_is_encoded_without_fallback() {
        let wire = build_native_proposals(
            &["aes128-sha1-prfsha1-modp1024".into()],
            ProposalProtocol::Ike,
            &[],
        )
        .unwrap();
        let proposals = crate::decode_sa_proposals(&wire).unwrap();
        let transforms = &proposals[0].transforms;
        assert_eq!(
            transforms
                .iter()
                .map(|transform| (transform.kind, transform.id))
                .collect::<Vec<_>>(),
            [(1, 12), (3, 2), (2, 2), (4, 2)]
        );
        assert!(
            build_native_proposals(
                &["aes128-sha1-prfsha1-modp2048".into()],
                ProposalProtocol::Ike,
                &[],
            )
            .is_ok()
        );
        assert!(
            build_gcm_proposals(
                &["aes128-sha1-prfsha1-modp1024".into()],
                ProposalProtocol::Ike,
                &[],
            )
            .is_err()
        );
    }

    #[test]
    fn native_proposals_reject_partial_or_unimplemented_cbc_suites() {
        for (configured, token) in [("aes128-sha256-prfsha384-modp2048", "prfsha384")] {
            assert_eq!(
                build_native_proposals(&[configured.into()], ProposalProtocol::Ike, &[]),
                Err(ProposalConfigError::UnsupportedToken {
                    proposal: 1,
                    token: token.into(),
                }),
            );
        }
        for invalid in [
            "aes128-modp2048",
            "aes192-sha256-prfsha256-modp2048",
            "aes128-sha256-sha512-prfsha256-modp2048",
            "aes128gcm16-sha256-prfsha256-modp2048",
            "aes128gcm16-prfsha256-modp3072",
        ] {
            assert!(
                build_native_proposals(&[invalid.into()], ProposalProtocol::Ike, &[]).is_err(),
                "accepted {invalid}"
            );
        }
        assert!(
            build_native_proposals(
                &["aes256-sha512-esn".into()],
                ProposalProtocol::Esp,
                &[0, 0, 0, 1],
            )
            .is_err()
        );
    }
}
