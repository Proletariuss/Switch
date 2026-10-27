use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use serde::{Deserialize, Serialize};

use super::{error_response, invalid_json};

const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestBody {
    response: String,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct CallState {
    pub(super) state: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(super) number: String,
    pub(super) incoming: bool,
    pub(super) active: bool,
}

pub(super) fn handle(body: &[u8]) -> Response<Full<Bytes>> {
    let request: RequestBody = match serde_json::from_slice::<RequestBody>(body) {
        Ok(request) if request.response.len() <= MAX_RESPONSE_BYTES => request,
        Ok(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_call_state_observation",
                "call-state response exceeds its byte limit",
            );
        }
        Err(_) => return invalid_json(),
    };
    crate::json_response(StatusCode::OK, &project(&request.response))
}

mod clcc;

pub(super) fn observe(response: &str) -> (CallState, bool) {
    let observation = clcc::observe(response);
    (observation.display, observation.safe_voice_idle)
}

pub(super) fn project(response: &str) -> CallState {
    observe(response).0
}

#[cfg(test)]
mod tests {
    use super::{CallState, MAX_RESPONSE_BYTES, project};

    #[test]
    fn clcc_projection_preserves_voice_states_without_using_number_presence() {
        for (input, expected) in [
            (
                "+CLCC: 1,1,4,0,0,\"+8613800000000\"\r\nOK",
                CallState {
                    state: "incoming".into(),
                    number: "+8613800000000".into(),
                    incoming: true,
                    active: false,
                },
            ),
            (
                "+CLCC: 1,0,2,0,0,\"123\"\r\nOK",
                CallState {
                    state: "dialing".into(),
                    number: "123".into(),
                    incoming: false,
                    active: false,
                },
            ),
            (
                "+CLCC: 1,0,3,0,0,\"123\"\r\nOK",
                CallState {
                    state: "alerting".into(),
                    number: "123".into(),
                    incoming: false,
                    active: false,
                },
            ),
            (
                "+CLCC: 1,0,0,0,0,\"123\"\r\nOK",
                CallState {
                    state: "active".into(),
                    number: "123".into(),
                    incoming: false,
                    active: true,
                },
            ),
            (
                "+CLCC: 1,0,0,0,0\r\n+CLCC: 2,1,6,0,0,\"123\"\r\nOK",
                CallState {
                    state: "active".into(),
                    number: String::new(),
                    incoming: false,
                    active: true,
                },
            ),
            (
                "+CLCC: 2,1,6,0,0,\"123\"\r\n+CLCC: 1,0,0,0,0\r\nOK",
                CallState {
                    state: "active".into(),
                    number: String::new(),
                    incoming: false,
                    active: true,
                },
            ),
        ] {
            assert_eq!(project(input), expected, "{input}");
        }
    }

    #[test]
    fn clcc_projection_skips_malformed_rows_and_preserves_multi_call_flags() {
        let state = project(
            "noise\r\n+CLCC: invalid\r\n+CLCC: 1,1,4,0,0,\"555\"\r\n+CLCC: 2,0,0,0,0,\"666\"\r\nOK",
        );
        assert_eq!(state.state, "active");
        assert_eq!(state.number, "666");
        assert!(state.incoming);
        assert!(state.active);
    }

    #[test]
    fn projection_rejects_malformed_or_oversized_requests() {
        assert_eq!(
            super::handle(b"{\"response\":\"ok\",\"unknown\":true}").status(),
            hyper::StatusCode::BAD_REQUEST
        );
        let oversized = serde_json::to_vec(&serde_json::json!({
            "response": "x".repeat(MAX_RESPONSE_BYTES + 1)
        }))
        .unwrap();
        assert_eq!(
            super::handle(&oversized).status(),
            hyper::StatusCode::BAD_REQUEST
        );
    }
}
