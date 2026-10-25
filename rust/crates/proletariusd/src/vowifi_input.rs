//! Canonical VoWiFi user-input boundary shared by start and diagnostics.
//! Swift transports the raw fields; only Rust decides whether and how they
//! become a proxy endpoint or ePDG port.

pub(crate) fn normalize_proxy(
    legacy_endpoint: &str,
    raw_host: Option<&str>,
    raw_port: Option<&str>,
) -> Result<String, &'static str> {
    if raw_host.is_none() && raw_port.is_none() {
        let endpoint = legacy_endpoint.trim();
        if endpoint.is_empty() {
            return Ok(String::new());
        }
        let endpoint = endpoint
            .strip_prefix("socks5://")
            .or_else(|| endpoint.strip_prefix("socks5h://"))
            .unwrap_or(endpoint);
        let (host, port) = split_legacy_endpoint(endpoint)?;
        return canonical_proxy(host, port);
    }
    let host = raw_host.unwrap_or_default().trim();
    let port = raw_port.unwrap_or_default().trim();
    if host.is_empty() && port.is_empty() {
        return Ok(String::new());
    }
    if host.is_empty() || port.is_empty() {
        return Err("前置代理地址无效，请同时填写合法主机和端口");
    }
    canonical_proxy(host, port)
}

fn split_legacy_endpoint(endpoint: &str) -> Result<(&str, &str), &'static str> {
    if let Some(rest) = endpoint.strip_prefix('[') {
        let (host, suffix) = rest
            .split_once(']')
            .ok_or("前置代理地址无效，请同时填写合法主机和端口")?;
        let port = suffix
            .strip_prefix(':')
            .ok_or("前置代理地址无效，请同时填写合法主机和端口")?;
        return Ok((host, port));
    }
    endpoint
        .rsplit_once(':')
        .ok_or("前置代理地址无效，请同时填写合法主机和端口")
}

fn canonical_proxy(host: &str, port: &str) -> Result<String, &'static str> {
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|value| *value != 0)
        .ok_or("前置代理端口无效")?;
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let host = if let Ok(address) = host.parse::<std::net::IpAddr>() {
        match address {
            std::net::IpAddr::V4(address) => address.to_string(),
            std::net::IpAddr::V6(address) => format!("[{address}]"),
        }
    } else if crate::transport_selection::valid_hostname(host) {
        host.trim_end_matches('.').to_ascii_lowercase()
    } else {
        return Err("前置代理地址无效，请同时填写合法主机和端口");
    };
    Ok(format!("socks5://{host}:{port}"))
}

pub(crate) fn normalize_epdg(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    if !crate::transport_selection::valid_hostname(value) {
        return Err("ePDG 地址无效");
    }
    Ok(value.to_owned())
}

pub(crate) fn normalize_epdg_port(legacy: i64, raw: Option<&str>) -> Result<i64, &'static str> {
    let Some(raw) = raw else { return Ok(legacy) };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(0);
    }
    raw.parse::<u16>()
        .ok()
        .filter(|value| *value != 0)
        .map(i64::from)
        .ok_or("ePDG 端口无效（1-65535）")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_fields_have_one_rust_canonical_form() {
        for (host, port, expected) in [
            ("", "", ""),
            ("127.0.0.1", "1080", "socks5://127.0.0.1:1080"),
            ("proxy.example", "1081", "socks5://proxy.example:1081"),
            ("2001:db8::1", "1082", "socks5://[2001:db8::1]:1082"),
            ("[2001:db8::2]", "1083", "socks5://[2001:db8::2]:1083"),
        ] {
            assert_eq!(
                normalize_proxy("ignored", Some(host), Some(port)).unwrap(),
                expected
            );
        }
        for (legacy, expected) in [
            ("127.0.0.1:1080", "socks5://127.0.0.1:1080"),
            (
                "socks5h://PROXY.EXAMPLE.:1081",
                "socks5://proxy.example:1081",
            ),
            (
                "socks5://[2001:0db8::1]:1082",
                "socks5://[2001:db8::1]:1082",
            ),
        ] {
            assert_eq!(normalize_proxy(legacy, None, None).unwrap(), expected);
        }
        for (host, port) in [
            ("proxy.example", ""),
            ("", "1080"),
            ("socks5://proxy.example", "1080"),
            ("proxy example", "1080"),
            ("proxy.example", "0"),
            ("proxy.example", "65536"),
        ] {
            assert!(
                normalize_proxy("", Some(host), Some(port)).is_err(),
                "{host}:{port}"
            );
        }
        for legacy in [
            "http://proxy.example:1080",
            "proxy example:1080",
            "proxy.example:0",
            "proxy.example:65536",
            "proxy.example",
        ] {
            assert!(normalize_proxy(legacy, None, None).is_err(), "{legacy}");
        }
    }

    #[test]
    fn epdg_and_port_are_fail_closed() {
        assert_eq!(
            normalize_epdg(" wlan.example.test ").unwrap(),
            "wlan.example.test"
        );
        assert!(normalize_epdg("https://wlan.example.test").is_err());
        assert_eq!(normalize_epdg_port(500, None).unwrap(), 500);
        assert_eq!(normalize_epdg_port(500, Some(" ")).unwrap(), 0);
        assert_eq!(normalize_epdg_port(0, Some("4500")).unwrap(), 4500);
        assert!(normalize_epdg_port(0, Some("65536")).is_err());
    }
}
