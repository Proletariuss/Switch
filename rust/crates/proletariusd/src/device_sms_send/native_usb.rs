//! Native direct USB SMS route. Display caches are never submission authority.
use super::*;
impl Service {
    pub(super) fn native_usb_selected(&self, body: &SendRequest) -> bool {
        matches!(body.device.as_str(), "" | "dji")
            && crate::module_discovery::uses_native_usb(&self.module_radio, &self.usb_at)
    }
    pub(super) async fn capture_native_usb(&self, body: &SendRequest) -> Result<Binding> {
        // A configured transport is not a present/ready SMS device. Keep the
        // legacy selection errors before subscription/persistent-intent checks.
        if self.usb_at.device_observation_description().is_none()
            || self.module_radio.usb_transport_session().is_none()
        {
            return Err(self.missing_native_usb(body.device.is_empty()).await);
        }
        if !body.expected_subscription_id.is_empty() {
            return Err(conflict());
        }
        if self.native_module_sms.has_intent() || self.module_radio.vowifi_owns_sim_channel() {
            return Err(native_reader::expired());
        }
        let service = self.clone();
        let implicit = body.device.is_empty();
        // A disappearing prepare waiter cannot leave an acquired RF reservation
        // behind while the real identity read is still running.
        self.spawn_owned(async move {
            let session = service
                .module_radio
                .usb_transport_session()
                .ok_or_else(|| {
                    service.usb_prepare_not_ready(implicit, "DJI 模块短信通道正在连接")
                })?;
            let reservation =
                service.module_radio.reserve_esim().await.map_err(|_| {
                    service.usb_prepare_not_ready(implicit, "DJI 模块短信通道正在连接")
                })?;
            let observed = service.usb_at.capture_native_sms(&session).await;
            let released = service.module_radio.release_esim(reservation).await;
            if released.is_err() {
                return Err(service.usb_prepare_not_ready(implicit, "DJI 模块短信通道正在连接"));
            }
            let (actual, generation) = observed
                .map_err(|_| service.usb_prepare_not_ready(implicit, "DJI 模块短信通道正在连接"))?;
            if actual != session
                || service.module_radio.usb_transport_session().as_deref() != Some(session.as_str())
                || service.native_module_sms.has_intent()
                || !service
                    .usb_at
                    .native_sms_binding_current(&session, generation)
            {
                return Err(native_reader::expired());
            }
            Ok(Binding {
                binding: format!("{RUST_USB_SESSION_BINDING_PREFIX}{session}:{generation}"),
                device: "dji".into(),
                subscription_id: String::new(),
                owner: "rust_usb_at".into(),
                expected_iccid: String::new(),
            })
        })?
        .await
        .map_err(|_| self.usb_prepare_not_ready(implicit, "DJI 模块状态暂不可读取"))?
        .into_inner()
    }

    fn usb_prepare_not_ready(&self, implicit: bool, detail: &str) -> Failure {
        // These are the established selection codes/status/recoverable values,
        // not a claim that the durable send store failed. No CMGS is admitted.
        if implicit {
            return Failure(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({
                    "code":"sms_device_unavailable", "error":"当前没有可用的短信发送设备", "recoverable":true,
                }),
            );
        }
        let phone = self
            .usb_at
            .cached_device_observation()
            .filter(|v| v.status["sim_inserted"] == true)
            .and_then(|v| {
                v.status["phone_number"]
                    .as_str()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "未知号码".into());
        Failure(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({
                "code":"sms_device_not_ready", "error":format!("{phone}_DJI：{detail}"), "recoverable":true,
            }),
        )
    }

    async fn missing_native_usb(&self, implicit: bool) -> Failure {
        let hardware = self
            .usb_at
            .native_hardware_observation()
            .await
            .ok()
            .filter(|v| self.usb_at.hardware_observation_current(v.owner_epoch));
        let unknown = hardware
            .as_ref()
            .is_none_or(|v| v.source == crate::module_discovery::ObservationSource::Unavailable);
        if implicit && unknown {
            return Failure(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({
                    "code":"sms_device_selection_unavailable", "error":"无法确认短信通道状态，请稍后重试", "recoverable":true,
                }),
            );
        }
        let detail = if unknown {
            "DJI 模块状态暂不可读取"
        } else if hardware.is_some_and(|v| v.device.is_some()) {
            "DJI 模块短信通道正在连接"
        } else {
            "未检测到 DJI 模块"
        };
        self.usb_prepare_not_ready(implicit, detail)
    }
    pub(super) fn current_usb_binding(&self, session: &str, generation: u64) -> bool {
        self.module_radio.usb_transport_session().as_deref() == Some(session)
            && !self.module_radio.vowifi_owns_sim_channel()
            && !self.native_module_sms.has_intent()
            && if self.usb_at.owns_native_sms_binding(session, generation) {
                self.usb_at.native_sms_binding_current(session, generation)
            } else {
                self.usb_at
                    .has_active_sim_identity_binding(session, generation)
            }
    }
    pub(super) async fn execute_native_usb(
        &self,
        session: String,
        generation: u64,
        phone: String,
        message: String,
    ) -> Result<Value> {
        let reservation = self
            .module_radio
            .reserve_esim()
            .await
            .map_err(|_| unknown())?;
        let outcome = if self.module_radio.usb_transport_session().as_deref()
            == Some(session.as_str())
            && !self.native_module_sms.has_intent()
            && self.usb_at.native_sms_binding_current(&session, generation)
        {
            self.usb_at
                .submit_native_sms(session, generation, phone, message)
                .await
                .map_err(|_| unknown())
        } else {
            Err(unknown())
        };
        let released = self.module_radio.release_esim(reservation).await;
        if released.is_err() {
            return Err(unknown());
        }
        let outcome = outcome.map_err(|_| unknown())?;
        Ok(json!({"accepted":outcome.complete,"segments":outcome.accepted_segments}))
    }
}
#[cfg(test)]
mod tests;
