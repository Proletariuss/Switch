//! Native SOCKS5 capability probe used by the compatibility runtime while
//! live IKE/IMS ownership is migrated. It never resolves the ePDG and never
//! opens a direct UDP socket.

mod udp_diagnostic;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use serde::Deserialize;
use serde_json::json;
use tokio::net::lookup_host;
use tokio::task::JoinSet;
use tokio::time::timeout;

const BODY_LIMIT: usize = 4_096;
// This is the complete proxy endpoint budget, including resolving the proxy
// host and all bounded SOCKS candidates. The Go compatibility caller allows
// seven seconds for the local control request, so keeping the work itself to
// six seconds leaves IPC/JSON headroom instead of turning a slow first DNS
// answer into a false "system proxy unavailable" result.
const PROBE_BUDGET: Duration = Duration::from_secs(6);
const MAX_PROXY_ADDRESSES: usize = 8;

#[derive(Deserialize)]
struct ProbeInput {
    proxy: String,
    #[serde(default)]
    udp_dns: Option<udp_diagnostic::DnsProbeInput>,
}

pub(crate) struct EndpointProbe {
    pub address: SocketAddr,
    pub result: proletarius_vowifi::Socks5Probe,
}

pub(crate) async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
    // The explicit diagnostic shares one budget with request decoding; a slow
    // body cannot buy a fresh six-second network window.
    let diagnostic_deadline = tokio::time::Instant::now() + PROBE_BUDGET;
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        BODY_LIMIT,
        Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return failure(StatusCode::BAD_REQUEST, "invalid_proxy_probe"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(input) = serde_json::from_slice::<ProbeInput>(&body) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_proxy_probe");
    };
    if let Some(target) = input.udp_dns {
        let target = match target.validate() {
            Ok(target) => target,
            Err(code) => return failure(StatusCode::BAD_REQUEST, code),
        };
        return crate::json_response(
            StatusCode::OK,
            &udp_diagnostic::diagnose(&input.proxy, target, remaining_until(diagnostic_deadline))
                .await,
        );
    }
    match probe_endpoint(&input.proxy).await {
        Ok(probe) => crate::json_response(
            StatusCode::OK,
            &json!({
                "ok": true,
                "latency_ms": probe.result.handshake_latency_ms,
                "tcp_connected": true,
                "authenticated": true,
                "udp_associate": probe.result.udp_associate,
                "udp_roundtrip": false,
                "udp_ready": false,
                "scope": "proxy_handshake",
            }),
        ),
        Err("invalid_proxy_address") | Err("proxy_address_unavailable") => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "proxy_address_unavailable",
        ),
        Err("socks5_probe_timeout") => crate::json_response(
            StatusCode::OK,
            &json!({"ok":false,"latency_ms":0,"udp_ready":false,"error_code":"socks5_probe_timeout"}),
        ),
        Err(_) => crate::json_response(
            StatusCode::OK,
            &json!({"ok":false,"latency_ms":0,"udp_ready":false,"error_code":"socks5_handshake_failed"}),
        ),
    }
}

pub(crate) async fn probe_endpoint(proxy: &str) -> Result<EndpointProbe, &'static str> {
    probe_endpoint_with_budget(proxy, PROBE_BUDGET).await
}

// Automatic system-proxy discovery runs before this probe and therefore uses
// the remaining transport-selection budget. Explicit endpoints intentionally
// call the public wrapper above and retain the full six-second UDP ASSOCIATE
// verification window.
pub(crate) async fn probe_endpoint_with_budget(
    proxy: &str,
    budget: Duration,
) -> Result<EndpointProbe, &'static str> {
    let proxy = proxy
        .trim()
        .strip_prefix("socks5://")
        .or_else(|| proxy.trim().strip_prefix("socks5h://"))
        .unwrap_or(proxy.trim());
    if proxy.is_empty() || proxy.len() > 512 {
        return Err("invalid_proxy_address");
    }
    if budget.is_zero() {
        return Err("socks5_probe_timeout");
    }
    let deadline = tokio::time::Instant::now() + budget;
    let addresses = timeout(remaining_until(deadline), resolve_candidates(proxy))
        .await
        .map_err(|_| "proxy_address_unavailable")?
        .map_err(|_| "proxy_address_unavailable")?;
    probe_resolved_addresses_before(addresses, deadline).await
}

fn remaining_until(deadline: tokio::time::Instant) -> Duration {
    deadline.saturating_duration_since(tokio::time::Instant::now())
}

async fn resolve_candidates(proxy: &str) -> Result<Vec<SocketAddr>, ()> {
    if let Ok(address) = proxy.parse::<SocketAddr>() {
        return Ok(vec![address]);
    }
    let mut seen = HashSet::new();
    let addresses = lookup_host(proxy)
        .await
        .map_err(|_| ())?
        .filter(|address| seen.insert(*address))
        .take(MAX_PROXY_ADDRESSES)
        .collect::<Vec<_>>();
    (!addresses.is_empty()).then_some(addresses).ok_or(())
}

// A system proxy hostname (notably localhost) can resolve to both ::1 and
// 127.0.0.1. Probe every bounded candidate under one shared time budget: a
// listener bound to only one family must not make an otherwise usable SOCKS5
// UDP endpoint look unavailable. Prefer a real UDP ASSOCIATE result, but keep
// the earliest TCP-only success for callers that merely need a compatibility
// probe result.
#[cfg(test)]
async fn probe_resolved_addresses(
    addresses: Vec<SocketAddr>,
) -> Result<EndpointProbe, &'static str> {
    probe_resolved_addresses_before(addresses, tokio::time::Instant::now() + PROBE_BUDGET).await
}

async fn probe_resolved_addresses_before(
    addresses: Vec<SocketAddr>,
    deadline: tokio::time::Instant,
) -> Result<EndpointProbe, &'static str> {
    let mut probes = JoinSet::new();
    for (index, address) in addresses.into_iter().enumerate() {
        probes.spawn(async move {
            (
                index,
                address,
                proletarius_vowifi::probe_socks5(address).await,
            )
        });
    }

    let mut fallback: Option<(usize, EndpointProbe)> = None;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            probes.abort_all();
            return fallback
                .map(|(_, probe)| probe)
                .ok_or("socks5_probe_timeout");
        }
        match timeout(remaining, probes.join_next()).await {
            Ok(Some(Ok((index, address, Ok(result))))) => {
                let probe = EndpointProbe { address, result };
                if probe.result.udp_associate {
                    probes.abort_all();
                    return Ok(probe);
                }
                if fallback
                    .as_ref()
                    .is_none_or(|(existing, _)| index < *existing)
                {
                    fallback = Some((index, probe));
                }
            }
            Ok(Some(Ok((_, _, Err(_))))) | Ok(Some(Err(_))) => {}
            Ok(None) => {
                return fallback
                    .map(|(_, probe)| probe)
                    .ok_or("socks5_handshake_failed");
            }
            Err(_) => {
                probes.abort_all();
                return fallback
                    .map(|(_, probe)| probe)
                    .ok_or("socks5_probe_timeout");
            }
        }
    }
}

fn failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"前置代理地址无效","code":code,"retryable":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn associate_only_http_probe_does_not_report_actual_udp_ready() {
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let mock = tokio::spawn(async move {
            for connection in 0..2 {
                let (mut stream, _) = proxy.accept().await.unwrap();
                let mut greeting = [0; 3];
                stream.read_exact(&mut greeting).await.unwrap();
                stream.write_all(&[5, 0]).await.unwrap();
                if connection == 1 {
                    let mut request = [0; 10];
                    stream.read_exact(&mut request).await.unwrap();
                    stream
                        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                        .await
                        .unwrap();
                }
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    hyper_util::rt::TokioIo::new(stream),
                    hyper::service::service_fn(|request| async {
                        Ok::<_, std::convert::Infallible>(handle(request).await)
                    }),
                )
                .await
                .unwrap();
        });
        let body = json!({"proxy":proxy_address.to_string()}).to_string();
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client.write_all(format!("POST /api/v2/vowifi/proxy-probe HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        let result: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(
            result["udp_ready"], false,
            "Associate without any datagram is not UDP reachability"
        );
        assert_eq!(result["udp_associate"], true);
        assert_eq!(result["udp_roundtrip"], false);
        server.await.unwrap();
        mock.await.unwrap();
    }

    #[tokio::test]
    async fn resolves_literal_and_rejects_missing_port() {
        assert_eq!(
            resolve_candidates("127.0.0.1:7897").await.unwrap(),
            vec!["127.0.0.1:7897".parse().unwrap()]
        );
        assert!(resolve_candidates("127.0.0.1").await.is_err());
    }

    #[tokio::test]
    async fn later_resolved_ipv4_proxy_is_used_when_ipv6_is_unavailable() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ipv4 = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for connection in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut greeting = [0_u8; 3];
                stream.read_exact(&mut greeting).await.unwrap();
                stream.write_all(&[5, 0]).await.unwrap();
                if connection == 1 {
                    let mut request = [0_u8; 10];
                    stream.read_exact(&mut request).await.unwrap();
                    stream
                        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                        .await
                        .unwrap();
                }
            }
        });
        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), ipv4.port());
        let probe = probe_resolved_addresses(vec![ipv6, ipv4]).await.unwrap();
        assert_eq!(probe.address, ipv4);
        assert!(probe.result.udp_associate);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn retains_earliest_tcp_only_probe_when_no_candidate_supports_udp() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ipv4 = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for connection in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut greeting = [0_u8; 3];
                stream.read_exact(&mut greeting).await.unwrap();
                stream.write_all(&[5, 0]).await.unwrap();
                if connection == 1 {
                    let mut request = [0_u8; 10];
                    stream.read_exact(&mut request).await.unwrap();
                    stream
                        .write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0])
                        .await
                        .unwrap();
                }
            }
        });
        let unavailable = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);
        let probe = probe_resolved_addresses(vec![ipv4, unavailable])
            .await
            .unwrap();
        assert_eq!(probe.address, ipv4);
        assert!(!probe.result.udp_associate);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn candidate_probes_cannot_extend_the_original_resolution_budget() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let start = tokio::time::Instant::now();
        let result =
            probe_resolved_addresses_before(vec![address], start + Duration::from_millis(25)).await;
        match result {
            Err(error) => assert_eq!(error, "socks5_probe_timeout"),
            Ok(_) => panic!("hung candidate unexpectedly completed"),
        }
        assert!(
            start.elapsed() < Duration::from_millis(250),
            "candidate probe outlived shared deadline: {:?}",
            start.elapsed()
        );
        server.abort();
    }

    #[tokio::test]
    async fn real_local_proxy_probe_completes_when_enabled() {
        if std::env::var("PROLETARIUS_TEST_REAL_PROXY").as_deref() != Ok("1") {
            return;
        }
        let proxy = std::env::var("PROLETARIUS_TEST_PROXY_ENDPOINT")
            .unwrap_or_else(|_| "127.0.0.1:7897".to_owned());
        let probe = probe_endpoint(&proxy)
            .await
            .expect("configured real proxy must complete the SOCKS5 handshake");
        if std::env::var("PROLETARIUS_TEST_REQUIRE_UDP").as_deref() == Ok("1") {
            assert!(
                probe.result.udp_associate,
                "configured proxy does not support SOCKS5 UDP ASSOCIATE"
            );
        }
    }
}
