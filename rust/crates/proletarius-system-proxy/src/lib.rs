use std::collections::HashMap;

use url::Url;

const OUTPUT_CAPACITY: usize = 2048;

#[derive(Clone, Eq, PartialEq)]
pub enum Decision {
    Direct,
    Proxy(Url),
}

impl std::fmt::Debug for Decision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => formatter.write_str("Direct"),
            Self::Proxy(_) => formatter.write_str("Proxy([REDACTED])"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolveError {
    NotFound,
    NativeResolutionFailed,
    InvalidConfiguration,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotFound => "system_proxy_not_found",
            Self::NativeResolutionFailed => "system_proxy_resolution_failed",
            Self::InvalidConfiguration => "telegram_proxy_invalid",
        })
    }
}

impl std::error::Error for ResolveError {}

/// Resolve the same HTTPS target used by Telegram. Environment proxies take
/// precedence; otherwise macOS CFNetwork evaluates static settings and PAC.
/// The returned URL may contain credentials, so its Debug output is redacted.
pub fn resolve_for_url(target: &str) -> Result<Decision, ResolveError> {
    let target_url = Url::parse(target).map_err(|_| ResolveError::InvalidConfiguration)?;
    let env = proxy_environment();
    let env_proxy = first_environment_proxy(&env);
    let decision = if let Some(value) = env_proxy {
        Decision::Proxy(parse_proxy_url(&normalize_proxy_url(value))?)
    } else {
        native_decision(target)?
    };
    if matches_no_proxy(&target_url, &no_proxy_list(&env)) {
        return Ok(Decision::Direct);
    }
    Ok(decision)
}

fn proxy_environment() -> HashMap<String, String> {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "NO_PROXY",
        "no_proxy",
    ]
    .into_iter()
    .filter_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| (name.to_owned(), value))
    })
    .collect()
}

fn first_environment_proxy(environment: &HashMap<String, String>) -> Option<&str> {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .into_iter()
    .filter_map(|name| environment.get(name))
    .map(String::as_str)
    .find(|value| !value.trim().is_empty())
}

fn no_proxy_list(environment: &HashMap<String, String>) -> Vec<String> {
    let value = environment
        .get("NO_PROXY")
        .filter(|value| !value.is_empty())
        .or_else(|| environment.get("no_proxy"))
        .map(String::as_str)
        .unwrap_or_default();
    value
        .split(',')
        .map(|part| part.trim().to_ascii_lowercase())
        .collect()
}

fn matches_no_proxy(target: &Url, entries: &[String]) -> bool {
    let Some(host) = target.host_str().map(|host| host.trim_matches(['[', ']'])) else {
        return false;
    };
    let port = target
        .port()
        .map(|value| value.to_string())
        .unwrap_or_default();
    let host = host.to_ascii_lowercase();
    entries.iter().any(|entry| {
        if entry.is_empty() {
            return false;
        }
        if entry == "*" {
            return true;
        }
        let (entry_host, entry_port) = entry.split_once(':').unwrap_or((entry, ""));
        let entry_host = entry_host.strip_prefix('.').unwrap_or(entry_host);
        if !entry_port.is_empty() && entry_port != port {
            return false;
        }
        host == entry_host || host.ends_with(&format!(".{entry_host}"))
    })
}

fn normalize_proxy_url(value: &str) -> String {
    let value = value.trim();
    if value.contains("://") {
        value.to_owned()
    } else {
        format!("http://{value}")
    }
}

fn parse_proxy_url(value: &str) -> Result<Url, ResolveError> {
    let mut url = Url::parse(value).map_err(|_| ResolveError::InvalidConfiguration)?;
    if url.host_str().is_none()
        || !has_explicit_port(value)
        || url.port_or_known_default().unwrap_or_default() == 0
        || !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
        || (matches!(url.scheme(), "socks5" | "socks5h") && !matches!(url.path(), "" | "/"))
    {
        return Err(ResolveError::InvalidConfiguration);
    }
    if url.scheme() == "socks5" {
        // An inherited SOCKS5 environment setting must pass the target host
        // to the proxy. Keep the original proxy address and credentials.
        url.set_scheme("socks5h")
            .map_err(|_| ResolveError::InvalidConfiguration)?;
    }
    Ok(url)
}

fn has_explicit_port(value: &str) -> bool {
    let Some((_, remainder)) = value.split_once("://") else {
        return false;
    };
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    let host_and_port = authority.rsplit('@').next().unwrap_or_default();
    if host_and_port.starts_with('[') {
        return host_and_port
            .split_once(']')
            .and_then(|(_, tail)| tail.strip_prefix(':'))
            .is_some_and(|port| {
                !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
            });
    }
    host_and_port
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(target_os = "macos")]
fn native_decision(target: &str) -> Result<Decision, ResolveError> {
    use std::ffi::{CString, c_char, c_int};

    unsafe extern "C" {
        fn switch_cfnetwork_system_proxy(
            target: *const c_char,
            output: *mut c_char,
            output_size: usize,
        ) -> c_int;
    }

    let target = CString::new(target).map_err(|_| ResolveError::InvalidConfiguration)?;
    let mut output = vec![0_u8; OUTPUT_CAPACITY];
    // The C shim only writes within the provided buffer and does not retain
    // either pointer after the call returns.
    let result = unsafe {
        switch_cfnetwork_system_proxy(target.as_ptr(), output.as_mut_ptr().cast(), output.len())
    };
    parse_native_result(result, &output)
}

#[cfg(not(target_os = "macos"))]
fn native_decision(_target: &str) -> Result<Decision, ResolveError> {
    Err(ResolveError::NotFound)
}

fn parse_native_result(result: i32, output: &[u8]) -> Result<Decision, ResolveError> {
    match result {
        0 => Err(ResolveError::NotFound),
        1 => {
            let text = std::ffi::CStr::from_bytes_until_nul(output)
                .map_err(|_| ResolveError::NativeResolutionFailed)?
                .to_str()
                .map_err(|_| ResolveError::NativeResolutionFailed)?;
            if text == "DIRECT" {
                return Ok(Decision::Direct);
            }
            let fields: Vec<_> = text.split('\n').collect();
            if fields.len() != 5 {
                return Err(ResolveError::NativeResolutionFailed);
            }
            let proxy = Url::parse(&format!(
                "{}://{}:{}",
                fields[0],
                bracket_host(fields[1]),
                fields[2]
            ))
            .map_err(|_| ResolveError::NativeResolutionFailed)?;
            let port = fields[2]
                .parse::<u16>()
                .map_err(|_| ResolveError::NativeResolutionFailed)?;
            if port == 0 || !matches!(fields[0], "http" | "socks5h") || proxy.host_str().is_none() {
                return Err(ResolveError::NativeResolutionFailed);
            }
            let mut proxy = proxy;
            if !fields[3].is_empty() || !fields[4].is_empty() {
                proxy
                    .set_username(fields[3])
                    .map_err(|_| ResolveError::NativeResolutionFailed)?;
                proxy
                    .set_password(Some(fields[4]))
                    .map_err(|_| ResolveError::NativeResolutionFailed)?;
            }
            Ok(Decision::Proxy(proxy))
        }
        _ => Err(ResolveError::NativeResolutionFailed),
    }
}

fn bracket_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(values: &[(&str, &str)]) -> HashMap<String, String> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn environment_proxy_priority_matches_existing_go_behavior() {
        let environment = environment(&[
            ("HTTPS_PROXY", "https://first.example:8443"),
            ("https_proxy", "https://second.example:8443"),
            ("ALL_PROXY", "socks5h://third.example:1080"),
            ("HTTP_PROXY", "http://last.example:8080"),
        ]);
        assert_eq!(
            first_environment_proxy(&environment),
            Some("https://first.example:8443")
        );
    }

    #[test]
    fn no_proxy_domain_port_and_wildcard_rules_are_applied() {
        let target = Url::parse("https://api.telegram.org/bot/send").unwrap();
        assert!(matches_no_proxy(
            &target,
            &no_proxy_list(&environment(&[("NO_PROXY", ".telegram.org")]))
        ));
        assert!(!matches_no_proxy(
            &target,
            &no_proxy_list(&environment(&[("NO_PROXY", ".telegram.org:443")]))
        ));
        assert!(!matches_no_proxy(
            &target,
            &no_proxy_list(&environment(&[("NO_PROXY", "telegram.org:80")]))
        ));
        assert!(matches_no_proxy(
            &target,
            &no_proxy_list(&environment(&[("no_proxy", "*")]))
        ));
        let explicit_port = Url::parse("https://api.telegram.org:8443/bot/send").unwrap();
        assert!(matches_no_proxy(
            &explicit_port,
            &no_proxy_list(&environment(&[("no_proxy", "telegram.org:8443")]))
        ));
        assert_eq!(
            no_proxy_list(&environment(&[
                ("NO_PROXY", ""),
                ("no_proxy", "telegram.org")
            ])),
            vec!["telegram.org"]
        );
    }

    #[test]
    fn proxy_url_validation_requires_supported_scheme_and_explicit_port() {
        for value in [
            "http://user:secret@proxy.example:8080",
            "http://proxy.example:80",
            "socks5://proxy.example:1080",
            "socks5h://proxy.example:1080",
        ] {
            assert!(parse_proxy_url(value).is_ok());
        }
        for value in [
            "http://proxy.example",
            "ftp://proxy.example:21",
            "socks5://proxy.example:1080/path",
        ] {
            assert_eq!(
                parse_proxy_url(value),
                Err(ResolveError::InvalidConfiguration)
            );
        }
    }

    #[test]
    fn environment_socks5_preserves_authentication_and_uses_proxy_dns() {
        let value = first_environment_proxy(&environment(&[(
            "HTTPS_PROXY",
            "socks5://user:secret@proxy.example:1080",
        )]))
        .unwrap()
        .to_owned();
        let proxy = parse_proxy_url(&normalize_proxy_url(&value)).unwrap();
        assert_eq!(proxy.scheme(), "socks5h");
        assert_eq!(proxy.username(), "user");
        assert_eq!(proxy.password(), Some("secret"));
        assert_eq!(proxy.host_str(), Some("proxy.example"));
        assert_eq!(proxy.port(), Some(1080));
        assert_eq!(
            parse_proxy_url("socks5h://proxy.example:1080")
                .unwrap()
                .scheme(),
            "socks5h"
        );
        assert_eq!(
            parse_proxy_url("https://proxy.example:8443")
                .unwrap()
                .scheme(),
            "https"
        );
    }

    #[test]
    fn decisions_never_print_proxy_credentials() {
        let proxy = parse_proxy_url("http://user:secret@proxy.example:8080").unwrap();
        let debug = format!("{:?}", Decision::Proxy(proxy));
        assert_eq!(debug, "Proxy([REDACTED])");
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn native_proxy_fields_preserve_authentication_and_reject_bad_results() {
        let result = parse_native_result(1, b"http\nproxy.example\n8443\nuser\nsecret\0").unwrap();
        let Decision::Proxy(proxy) = result else {
            panic!("native proxy result should produce a proxy URL");
        };
        assert_eq!(proxy.scheme(), "http");
        assert_eq!(proxy.username(), "user");
        assert_eq!(proxy.password(), Some("secret"));
        assert_eq!(format!("{:?}", Decision::Proxy(proxy)), "Proxy([REDACTED])");
        assert_eq!(
            parse_native_result(1, b"http\nproxy.example\n99999\n\n\0"),
            Err(ResolveError::NativeResolutionFailed)
        );
        let Decision::Proxy(socks) =
            parse_native_result(1, b"socks5h\nproxy.example\n1080\n\n\0").unwrap()
        else {
            panic!("native SOCKS proxy should produce a proxy URL");
        };
        assert_eq!(socks.scheme(), "socks5h");
        assert_eq!(
            parse_native_result(1, b"socks5\nproxy.example\n1080\n\n\0"),
            Err(ResolveError::NativeResolutionFailed),
            "CFNetwork must not produce local-DNS SOCKS URLs"
        );
        assert_eq!(
            parse_native_result(1, b"https\nproxy.example\n8443\n\n\0"),
            Err(ResolveError::NativeResolutionFailed),
            "macOS HTTPS proxy settings must use plain HTTP CONNECT"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cfnetwork_executes_synthetic_pac_without_network_access() {
        use std::ffi::{CString, c_char, c_int};

        unsafe extern "C" {
            fn switch_cfnetwork_script_proxy(
                target: *const c_char,
                script: *const c_char,
                output: *mut c_char,
                output_size: usize,
            ) -> c_int;
        }

        fn run(script: &str) -> Result<Decision, ResolveError> {
            let target = CString::new("https://api.telegram.org").unwrap();
            let script = CString::new(script).unwrap();
            let mut output = vec![0_u8; OUTPUT_CAPACITY];
            // Both C strings and the output buffer remain live through this call.
            let result = unsafe {
                switch_cfnetwork_script_proxy(
                    target.as_ptr(),
                    script.as_ptr(),
                    output.as_mut_ptr().cast(),
                    output.len(),
                )
            };
            parse_native_result(result, &output)
        }

        let proxy = run("function FindProxyForURL(url, host) { return 'PROXY proxy.example:8443; SOCKS5 127.0.0.1:1080; DIRECT'; }").unwrap();
        assert!(matches!(proxy, Decision::Proxy(_)));
        assert_eq!(
            run("function FindProxyForURL(url, host) { return 'DIRECT'; }").unwrap(),
            Decision::Direct
        );
        let http = run("function FindProxyForURL(url, host) { return 'PROXY proxy.example:8080; SOCKS5 127.0.0.1:1080; DIRECT'; }").unwrap();
        let Decision::Proxy(http) = http else {
            panic!("PAC HTTP proxy should be selected before later fallbacks");
        };
        assert_eq!(http.scheme(), "http");
        assert_eq!(http.host_str(), Some("proxy.example"));
        let socks =
            run("function FindProxyForURL(url, host) { return 'SOCKS 127.0.0.1:1080; DIRECT'; }")
                .unwrap();
        let Decision::Proxy(socks) = socks else {
            panic!("PAC should continue to the next supported proxy candidate");
        };
        assert_eq!(socks.scheme(), "socks5h");
        assert_eq!(socks.port(), Some(1080));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cfnetwork_static_https_and_socks_settings_use_connect_and_remote_dns() {
        use std::ffi::{CString, c_char, c_int};

        unsafe extern "C" {
            fn switch_cfnetwork_fixture_proxy(
                target: *const c_char,
                host: *const c_char,
                port: c_int,
                kind: c_int,
                output: *mut c_char,
                output_size: usize,
            ) -> c_int;
        }

        fn run(kind: i32) -> Decision {
            let target = CString::new("https://api.telegram.org/bot/sendMessage").unwrap();
            let host = CString::new("127.0.0.1").unwrap();
            let mut output = vec![0_u8; OUTPUT_CAPACITY];
            // The fixture evaluates in-memory settings; it does not modify the
            // host's proxy configuration or contact the Telegram endpoint.
            let result = unsafe {
                switch_cfnetwork_fixture_proxy(
                    target.as_ptr(),
                    host.as_ptr(),
                    1080,
                    kind,
                    output.as_mut_ptr().cast(),
                    output.len(),
                )
            };
            parse_native_result(result, &output).unwrap()
        }

        let Decision::Proxy(https_target) = run(1) else {
            panic!("HTTPS target must use the configured HTTP CONNECT proxy");
        };
        assert_eq!(https_target.as_str(), "http://127.0.0.1:1080/");

        let Decision::Proxy(socks) = run(2) else {
            panic!("HTTPS target must use the configured SOCKS proxy");
        };
        assert_eq!(socks.as_str(), "socks5h://127.0.0.1:1080");
    }
}
