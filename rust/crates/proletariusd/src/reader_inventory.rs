use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use proletarius_api_contract::{
    ErrorResponse, ReaderAutoConnect, ReaderDiscoveryDevice, UpstreamReaderList,
};
use proletarius_reader::{
    CardPresence, DiscoveryError, DiscoverySnapshot, PcscReaderScanner, ReaderCardStatus,
    ReaderObservation, ReaderScanner,
};
use std::path::PathBuf;
use std::sync::OnceLock;

use std::time::Duration;

use crate::ServerState;

const BLE_STATUS_TIMEOUT: Duration = Duration::from_secs(2);

fn scanner() -> &'static crate::blocking_pool::BlockingPool {
    static SCANNER: OnceLock<crate::blocking_pool::BlockingPool> = OnceLock::new();
    SCANNER.get_or_init(|| crate::blocking_pool::BlockingPool::new(1))
}

pub(crate) async fn scan()
-> Result<Result<Vec<ReaderObservation>, DiscoveryError>, tokio::task::JoinError> {
    // Inventory polling must not create a blocking thread for every UI refresh.
    // Cancellation drops waiting scans; a scan already in PC/SC owns its slot
    // until the system call returns, even if the HTTP client disconnected.
    scanner().run(|| PcscReaderScanner.scan()).await
}

/// Card readiness adds power state without creating a second PC/SC scan pool.
/// The ordinary inventory and disappearance monitor keep their original DTO.
pub(crate) async fn scan_card_status()
-> Result<Result<Vec<ReaderCardStatus>, DiscoveryError>, tokio::task::JoinError> {
    scanner().run(|| PcscReaderScanner.scan_card_status()).await
}

pub(crate) async fn handle(state: &ServerState) -> Response<Full<Bytes>> {
    let observations = match scan().await {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return failure(StatusCode::BAD_GATEWAY, &error.to_string()),
        Err(error) => return failure(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let discovery = reconcile(state, observations).await;
    let mut response = response_from_discovery(&discovery);
    response.reader_generation = Some(state.reader_gate.selection_snapshot().generation);
    response.instance_id = Some(state.snapshot_instance_id.clone());
    response.auto_connect = proposal(state, &discovery);
    crate::json_response(StatusCode::OK, &response)
}

/// Selection policy consumes one inventory and the retained lifecycle facts.
/// A retained/pending owner suppresses replacement even when enumeration
/// misses it; only the lifecycle's physical-close path can retire that owner.
pub(crate) fn propose(
    discovery: &DiscoverySnapshot,
    selection: &crate::reader_reservation::SelectionSnapshot,
    instance: &str,
) -> Option<ReaderAutoConnect> {
    if selection.pending
        || !selection.reader.is_empty()
        || discovery.selected.is_some()
        || discovery.revision == u64::MAX
        || instance.is_empty()
    {
        return None;
    }
    let mut names = std::collections::BTreeSet::new();
    if discovery.readers.iter().any(|reader| {
        reader.name.trim().is_empty()
            || reader.name.len() > 512
            || reader.name.chars().any(char::is_control)
            || !names.insert(reader.name.as_str())
    }) {
        return None;
    }
    let reader = discovery
        .readers
        .iter()
        .find(|reader| reader.card == CardPresence::Present && !reader.muted)?;
    Some(ReaderAutoConnect {
        reader: reader.name.clone(),
        inventory_revision: discovery.revision,
        reader_generation: selection.generation,
        instance_id: instance.to_owned(),
    })
}

pub(crate) fn proposal(
    state: &ServerState,
    discovery: &DiscoverySnapshot,
) -> Option<ReaderAutoConnect> {
    let current = state
        .reader_discovery
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if current.snapshot() != *discovery {
        return None;
    }
    propose(
        discovery,
        &state.reader_gate.selection_snapshot(),
        &state.snapshot_instance_id,
    )
}

/// Publish native selection only when present in the current observations.
/// CoreBluetooth status comes directly from its Swift system bridge; Rust
/// never accepts selection authority from the compatibility backend.
pub(crate) async fn reconcile(
    state: &ServerState,
    observations: Vec<ReaderObservation>,
) -> DiscoverySnapshot {
    let mut observations = observations;
    let selected = crate::reader_reservation::gate().selected_reader();
    let selected_name = selected.as_deref().unwrap_or_default();
    let socket = std::env::var_os("PROLETARIUS_BLE_SOCKET").map(PathBuf::from);
    let bridge_facts = tokio::time::timeout(
        BLE_STATUS_TIMEOUT,
        crate::reader_ble_status::reader_status_facts(
            socket.as_deref(),
            state.app_token.as_deref(),
            selected_name,
        ),
    )
    .await
    .unwrap_or_else(|_| Err("BLE status query timed out".into()));
    match bridge_facts {
        Ok((Some(name), _)) => append_ble_reader(&mut observations, &name),
        Ok((None, crate::reader_ble_status::ReaderPresence::Indeterminate)) | Err(_)
            if valid_ble_reader(selected_name) =>
        {
            // Preserve the selected BLE identity while CoreBluetooth reports
            // a transition or cannot answer. An incomplete status sample is
            // not evidence that the active reader disappeared.
            append_ble_reader(&mut observations, selected_name);
        }
        _ => {}
    }
    state
        .reader_discovery
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .reconcile(observations, selected.as_deref())
}

fn append_ble_reader(observations: &mut Vec<ReaderObservation>, name: &str) {
    if !valid_ble_reader(name) || observations.iter().any(|reader| reader.name == name) {
        return;
    }
    observations.push(ReaderObservation {
        name: name.to_owned(),
        // A CoreBluetooth reader connection does not observe a removable
        // card slot. Explicit BLE Use/Connect remains a separate user action.
        card: CardPresence::Unknown,
        in_use: false,
        muted: false,
        atr: Vec::new(),
    });
}

fn valid_ble_reader(name: &str) -> bool {
    let Some(id) = name.strip_prefix("BLE:") else {
        return false;
    };
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn response_from_discovery(discovery: &DiscoverySnapshot) -> UpstreamReaderList {
    UpstreamReaderList {
        readers: discovery
            .readers
            .iter()
            .map(|reader| reader.name.clone())
            .collect(),
        selected: discovery.selected.clone(),
        devices: Some(discovery.readers.iter().map(discovery_device).collect()),
        inventory_revision: Some(discovery.revision),
        reader_generation: None,
        instance_id: None,
        auto_connect: None,
    }
}

/// List fallback and aggregate snapshots expose the same observation fields.
/// PRESENT/non-MUTE is an auto-connect hint, not an APDU success guarantee.
pub(crate) fn discovery_device(reader: &ReaderObservation) -> ReaderDiscoveryDevice {
    ReaderDiscoveryDevice {
        name: reader.name.clone(),
        card_state: reader.card.as_str().into(),
        in_use: reader.in_use,
        muted: reader.muted,
        atr_hex: encode_hex(&reader.atr),
    }
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn failure(status: StatusCode, detail: &str) -> Response<Full<Bytes>> {
    let message = format!("枚举读卡器失败：{detail}");
    crate::json_response(
        status,
        &ErrorResponse {
            error: &message,
            code: "reader_list_failed",
            retryable: true,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proletarius_reader::{CardPresence, ReaderDiscovery, ReaderObservation};

    fn observed(name: &str, card: CardPresence, muted: bool) -> ReaderObservation {
        ReaderObservation {
            name: name.into(),
            card,
            muted,
            in_use: false,
            atr: vec![],
        }
    }

    #[test]
    fn auto_selection_uses_unambiguous_same_inventory_card_facts() {
        let empty = crate::reader_reservation::SelectionSnapshot::default();
        let mut inventory = ReaderDiscovery::default();
        for observations in [
            vec![],
            vec![observed("Unknown", CardPresence::Unknown, false)],
            vec![observed("Muted", CardPresence::Present, true)],
            vec![
                observed("Duplicate", CardPresence::Present, false),
                observed("Duplicate", CardPresence::Present, false),
            ],
        ] {
            let snapshot = inventory.reconcile(observations, None);
            assert!(propose(&snapshot, &empty, "instance").is_none());
        }
        let snapshot = inventory.reconcile(
            vec![
                observed("BLE", CardPresence::Unknown, false),
                observed("Ready", CardPresence::Present, false),
            ],
            None,
        );
        let proposal = propose(&snapshot, &empty, "instance").unwrap();
        assert_eq!(proposal.reader, "Ready");
        assert_eq!(proposal.inventory_revision, snapshot.revision);
        assert_eq!(proposal.reader_generation, empty.generation);
        assert_eq!(proposal.instance_id, "instance");
    }

    #[test]
    fn retained_or_pending_owner_is_not_replaced_by_an_inventory_miss() {
        let snapshot = DiscoverySnapshot {
            revision: 2,
            readers: vec![observed("Replacement", CardPresence::Present, false)],
            selected: None,
        };
        for selection in [
            crate::reader_reservation::SelectionSnapshot {
                reader: "Retained absent from enumeration".into(),
                generation: 7,
                ..Default::default()
            },
            crate::reader_reservation::SelectionSnapshot {
                pending: true,
                generation: 7,
                ..Default::default()
            },
        ] {
            assert!(propose(&snapshot, &selection, "instance").is_none());
        }
    }

    #[test]
    fn reordered_inventory_and_generation_changes_rebind_the_proposal() {
        let mut inventory = ReaderDiscovery::default();
        let mut selection = crate::reader_reservation::SelectionSnapshot::default();
        let a = observed("A", CardPresence::Present, false);
        let b = observed("B", CardPresence::Present, false);
        let first = inventory.reconcile(vec![a.clone(), b.clone()], None);
        let original = propose(&first, &selection, "instance").unwrap();
        let reordered = inventory.reconcile(vec![b, a], None);
        let replacement = propose(&reordered, &selection, "instance").unwrap();
        assert_eq!(replacement.reader, "B");
        assert_ne!(replacement.inventory_revision, original.inventory_revision);
        selection.generation += 1;
        assert_ne!(
            propose(&reordered, &selection, "instance").unwrap(),
            replacement
        );
        assert_ne!(
            propose(&reordered, &selection, "new-instance").unwrap(),
            replacement
        );
        let exhausted = DiscoverySnapshot {
            revision: u64::MAX,
            ..reordered
        };
        assert!(propose(&exhausted, &selection, "instance").is_none());
    }

    #[test]
    fn missing_native_selection_is_removed_from_the_read_only_snapshot() {
        let readers = ["Reader A".to_owned()];
        let response = UpstreamReaderList {
            readers: readers.to_vec(),
            selected: Some("Reader B".to_owned()),
            devices: None,
            inventory_revision: None,
            reader_generation: None,
            instance_id: None,
            auto_connect: None,
        };
        let mut discovery = ReaderDiscovery::default();
        let snapshot = discovery.reconcile(
            vec![ReaderObservation {
                name: "Reader A".into(),
                card: CardPresence::Present,
                in_use: false,
                muted: false,
                atr: vec![],
            }],
            response.selected.as_deref(),
        );
        assert_eq!(snapshot.selected, None);
        assert_eq!(snapshot.readers.len(), 1);
    }

    #[test]
    fn native_response_does_not_require_an_upstream_payload() {
        let discovery = DiscoverySnapshot {
            revision: 7,
            readers: vec![ReaderObservation {
                name: "Reader A".into(),
                card: CardPresence::Present,
                in_use: false,
                muted: false,
                atr: vec![0x3b, 0x00],
            }],
            selected: Some("Reader A".into()),
        };
        let response = response_from_discovery(&discovery);
        assert_eq!(response.readers, vec!["Reader A"]);
        assert_eq!(response.selected.as_deref(), Some("Reader A"));
    }

    #[test]
    fn pending_native_selection_clears_previous_discovery_hint() {
        let mut discovery = ReaderDiscovery::default();
        let observations = vec![ReaderObservation {
            name: "Reader A".into(),
            card: CardPresence::Present,
            in_use: false,
            muted: false,
            atr: vec![],
        }];
        assert_eq!(
            discovery
                .reconcile(observations.clone(), Some("Reader A"))
                .selected
                .as_deref(),
            Some("Reader A")
        );
        // A pending/disconnected native owner supplies no selection, even if
        // the reader is still present and the previous discovery selected it.
        assert_eq!(discovery.reconcile(observations, None).selected, None);
    }

    #[test]
    fn ble_identity_requires_uuid_shape() {
        assert!(valid_ble_reader("BLE:12345678-1234-1234-1234-123456789abc"));
        assert!(!valid_ble_reader("BLE:../outside"));
        assert!(!valid_ble_reader("USB reader"));
    }

    #[test]
    fn ble_bridge_connection_does_not_fabricate_card_presence() {
        let name = "BLE:12345678-1234-1234-1234-123456789abc";
        let mut observations = Vec::new();
        append_ble_reader(&mut observations, name);
        append_ble_reader(&mut observations, name);
        append_ble_reader(&mut observations, "BLE:invalid");
        assert_eq!(observations.len(), 1);
        let reader = &observations[0];
        assert_eq!(reader.name, name);
        assert_eq!(reader.card, CardPresence::Unknown);
        assert!(!reader.muted);
        assert!(!reader.in_use);
        assert!(reader.atr.is_empty());
    }

    #[test]
    fn native_list_exposes_same_scan_card_evidence_without_claiming_ble_presence() {
        let discovery = DiscoverySnapshot {
            revision: 1,
            selected: None,
            readers: vec![
                ReaderObservation {
                    name: "Reader".into(),
                    card: CardPresence::Present,
                    in_use: true,
                    muted: false,
                    atr: vec![0x00, 0x3b],
                },
                ReaderObservation {
                    name: "BLE:fixture".into(),
                    card: CardPresence::Unknown,
                    in_use: false,
                    muted: false,
                    atr: vec![],
                },
            ],
        };
        let response = response_from_discovery(&discovery);
        let devices = response.devices.as_ref().unwrap();
        assert_eq!(response.readers, ["Reader", "BLE:fixture"]);
        assert!(response.selected.is_none());
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].card_state, "present");
        assert!(devices[0].in_use);
        assert!(!devices[0].muted);
        assert_eq!(devices[0].atr_hex, "003b");
        assert_eq!(devices[1].card_state, "unknown");
        assert!(devices[1].atr_hex.is_empty());
        assert_eq!(
            serde_json::to_value(response).unwrap()["selected"],
            serde_json::Value::Null
        );
    }
}
