//! Negotiated IMS transport policy, not an installed security association.
use crate::{aka_exchange::SecurityKeys, security_esp::Suite, security_offer::SecurityOffer};
use std::{
    net::IpAddr,
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};
use zeroize::Zeroizing;

static NEXT_BINDING_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SecurityBindingId(NonZeroU64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flow {
    pub outbound_spi: u32,
    pub inbound_spi: u32,
    pub local_port: u16,
    pub remote_port: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalEndpoints {
    pub spi_c: u32,
    pub spi_s: u32,
    pub port_c: u16,
    pub port_s: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecurityBinding {
    binding_id: SecurityBindingId,
    pub local_ip: IpAddr,
    pub remote_ip: IpAddr,
    pub client_flow: Flow,
    pub server_flow: Flow,
}
pub struct SecurityPolicy {
    binding_id: SecurityBindingId,
    pub(crate) local_ip: IpAddr,
    pub(crate) remote_ip: IpAddr,
    pub(crate) client_flow: Flow,
    pub(crate) server_flow: Flow,
    pub(crate) suite: Suite,
    pub(crate) encryption_key: Zeroizing<Vec<u8>>,
    pub(crate) integrity_key: Zeroizing<Vec<u8>>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum PolicyError {
    UnsupportedTransform,
    InvalidAddress,
    InvalidEndpoints,
    BindingIdExhausted,
}

impl SecurityPolicy {
    pub fn suite(&self) -> Suite {
        self.suite
    }
    pub fn binding(&self) -> SecurityBinding {
        SecurityBinding {
            binding_id: self.binding_id,
            local_ip: self.local_ip,
            remote_ip: self.remote_ip,
            client_flow: self.client_flow,
            server_flow: self.server_flow,
        }
    }

    /// Existing fixed-suite API, retained for callers that explicitly require AES/SHA1.
    pub fn aes_sha1(
        local_ip: IpAddr,
        remote_ip: IpAddr,
        local: LocalEndpoints,
        remote: &SecurityOffer,
        keys: SecurityKeys,
    ) -> Result<Self, PolicyError> {
        if remote.algorithm != "hmac-sha-1-96" || remote.encryption != "aes-cbc" {
            return Err(PolicyError::UnsupportedTransform);
        }
        Self::negotiated(local_ip, remote_ip, local, remote, keys)
    }
    /// Install exactly the selected, supported IMS suite. This never chooses
    /// another offer or falls back to plaintext after a negotiation failure.
    pub fn negotiated(
        local_ip: IpAddr,
        remote_ip: IpAddr,
        local: LocalEndpoints,
        remote: &SecurityOffer,
        keys: SecurityKeys,
    ) -> Result<Self, PolicyError> {
        if remote.protocol != "esp" || remote.mode != "trans" {
            return Err(PolicyError::UnsupportedTransform);
        }
        let suite = Suite::selected(&remote.encryption, &remote.algorithm)
            .map_err(|_| PolicyError::UnsupportedTransform)?;
        let valid = |ip: IpAddr| {
            !ip.is_unspecified()
                && !ip.is_multicast()
                && ip != IpAddr::V4(std::net::Ipv4Addr::BROADCAST)
        };
        if local_ip.is_ipv4() != remote_ip.is_ipv4()
            || !valid(local_ip)
            || !valid(remote_ip)
            || local_ip == remote_ip
        {
            return Err(PolicyError::InvalidAddress);
        }
        if [local.spi_c, local.spi_s, remote.spi_c, remote.spi_s].contains(&0)
            || [local.port_c, local.port_s, remote.port_c, remote.port_s].contains(&0)
            || local.spi_c == local.spi_s
            || remote.spi_c == remote.spi_s
        {
            return Err(PolicyError::InvalidEndpoints);
        }
        let binding_id = next_binding_id()?;
        let (encryption_key, integrity_key) = suite.derive(&keys.ck, &keys.ik);
        Ok(Self {
            binding_id,
            local_ip,
            remote_ip,
            client_flow: Flow {
                outbound_spi: remote.spi_s,
                inbound_spi: local.spi_c,
                local_port: local.port_c,
                remote_port: remote.port_s,
            },
            server_flow: Flow {
                outbound_spi: remote.spi_c,
                inbound_spi: local.spi_s,
                local_port: local.port_s,
                remote_port: remote.port_c,
            },
            suite,
            encryption_key,
            integrity_key,
        })
    }
}

fn next_binding_id() -> Result<SecurityBindingId, PolicyError> {
    let current = NEXT_BINDING_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| PolicyError::BindingIdExhausted)?;
    NonZeroU64::new(current)
        .map(SecurityBindingId)
        .ok_or(PolicyError::BindingIdExhausted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_go_direction_and_key_mapping() {
        let remote = crate::security_offer::parse_security_server(
            "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;spi-c=111;spi-s=222;port-c=6050;port-s=6060",
        )
        .unwrap()
        .remove(0);
        let local = || LocalEndpoints {
            spi_c: 333,
            spi_s: 444,
            port_c: 5050,
            port_s: 5060,
        };
        let keys = || SecurityKeys {
            ck: Zeroizing::new([0x11; 16]),
            ik: Zeroizing::new([0x22; 16]),
        };
        let own = "10.0.0.2".parse().unwrap();
        let peer = "10.0.0.9".parse().unwrap();
        let policy = SecurityPolicy::aes_sha1(own, peer, local(), &remote, keys()).unwrap();
        assert_eq!(
            policy.client_flow,
            Flow {
                outbound_spi: 222,
                inbound_spi: 333,
                local_port: 5050,
                remote_port: 6060
            }
        );
        assert_eq!(
            policy.server_flow,
            Flow {
                outbound_spi: 111,
                inbound_spi: 444,
                local_port: 5060,
                remote_port: 6050
            }
        );
        assert_eq!(policy.encryption_key.as_slice(), &[0x11; 16]);
        assert_eq!(&policy.integrity_key[..16], &[0x22; 16]);
        assert_eq!(&policy.integrity_key[16..], &[0; 4]);
        assert!(matches!(
            SecurityPolicy::aes_sha1(own, "::1".parse().unwrap(), local(), &remote, keys()),
            Err(PolicyError::InvalidAddress)
        ));
        let mut invalid = local();
        invalid.spi_s = invalid.spi_c;
        assert!(matches!(
            SecurityPolicy::aes_sha1(own, peer, invalid, &remote, keys()),
            Err(PolicyError::InvalidEndpoints)
        ));

        let first = SecurityPolicy::aes_sha1(own, peer, local(), &remote, keys()).unwrap();
        let second = SecurityPolicy::aes_sha1(own, peer, local(), &remote, keys()).unwrap();
        assert_ne!(
            first.binding(),
            second.binding(),
            "each prepared registration must carry unique installation evidence"
        );
    }

    #[test]
    fn selected_suites_are_exact_and_unsupported_negotiation_does_not_fallback() {
        use crate::security_offer::{ClientSecurityMechanism, negotiate_security_server};
        let local = || LocalEndpoints {
            spi_c: 333,
            spi_s: 444,
            port_c: 5050,
            port_s: 5060,
        };
        let keys = || SecurityKeys {
            ck: Zeroizing::new([0x11; 16]),
            ik: Zeroizing::new([0x22; 16]),
        };
        let own = "10.0.0.2".parse().unwrap();
        let peer = "10.0.0.9".parse().unwrap();
        for enc in ["aes-cbc", "des-ede3-cbc", "null"] {
            for auth in ["hmac-sha-1-96", "hmac-md5-96"] {
                let offer = crate::security_offer::parse_security_server(&format!(
                    "ipsec-3gpp;alg={auth};ealg={enc};spi-c=111;spi-s=222;port-c=6050;port-s=6060"
                ))
                .unwrap()
                .remove(0);
                let policy =
                    SecurityPolicy::negotiated(own, peer, local(), &offer, keys()).unwrap();
                assert_eq!(policy.suite(), Suite::selected(enc, auth).unwrap());
                assert_eq!(
                    policy.encryption_key.len(),
                    policy.suite().encryption_key_len()
                );
                assert_eq!(
                    policy.integrity_key.len(),
                    policy.suite().integrity_key_len()
                );
                if enc != "aes-cbc" || auth != "hmac-sha-1-96" {
                    assert!(matches!(
                        SecurityPolicy::aes_sha1(own, peer, local(), &offer, keys()),
                        Err(PolicyError::UnsupportedTransform)
                    ));
                }
            }
        }
        let offered = "ipsec-3gpp;q=1;alg=hmac-sha-256-128;ealg=aes-cbc;spi-c=111;spi-s=222;port-c=6050;port-s=6060,ipsec-3gpp;q=0.1;alg=hmac-md5-96;ealg=null;spi-c=111;spi-s=222;port-c=6050;port-s=6060";
        let mechanisms = [ClientSecurityMechanism {
            algorithm: "hmac-sha-256-128",
            encryption: "aes-cbc",
            protocol: Some("esp"),
            mode: Some("trans"),
        }];
        let selected = negotiate_security_server(offered, &mechanisms, true).unwrap();
        assert_eq!(selected.selected.algorithm, "hmac-sha-256-128");
        assert!(matches!(
            SecurityPolicy::negotiated(own, peer, local(), &selected.selected, keys()),
            Err(PolicyError::UnsupportedTransform)
        ));
        for (prot, mode) in [("ah", "trans"), ("esp", "tun")] {
            let mut offer = selected.selected.clone();
            offer.algorithm = "hmac-sha-1-96".into();
            offer.protocol = prot.into();
            offer.mode = mode.into();
            assert!(matches!(
                SecurityPolicy::negotiated(own, peer, local(), &offer, keys()),
                Err(PolicyError::UnsupportedTransform)
            ));
        }
    }
}
