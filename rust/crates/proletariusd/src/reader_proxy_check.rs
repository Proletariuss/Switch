//! Rust owner for the VoWiFi proxy diagnostic used by the reader settings UI.
//! The check confirms only the proxy transport and optional IP geolocation; it
//! never attempts IKE, IMS registration, or a direct UDP probe.

use std::time::Duration;

use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::json;

const BODY_LIMIT: usize = 4_096;
const BODY_TIMEOUT: Duration = Duration::from_secs(2);
const GEO_TIMEOUT: Duration = Duration::from_secs(4);
const GEO_RESPONSE_LIMIT: usize = 64 * 1024;

#[derive(Default, Deserialize)]
struct ProxyCheckInput {
    #[serde(default)]
    proxy: String,
    #[serde(default)]
    proxy_host: Option<String>,
    #[serde(default)]
    proxy_port: Option<String>,
    #[serde(default)]
    epdg: Option<String>,
    #[serde(default)]
    session_epdg: Option<String>,
    #[serde(default)]
    reader_epdg: Option<String>,
}

#[derive(Serialize)]
struct ProxyCheckResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    probe_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latency_ms: Option<u64>,
    udp_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    udp_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_epdg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    epdg_source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct EffectiveEpdg {
    value: String,
    source: Option<&'static str>,
}

#[derive(Deserialize)]
struct GeoResponse {
    #[serde(default)]
    status: String,
    #[serde(default)]
    query: String,
    #[serde(default, rename = "countryCode")]
    country_code: String,
    #[serde(default, rename = "country")]
    country: String,
}

pub(crate) async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        BODY_LIMIT,
        BODY_TIMEOUT,
    )
    .await
    {
        Ok(body) => body,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return error_response("请求内容无效或过大"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return error_response("读取代理检测请求超时");
        }
    };
    let input = match serde_json::from_slice::<ProxyCheckInput>(&body) {
        Ok(input) => input,
        Err(_) => return error_response("请求格式错误"),
    };
    check_proxy(input).await
}

async fn check_proxy(input: ProxyCheckInput) -> Response<Full<Bytes>> {
    let proxy = match crate::vowifi_input::normalize_proxy(
        &input.proxy,
        input.proxy_host.as_deref(),
        input.proxy_port.as_deref(),
    ) {
        Ok(proxy) => proxy,
        Err(message) => return error_response(message),
    };
    let epdg = match effective_epdg(&input) {
        Ok(epdg) => epdg,
        Err(message) => return error_response(message),
    };
    let effective_epdg = (!epdg.value.is_empty()).then(|| epdg.value.clone());
    let proxy = proxy.as_str();
    if proxy.is_empty() {
        return response(ProxyCheckResponse {
            ok: true,
            probe_message: Some(
                "未手动填写前置代理；会话启动时由 Rust 检测 macOS SOCKS / Surge".into(),
            ),
            country_code: None,
            country: None,
            exit_ip: None,
            latency_ms: None,
            udp_ready: false,
            transport: Some("automatic-system".into()),
            udp_message: Some(
                "HTTP/PAC 不会被当作 UDP 代理；未发现可验证 SOCKS 时才按当前网络选路".into(),
            ),
            effective_epdg,
            epdg_source: epdg.source,
            error: None,
        });
    }

    let probe = match crate::proxy_probe::probe_endpoint(proxy).await {
        Ok(probe) => probe,
        Err(code) => {
            return response(ProxyCheckResponse {
                ok: false,
                probe_message: None,
                country_code: None,
                country: None,
                exit_ip: None,
                latency_ms: None,
                udp_ready: false,
                transport: None,
                udp_message: None,
                effective_epdg,
                epdg_source: epdg.source,
                error: Some(format!("代理连接失败: {}", local_probe_error(code))),
            });
        }
    };

    let (geo, selection) = tokio::join!(query_exit_ip(proxy), select_transport(proxy, &epdg.value));
    let mut probe_message = "SOCKS5 本地握手正常".to_owned();
    if !probe.result.udp_associate {
        probe_message.push_str("；UDP ASSOCIATE 不可用");
    }

    let (country_code, country, exit_ip) = match geo {
        Ok(geo) => (
            nonempty(geo.country_code),
            nonempty(geo.country),
            nonempty(geo.query),
        ),
        Err(()) => {
            probe_message.push_str("；出口归属查询不可用（不影响本地代理检测）");
            (None, None, None)
        }
    };

    let (transport, udp_message) = if epdg.value.is_empty() {
        (
            None,
            Some("未提供 ePDG；本次仅验证代理握手，未验证实际 UDP 往返".into()),
        )
    } else {
        match selection {
            Ok(selected) => (
                Some(selected.mode.as_str().to_owned()),
                Some("传输配置已确认；实际 UDP 往返及 IKE/IMS 就绪仍需独立验证".into()),
            ),
            Err(error) => (None, Some(selection_error_message(error))),
        }
    };

    response(ProxyCheckResponse {
        ok: true,
        probe_message: Some(probe_message),
        country_code,
        country,
        exit_ip,
        latency_ms: Some(probe.result.handshake_latency_ms),
        // This diagnostic deliberately does not send a UDP packet to ePDG.
        udp_ready: false,
        transport,
        udp_message,
        effective_epdg,
        epdg_source: epdg.source,
        error: None,
    })
}

fn effective_epdg(input: &ProxyCheckInput) -> Result<EffectiveEpdg, &'static str> {
    for (source, candidate) in [
        ("user_override", input.epdg.as_deref()),
        ("session_projection", input.session_epdg.as_deref()),
        ("reader_projection", input.reader_epdg.as_deref()),
    ] {
        let Some(candidate) = candidate.filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        return crate::vowifi_input::normalize_epdg(candidate).map(|value| EffectiveEpdg {
            value,
            source: Some(source),
        });
    }
    Ok(EffectiveEpdg {
        value: String::new(),
        source: None,
    })
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

async fn select_transport(
    proxy: &str,
    epdg: &str,
) -> Result<
    crate::transport_selection::SelectedTransport,
    crate::transport_selection::SessionSelectionError,
> {
    if epdg.is_empty() {
        return Err(crate::transport_selection::SessionSelectionError::InvalidEpdg);
    }
    crate::transport_selection::select_for_session(proxy, epdg).await
}

async fn query_exit_ip(proxy: &str) -> Result<GeoResponse, ()> {
    let endpoint = normalize_proxy_endpoint(proxy)?;
    let proxy = reqwest::Proxy::all(format!("socks5h://{endpoint}")).map_err(|_| ())?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(GEO_TIMEOUT)
        .redirect(Policy::none())
        .build()
        .map_err(|_| ())?;
    let mut response = client
        .get("http://ip-api.com/json/")
        .header(reqwest::header::USER_AGENT, "Proletarius/1.0")
        .send()
        .await
        .map_err(|_| ())?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|length| length > GEO_RESPONSE_LIMIT as u64)
    {
        return Err(());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if body.len().saturating_add(chunk.len()) > GEO_RESPONSE_LIMIT {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    let geo = serde_json::from_slice::<GeoResponse>(&body).map_err(|_| ())?;
    if geo.status != "success" || geo.query.is_empty() {
        return Err(());
    }
    Ok(geo)
}

fn normalize_proxy_endpoint(proxy: &str) -> Result<&str, ()> {
    let endpoint = proxy
        .trim()
        .strip_prefix("socks5://")
        .or_else(|| proxy.trim().strip_prefix("socks5h://"))
        .unwrap_or(proxy.trim());
    if endpoint.is_empty()
        || endpoint.len() > 512
        || endpoint
            .chars()
            .any(|character| matches!(character, '/' | '@' | '\r' | '\n'))
        || endpoint.parse::<std::net::SocketAddr>().is_err()
            && !endpoint.rsplit_once(':').is_some_and(|(host, port)| {
                !host.is_empty()
                    && port.parse::<u16>().is_ok_and(|port| port != 0)
                    && host.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'.' | b'-' | b'[' | b']' | b':')
                    })
            })
    {
        return Err(());
    }
    Ok(endpoint)
}

fn local_probe_error(code: &str) -> &'static str {
    match code {
        "invalid_proxy_address" => "代理地址无效",
        "proxy_address_unavailable" => "无法解析或连接代理",
        "socks5_probe_timeout" => "SOCKS5 探测超时",
        _ => "SOCKS5 握手失败",
    }
}

fn selection_error_message(error: crate::transport_selection::SessionSelectionError) -> String {
    use crate::transport_selection::SessionSelectionError;
    match error {
        SessionSelectionError::InvalidEpdg => "ePDG 域名无效".into(),
        SessionSelectionError::TimedOut => "ePDG 传输检测超时".into(),
        SessionSelectionError::MissingVerifiedProxy => "未能验证 SOCKS5 UDP 代理".into(),
        SessionSelectionError::Policy { error, .. } => error.to_string(),
    }
}

fn response(value: ProxyCheckResponse) -> Response<Full<Bytes>> {
    crate::json_response(StatusCode::OK, &value)
}

fn error_response(message: &str) -> Response<Full<Bytes>> {
    crate::json_response(StatusCode::BAD_REQUEST, &json!({"error":message}))
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn empty_proxy_keeps_ui_friendly_result_shape() {
        let result = check_proxy(ProxyCheckInput {
            proxy: "  ".into(),
            proxy_host: Some(String::new()),
            proxy_port: Some(String::new()),
            epdg: None,
            ..Default::default()
        })
        .await;
        let body = result.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["transport"], "automatic-system");
        assert!(value.get("error").is_none());
        assert_eq!(value["udp_ready"], false);
    }

    #[tokio::test]
    async fn missing_proxy_matches_the_legacy_empty_proxy_response() {
        let input: ProxyCheckInput = serde_json::from_slice(br#"{"epdg":"epdg.example"}"#).unwrap();
        let result = check_proxy(input).await;
        let body = result.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["transport"], "automatic-system");
        assert_eq!(value["udp_ready"], false);
        assert_eq!(value["effective_epdg"], "epdg.example");
        assert_eq!(value["epdg_source"], "user_override");
    }

    #[test]
    fn rust_owns_effective_epdg_precedence_and_normalization() {
        let selected = effective_epdg(&ProxyCheckInput {
            epdg: Some("  ".into()),
            session_epdg: Some(" session.example.test ".into()),
            reader_epdg: Some("reader.example.test".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            selected,
            EffectiveEpdg {
                value: "session.example.test".into(),
                source: Some("session_projection"),
            }
        );

        let selected = effective_epdg(&ProxyCheckInput {
            epdg: Some(" override.example.test ".into()),
            session_epdg: Some("session.example.test".into()),
            reader_epdg: Some("reader.example.test".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            selected,
            EffectiveEpdg {
                value: "override.example.test".into(),
                source: Some("user_override"),
            }
        );

        let selected = effective_epdg(&ProxyCheckInput {
            epdg: None,
            session_epdg: None,
            reader_epdg: Some(" reader.example.test ".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            selected,
            EffectiveEpdg {
                value: "reader.example.test".into(),
                source: Some("reader_projection"),
            }
        );
        assert!(
            effective_epdg(&ProxyCheckInput {
                epdg: Some("https://invalid.example".into()),
                session_epdg: Some("session.example.test".into()),
                ..Default::default()
            })
            .is_err(),
            "an invalid explicit override must not fall through to another ePDG"
        );
    }

    #[tokio::test]
    async fn automatic_proxy_mode_still_validates_epdg_in_rust() {
        let result = check_proxy(ProxyCheckInput {
            proxy: String::new(),
            proxy_host: Some(String::new()),
            proxy_port: Some(String::new()),
            epdg: Some("https://invalid.example".into()),
            ..Default::default()
        })
        .await;
        assert_eq!(result.status(), StatusCode::BAD_REQUEST);
        let body = result.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value, serde_json::json!({"error":"ePDG 地址无效"}));
    }

    #[tokio::test]
    async fn malformed_request_uses_the_legacy_error_shape() {
        let result = error_response("请求格式错误");
        assert_eq!(result.status(), StatusCode::BAD_REQUEST);
        let body = result.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value, serde_json::json!({"error":"请求格式错误"}));
    }

    #[tokio::test]
    async fn local_proxy_failure_is_a_diagnostic_result_without_external_access() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = listener.local_addr().unwrap().to_string();
        drop(listener);
        let result = check_proxy(ProxyCheckInput {
            proxy,
            proxy_host: None,
            proxy_port: None,
            epdg: None,
            ..Default::default()
        })
        .await;
        let body = result.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["ok"], false);
        assert!(
            value["error"]
                .as_str()
                .unwrap()
                .starts_with("代理连接失败:")
        );
        assert_eq!(value["udp_ready"], false);
    }

    #[test]
    fn proxy_endpoint_normalization_rejects_credentials_and_paths() {
        assert_eq!(
            normalize_proxy_endpoint("socks5h://127.0.0.1:1080"),
            Ok("127.0.0.1:1080")
        );
        assert!(normalize_proxy_endpoint("user:secret@127.0.0.1:1080").is_err());
        assert!(normalize_proxy_endpoint("127.0.0.1:1080/path").is_err());
        assert!(normalize_proxy_endpoint("[::1]:1080").is_ok());
    }

    #[tokio::test]
    async fn check_endpoint_probes_socks_and_keeps_geolocation_optional() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(value) => value,
                    Err(_) => return,
                };
                tokio::spawn(async move { serve_socks_probe(stream).await });
            }
        });
        let result = check_proxy(ProxyCheckInput {
            proxy: address.to_string(),
            proxy_host: None,
            proxy_port: None,
            epdg: None,
            ..Default::default()
        })
        .await;
        server.abort();
        let body = result.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["udp_ready"], false);
        assert!(value["latency_ms"].is_number());
        assert!(value["probe_message"].as_str().unwrap().contains("SOCKS5"));
        assert!(
            value["udp_message"]
                .as_str()
                .unwrap()
                .contains("未提供 ePDG")
        );
        assert!(value.get("exit_ip").is_none());
    }

    async fn serve_socks_probe(mut stream: tokio::net::TcpStream) {
        let mut header = [0; 2];
        if stream.read_exact(&mut header).await.is_err() || header[0] != 5 || header[1] > 16 {
            return;
        }
        let mut methods = vec![0; usize::from(header[1])];
        if stream.read_exact(&mut methods).await.is_err()
            || stream.write_all(&[5, 0]).await.is_err()
        {
            return;
        }
        let mut request_header = [0; 4];
        let Ok(_) = stream.read_exact(&mut request_header).await else {
            return;
        };
        if request_header[1] == 3 {
            let _ = stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await;
        } else {
            let _ = stream.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await;
        }
    }
}
