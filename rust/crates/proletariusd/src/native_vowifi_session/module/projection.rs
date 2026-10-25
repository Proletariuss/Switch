use super::*;

pub(super) fn progress(
    snapshot: Option<&ordinary::status::Snapshot>,
    stopping: bool,
) -> (u8, &'static str) {
    if stopping {
        (0, "正在释放 VoWiFi 资源，尚未确认关闭")
    } else if snapshot.is_some_and(|s| s.sms_ready && s.registered) {
        (5, "IMS 已注册，SMS 安全通道已就绪")
    } else if snapshot.is_some_and(|s| s.registered) {
        (4, "IMS 已注册，正在等待 SMS 安全通道")
    } else if snapshot.is_some_and(|s| s.register_started) {
        (4, "正在通过隧道注册 IMS...")
    } else if snapshot.is_some_and(|s| s.tunnel_ready) {
        (3, "IKEv2、NAT-T 与用户态 ESP 隧道已就绪")
    } else if snapshot.is_some_and(|s| s.tunnel_started) {
        (2, "正在通过 EAP-AKA 建立 IKEv2 隧道...")
    } else {
        (1, "正在读取 SIM 身份与运营商信息")
    }
}
impl Service {
    /// Cache-only overlay while this exact owner holds the shared SIM channel.
    /// Preparing, lost or stopping capabilities cannot publish a previous SIM.
    pub(crate) fn metadata_observation(&self) -> Option<Value> {
        let record = self.record()?;
        if !record.claim.try_is_current().unwrap_or(true) {
            return None;
        }
        let state = record.state.lock().unwrap_or_else(|e| e.into_inner());
        let card = state.card.as_ref().filter(|c| c.is_current());
        let radio = state.card.as_ref().map(|c| c.status());
        let mode = radio.and_then(|c| c.cfun_mode).map_or(-1, i32::from);
        let mut value = json!({"iccid":"","imsi":"","phone_number":"","operator":"",
            "sim_inserted":false,"signal_dbm":-999,"reg_status":0,"ps_attached":false,
            "operating_mode":mode,"network_mode":"VoWiFi 正在启动","reg_status_text":"模块 VoWiFi 占用 SIM 通道"});
        if let Some(card) = card {
            let identity = card.identity();
            value["iccid"] = json!(identity.iccid.as_str());
            value["imsi"] = json!(identity.imsi.as_str());
            value["sim_inserted"] = json!(true);
            value["native_mcc"] = json!(identity.mcc);
            value["native_mnc"] = json!(identity.mnc);
            value["native_spn"] = json!(identity.spn);
            let snapshot = state.handle.as_ref().map(|h| h.snapshot());
            let number = snapshot
                .as_ref()
                .and_then(|s| s.registered_msisdn.as_deref())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&identity.phone_number);
            value["phone_number"] = json!(number);
            value["operator"] = state
                .base
                .as_ref()
                .and_then(|b| b.get("profile_name"))
                .cloned()
                .unwrap_or(json!("Wi-Fi Calling"));
        }
        if mode == 4 {
            value["network_mode"] = json!("Wi-Fi Calling");
            value["reg_status_text"] = json!("蜂窝射频已关闭（Wi-Fi Calling）");
        }
        Some(value)
    }
    pub(crate) fn session_row(&self) -> Option<Value> {
        let record = self.record()?;
        if *record.stop.borrow() || !matches!(record.claim.try_is_current(), Ok(true)) {
            return None;
        }
        let state = record.state.lock().unwrap_or_else(|e| e.into_inner());
        let handle = state.handle.as_ref()?;
        let card = state.card.as_ref().filter(|c| c.is_current())?;
        let snapshot = handle.snapshot();
        if state.stopping
            || state.retired
            || matches!(
                snapshot.phase,
                ordinary::Phase::Draining | ordinary::Phase::Released
            )
        {
            return None;
        }
        let mut value = state.base.clone().unwrap_or_else(|| json!({}));
        value["reader_id"] = json!(card.reader());
        value["reader_generation"] = json!(card.generation());
        value["sim_id"] = json!(card.identity().iccid.as_str());
        value["available"] = json!(true);
        value["imsi"] = json!(card.identity().imsi.as_str());
        value["mcc"] = json!(card.identity().mcc);
        value["mnc"] = json!(card.identity().mnc);
        value["tunnel_state"] = json!(if snapshot.tunnel_ready {
            "connected"
        } else if snapshot.termination.is_some() {
            "error"
        } else if snapshot.tunnel_started {
            "connecting"
        } else {
            "disconnected"
        });
        value["ims_state"] = json!(if snapshot.registered {
            "registered"
        } else if snapshot.termination.is_some() {
            "failed"
        } else if snapshot.register_started {
            "registering"
        } else {
            "idle"
        });
        value["sms_ready"] = json!(snapshot.sms_ready);
        value["call_ready"] = json!(snapshot.call_ready);
        value["sms_state"] = json!(if snapshot.sms_ready {
            "ready"
        } else {
            "unavailable"
        });
        value["call_state"] = json!(if snapshot.call_ready {
            "ready"
        } else {
            "unavailable"
        });
        let (step, description) = progress(Some(&snapshot), false);
        value["progress_step"] = json!(step);
        value["progress_desc"] = json!(description);
        if let Some(error) = &state.error {
            value["message"] = json!(error);
        }
        if let Some(v) = snapshot.local_ipv4.first() {
            value["local_ipv4"] = json!(v.to_string());
        }
        if let Some(v) = snapshot.local_ipv6.first() {
            value["local_ipv6"] = json!(v.to_string());
        }
        if !snapshot.pcscf.is_empty() {
            value["pcscf"] = json!(
                snapshot
                    .pcscf
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            );
        }
        if let Some(v) = &snapshot.registered_msisdn {
            value["registered_msisdn"] = json!(v);
        }
        if let Some(v) = snapshot.ims_transport {
            value["ims_transport"] = json!(v);
        }
        value["ims_security_mode"] = json!(if snapshot.ims_secured {
            "ipsec"
        } else {
            "none"
        });
        value["incoming_listener_started"] = json!(snapshot.incoming.listener_started);
        value["incoming_accept_count"] = json!(snapshot.incoming.accept_count);
        value["incoming_reachability"] = json!(snapshot.incoming.reachability.as_str());
        if let Some(error) = snapshot.incoming.last_response_write_error {
            value["incoming_last_response_write_error"] = json!(error.as_str());
        }
        if let Some(at) = snapshot
            .incoming
            .last_sip_at
            .and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|v| i128::try_from(v.as_nanos()).ok())
            .and_then(|v| time::OffsetDateTime::from_unix_timestamp_nanos(v).ok())
            .and_then(|v| {
                v.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
        {
            value["incoming_last_sip_at"] = json!(at);
        }
        Some(value)
    }
}
