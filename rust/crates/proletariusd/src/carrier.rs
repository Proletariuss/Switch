use bytes::Bytes;
#[cfg(test)]
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use proletarius_vowifi::carrier::{Registry, SimIdentity};
use proletarius_vowifi::carrier_runtime_profile::RuntimeCarrierProfile;
use serde_json::json;
use std::sync::OnceLock;

static REGISTRY: OnceLock<Result<Registry, String>> = OnceLock::new();

pub(crate) async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
    let sim = match decode(request.into_body()).await {
        Ok(sim) => sim,
        Err((status, code)) => return error(status, code),
    };
    let result = tokio::task::spawn_blocking(move || resolve(sim)).await;
    match result {
        Ok(response) => response,
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "carrier_worker_failed"),
    }
}

async fn decode<B: hyper::body::Body<Data = Bytes> + Unpin>(
    body: B,
) -> Result<SimIdentity, (StatusCode, &'static str)> {
    let bytes = crate::http_server::collect_body_with_timeout(
        body,
        4096,
        std::time::Duration::from_secs(5),
    )
    .await
    .map_err(|error| match error {
        crate::http_server::BodyReadError::Timeout => {
            (StatusCode::REQUEST_TIMEOUT, "request_body_timeout")
        }
        crate::http_server::BodyReadError::TooLarge => {
            (StatusCode::PAYLOAD_TOO_LARGE, "request_body_too_large")
        }
        crate::http_server::BodyReadError::Invalid => {
            (StatusCode::BAD_REQUEST, "invalid_request_body")
        }
    })?;
    serde_json::from_slice::<SimIdentity>(&bytes)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid_carrier_identity"))
}

fn resolve(sim: SimIdentity) -> Response<Full<Bytes>> {
    let registry = match REGISTRY.get_or_init(Registry::embedded) {
        Ok(registry) => registry,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "carrier_registry_invalid",
            );
        }
    };
    let selected = match registry.resolve(&sim) {
        Ok(selected) => selected,
        Err(_) => return error(StatusCode::UNPROCESSABLE_ENTITY, "carrier_selection_failed"),
    };
    let runtime_profile = match selected.as_ref() {
        Some(profile) => RuntimeCarrierProfile::expand(profile, &sim, &registry.revision),
        None => Ok(RuntimeCarrierProfile::standard(&sim, &registry.revision)),
    };
    let runtime_profile = match runtime_profile {
        Ok(profile) => profile,
        Err(_) => return error(StatusCode::UNPROCESSABLE_ENTITY, "carrier_profile_invalid"),
    };
    let (matched, id, bundle, selector, configuration) = match selected {
        Some(profile) => (
            true,
            Some(profile.id),
            Some(profile.bundle),
            Some(profile.selector),
            Some(profile.configuration),
        ),
        None => (false, None, None, None, None),
    };
    crate::json_response(
        StatusCode::OK,
        &json!({
            "matched": matched,
            "id": id,
            "bundle": bundle,
            "selector": selector,
            "revision": registry.revision,
            "configuration": configuration,
            "runtime_profile": runtime_profile,
        }),
    )
}

/// Resolve the same public carrier ePDG value used by the Go compatibility
/// profile helper. This is shared by the Rust reader-info projection so the
/// UI response does not have to ask Go to make a second carrier decision.
pub(crate) fn resolve_epdg_fqdn(sim: &SimIdentity) -> Result<String, String> {
    let registry = REGISTRY
        .get_or_init(Registry::embedded)
        .as_ref()
        .map_err(|error| error.clone())?;
    let selected = registry.resolve(sim)?;
    let profile = match selected.as_ref() {
        Some(profile) => RuntimeCarrierProfile::expand(profile, sim, &registry.revision)?,
        None => RuntimeCarrierProfile::standard(sim, &registry.revision),
    };
    if let Some(candidate) = profile.epdg_candidates.as_ref().and_then(|items| {
        items
            .iter()
            .map(String::as_str)
            .find(|item| !item.trim().is_empty())
    }) {
        return Ok(candidate.trim().to_owned());
    }
    Ok(format!(
        "epdg.epc.mnc{}.mcc{}.pub.3gppnetwork.org",
        pad3(&sim.mnc),
        pad3(&sim.mcc)
    ))
}

fn pad3(value: &str) -> String {
    format!("{value:0>3}")
}

fn error(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    // No SIM identity or configuration secrets in error responses or logs.
    crate::json_response(status, &json!({"error":code,"code":code,"retryable":false}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn identity_decoder_bounds_and_validates_input() {
        assert!(matches!(
            decode(Full::new(Bytes::from(vec![b' '; 4097]))).await,
            Err((StatusCode::PAYLOAD_TOO_LARGE, _))
        ));
        assert!(matches!(
            decode(Full::new(Bytes::from_static(b"{"))).await,
            Err((StatusCode::BAD_REQUEST, _))
        ));
        let sim = decode(Full::new(Bytes::from_static(
            br#"{"mcc":"234","mnc":"33"}"#,
        )))
        .await
        .unwrap();
        assert_eq!(sim.mnc, "33");
    }
    #[test]
    fn native_resolver_does_not_require_go_or_hardware() {
        assert_eq!(
            resolve(SimIdentity {
                mcc: "234".into(),
                mnc: "33".into(),
                ..Default::default()
            })
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            resolve(SimIdentity::default()).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn three_hk_runtime_response_preserves_user_template_and_registry_provenance() {
        let response = resolve(SimIdentity {
            mcc: "454".into(),
            mnc: "03".into(),
            ..Default::default()
        });
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["id"], "apple_hutchison_hk");
        assert_eq!(payload["configuration"]["ims"]["ip_stack"], "ipv4v6");
        let runtime = &payload["runtime_profile"];
        assert_eq!(runtime["source"], "user-template");
        assert_eq!(runtime["ip_stack"], "ipv4");
        assert_eq!(runtime["epdg_candidates"], json!(["wlan.three.com.hk"]));
        let supplied: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../data/carriers/user_templates/3_hk.json"
        ))
        .unwrap();
        assert_eq!(runtime["vowifi_template"], supplied);
    }
}
