#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub port: String,
    pub esim_available: bool,
    pub usb_device: Option<HealthUsbDevice>,
    pub discovery_error: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthUsbDevice {
    pub product: String,
    pub vendor: String,
    pub vendor_id: String,
    pub product_id: String,
    pub location_id: String,
    pub speed: String,
    pub mode: String,
    pub interfaces: Option<Vec<HealthUsbInterface>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthUsbInterface {
    pub number: i64,
    #[serde(rename = "class")]
    pub class_id: i64,
    pub subclass: i64,
    #[serde(rename = "protocol")]
    pub protocol_id: i64,
    pub endpoints: i64,
}

#[derive(Debug, Serialize)]
pub struct RuntimeResponse<'a> {
    pub ok: bool,
    pub backend: &'a str,
    /// Version embedded in the Rust backend package.
    pub version: &'a str,
    /// Version and source identity supplied by the signed macOS build.
    pub app_release_version: &'a str,
    pub build_number: &'a str,
    pub source_digest: &'a str,
    pub migration_stage: &'a str,
    pub hardware_ownership: bool,
    pub hardware_owner: &'a str,
    pub go_backend_connected: bool,
    pub go_backend_restarting: bool,
    pub go_backend_phase: &'a str,
    pub go_backend_generation: u64,
    pub go_restart_attempts: u32,
    pub proxy_contract: &'a str,
    pub sim_protocol_contract: &'a str,
    pub ike_wire_contract: &'a str,
    pub ims_protocol_contract: &'a str,
    pub sms_protocol_contract: &'a str,
    pub call_protocol_contract: &'a str,
    pub store_contract: &'a str,
    pub snapshot_schema_version: u16,
    pub ownership_schema_version: u16,
    pub default_runtime: &'a str,
    pub cutover_ready: bool,
    pub sms_store_health: SmsStoreHealth,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct SmsStoreStatus {
    pub state: &'static str,
    pub reason: Option<&'static str>,
    pub restart_required: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct SmsStoreHealth {
    pub schema_version: u16,
    pub degraded: bool,
    pub device_send: SmsStoreStatus,
    pub nwait: SmsStoreStatus,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct MigrationComponent<'a> {
    pub id: &'a str,
    pub owner: &'a str,
    pub state: &'a str,
    pub requires_go_runtime: bool,
    pub blocks_go_removal: bool,
    pub detail: &'a str,
}

#[derive(Debug, Serialize)]
pub struct MigrationReadinessResponse<'a> {
    pub schema_version: u16,
    pub ready_to_remove_go: bool,
    /// Number of components that no longer require the Go runtime.  These
    /// counters make migration progress observable without inferring it from
    /// the component list in each client.
    pub rust_owned_components: usize,
    pub go_blocking_components: usize,
    pub rust_ownership_percent: u8,
    pub components: &'a [MigrationComponent<'a>],
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse<'a> {
    pub error: &'a str,
    pub code: &'a str,
    pub retryable: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpstreamHealth {
    pub ok: bool,
    pub port: Option<String>,
    pub esim_available: Option<bool>,
    pub discovery_error: Option<String>,
    pub usb_device: Option<UpstreamUsbDevice>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpstreamUsbDevice {
    pub product: Option<String>,
    pub vendor: Option<String>,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub location_id: Option<String>,
    pub speed: Option<String>,
    pub mode: Option<String>,
    pub interfaces: Option<Vec<UpstreamUsbInterface>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpstreamUsbInterface {
    pub number: Option<i64>,
    #[serde(rename = "class")]
    pub class_id: Option<i64>,
    pub subclass: Option<i64>,
    #[serde(rename = "protocol")]
    pub protocol_id: Option<i64>,
    pub endpoints: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpstreamDeviceStatus {
    pub imei: Option<String>,
    pub firmware: Option<String>,
    pub iccid: Option<String>,
    pub imsi: Option<String>,
    pub phone_number: Option<String>,
    pub native_spn: Option<String>,
    pub native_mcc: Option<String>,
    pub native_mnc: Option<String>,
    pub gid1: Option<String>,
    pub gid2: Option<String>,
    pub pnn: Option<Value>,
    pub opl: Option<Value>,
    pub sim_service_table: Option<Value>,
    #[serde(rename = "operator")]
    pub operator_name: Option<String>,
    pub sim_inserted: Option<bool>,
    pub signal_dbm: Option<i64>,
    pub signal_rsrp: Option<i64>,
    pub signal_rsrq: Option<i64>,
    pub signal_sinr: Option<i64>,
    pub nr5g_signal_sinr: Option<i64>,
    pub radio_band: Option<String>,
    pub radio_channel: Option<u64>,
    pub reg_status: Option<i64>,
    pub reg_status_text: Option<String>,
    pub ps_attached: Option<bool>,
    pub lac: Option<String>,
    pub cell_id: Option<String>,
    pub apn: Option<String>,
    pub ims_status: Option<i64>,
    pub network_mode: Option<String>,
    pub network_duplex: Option<String>,
    pub usbnet_mode: Option<i64>,
    pub operating_mode: Option<i64>,
    pub hardware_status: Option<String>,
    pub discovery_error: Option<String>,
    pub usb_device: Option<UpstreamUsbDevice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qmi: Option<UpstreamQmiStatus>,
}

/// Read-only lifecycle of the optional QMI owner. Session initialization does
/// not prove a live link, SIM state, radio service, or IMS registration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UpstreamQmiStatus {
    pub enabled: bool,
    pub session_initialized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initialization_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpstreamReaderList {
    #[serde(default)]
    pub readers: Vec<String>,
    pub selected: Option<String>,
    /// Read-only card observations captured with this inventory. Older
    /// clients may ignore them; absence is not evidence for auto-selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<ReaderDiscoveryDevice>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_revision: Option<u64>,
    /// Generation of the currently selected reader lifecycle, or of the
    /// empty selection that this inventory observed.  Public lifecycle
    /// mutations bind to this value so a delayed request cannot be applied
    /// after a disconnect/reconnect transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reader_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_connect: Option<ReaderAutoConnect>,
}

/// A read-only proposal, revalidated by the existing lifecycle owner before
/// any connect transition. It is not a lease or a physical-close receipt.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReaderAutoConnect {
    pub reader: String,
    pub inventory_revision: u64,
    pub reader_generation: u64,
    pub instance_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpstreamReaderInfo {
    pub status: UpstreamReaderStatus,
    pub usim_info: Option<UpstreamUsimInfo>,
    pub epdg_fqdn: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpstreamReaderStatus {
    pub connected: bool,
    pub reader_name: Option<String>,
    pub reader_generation: Option<u64>,
    pub has_card: bool,
    pub card_type: Option<String>,
    pub has_usim_info: bool,
    pub has_esim: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_muted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_unpowered: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_ready: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ready: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_ready: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpstreamUsimInfo {
    pub iccid: String,
    pub imsi: String,
    pub phone_number: Option<String>,
    pub operator_name: Option<String>,
    pub operator_source: Option<String>,
    pub mcc: String,
    pub mnc: String,
    pub gid1: Option<String>,
    pub smsc: Option<String>,
    pub private_identity: Option<String>,
    pub public_identity: Option<String>,
    pub home_domain: Option<String>,
    pub loaded_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceSnapshot {
    pub schema_version: u16,
    pub instance_id: String,
    pub revision: u64,
    pub ok: bool,
    pub complete: bool,
    pub captured_at_ms: u64,
    pub source: &'static str,
    pub module: ModuleSnapshot,
    pub reader: ReaderSnapshot,
    pub issues: Vec<SnapshotIssue>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ModuleSnapshot {
    pub health: Option<HealthResponse>,
    pub status: Option<UpstreamDeviceStatus>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ReaderSnapshot {
    #[serde(default)]
    pub readers: Vec<String>,
    pub selected: Option<String>,
    pub info: Option<UpstreamReaderInfo>,
    pub vowifi: Option<UpstreamReaderVoWiFiStatus>,
    pub inventory_source: String,
    pub inventory_revision: u64,
    /// Reader selection generation observed with this inventory snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reader_generation: Option<u64>,
    #[serde(default)]
    pub devices: Vec<ReaderDiscoveryDevice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_connect: Option<ReaderAutoConnect>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReaderDiscoveryDevice {
    pub name: String,
    pub card_state: String,
    pub in_use: bool,
    pub muted: bool,
    pub atr_hex: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpstreamReaderVoWiFiStatus {
    pub reader_id: Option<String>,
    pub sim_id: Option<String>,
    pub reader_generation: Option<u64>,
    pub available: bool,
    pub mcc: Option<String>,
    pub mnc: Option<String>,
    pub imsi: Option<String>,
    pub imei: Option<String>,
    pub epdg_fqdn: Option<String>,
    pub epdg_source: Option<String>,
    pub profile_id: Option<String>,
    pub profile_name: Option<String>,
    pub profile_source: Option<String>,
    pub tunnel_state: String,
    pub ims_state: String,
    pub message: Option<String>,
    pub local_ipv4: Option<String>,
    pub local_ipv6: Option<String>,
    pub pcscf: Option<Vec<String>>,
    pub ike_encryption: Option<String>,
    pub esp_encryption: Option<String>,
    pub registered_msisdn: Option<String>,
    pub ims_transport: Option<String>,
    pub ims_security_mode: Option<String>,
    pub sms_ready: Option<bool>,
    pub sms_state: Option<String>,
    pub call_ready: Option<bool>,
    pub call_state: Option<String>,
    #[serde(default)]
    pub progress_step: u8,
    pub progress_desc: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotIssue {
    pub endpoint: &'static str,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

/// Versioned, one-time transfer of Swift's forwarding admission progress.
/// This payload contains opaque message digests and a cursor, never message
/// text, provider credentials, or Keychain values.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SmsForwardingHandoffV1 {
    pub schema_version: u16,
    pub handoff_id: String,
    pub device_sms: DeviceSmsForwardingHandoff,
    pub nwait_sms: NwaitSmsForwardingHandoff,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSmsForwardingHandoff {
    /// SHA-256 `notificationID` values already acknowledged by Swift.
    pub accepted_notification_ids: Vec<String>,
    /// `None` means the old defaults key was never set.
    #[serde(default)]
    pub was_enabled: Option<bool>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NwaitSmsForwardingHandoff {
    /// Opaque archive change cursor; never interpreted as a notification cursor.
    #[serde(default)]
    pub cursor: Option<String>,
    pub seeded: bool,
    /// `None` means the old defaults key was never set.
    #[serde(default)]
    pub was_enabled: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmsForwardingHandoffError {
    UnsupportedVersion,
    InvalidHandoffId,
    TooManyAcceptedIds,
    InvalidAcceptedId,
    DuplicateAcceptedId,
    CursorTooLong,
}

impl SmsForwardingHandoffV1 {
    pub fn validate(&self) -> Result<(), SmsForwardingHandoffError> {
        if self.schema_version != 1 {
            return Err(SmsForwardingHandoffError::UnsupportedVersion);
        }
        if !is_canonical_lowercase_uuid(&self.handoff_id) {
            return Err(SmsForwardingHandoffError::InvalidHandoffId);
        }
        if self.device_sms.accepted_notification_ids.len() > 100_000 {
            return Err(SmsForwardingHandoffError::TooManyAcceptedIds);
        }
        let mut previous = std::collections::HashSet::with_capacity(
            self.device_sms.accepted_notification_ids.len(),
        );
        for id in &self.device_sms.accepted_notification_ids {
            if id.len() != 64
                || !id
                    .as_bytes()
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
            {
                return Err(SmsForwardingHandoffError::InvalidAcceptedId);
            }
            if !previous.insert(id) {
                return Err(SmsForwardingHandoffError::DuplicateAcceptedId);
            }
        }
        if self
            .nwait_sms
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.len() > 4096)
        {
            return Err(SmsForwardingHandoffError::CursorTooLong);
        }
        Ok(())
    }
}

impl std::fmt::Debug for SmsForwardingHandoffV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SmsForwardingHandoffV1")
            .field("schema_version", &self.schema_version)
            .field("handoff_id", &"[REDACTED]")
            .field(
                "accepted_notification_id_count",
                &self.device_sms.accepted_notification_ids.len(),
            )
            .field("device_was_enabled", &self.device_sms.was_enabled)
            .field("nwait_cursor_present", &self.nwait_sms.cursor.is_some())
            .field("nwait_seeded", &self.nwait_sms.seeded)
            .field("nwait_was_enabled", &self.nwait_sms.was_enabled)
            .finish()
    }
}

fn is_canonical_lowercase_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes[index] == b'-')
        && bytes.iter().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index)
                || byte.is_ascii_digit()
                || (b'a'..=b'f').contains(byte)
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn reader_observation_survives_typed_aggregate_without_breaking_legacy_status() {
        let legacy = serde_json::json!({"status":{
            "connected":true,"reader_name":"Reader","reader_generation":7,
            "has_card":true,"has_usim_info":false,"has_esim":false
        }});
        let old: super::UpstreamReaderInfo = serde_json::from_value(legacy.clone()).unwrap();
        assert!(old.status.card_state.is_none());
        assert!(old.status.metadata_ready.is_none());
        let encoded_old = serde_json::to_value(old).unwrap();
        assert!(encoded_old["status"].get("card_state").is_none());
        assert_eq!(encoded_old["status"]["has_card"], true);

        let mut current = legacy;
        let additions = serde_json::json!({"card_state":"present","presence_source":"pcsc",
            "presence_error":"scan_failed","card_muted":true,"card_unpowered":false,
            "card_ready":false,"session_ready":true,"metadata_ready":false});
        for (key, value) in additions.as_object().unwrap() {
            current["status"][key] = value.clone();
        }
        let parsed: super::UpstreamReaderInfo = serde_json::from_value(current).unwrap();
        let encoded = serde_json::to_value(parsed).unwrap();
        for (key, value) in additions.as_object().unwrap() {
            assert_eq!(&encoded["status"][key], value, "aggregate lost {key}");
        }
        assert!(encoded["usim_info"].is_null());
    }

    use super::{
        SmsForwardingHandoffError, SmsForwardingHandoffV1, UpstreamDeviceStatus, UpstreamHealth,
        UpstreamReaderInfo, UpstreamReaderList, UpstreamReaderVoWiFiStatus,
    };

    const HEALTH_CONNECTED: &str = include_str!("../fixtures/health-connected.json");
    const STATUS_CONNECTED: &str = include_str!("../fixtures/status-connected.json");
    const READER_LIST_CONNECTED: &str = include_str!("../fixtures/reader-list-connected.json");
    const READER_INFO_CONNECTED: &str = include_str!("../fixtures/reader-info-connected.json");
    const READER_VOWIFI_CONNECTED: &str = include_str!("../fixtures/reader-vowifi-connected.json");

    #[test]
    fn decodes_disconnected_module_shape() {
        let status: UpstreamDeviceStatus = serde_json::from_str(
            r#"{"operator":"未连接","signal_dbm":null,"sim_inserted":false,"usb_device":null}"#,
        )
        .expect("status fixture");
        assert_eq!(status.operator_name.as_deref(), Some("未连接"));
        assert_eq!(status.signal_dbm, None);
        assert_eq!(status.sim_inserted, Some(false));
    }

    #[test]
    fn preserves_module_metrics_and_typed_usb_shape() {
        let status: UpstreamDeviceStatus = serde_json::from_str(
            r#"{
                "operator":"Example",
                "signal_dbm":-83,
                "signal_rsrp":-102,
                "signal_rsrq":-12,
                "signal_sinr":18,
                "reg_status":1,
                "ps_attached":true,
                "ims_status":1,
                "usbnet_mode":1,
                "usb_device":{
                    "product":"Modem",
                    "vendor_id":"2ca3",
                    "interfaces":[{"number":2,"class":255,"protocol":1}]
                }
            }"#,
        )
        .expect("full module fixture");
        assert_eq!(status.signal_rsrp, Some(-102));
        assert_eq!(status.signal_sinr, Some(18));
        assert_eq!(status.usbnet_mode, Some(1));
        let usb = status.usb_device.expect("typed USB device");
        assert_eq!(usb.vendor_id.as_deref(), Some("2ca3"));
        assert_eq!(usb.interfaces.expect("interfaces")[0].class_id, Some(255));
    }

    #[test]
    fn replays_sanitized_connected_device_contract() {
        let health: UpstreamHealth =
            serde_json::from_str(HEALTH_CONNECTED).expect("connected health fixture");
        assert!(health.ok);
        let health_usb = health.usb_device.expect("fixture USB device");
        assert_eq!(health_usb.product_id.as_deref(), Some("4006"));
        assert_eq!(
            health_usb.interfaces.expect("fixture interfaces")[0].protocol_id,
            Some(1)
        );

        let status: UpstreamDeviceStatus =
            serde_json::from_str(STATUS_CONNECTED).expect("connected status fixture");
        assert_eq!(status.operator_name.as_deref(), Some("Fixture Carrier"));
        assert_eq!(status.signal_rsrp, Some(-102));
        assert_eq!(status.radio_channel, Some(1300));
        assert_eq!(status.ims_status, Some(1));
        assert_eq!(status.operating_mode, Some(0));
        let replayed = serde_json::to_value(&status).expect("serialize connected status");
        assert_eq!(replayed["phone_number"], "+10000000000");
        assert_eq!(replayed["network_duplex"], "FDD");
        assert_eq!(replayed["sim_service_table"]["kind"], "ust");

        let list: UpstreamReaderList =
            serde_json::from_str(READER_LIST_CONNECTED).expect("connected reader list fixture");
        let info: UpstreamReaderInfo =
            serde_json::from_str(READER_INFO_CONNECTED).expect("connected reader info fixture");
        let vowifi: UpstreamReaderVoWiFiStatus =
            serde_json::from_str(READER_VOWIFI_CONNECTED).expect("connected reader VoWiFi fixture");
        assert_eq!(list.selected.as_deref(), info.status.reader_name.as_deref());
        assert!(info.status.connected);
        assert_eq!(
            info.usim_info
                .expect("fixture USIM identity")
                .operator_name
                .as_deref(),
            Some("Fixture Carrier")
        );
        assert_eq!(vowifi.tunnel_state, "connected");
        assert_eq!(vowifi.ims_state, "registered");
        assert_eq!(vowifi.sms_ready, Some(true));
        assert_eq!(vowifi.call_ready, Some(true));
        let replayed = serde_json::to_value(vowifi).expect("serialize VoWiFi fixture");
        assert_eq!(replayed["transport_mode"], "enhanced-mode-utun");
    }

    #[test]
    fn reader_list_allows_no_selection() {
        let list: UpstreamReaderList =
            serde_json::from_str(r#"{"readers":["Reader A"]}"#).expect("reader list");
        assert_eq!(list.readers, ["Reader A"]);
        assert_eq!(list.selected, None);
        assert!(list.devices.is_none());
        assert!(serde_json::to_value(list).unwrap().get("devices").is_none());
    }

    #[test]
    fn reader_list_retains_additive_card_observations_without_changing_names_or_selection() {
        let raw = serde_json::json!({"readers":["Reader","BLE:fixture"],"selected":"Reader",
            "devices":[{"name":"Reader","card_state":"present","in_use":true,"muted":false,"atr_hex":"003b"},
                       {"name":"BLE:fixture","card_state":"unknown","in_use":false,"muted":false,"atr_hex":""}]});
        let list: UpstreamReaderList = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(list.readers, ["Reader", "BLE:fixture"]);
        assert_eq!(list.selected.as_deref(), Some("Reader"));
        assert_eq!(list.devices.as_ref().unwrap()[1].card_state, "unknown");
        assert_eq!(serde_json::to_value(list).unwrap(), raw);
    }

    #[test]
    fn reader_info_accepts_optional_identity_fields() {
        let info: UpstreamReaderInfo = serde_json::from_str(
            r#"{"status":{"connected":true,"reader_name":"Reader A","has_card":true,"card_type":"euicc","has_usim_info":false,"has_esim":true}}"#,
        )
        .expect("reader info");
        assert!(info.status.connected);
        assert_eq!(info.status.reader_name.as_deref(), Some("Reader A"));
        assert!(info.usim_info.is_none());
    }

    #[test]
    fn forwarding_handoff_v1_preserves_independent_source_progress_and_redacts_debug() {
        let digest = "a".repeat(64);
        let raw = format!(
            r#"{{"schema_version":1,"handoff_id":"123e4567-e89b-12d3-a456-426614174000","device_sms":{{"accepted_notification_ids":["{digest}"],"was_enabled":null}},"nwait_sms":{{"cursor":"cursor-opaque","seeded":true,"was_enabled":false}}}}"#
        );
        let handoff: SmsForwardingHandoffV1 =
            serde_json::from_str(&raw).expect("versioned forwarding handoff");
        handoff.validate().expect("valid source snapshot");
        assert_eq!(handoff.device_sms.was_enabled, None);
        assert_eq!(handoff.nwait_sms.cursor.as_deref(), Some("cursor-opaque"));
        assert!(handoff.nwait_sms.seeded);
        assert_eq!(handoff.nwait_sms.was_enabled, Some(false));
        let debug = format!("{handoff:?}");
        assert!(!debug.contains(&digest));
        assert!(!debug.contains("cursor-opaque"));
    }

    #[test]
    #[ignore = "requires scripts/test-forwarding-handoff-compat.sh for Swift verification"]
    fn swift_forwarding_handoff_fixture_round_trips() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        use std::path::PathBuf;

        let input = PathBuf::from(
            std::env::var_os("SWITCH_FORWARDING_HANDOFF_FIXTURE_PATH")
                .expect("cross-language script must provide the Swift fixture"),
        );
        let output = PathBuf::from(
            std::env::var_os("SWITCH_FORWARDING_HANDOFF_ROUNDTRIP_PATH")
                .expect("cross-language script must provide the Rust roundtrip path"),
        );
        let switch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .expect("resolve Switch root");
        let build_root = switch_root
            .join("build")
            .canonicalize()
            .expect("resolve build root");
        for path in [&input, &output] {
            let parent = path
                .parent()
                .and_then(|parent| parent.canonicalize().ok())
                .expect("fixture parent exists");
            assert!(
                parent.starts_with(&build_root),
                "fixture must stay in Switch/build"
            );
        }

        let bytes = std::fs::read(&input).expect("read Swift snapshot fixture");
        let handoff: SmsForwardingHandoffV1 =
            serde_json::from_slice(&bytes).expect("decode Swift handoff DTO");
        handoff.validate().expect("validate Swift handoff DTO");
        assert_eq!(handoff.schema_version, 1);
        assert_eq!(
            handoff.device_sms.accepted_notification_ids,
            ["a".repeat(64)]
        );
        assert_eq!(handoff.device_sms.was_enabled, Some(true));
        assert_eq!(handoff.nwait_sms.cursor.as_deref(), Some("opaque-cursor"));
        assert!(handoff.nwait_sms.seeded);
        assert_eq!(handoff.nwait_sms.was_enabled, Some(false));

        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut file = options
            .open(&output)
            .expect("create Rust roundtrip fixture without overwrite");
        file.write_all(&serde_json::to_vec(&handoff).expect("encode Rust handoff DTO"))
            .expect("write Rust roundtrip fixture");
        file.sync_all().expect("synchronize Rust roundtrip fixture");
        std::fs::File::open(output.parent().expect("fixture parent"))
            .expect("open fixture directory")
            .sync_all()
            .expect("synchronize fixture directory");
    }

    #[test]
    fn forwarding_handoff_rejects_versions_ids_duplicates_and_unreviewed_fields() {
        let id = "a".repeat(64);
        let body = |version: u16, ids: Vec<String>| {
            serde_json::json!({
                "schema_version": version,
                "handoff_id": "123e4567-e89b-12d3-a456-426614174000",
                "device_sms": {"accepted_notification_ids": ids, "was_enabled": true},
                "nwait_sms": {"cursor": null, "seeded": false, "was_enabled": null}
            })
        };
        let unsupported: SmsForwardingHandoffV1 =
            serde_json::from_value(body(2, Vec::new())).unwrap();
        assert_eq!(
            unsupported.validate(),
            Err(SmsForwardingHandoffError::UnsupportedVersion)
        );

        let duplicate: SmsForwardingHandoffV1 =
            serde_json::from_value(body(1, vec![id.clone(), id])).unwrap();
        assert_eq!(
            duplicate.validate(),
            Err(SmsForwardingHandoffError::DuplicateAcceptedId)
        );

        let malformed: SmsForwardingHandoffV1 =
            serde_json::from_value(body(1, vec!["not-a-digest".to_owned()])).unwrap();
        assert_eq!(
            malformed.validate(),
            Err(SmsForwardingHandoffError::InvalidAcceptedId)
        );

        let with_credentials = r#"{"schema_version":1,"handoff_id":"123e4567-e89b-12d3-a456-426614174000","device_sms":{"accepted_notification_ids":[]},"nwait_sms":{"seeded":false},"telegram_token":"not-allowed"}"#;
        assert!(serde_json::from_str::<SmsForwardingHandoffV1>(with_credentials).is_err());

        let mut bad_uuid: SmsForwardingHandoffV1 =
            serde_json::from_value(body(1, Vec::new())).unwrap();
        bad_uuid.handoff_id = "123E4567-E89B-12D3-A456-426614174000".to_owned();
        assert_eq!(
            bad_uuid.validate(),
            Err(SmsForwardingHandoffError::InvalidHandoffId)
        );

        let mut long_cursor: SmsForwardingHandoffV1 =
            serde_json::from_value(body(1, Vec::new())).unwrap();
        long_cursor.nwait_sms.cursor = Some("c".repeat(4097));
        assert_eq!(
            long_cursor.validate(),
            Err(SmsForwardingHandoffError::CursorTooLong)
        );
    }
}
