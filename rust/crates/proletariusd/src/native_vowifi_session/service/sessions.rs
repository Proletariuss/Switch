//! Native reader and module observations; no compatibility process owns this list.
use super::*;

impl Service {
    pub(super) async fn public_sessions(&self) -> Response<Full<Bytes>> {
        let mut sessions = Vec::new();
        if let Some(module) = self
            .0
            .resources
            .lifecycle
            .upgrade()
            .and_then(|lifecycle| lifecycle.retained_module_service())
            .and_then(|service| service.session_row())
        {
            sessions.push(module);
        }
        if let Some(native) = self
            .reader_observation()
            .filter(|observation| observation.session_active && observation.identity_valid)
        {
            sessions.push(native.status);
        }
        crate::json_response(StatusCode::OK, &json!({"sessions": sessions}))
    }
}
