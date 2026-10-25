use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ServerState;
use proletarius_api_contract::{
    DeviceSnapshot, ModuleSnapshot, ReaderSnapshot, SnapshotIssue, UpstreamReaderInfo,
    UpstreamReaderVoWiFiStatus,
};

const HEALTH_PATH: &str = "/api/health";
const STATUS_PATH: &str = "/api/status";
const READER_INFO_PATH: &str = "/api/reader/info";
const READER_VOWIFI_PATH: &str = "/api/reader/vowifi";
const RUST_READER_DISCOVERY_PATH: &str = "/api/v2/reader-discovery";
static INSTANCE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) async fn capture(state: &ServerState) -> DeviceSnapshot {
    // Assign the revision before any I/O starts. If concurrent captures finish
    // out of order, consumers can discard an older response deterministically.
    let revision = next_revision(&state.snapshot_revision);
    let (health_result, status_result, discovery_result) = tokio::join!(
        crate::health_projection::capture(state),
        crate::device_status_projection::capture_typed(state),
        crate::reader_inventory::scan(),
    );

    let mut issues = Vec::new();
    let health = result_or_issue(HEALTH_PATH, health_result, &mut issues);
    let status = result_or_issue(STATUS_PATH, status_result, &mut issues);
    let mut reader = ReaderSnapshot::default();

    match discovery_result {
        Ok(Ok(observations)) => {
            let discovery = crate::reader_inventory::reconcile(state, observations).await;
            reader.auto_connect = crate::reader_inventory::proposal(state, &discovery);
            reader.readers = discovery
                .readers
                .iter()
                .map(|item| item.name.clone())
                .collect();
            reader.selected = discovery.selected;
            reader.inventory_source = "rust-pcsc".into();
            reader.inventory_revision = discovery.revision;
            reader.reader_generation = Some(state.reader_gate.selection_snapshot().generation);
            reader.devices = discovery
                .readers
                .iter()
                .map(crate::reader_inventory::discovery_device)
                .collect();
        }
        Ok(Err(error)) => issues.push(SnapshotIssue {
            endpoint: RUST_READER_DISCOVERY_PATH,
            code: "rust_pcsc_discovery_failed",
            message: error.to_string(),
            retryable: true,
        }),
        Err(error) => issues.push(SnapshotIssue {
            endpoint: RUST_READER_DISCOVERY_PATH,
            code: "rust_pcsc_discovery_task_failed",
            message: error.to_string(),
            retryable: true,
        }),
    }

    let selected_binding = crate::reader_reservation::gate().selected_reader_generation();
    let selected_is_current = reader.selected.as_ref().is_some_and(|selected| {
        reader.readers.contains(selected)
            && selected_binding
                .as_ref()
                .is_some_and(|(name, _)| name == selected)
    });
    if reader.selected.is_some() && !selected_is_current {
        issues.push(SnapshotIssue {
            endpoint: RUST_READER_DISCOVERY_PATH,
            code: "reader_selection_stale",
            message: "selected reader changed or is not present in the enumerated reader list"
                .into(),
            retryable: true,
        });
        reader.selected = None;
    }

    if selected_is_current {
        let (info_result, vowifi_result) =
            tokio::join!(capture_reader_info(state), capture_reader_vowifi(state),);
        reader.info = result_or_issue(READER_INFO_PATH, info_result, &mut issues);
        reader.vowifi = result_or_issue(READER_VOWIFI_PATH, vowifi_result, &mut issues);
        retain_authoritative_selection(
            &mut reader,
            selected_binding.as_ref(),
            crate::reader_reservation::gate()
                .selected_reader_generation()
                .as_ref(),
            &mut issues,
        );
        validate_reader_identity(&mut reader, &mut issues);
    }

    let ok = health.is_some() || status.is_some() || reader.inventory_source == "rust-pcsc";
    DeviceSnapshot {
        schema_version: 2,
        instance_id: state.snapshot_instance_id.clone(),
        revision,
        ok,
        complete: issues.is_empty(),
        captured_at_ms: captured_at_ms(),
        source: "rust-native",
        module: ModuleSnapshot { health, status },
        reader,
        issues,
    }
}

async fn capture_reader_info(
    state: &ServerState,
) -> Result<UpstreamReaderInfo, proletarius_domain::BackendFailure> {
    let value = crate::reader_info_projection::capture(state).await?;
    serde_json::from_value(value).map_err(|error| {
        proletarius_domain::BackendFailure::Io(format!("native reader information DTO: {error}"))
    })
}

pub(crate) async fn capture_reader_vowifi(
    state: &ServerState,
) -> Result<UpstreamReaderVoWiFiStatus, proletarius_domain::BackendFailure> {
    let service = state
        .native_vowifi_lifecycle
        .service(state)
        .map_err(proletarius_domain::BackendFailure::Io)?;
    // The native owner also projects the idle/no-intent reader. Polling the
    // activity snapshot must not resurrect the retired Go reader status path.
    decode_native_vowifi(service.public_status().await)
}

// Frozen race oracle for the retired conditional compatibility observation.
#[cfg(test)]
async fn capture_reader_vowifi_with(
    native: impl Fn() -> Option<serde_json::Value>,
    compatibility: impl std::future::Future<
        Output = Result<UpstreamReaderVoWiFiStatus, proletarius_domain::BackendFailure>,
    >,
) -> Result<UpstreamReaderVoWiFiStatus, proletarius_domain::BackendFailure> {
    if let Some(status) = native() {
        return decode_native_vowifi(status);
    }
    let legacy = compatibility.await;
    // Go may finish an observation after a native start acquired ownership.
    // A failed native observation is authoritative too: never restore an old
    // ready state by falling back to the late compatibility response.
    match native() {
        Some(status) => decode_native_vowifi(status),
        None => legacy,
    }
}

fn decode_native_vowifi(
    value: serde_json::Value,
) -> Result<UpstreamReaderVoWiFiStatus, proletarius_domain::BackendFailure> {
    serde_json::from_value(value).map_err(|error| {
        proletarius_domain::BackendFailure::Io(format!("native reader VoWiFi status DTO: {error}"))
    })
}

pub(crate) fn new_instance_id() -> String {
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = INSTANCE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{started_at:x}-{sequence:x}", std::process::id())
}

fn next_revision(counter: &AtomicU64) -> u64 {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_add(1))
        })
        .unwrap_or(u64::MAX)
        .saturating_add(1)
}

// Card facts can arrive after a disconnect/reconnect to the same
// reader name. Only the unchanged native selection generation may publish them.
fn retain_authoritative_selection(
    reader: &mut ReaderSnapshot,
    expected: Option<&(String, u64)>,
    current: Option<&(String, u64)>,
    issues: &mut Vec<SnapshotIssue>,
) {
    if expected.is_none() || expected != current {
        reader.selected = None;
        reader.info = None;
        reader.vowifi = None;
        issues.push(SnapshotIssue {
            endpoint: RUST_READER_DISCOVERY_PATH,
            code: "reader_selection_changed",
            message: "reader selection changed while capturing its status".into(),
            retryable: true,
        });
        return;
    }
    let (name, generation) = expected.expect("checked native binding");
    if reader.info.as_ref().is_some_and(|info| {
        !info.status.connected
            || info.status.reader_name.as_deref() != Some(name.as_str())
            || info.status.reader_generation != Some(*generation)
    }) {
        reader.info = None;
        reader.vowifi = None;
        issues.push(SnapshotIssue {
            endpoint: READER_INFO_PATH,
            code: "reader_identity_mismatch",
            message: "reader info does not match the native selection generation".into(),
            retryable: true,
        });
    }
}

fn validate_reader_identity(reader: &mut ReaderSnapshot, issues: &mut Vec<SnapshotIssue>) {
    let reader_identity_valid = matches!(
        (&reader.selected, &reader.info),
        (Some(selected), Some(info))
            if info.status.connected
                && info.status.reader_name.as_deref() == Some(selected.as_str())
    );
    if !reader_identity_valid && reader.selected.is_some() && reader.info.is_some() {
        issues.push(SnapshotIssue {
            endpoint: READER_INFO_PATH,
            code: "reader_identity_mismatch",
            message: "reader info does not match the selected reader".into(),
            retryable: true,
        });
    }

    let Some(vowifi) = reader.vowifi.as_ref() else {
        return;
    };
    if !vowifi_requires_identity_binding(vowifi) {
        return;
    }

    let identity_issue = if !reader_identity_valid {
        Some((
            "vowifi_identity_unverified",
            "reader identity was unavailable while a VoWiFi session status was returned",
        ))
    } else {
        let selected = reader.selected.as_deref().unwrap_or_default();
        let info = reader.info.as_ref().expect("validated above");
        let expected_sim_id = info.usim_info.as_ref().and_then(current_sim_id);
        let generation_matches = matches!(
            (info.status.reader_generation, vowifi.reader_generation),
            (Some(expected), Some(actual)) if expected != 0 && expected == actual
        );
        let reader_matches = vowifi.reader_id.as_deref() == Some(selected);
        let sim_matches =
            expected_sim_id.is_some_and(|expected| vowifi.sim_id.as_deref() == Some(expected));
        if generation_matches && reader_matches && sim_matches {
            None
        } else {
            Some((
                "vowifi_identity_mismatch",
                "VoWiFi session does not belong to the selected reader/SIM generation",
            ))
        }
    };

    if let Some((code, message)) = identity_issue {
        // A status snapshot is read-only. The captured session owner remains responsible
        // for teardown, while Rust refuses to re-publish an unbound ready
        // state as if it belonged to the newly selected profile.
        reader.vowifi = None;
        issues.push(SnapshotIssue {
            endpoint: READER_VOWIFI_PATH,
            code,
            message: message.into(),
            retryable: true,
        });
    }
}

fn current_sim_id(info: &proletarius_api_contract::UpstreamUsimInfo) -> Option<&str> {
    let iccid = info.iccid.trim();
    if !iccid.is_empty() {
        return Some(iccid);
    }
    let imsi = info.imsi.trim();
    (!imsi.is_empty()).then_some(imsi)
}

fn vowifi_requires_identity_binding(vowifi: &UpstreamReaderVoWiFiStatus) -> bool {
    vowifi
        .reader_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || vowifi
            .sim_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || vowifi.reader_generation.is_some()
        || vowifi.tunnel_state != "disconnected"
        || vowifi.ims_state != "idle"
        || vowifi.sms_ready.unwrap_or(false)
        || vowifi.call_ready.unwrap_or(false)
}

fn result_or_issue<T>(
    endpoint: &'static str,
    result: Result<T, proletarius_domain::BackendFailure>,
    issues: &mut Vec<SnapshotIssue>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            issues.push(SnapshotIssue {
                endpoint,
                code: "upstream_snapshot_failed",
                message: error.to_string(),
                retryable: true,
            });
            None
        }
    }
}

fn captured_at_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use super::{
        capture_reader_vowifi_with, new_instance_id, next_revision, result_or_issue,
        retain_authoritative_selection, validate_reader_identity,
    };
    use crate::reader_inventory::encode_hex;
    use proletarius_api_contract::{ReaderSnapshot, UpstreamReaderInfo, UpstreamReaderStatus};
    use proletarius_domain::BackendFailure;
    use proletarius_reader::{CardPresence, ReaderObservation};

    #[tokio::test]
    async fn native_status_and_native_failure_never_poll_compatibility() {
        for native in [
            serde_json::json!({"available":true,"tunnel_state":"connected","ims_state":"registered"}),
            serde_json::json!({"available":false,"tunnel_state":"error","ims_state":"failed"}),
            serde_json::json!({"invalid":"native status"}),
        ] {
            let queried = std::cell::Cell::new(false);
            let result = capture_reader_vowifi_with(|| Some(native.clone()), async {
                queried.set(true);
                Err(BackendFailure::GoBackendUnavailable(
                    "must not query".into(),
                ))
            })
            .await;
            assert!(!queried.get());
            if native.get("invalid").is_some() {
                assert!(matches!(result, Err(BackendFailure::Io(_))));
            } else {
                assert_eq!(
                    result.unwrap().tunnel_state,
                    native["tunnel_state"].as_str().unwrap()
                );
            }
        }
    }

    #[tokio::test]
    async fn native_owner_acquired_during_compatibility_await_replaces_late_go_ready() {
        let native = std::cell::RefCell::new(None);
        let result = capture_reader_vowifi_with(|| native.borrow().clone(), async {
            native.replace(Some(
                serde_json::json!({"available":false,"tunnel_state":"error","ims_state":"failed"}),
            ));
            Ok(bound_reader_snapshot().vowifi.unwrap())
        })
        .await
        .unwrap();
        assert_eq!(result.tunnel_state, "error");
        assert_eq!(result.ims_state, "failed");
        assert!(!result.available);
    }

    #[tokio::test]
    async fn compatibility_status_is_used_only_without_native_observation() {
        let result = capture_reader_vowifi_with(|| None, async {
            Ok(bound_reader_snapshot().vowifi.unwrap())
        })
        .await
        .unwrap();
        assert_eq!(result.reader_id.as_deref(), Some("Reader A"));
        let result = capture_reader_vowifi_with(|| None, async {
            Err(BackendFailure::GoBackendUnavailable("offline".into()))
        })
        .await;
        assert!(matches!(
            result,
            Err(BackendFailure::GoBackendUnavailable(_))
        ));
    }

    #[test]
    fn instance_ids_remain_unique_under_concurrent_creation() {
        let workers = (0..8)
            .map(|_| std::thread::spawn(|| (0..128).map(|_| new_instance_id()).collect::<Vec<_>>()))
            .collect::<Vec<_>>();
        let ids = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 8 * 128);
    }

    #[test]
    fn snapshot_revision_is_monotonic_and_saturating() {
        let counter = AtomicU64::new(0);
        assert_eq!(next_revision(&counter), 1);
        assert_eq!(next_revision(&counter), 2);

        let saturated = AtomicU64::new(u64::MAX);
        assert_eq!(next_revision(&saturated), u64::MAX);
        assert_eq!(next_revision(&saturated), u64::MAX);
    }

    #[test]
    fn partial_failure_is_preserved_as_an_issue() {
        let mut issues = Vec::new();
        let value: Option<()> = result_or_issue(
            "/api/status",
            Err(BackendFailure::GoBackendUnavailable("timeout".into())),
            &mut issues,
        );
        assert!(value.is_none());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].endpoint, "/api/status");
        assert!(issues[0].retryable);
    }

    #[test]
    fn native_selection_generation_fences_pending_and_same_reader_aba_results() {
        let initial = ("Reader A".to_owned(), 7);
        for current in [
            None,
            Some(("Reader A".to_owned(), 8)),
            Some(("Reader B".to_owned(), 9)),
        ] {
            let mut reader = bound_reader_snapshot();
            let mut issues = Vec::new();
            retain_authoritative_selection(
                &mut reader,
                Some(&initial),
                current.as_ref(),
                &mut issues,
            );
            assert!(reader.selected.is_none());
            assert!(reader.info.is_none());
            assert!(reader.vowifi.is_none());
            assert_eq!(issues[0].code, "reader_selection_changed");
        }
    }

    #[test]
    fn stale_go_generation_cannot_publish_card_facts_under_current_native_selection() {
        let native = ("Reader A".to_owned(), 8);
        let mut reader = bound_reader_snapshot();
        let mut issues = Vec::new();
        retain_authoritative_selection(&mut reader, Some(&native), Some(&native), &mut issues);
        assert_eq!(reader.selected.as_deref(), Some("Reader A"));
        assert!(reader.info.is_none());
        assert!(reader.vowifi.is_none());
        assert_eq!(issues[0].code, "reader_identity_mismatch");
        let native = ("Reader A".to_owned(), 7);
        let mut reader = bound_reader_snapshot();
        issues.clear();
        retain_authoritative_selection(&mut reader, Some(&native), Some(&native), &mut issues);
        assert!(reader.info.is_some());
        assert!(reader.vowifi.is_some());
        assert!(issues.is_empty());
    }

    #[test]
    fn mismatched_reader_identity_is_reported() {
        let mut reader = ReaderSnapshot {
            readers: vec!["Reader A".into()],
            selected: Some("Reader A".into()),
            info: Some(UpstreamReaderInfo {
                status: UpstreamReaderStatus {
                    connected: true,
                    reader_name: Some("Reader B".into()),
                    reader_generation: Some(1),
                    has_card: true,
                    card_type: Some("euicc".into()),
                    has_usim_info: true,
                    has_esim: true,
                    card_state: None,
                    presence_source: None,
                    presence_error: None,
                    card_muted: None,
                    card_unpowered: None,
                    card_ready: None,
                    session_ready: None,
                    metadata_ready: None,
                },
                usim_info: None,
                epdg_fqdn: None,
            }),
            vowifi: None,
            inventory_source: "rust-pcsc".into(),
            inventory_revision: 1,
            reader_generation: Some(1),
            devices: Vec::new(),
            auto_connect: None,
        };
        let mut issues = Vec::new();
        validate_reader_identity(&mut reader, &mut issues);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "reader_identity_mismatch");
    }

    #[test]
    fn live_vowifi_is_quarantined_when_reader_sim_or_generation_mismatch() {
        for mismatch in ["reader", "sim", "generation", "missing_generation"] {
            let mut reader = bound_reader_snapshot();
            let status = reader.vowifi.as_mut().expect("live status");
            match mismatch {
                "reader" => status.reader_id = Some("Reader B".into()),
                "sim" => status.sim_id = Some("8985200014631959073".into()),
                "generation" => status.reader_generation = Some(6),
                "missing_generation" => status.reader_generation = None,
                _ => unreachable!(),
            }

            let mut issues = Vec::new();
            validate_reader_identity(&mut reader, &mut issues);

            assert!(reader.vowifi.is_none(), "{mismatch} status was retained");
            assert!(issues.iter().any(|issue| {
                issue.endpoint == "/api/reader/vowifi"
                    && issue.code == "vowifi_identity_mismatch"
                    && issue.retryable
            }));
        }
    }

    #[test]
    fn live_vowifi_is_not_reused_when_go_reader_identity_is_unavailable() {
        let mut reader = bound_reader_snapshot();
        reader.info = None;
        let mut issues = Vec::new();

        validate_reader_identity(&mut reader, &mut issues);

        assert!(reader.vowifi.is_none());
        assert!(issues.iter().any(|issue| {
            issue.endpoint == "/api/reader/vowifi"
                && issue.code == "vowifi_identity_unverified"
                && issue.retryable
        }));
    }

    fn bound_reader_snapshot() -> ReaderSnapshot {
        let info: UpstreamReaderInfo = serde_json::from_value(serde_json::json!({
            "status": {
                "connected": true,
                "reader_name": "Reader A",
                "reader_generation": 7,
                "has_card": true,
                "card_type": "euicc",
                "has_usim_info": true,
                "has_esim": true
            },
            "usim_info": {
                "iccid": "8985200014631959072",
                "imsi": "234330123456789",
                "mcc": "234",
                "mnc": "33"
            }
        }))
        .expect("reader info fixture");
        let vowifi = serde_json::from_value(serde_json::json!({
            "reader_id": "Reader A",
            "sim_id": "8985200014631959072",
            "reader_generation": 7,
            "available": true,
            "tunnel_state": "connected",
            "ims_state": "registered",
            "sms_ready": true,
            "call_ready": true
        }))
        .expect("VoWiFi fixture");
        ReaderSnapshot {
            readers: vec!["Reader A".into()],
            selected: Some("Reader A".into()),
            info: Some(info),
            vowifi: Some(vowifi),
            inventory_source: "rust-pcsc".into(),
            inventory_revision: 1,
            reader_generation: Some(7),
            devices: Vec::new(),
            auto_connect: None,
        }
    }

    #[test]
    fn reader_atr_is_encoded_without_go_inventory() {
        let observation = ReaderObservation {
            name: "A".into(),
            card: CardPresence::Present,
            in_use: true,
            muted: false,
            atr: vec![0x3b, 0x00],
        };
        assert_eq!(encode_hex(&observation.atr), "3b00");
    }
}
