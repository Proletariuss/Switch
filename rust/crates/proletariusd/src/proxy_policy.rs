//! Proxy-preferred transport policy. This compatibility endpoint remains for
//! diagnostics; live DNS/route/probe evidence is now collected by Rust's
//! `select-transport` route.

use std::net::IpAddr;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Incoming};
use proletarius_vowifi::{ProxyEvidence, ProxyMode, RouteEvidence, select_proxy_mode};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct RouteInput {
    address: String,
    interface: Option<String>,
}

#[derive(Deserialize)]
struct PolicyInput {
    proxy_is_loopback: bool,
    #[serde(default)]
    epdg_addresses: Vec<String>,
    #[serde(default)]
    routes: Vec<RouteInput>,
    socks5_udp_associate: bool,
}

pub(crate) async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let body = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        16_384,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(value) => value,
        Err(
            crate::http_server::BodyReadError::TooLarge
            | crate::http_server::BodyReadError::Invalid,
        ) => return failure(StatusCode::BAD_REQUEST, "invalid_evidence"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(input) = serde_json::from_slice::<PolicyInput>(&body) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_evidence");
    };
    evaluate(input)
}

fn evaluate(input: PolicyInput) -> Response<Full<Bytes>> {
    let Some(epdg_addresses) = input
        .epdg_addresses
        .iter()
        .map(|value| value.parse::<IpAddr>().ok())
        .collect::<Option<Vec<_>>>()
    else {
        return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_address");
    };
    let mut routes = Vec::with_capacity(input.routes.len());
    for route in input.routes {
        let Ok(address) = route.address.parse::<IpAddr>() else {
            return failure(StatusCode::UNPROCESSABLE_ENTITY, "invalid_address");
        };
        routes.push(RouteEvidence {
            address,
            interface: route.interface,
        });
    }
    let evidence = ProxyEvidence {
        proxy_is_loopback: input.proxy_is_loopback,
        epdg_addresses,
        routes,
        socks5_udp_associate: input.socks5_udp_associate,
    };
    match select_proxy_mode(&evidence) {
        Ok(ProxyMode::EnhancedFakeIp(address)) => crate::json_response(
            StatusCode::OK,
            &json!({"mode":"enhanced-mode-fake-ip","address":address.to_string()}),
        ),
        Ok(ProxyMode::EnhancedUtun { address, interface }) => crate::json_response(
            StatusCode::OK,
            &json!({"mode":"enhanced-mode-utun","address":address.to_string(),"interface":interface}),
        ),
        Ok(ProxyMode::Socks5UdpDomain) => {
            crate::json_response(StatusCode::OK, &json!({"mode":"socks5-udp-domain"}))
        }
        Ok(ProxyMode::Direct(address)) => crate::json_response(
            StatusCode::OK,
            &json!({"mode":"direct","address":address.to_string()}),
        ),
        Err(error) => crate::json_response(
            StatusCode::OK,
            &json!({"mode":"rejected","error_code":match error {
                proletarius_vowifi::ProxyPolicyError::Socks5UdpUnavailable => "socks5_udp_unavailable",
                proletarius_vowifi::ProxyPolicyError::UnverifiedPublicRoute => "unverified_public_route",
            }}),
        ),
    }
}

fn failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"前置代理证据无效","code":code,"retryable":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn json(response: Response<Full<Bytes>>) -> serde_json::Value {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn prioritizes_fake_ip_and_accepts_public_direct_route() {
        let fake = evaluate(PolicyInput {
            proxy_is_loopback: true,
            epdg_addresses: vec!["87.194.8.8".into(), "198.18.0.14".into()],
            routes: vec![RouteInput {
                address: "198.18.0.14".into(),
                interface: Some("utun8".into()),
            }],
            socks5_udp_associate: true,
        });
        assert_eq!(
            json(fake).await,
            json!({"mode":"enhanced-mode-fake-ip","address":"198.18.0.14"})
        );

        let direct = evaluate(PolicyInput {
            proxy_is_loopback: true,
            epdg_addresses: vec!["87.194.8.8".into()],
            routes: vec![RouteInput {
                address: "87.194.8.8".into(),
                interface: Some("en0".into()),
            }],
            socks5_udp_associate: false,
        });
        assert_eq!(
            json(direct).await,
            json!({"mode":"direct","address":"87.194.8.8"})
        );
    }

    #[tokio::test]
    async fn accepts_only_verified_utun_or_proxy_side_domain() {
        let utun = evaluate(PolicyInput {
            proxy_is_loopback: true,
            epdg_addresses: vec!["87.194.8.8".into()],
            routes: vec![RouteInput {
                address: "87.194.8.8".into(),
                interface: Some("utun8".into()),
            }],
            socks5_udp_associate: false,
        });
        assert_eq!(
            json(utun).await,
            json!({"mode":"enhanced-mode-utun","address":"87.194.8.8","interface":"utun8"})
        );

        let domain = evaluate(PolicyInput {
            proxy_is_loopback: false,
            epdg_addresses: vec![],
            routes: vec![],
            socks5_udp_associate: true,
        });
        assert_eq!(json(domain).await, json!({"mode":"socks5-udp-domain"}));
    }
}
