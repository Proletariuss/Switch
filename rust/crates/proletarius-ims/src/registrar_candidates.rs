use std::net::IpAddr;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterCandidate {
    pub registrar: String,
    pub transport: String,
}

/// Expands the configured IMS registrar list into ordered registrar/transport
/// pairs. The local address may be None; this function never performs DNS.
pub fn expand_register_candidates(
    addresses: &[String],
    local_ip: Option<&str>,
) -> Result<Vec<RegisterCandidate>, &'static str> {
    if addresses.len() > 64 || local_ip.is_some_and(|value| value.len() > 64) {
        return Err("REGISTER candidate input exceeds limits");
    }
    if addresses.iter().any(|value| value.len() > 512) {
        return Err("REGISTER candidate input exceeds limits");
    }

    let local_ip = local_ip
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(IpAddr::from_str)
        .transpose()
        .map_err(|_| "REGISTER local IP is invalid")?;

    let mut deduped = Vec::with_capacity(addresses.len());
    for address in addresses {
        let address = address.trim();
        if !address.is_empty() && !deduped.iter().any(|seen| seen == address) {
            deduped.push(address.to_owned());
        }
    }
    if deduped.is_empty() {
        return Ok(Vec::new());
    }

    let (mut primary, mut fallback) = split_by_family(&deduped, local_ip);
    if primary.is_empty() {
        primary.clone_from(&deduped);
    }
    let gateways = ranked_gateways(&deduped, local_ip);
    let mut out =
        Vec::with_capacity(primary.len().saturating_mul(gateways.len().max(1)) + fallback.len());

    for registrar in primary {
        if host_is_local(&registrar, local_ip) {
            if gateways.is_empty() {
                push_unique(&mut out, &registrar, &registrar);
            } else {
                for gateway in &gateways {
                    push_unique(&mut out, &registrar, gateway);
                }
            }
        } else {
            push_unique(&mut out, &registrar, &registrar);
        }
    }

    for registrar in fallback.drain(..) {
        if !host_is_local(&registrar, local_ip) {
            push_unique(&mut out, &registrar, &registrar);
        }
    }
    Ok(out)
}

fn split_by_family(addresses: &[String], local_ip: Option<IpAddr>) -> (Vec<String>, Vec<String>) {
    let mut primary = Vec::new();
    let mut fallback = Vec::new();
    for address in addresses {
        if matches_family(address, local_ip) {
            primary.push(address.clone());
        } else {
            fallback.push(address.clone());
        }
    }
    (primary, fallback)
}

fn matches_family(address: &str, local_ip: Option<IpAddr>) -> bool {
    let Some(local_ip) = local_ip else {
        return true;
    };
    parse_host_ip(address).is_some_and(|candidate| candidate.is_ipv4() == local_ip.is_ipv4())
}

fn host_is_local(address: &str, local_ip: Option<IpAddr>) -> bool {
    local_ip.is_some() && parse_host_ip(address) == local_ip
}

fn parse_host_ip(address: &str) -> Option<IpAddr> {
    let address = address.trim();
    let host = if let Some(rest) = address.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']')?;
        if !suffix.starts_with(':') || suffix.len() == 1 {
            return None;
        }
        host
    } else {
        let (host, port) = address.rsplit_once(':')?;
        if host.is_empty() || port.is_empty() || host.contains(':') {
            return None;
        }
        host
    };
    IpAddr::from_str(host).ok()
}

fn ranked_gateways(addresses: &[String], local_ip: Option<IpAddr>) -> Vec<String> {
    let mut candidates: Vec<(String, i32)> = addresses
        .iter()
        .filter(|address| !host_is_local(address, local_ip))
        .filter_map(|address| {
            let host = address_host(address)?;
            Some((address.clone(), gateway_score(&host.to_ascii_lowercase())))
        })
        .collect();
    candidates.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let mut seen = Vec::new();
    candidates
        .into_iter()
        .filter_map(|(address, _)| {
            if seen.iter().any(|value| value == &address) {
                None
            } else {
                seen.push(address.clone());
                Some(address)
            }
        })
        .collect()
}

fn address_host(address: &str) -> Option<String> {
    let address = address.trim();
    if let Some(rest) = address.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']')?;
        if !suffix.starts_with(':') || suffix.len() == 1 {
            return None;
        }
        return Some(host.to_owned());
    }
    let (host, port) = address.rsplit_once(':')?;
    if host.is_empty() || port.is_empty() || host.contains(':') {
        return None;
    }
    Some(host.to_owned())
}

fn gateway_score(host: &str) -> i32 {
    if host.starts_with("2a03:dd00:1f80:") {
        100
    } else if host.starts_with("2a03:dd00:1f81:810:") {
        80
    } else if host.starts_with("2a03:dd00:1f81:10:") || host.starts_with("2a03:dd00:1f81:5010:") {
        10
    } else if host.starts_with("2a03:dd00:1f81:") {
        40
    } else {
        30
    }
}

fn push_unique(out: &mut Vec<RegisterCandidate>, registrar: &str, transport: &str) {
    if !out
        .iter()
        .any(|candidate| candidate.registrar == registrar && candidate.transport == transport)
    {
        out.push(RegisterCandidate {
            registrar: registrar.to_owned(),
            transport: transport.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(value: &str) -> String {
        value.to_owned()
    }

    #[test]
    fn local_registrar_uses_ranked_ike_gateways_and_keeps_family_order() {
        let local = "2a03:dd00:1381:7dce:fb36:ef7b:d917:548b";
        let values = [
            address("[2a03:dd00:1381:7dce:fb36:ef7b:d917:548b]:5060"),
            address("[2a03:dd00:1f81:3010::4]:5060"),
            address("[2a03:dd00:1f80:4860::4]:5060"),
        ];
        let got = expand_register_candidates(&values, Some(local)).unwrap();
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].registrar, values[0]);
        assert_eq!(got[0].transport, values[2]);
        assert_eq!(got[1].transport, values[1]);
        assert_eq!(got[2].registrar, values[1]);
        assert_eq!(got[2].transport, values[1]);
        assert_eq!(got[3].registrar, values[2]);
        assert_eq!(got[3].transport, values[2]);
    }

    #[test]
    fn normal_registrar_pairs_and_input_deduplication_preserve_first_order() {
        let values = [
            address("192.0.2.10:5060"),
            address("192.0.2.10:5060"),
            address("registrar.example:5060"),
        ];
        let got = expand_register_candidates(&values, Some("192.0.2.1")).unwrap();
        assert_eq!(
            got,
            vec![
                RegisterCandidate {
                    registrar: values[0].clone(),
                    transport: values[0].clone()
                },
                RegisterCandidate {
                    registrar: values[2].clone(),
                    transport: values[2].clone()
                },
            ]
        );
    }

    #[test]
    fn invalid_local_ip_and_oversized_inputs_fail_closed() {
        assert!(expand_register_candidates(&[], Some("bad-ip")).is_err());
        assert!(expand_register_candidates(&vec!["x".repeat(513)], None).is_err());
        assert!(expand_register_candidates(&vec!["x:5060".to_owned(); 65], None).is_err());
    }
}
