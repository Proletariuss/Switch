use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Incoming};
use proletarius_vowifi::{reconnect_delay_seconds, registration_renewal_delay_seconds};
use serde::Deserialize;
use serde_json::json;

const BODY_LIMIT: usize = 4_096;

#[derive(Deserialize)]
struct PolicyInput {
    operation: String,
    #[serde(default)]
    expires_seconds: u64,
    #[serde(default)]
    attempt: u32,
}

pub(crate) async fn handle(request: Request<Incoming>) -> Response<Full<Bytes>> {
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
        ) => return failure(StatusCode::BAD_REQUEST, "invalid_policy_request"),
        Err(crate::http_server::BodyReadError::Timeout) => {
            return failure(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
    };
    let Ok(input) = serde_json::from_slice::<PolicyInput>(&body) else {
        return failure(StatusCode::BAD_REQUEST, "invalid_policy_request");
    };
    policy_response(&input)
}

fn policy_response(input: &PolicyInput) -> Response<Full<Bytes>> {
    match input.operation.trim() {
        "registration_renewal" => match registration_renewal_delay_seconds(input.expires_seconds) {
            Some(delay) => crate::json_response(
                StatusCode::OK,
                &json!({"operation":"registration_renewal","delay_seconds":delay}),
            ),
            None => failure(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_registration_expiry",
            ),
        },
        "reconnect" => crate::json_response(
            StatusCode::OK,
            &json!({
                "operation":"reconnect",
                "delay_seconds":reconnect_delay_seconds(input.attempt),
            }),
        ),
        _ => failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_policy_operation",
        ),
    }
}

fn failure(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    crate::json_response(
        status,
        &json!({"error":"VoWiFi session policy request rejected","code":code,"retryable":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_requires_supported_operation_and_valid_expiry() {
        let response = policy_response(&PolicyInput {
            operation: "unknown".to_owned(),
            expires_seconds: 2_400,
            attempt: 0,
        });
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let response = policy_response(&PolicyInput {
            operation: "registration_renewal".to_owned(),
            expires_seconds: 0,
            attempt: 0,
        });
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
