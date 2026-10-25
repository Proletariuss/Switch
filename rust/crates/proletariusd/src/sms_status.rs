use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use serde_json::json;

use crate::message_archive::InboxState;
use crate::{message_archive, usb_at, usb_sms_receive};

pub(crate) async fn handle_public(
    inbox: InboxState,
    receiver: usb_sms_receive::Service,
    usb_at: usb_at::Owner,
) -> Response<Full<Bytes>> {
    let result = message_archive::public_count(inbox).await;
    respond(result, receiver.polling_status(usb_at.sms_status_config()))
}

fn respond(count: Result<usize, ()>, mut status: serde_json::Value) -> Response<Full<Bytes>> {
    match count {
        Ok(count) => {
            status["count"] = json!(count);
            crate::json_response(StatusCode::OK, &status)
        }
        Err(()) => crate::json_response(
            StatusCode::BAD_GATEWAY,
            &json!({
                "error": "短信列表未确认，请稍后重试",
                "code": "sms_inbox_unavailable",
                "retryable": true
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use hyper::StatusCode;

    #[test]
    fn serial_configuration_disables_usb_polling_before_first_snapshot() {
        let receiver = crate::test_usb_sms_receiver(
            InboxState::default(),
            std::env::temp_dir().join(format!("sms-status-test-{}.json", std::process::id())),
        );
        let status = receiver.polling_status(Some((true, 8_000)));
        assert_eq!(status["polling"], false);
        assert_eq!(status["poll_interval_s"], 8);
    }

    #[test]
    fn usb_configuration_preserves_polling_interval_and_legacy_timestamp_shape() {
        let receiver = crate::test_usb_sms_receiver(
            InboxState::default(),
            std::env::temp_dir().join(format!("sms-status-test-{}.json", std::process::id())),
        );
        let status = receiver.polling_status(Some((false, 12_500)));
        assert_eq!(status["polling"], true);
        assert_eq!(status["poll_interval_s"], 12);
        assert_eq!(status["last_poll"], "0001-01-01T00:00:00Z");
        assert_eq!(status["last_poll_error"], "");
    }

    #[tokio::test]
    async fn success_response_keeps_the_sms_status_wire_shape() {
        let response = respond(
            Ok(3),
            json!({
                "polling": true,
                "poll_interval_s": 8,
                "last_poll": "2026-10-05T03:00:00Z",
                "last_poll_error": ""
            }),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let body: Bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value,
            json!({
                "count": 3,
                "polling": true,
                "poll_interval_s": 8,
                "last_poll": "2026-10-05T03:00:00Z",
                "last_poll_error": ""
            })
        );
    }

    #[tokio::test]
    async fn unavailable_count_keeps_the_legacy_error_code_and_status() {
        let response = respond(Err(()), json!({}));
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["code"], "sms_inbox_unavailable");
        assert_eq!(value["retryable"], true);
        assert_eq!(value["error"], "短信列表未确认，请稍后重试");
    }
}
