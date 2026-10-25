//! Project independently observed tunnel, registration and consumer facts.
//! Historical facts from a released attempt cannot make its successor ready.
use super::*;

pub(super) struct ConnectionFacts {
    pub(super) tunnel_connected: bool,
    pub(super) ims_registered: bool,
}

pub(super) fn project(
    value: &mut serde_json::Value,
    intent: &intent::Snapshot,
    session: Option<&ordinary::status::Snapshot>,
    stopped: bool,
) -> ConnectionFacts {
    let cleanup_pending = !intent.cleanup_confirmed
        && (stopped
            || matches!(
                intent.phase,
                intent::Phase::Draining | intent::Phase::CleanupUnconfirmed
            ));
    value["stopping"] = json!(cleanup_pending);
    value["cleanup_confirmed"] = json!(intent.cleanup_confirmed);
    let active = !stopped
        && matches!(
            intent.phase,
            intent::Phase::Starting | intent::Phase::Registered
        );
    let live = session.filter(|s| {
        active
            && matches!(
                s.phase,
                ordinary::Phase::Starting | ordinary::Phase::Registered
            )
            && intent
                .generation
                .is_none_or(|generation| generation == s.binding.session_generation)
    });
    let tunnel_connected = live.is_some_and(|s| s.tunnel_ready);
    let ims_registered = live.is_some_and(|s| s.registered);
    let consumers_registered = intent.phase == intent::Phase::Registered
        && live.is_some_and(|s| s.phase == ordinary::Phase::Registered && s.registered);
    let connecting = !stopped
        && matches!(
            intent.phase,
            intent::Phase::Starting | intent::Phase::Backoff
        );
    let failed = !stopped
        && matches!(
            intent.phase,
            intent::Phase::Failed | intent::Phase::CleanupUnconfirmed
        );
    let sms_ready = consumers_registered && live.is_some_and(|s| s.sms_ready);
    let call_ready = consumers_registered && live.is_some_and(|s| s.call_ready);
    value["tunnel_state"] = json!(if tunnel_connected {
        "connected"
    } else if connecting {
        "connecting"
    } else if failed {
        "error"
    } else {
        "disconnected"
    });
    value["ims_state"] = json!(if ims_registered {
        "registered"
    } else if live.is_some_and(|s| s.register_started) {
        "registering"
    } else if failed {
        "failed"
    } else {
        "idle"
    });
    value["sms_ready"] = json!(sms_ready);
    value["call_ready"] = json!(call_ready);
    value["sms_state"] = json!(if sms_ready { "ready" } else { "unavailable" });
    value["call_state"] = json!(if call_ready { "ready" } else { "unavailable" });
    let (step, description) = if sms_ready {
        (5, "IMS 已注册，SMS 安全通道已就绪")
    } else if ims_registered {
        (4, "IMS 已注册，正在等待 SMS 安全通道")
    } else if live.is_some_and(|s| s.register_started) {
        (4, "正在通过隧道注册 IMS...")
    } else if tunnel_connected {
        (3, "IKEv2、NAT-T 与用户态 ESP 隧道已就绪")
    } else if live.is_some_and(|s| s.tunnel_started) {
        (2, "正在通过 EAP-AKA 建立 IKEv2 隧道...")
    } else if connecting {
        (1, "SIM 卡已就绪")
    } else {
        (0, "")
    };
    value["progress_step"] = json!(step);
    value["progress_desc"] = json!(if cleanup_pending {
        "正在释放 VoWiFi 资源，尚未确认关闭"
    } else if !stopped && intent.phase == intent::Phase::Backoff {
        "连接已中断，正在等待重新连接"
    } else {
        description
    });
    ConnectionFacts {
        tunnel_connected,
        ims_registered,
    }
}

#[cfg(test)]
mod tests;

/// A typed safe diagnostic can explain failure; it never changes readiness,
/// cleanup, or the stable termination code used by reconnect classification.
pub(super) fn failure(
    value: &mut serde_json::Value,
    code: Option<&'static str>,
    diagnostic: Option<super::super::startup_diagnostics::Diagnostic>,
) {
    if let Some(code) = code {
        value["error_code"] = json!(code);
        value["message"] = json!(code);
        if let Some(diagnostic) = diagnostic {
            value["message"] = json!(diagnostic.message);
            value["startup_failure"] = json!(diagnostic);
        }
    }
}
