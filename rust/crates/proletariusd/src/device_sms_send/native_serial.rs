//! Direct serial SMS capabilities bind the daemon, physical session and SIM.
use super::*;
use crate::modem_actor::SerialSmsBinding;

pub(super) fn serial_binding_namespace(value: &str) -> bool {
    [
        RUST_MODEM_SESSION_BINDING_PREFIX,
        RUST_MODEM_GENERATION_BINDING_PREFIX,
        RUST_MODEM_BINDING_PREFIX,
    ]
    .iter()
    .any(|prefix| value.starts_with(prefix))
}

fn encode(binding: &SerialSmsBinding) -> Option<String> {
    if !valid_token(&binding.owner_epoch)
        || binding.owner_epoch.contains(':')
        || binding.serial_session == 0
        || binding.sim_generation == 0
    {
        return None;
    }
    let token = format!(
        "{RUST_MODEM_SESSION_BINDING_PREFIX}{}:{}:{}",
        binding.owner_epoch, binding.serial_session, binding.sim_generation
    );
    valid_token(&token).then_some(token)
}

pub(super) fn serial_session_binding(value: &str) -> Option<SerialSmsBinding> {
    let mut fields = value
        .strip_prefix(RUST_MODEM_SESSION_BINDING_PREFIX)?
        .split(':');
    let binding = SerialSmsBinding {
        owner_epoch: fields.next()?.into(),
        serial_session: fields.next()?.parse().ok()?,
        sim_generation: fields.next()?.parse().ok()?,
    };
    (fields.next().is_none() && encode(&binding).as_deref() == Some(value)).then_some(binding)
}

impl Service {
    pub(super) fn native_serial_selected(&self, body: &SendRequest) -> bool {
        matches!(body.device.as_str(), "" | "dji")
            && (self.modem_actor.native_serial_managed()
                || self.module_radio.configured_usb_transport() == Some(false))
    }

    pub(super) async fn current_serial_binding(&self, expected: SerialSmsBinding) -> bool {
        if self.module_radio.serial_transport_session() != Some(expected.serial_session)
            || self.module_radio.vowifi_owns_sim_channel()
            || self.native_module_sms.has_intent()
        {
            return false;
        }
        let session = expected.serial_session;
        let actor = self.modem_actor.clone();
        let current =
            tokio::task::spawn_blocking(move || actor.serial_sms_binding_current(&expected))
                .await
                .ok()
                .and_then(|v| v.ok())
                .unwrap_or(false);
        current
            && self.module_radio.serial_transport_session() == Some(session)
            && !self.module_radio.vowifi_owns_sim_channel()
            && !self.native_module_sms.has_intent()
    }

    pub(super) async fn capture_native_serial(&self, body: &SendRequest) -> Result<Binding> {
        if self.native_module_sms.has_intent() || self.module_radio.vowifi_owns_sim_channel() {
            return Err(native_reader::expired());
        }
        let actor = self.modem_actor.clone();
        let observation = tokio::task::spawn_blocking(move || actor.serial_sms_observation())
            .await
            .ok()
            .and_then(|v| v.ok());
        let implicit = body.device.is_empty();
        let observation = observation.ok_or_else(|| serial_not_ready(implicit, true, ""))?;
        let expected = observation
            .binding
            .filter(|_| observation.ready)
            .ok_or_else(|| serial_not_ready(implicit, observation.unknown, &observation.phone))?;
        if self.module_radio.serial_transport_session() != Some(expected.serial_session) {
            return Err(serial_not_ready(implicit, true, ""));
        }
        if !body.expected_subscription_id.is_empty() {
            return Err(conflict());
        }
        if implicit && self.native_sms.has_intent() {
            return Err(Failure(
                StatusCode::CONFLICT,
                json!({
                    "error":"原生读卡器 VoWiFi 已占用短信通道，请明确选择 reader 或 dji",
                    "code":"sms_device_selection_required", "recoverable":true
                }),
            ));
        }
        let binding = encode(&expected).ok_or_else(unavailable)?;
        if !self.current_serial_binding(expected).await {
            return Err(native_reader::expired());
        }
        Ok(Binding {
            binding,
            device: "dji".into(),
            subscription_id: String::new(),
            owner: "rust_modem".into(),
            expected_iccid: String::new(),
        })
    }
}

fn serial_not_ready(implicit: bool, unknown: bool, phone: &str) -> Failure {
    let (code, message) = if implicit && unknown {
        (
            "sms_device_selection_unavailable",
            "无法确认短信通道状态，请稍后重试".into(),
        )
    } else if implicit {
        (
            "sms_device_unavailable",
            "当前没有可用的短信发送设备".into(),
        )
    } else {
        let phone = if phone.trim().is_empty() {
            "未知号码"
        } else {
            phone.trim()
        };
        let detail = if unknown {
            "DJI 模块状态暂不可读取"
        } else {
            "DJI 模块短信通道正在连接"
        };
        ("sms_device_not_ready", format!("{phone}_DJI：{detail}"))
    };
    Failure(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({"code":code,"error":message,"recoverable":true}),
    )
}

#[cfg(test)]
pub(super) mod tests;
