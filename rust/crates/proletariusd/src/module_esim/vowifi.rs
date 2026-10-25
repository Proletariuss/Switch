//! One call-safe native retirement boundary for all USB module eSIM writers.
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use serde_json::json;

#[derive(Debug)]
pub(crate) struct StopFailure {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}
impl StopFailure {
    fn from_code(code: &'static str) -> Self {
        let (status, message) = match code {
            "module_vowifi_starting" => (
                StatusCode::CONFLICT,
                "模块 VoWiFi 正在建立连接，请先等待连接完成或关闭后再操作 Profile",
            ),
            "module_vowifi_call_active" => (
                StatusCode::CONFLICT,
                "模块 VoWiFi 正在通话，请挂断后再操作 Profile",
            ),
            _ => (
                StatusCode::SERVICE_UNAVAILABLE,
                "无法确认模块 VoWiFi 及射频状态已安全恢复，尚未操作 Profile",
            ),
        };
        Self {
            status,
            code,
            message,
        }
    }
    pub(crate) fn response(&self) -> Response<Full<Bytes>> {
        crate::json_response(
            self.status,
            &json!({"error":self.message,"code":self.code,"recoverable":true}),
        )
    }
}

/// Call only after acquiring the eSIM operation permit, before obtaining the
/// RF reservation or submitting any APDU. The reservation still arbitrates a
/// competing new VoWiFi start after this exact retained owner finishes.
pub(crate) async fn stop_module_vowifi_before_write(
    state: &crate::ServerState,
) -> Result<(), StopFailure> {
    stop_on_lifecycle(&state.native_vowifi_lifecycle).await
}

async fn stop_on_lifecycle(
    lifecycle: &crate::native_vowifi_session::NativeLifecycle,
) -> Result<(), StopFailure> {
    // A real legacy claim cannot be mistaken for an idle native service. The
    // public native module path never needs a reverse Go stop callback.
    if lifecycle.ordinary_admission().has_go_ordinary_claim() {
        return Err(StopFailure::from_code("module_vowifi_cleanup_unconfirmed"));
    }
    if let Some(module) = lifecycle.retained_module_service() {
        module
            .stop_for_profile_switch()
            .await
            .map_err(StopFailure::from_code)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn no_native_module_is_idle_but_a_retained_legacy_claim_is_not_cleanup_proof() {
        let lifecycle = crate::native_vowifi_session::NativeLifecycle::default();
        assert!(stop_on_lifecycle(&lifecycle).await.is_ok());
        let gate = lifecycle.ordinary_admission();
        gate.set_go_producer("profile-stop-legacy").unwrap();
        let claim = gate
            .acquire_go("profile-stop-legacy", "module", "claim", 1)
            .unwrap();
        assert_eq!(
            stop_on_lifecycle(&lifecycle).await.unwrap_err().code,
            "module_vowifi_cleanup_unconfirmed"
        );
        drop(claim);
        assert!(
            stop_on_lifecycle(&lifecycle).await.is_err(),
            "dropped waiter cannot prove cleanup"
        );
        gate.current_go_claim()
            .unwrap()
            .unwrap()
            .release_confirmed()
            .unwrap();
        assert!(stop_on_lifecycle(&lifecycle).await.is_ok());
        // A module write must not stop an independent reader's native runtime.
        let reader = gate.acquire_native("reader", 1).unwrap();
        assert!(stop_on_lifecycle(&lifecycle).await.is_ok());
        assert!(reader.is_current());
        reader.release_confirmed().unwrap();
        gate.poison_for_test();
        assert!(
            stop_on_lifecycle(&lifecycle).await.is_err(),
            "unobservable authority fails closed"
        );
        gate.clear_test_poison();
    }

    #[test]
    fn unsafe_or_unconfirmed_retirement_never_projects_success() {
        for code in ["module_vowifi_starting", "module_vowifi_call_active"] {
            assert_eq!(StopFailure::from_code(code).status, StatusCode::CONFLICT);
        }
        for code in [
            "module_vowifi_cleanup_unconfirmed",
            "module_vowifi_worker_failed",
        ] {
            let failure = StopFailure::from_code(code);
            assert_eq!(failure.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(failure.code, code);
        }
    }
}
