use proletarius_api_contract::{MigrationComponent, MigrationReadinessResponse};

pub(crate) const OWNERSHIP_SCHEMA_VERSION: u16 = 2;

const COMPONENTS: &[MigrationComponent<'static>] = &[
    MigrationComponent {
        id: "gateway",
        owner: "rust",
        state: "native",
        requires_go_runtime: false,
        blocks_go_removal: false,
        detail: "Unix socket, authentication, native lifecycle supervision and recovery admission",
    },
    MigrationComponent {
        id: "device_snapshot",
        owner: "rust",
        state: "native-aggregate-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust aggregates native module health/status, reader inventory, authoritative reader identity and retained reader/module VoWiFi observations. Disconnected devices remain unavailable without a compatibility fallback. End-to-end physical acceptance remains pending.",
    },
    MigrationComponent {
        id: "dji_module",
        owner: "rust",
        state: "native-module-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust owns module discovery, metadata, serial/USB AT, optional QMI, SMS, radio policy, cellular call control and call-mode lifecycle, native LPA operations and module VoWiFi. Production startup and public dispatch require no Go child. Physical device/carrier acceptance and long-running recovery validation remain pending.",
    },
    MigrationComponent {
        id: "module_qmi_control",
        owner: "rust",
        state: "native-device-owner-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust selects the optional QMI backend and retains asynchronous libusb setup, CTL sync, IMS/IMSA/NAS/UIM client IDs, typed requests and joined cleanup. Late startup results retain their cleanup obligation. Physical QMI and shutdown acceptance remains pending.",
    },
    MigrationComponent {
        id: "modem_serial_port",
        owner: "rust",
        state: "native-serial-and-sms-owner-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust owns serial discovery, the exclusive handle, bounded AT queues, initialization and PDU-readiness gating, metadata and native SMS submission/collection. Stored-message work retains session/SIM/configuration fences, durable archive and verified cleanup. Native startup and policy consumers require no Go adapter. Physical serial/reconnect acceptance remains pending.",
    },
    MigrationComponent {
        id: "usb_at_transport",
        owner: "rust",
        state: "native-device-owner-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust owns the exclusive USB AT handle, bounded command transactions, leases, session fencing, device-status plans, SMS ingestion and storage cleanup proofs, and native eSIM/AKA/voice consumers. Physical USB re-enumeration and workflow acceptance remain pending.",
    },
    MigrationComponent {
        id: "module_radio_policy",
        owner: "rust",
        state: "native-policy-and-physical-plans-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust owns flight-mode preferences and APIs, startup/default RF policy, business reservations, persistent VoWiFi and cellular-call intents, and fixed-session USB/serial verification and restoration. Native startup, LPA and IMS lifecycle consumers retain the same admission and cleanup obligations. Physical recovery acceptance remains pending.",
    },
    MigrationComponent {
        id: "reader_inventory",
        owner: "rust",
        state: "native",
        requires_go_runtime: false,
        blocks_go_removal: false,
        detail: "PC/SC enumeration, revisioned discovery state, and confirmed disappearance handoff are Rust-owned; the native reader lifecycle retires the exact physical session and selection generation",
    },
    MigrationComponent {
        id: "reader_session",
        owner: "rust",
        state: "native-lifecycle-and-leases-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust owns connect/disconnect/refresh, selected-generation metadata, confirmed loss, PC/SC and BLE transport, APDU/reset/finalization and retained native leases. Reader, Nwait and module operations use native workers and journal/lease admission without a Go business hook. Physical PC/SC/BLE acceptance remains pending.",
    },
    MigrationComponent {
        id: "reader_ble_controls",
        owner: "rust",
        state: "native-with-swift-corebluetooth-bridge",
        requires_go_runtime: false,
        blocks_go_removal: false,
        detail: "Rust owns public BLE scan/connect/disconnect routing, request validation, bounded bridge framing, result projection and global admission against business owners and physical workers. An owned task retains both global and BLE I/O permits through bridge completion or its deadline even when the HTTP caller disappears. Swift retains CoreBluetooth. The Go compatibility lease route and expiry cleanup are removed; physical device acceptance remains pending",
    },
    MigrationComponent {
        id: "reader_identity",
        owner: "rust",
        state: "native-authoritative-snapshot",
        requires_go_runtime: false,
        blocks_go_removal: false,
        detail: "Rust reads and atomically publishes complete USIM/ISIM metadata with the reader selection generation and eUICC facts for PC/SC and BLE. Public information projection and native consumers use this authoritative snapshot; private receipts preserve SMSC routing. Failed refresh preserves the last valid snapshot. Swift retains the CoreBluetooth bridge.",
    },
    MigrationComponent {
        id: "reader_device_identity",
        owner: "rust",
        state: "native-persistence-and-provisioning",
        requires_go_runtime: false,
        blocks_go_removal: false,
        detail: "Rust exclusively owns the existing imei.json format, atomic persistence, first-use 15-digit IMEI generation and stable Luhn identity resolution for native VoWiFi consumers.",
    },
    MigrationComponent {
        id: "reader_vowifi",
        owner: "rust",
        state: "native-session-owner-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Public reader start/status/stop, session lists, calls, USSD and snapshot use retained Rust reader/module owners: exact card leases, IKE/ESP, protected IMS registration/renewal, SMS, calls, media and cleanup. No Go module session observation or alias dispatch remains. Physical card/carrier acceptance remains pending.",
    },
    MigrationComponent {
        id: "message_archive",
        owner: "rust",
        state: "native-store",
        requires_go_runtime: false,
        blocks_go_removal: false,
        detail: "Rust owns the locked atomic SMS archive, merged GET /api/sms API, native module AT stored-message ingestion and durable native IMS receive. No Go volatile inbox, producer, or delete/prune owner is required by the shipping runtime.",
    },
    MigrationComponent {
        id: "message_runtime",
        owner: "rust",
        state: "native-sms-runtime-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust owns native device facts and route selection, public send claims/archive/outcomes, serial/USB AT submission and collection, and reader/module/Nwait IMS MESSAGE transport and dispatch. Native IMS owns RP decoding, durable receive and acknowledgements, shared CSeq allocation, multipart receipts and no-replay unknown outcomes. Physical delivery/recovery acceptance remains pending.",
    },
    MigrationComponent {
        id: "multistandby_runtime_reporting",
        owner: "rust",
        state: "native-runtime-journal-report-and-scheduling",
        requires_go_runtime: false,
        blocks_go_removal: false,
        detail: "The retained Rust Nwait manager supplies actual attempt/generation observations directly to the compatible durable journal, online/recovery report and retry/renewal schedulers. Public Nwait uses the native facade; old Go manager, card broker, schedules and private store/runtime callbacks are retired. Offline compositions do not establish physical-card or carrier acceptance",
    },
    MigrationComponent {
        id: "sms_forwarding",
        owner: "rust",
        state: "native-queue-and-provider",
        requires_go_runtime: false,
        blocks_go_removal: false,
        detail: "Rust owns authenticated forwarding configuration, encrypted SWF1 queue admission/status/actions, Telegram/Bark providers and cancellable delivery lifecycle; Swift device/Nwait event producers use this Rust route",
    },
    MigrationComponent {
        id: "sms_rp_codec",
        owner: "rust",
        state: "native-codecs-and-ims-options-transaction-lease-owner",
        requires_go_runtime: false,
        blocks_go_removal: false,
        detail: "Rust owns RP-DATA, SMS-DELIVER and SMS-STATUS-REPORT codecs, originating and delivery-report SIP MESSAGE framing/CSeq allocation, and bounded OPTIONS transaction leases and fail-confirm decisions. Native IMS sessions own protected transport, stream framing/dispatch, durable receive acknowledgements, heartbeat execution, readiness and retained teardown. Physical runtime acceptance remains tracked by the related session and message components.",
    },
    MigrationComponent {
        id: "ims_heartbeat_supervisor",
        owner: "rust",
        state: "native-state-machine-and-transport-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust owns generation-scoped OPTIONS probes, confirmation timing, stale/duplicate fencing, protected SIP transport and response dispatch, retry timers, readiness effects and retained session teardown. Physical carrier recovery acceptance remains pending.",
    },
    MigrationComponent {
        id: "call_archive",
        owner: "rust",
        state: "native-facts-and-archive-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust cellular and IMS owners produce session-fenced call facts, immutable terminal records, durable archive and public history operations. Retained native cleanup waits for archive retirement. Physical call/history acceptance remains pending.",
    },
    MigrationComponent {
        id: "outgoing_call_policy",
        owner: "rust",
        state: "native-invite-and-sip-execution-acceptance-pending",
        requires_go_runtime: false,
        blocks_go_removal: true,
        detail: "Rust owns outgoing INVITE transitions, reliable provisional sequencing, early-media admission, CANCEL races, final ACK/BYE decisions, protected SIP delivery, dialog/media execution and retained cleanup. Physical voice/carrier acceptance remains pending.",
    },
    MigrationComponent {
        id: "mac_audio",
        owner: "swift",
        state: "native",
        requires_go_runtime: false,
        blocks_go_removal: false,
        detail: "CoreAudio device selection, AirPods routing and microphone/speaker bridge remain native Swift",
    },
];

pub(crate) fn readiness() -> MigrationReadinessResponse<'static> {
    let rust_owned_components = COMPONENTS
        .iter()
        .filter(|component| {
            component.owner == "rust"
                && !component.requires_go_runtime
                && !component.blocks_go_removal
        })
        .count();
    let go_blocking_components = COMPONENTS
        .iter()
        .filter(|component| component.requires_go_runtime || component.blocks_go_removal)
        .count();
    let total = COMPONENTS.len();
    let rust_ownership_percent = rust_owned_components
        .checked_mul(100)
        .and_then(|value| value.checked_div(total))
        .unwrap_or(0)
        .min(100) as u8;
    MigrationReadinessResponse {
        schema_version: OWNERSHIP_SCHEMA_VERSION,
        ready_to_remove_go: COMPONENTS
            .iter()
            .all(|component| !component.blocks_go_removal && !component.requires_go_runtime),
        rust_owned_components,
        go_blocking_components,
        rust_ownership_percent,
        components: COMPONENTS,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RouteOwner {
    Rust,
    GoCompatibility,
    Unknown,
}

pub(crate) fn route_owner(method: &str, path: &str) -> RouteOwner {
    let admission =
        crate::public_http::admission(&method.parse().unwrap_or(hyper::Method::GET), path);
    if matches!(
        admission,
        crate::public_http::Admission::MethodNotAllowed(_)
    ) {
        // The daemon rejects unsupported methods before route-owner dispatch;
        // these requests are fully handled by Rust and never reach Go.
        return RouteOwner::Rust;
    }
    let method = if method == "HEAD" && crate::public_http::head_uses_get(path) {
        "GET"
    } else {
        method
    };
    if matches!(
        (method, path),
        ("GET" | "POST", "/api/vowifi")
            | ("POST", "/api/reader/vowifi/ussd" | "/api/vowifi/reconcile")
    ) || (method == "POST"
        && matches!(
            path,
            "/api/reader/vowifi/calls/dial"
                | "/api/reader/vowifi/calls/answer"
                | "/api/reader/vowifi/calls/reject"
                | "/api/reader/vowifi/calls/hangup"
                | "/api/reader/vowifi/calls/dtmf"
                | "/api/vowifi/calls/dial"
                | "/api/vowifi/calls/answer"
                | "/api/vowifi/calls/reject"
                | "/api/vowifi/calls/hangup"
                | "/api/vowifi/calls/dtmf"
        ))
    {
        return RouteOwner::Rust;
    }
    if method == "GET" && matches!(path, "/api/reader/vowifi" | "/api/reader/vowifi/sessions") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/reader/vowifi" | "/api/reader/vowifi/reconcile") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/internal/ims/sms-transactions") {
        return RouteOwner::Rust;
    }
    if method == "POST"
        && matches!(
            path,
            "/api/call/answer" | "/api/call/dial" | "/api/call/dtmf" | "/api/call/hangup"
        )
    {
        return RouteOwner::Rust;
    }
    if method == "GET" && matches!(path, "/api/call/status") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/call/audio/start" | "/api/call/audio/stop") {
        return RouteOwner::Rust;
    }
    if method == "GET"
        && matches!(
            path,
            "/api/reader/vowifi/calls/status" | "/api/vowifi/calls/status"
        )
    {
        return RouteOwner::Rust;
    }
    if method == "GET" && matches!(path, "/api/call-mode/status") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/call-mode/download") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/call-mode/enable") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/call-mode/restore") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/call-mode/retry") {
        return RouteOwner::Rust;
    }
    if method == "GET" && matches!(path, "/api/vowifi/result") {
        return RouteOwner::Rust;
    }
    if method == "GET" && matches!(path, "/api/health") {
        return RouteOwner::Rust;
    }
    if method == "GET" && matches!(path, "/api/status") {
        return RouteOwner::Rust;
    }
    if (method == "GET" && matches!(path, "/api/sms/send-intents"))
        || (method == "POST" && matches!(path, "/api/sms/send-intents" | "/api/sms/send"))
    {
        return RouteOwner::Rust;
    }
    if (method == "GET" && matches!(path, "/api/diagnostics/events"))
        || (method == "POST" && matches!(path, "/api/internal/diagnostics/events/append"))
    {
        return RouteOwner::Rust;
    }
    if (method == "GET" && matches!(path, "/api/esim/notes"))
        || (method == "PUT" && matches!(path, "/api/esim/notes"))
    {
        return RouteOwner::Rust;
    }
    if (method == "GET" && matches!(path, "/api/esim/module-notes"))
        || (method == "PUT" && matches!(path, "/api/esim/module-notes"))
        || (method == "POST" && matches!(path, "/api/esim/phonebook/probe"))
        || (method == "POST" && matches!(path, "/api/esim/download"))
        || (method == "POST" && matches!(path, "/api/esim/download/confirm"))
        || (method == "POST" && matches!(path, "/api/esim/switch"))
        || (method == "PATCH" && matches!(path, "/api/esim/profile"))
        || (method == "DELETE" && matches!(path, "/api/esim/profile"))
    {
        return RouteOwner::Rust;
    }
    if (method == "GET" && matches!(path, "/api/reader/imei"))
        || (method == "PUT" && matches!(path, "/api/reader/imei"))
        || (method == "DELETE" && matches!(path, "/api/reader/imei"))
        || (method == "POST" && matches!(path, "/api/internal/reader-device-identity/resolve"))
    {
        return RouteOwner::Rust;
    }
    if method == "GET" && matches!(path, "/api/reader/ble/status") {
        return RouteOwner::Rust;
    }
    if method == "POST"
        && matches!(
            path,
            "/api/reader/ble/scan" | "/api/reader/ble/connect" | "/api/reader/ble/disconnect"
        )
    {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/reader/auth") {
        return RouteOwner::Rust;
    }
    if method == "GET"
        && matches!(
            path,
            "/api/reader/esim/health" | "/api/esim/health" | "/api/esim"
        )
    {
        return RouteOwner::Rust;
    }
    if (method == "GET" && matches!(path, "/api/sms/adopt"))
        || (method == "POST" && matches!(path, "/api/sms/adopt"))
        || (method == "GET" && matches!(path, "/api/sms/storage"))
    {
        return RouteOwner::Rust;
    }
    if (method == "GET" && path == "/api/sms/devices")
        || (method == "POST" && path == "/api/internal/sms/devices/select")
    {
        return RouteOwner::Rust;
    }
    if (method == "GET" && matches!(path, "/api/sms/forwarding/status"))
        || (method == "POST"
            && matches!(
                path,
                "/api/sms/forwarding/configure"
                    | "/api/sms/forwarding/enqueue"
                    | "/api/sms/forwarding/device-snapshot"
                    | "/api/sms/forwarding/test"
                    | "/api/sms/forwarding/queue-action"
            ))
    {
        return RouteOwner::Rust;
    }
    if crate::sms_export::is_route(path) {
        return RouteOwner::Rust;
    }
    if (method == "GET"
        && matches!(
            path,
            "/api/multistandby/messages"
                | "/api/multistandby/changes"
                | "/api/multistandby/history"
                | "/api/multistandby/conversations"
                | "/api/multistandby/cards"
                | "/api/multistandby/profiles"
                | "/api/multistandby/send-intents"
        ))
        || (method == "POST"
            && matches!(
                path,
                "/api/multistandby/send-intents" | "/api/multistandby/send-intents/submit"
            ))
        || (method == "DELETE" && path.starts_with("/api/multistandby/messages/"))
        || (method == "GET" && path == "/api/internal/usb-sms/inbox")
        || (method == "GET" && matches!(path, "/api/module/flight-mode" | "/api/network/recovery"))
        || (method == "POST" && matches!(path, "/api/network/recovery"))
        || (method == "POST"
            && matches!(
                path,
                "/api/module/flight-mode" | "/api/network/reboot-module" | "/api/network/usbnet"
            ))
        || (method == "POST"
            && matches!(
                path,
                "/api/internal/module-radio/configure"
                    | "/api/internal/module-radio/query"
                    | "/api/internal/module-radio/bootstrap"
                    | "/api/internal/module-radio/refresh"
                    | "/api/internal/module-radio/reservation"
                    | "/api/internal/module-radio/vowifi"
                    | "/api/internal/module-radio/action"
            ))
        || (method == "POST"
            && matches!(
                path,
                "/api/internal/usb-sms/refresh"
                    | "/api/internal/usb-sms/prune"
                    | "/api/internal/usb-sms/storage-operation"
            ))
        || path.starts_with(crate::message_archive::INTERNAL_PREFIX)
        || path.starts_with(crate::call_archive::INTERNAL_PREFIX)
        || (method == "POST"
            && matches!(
                path,
                "/api/internal/reader-business" | "/api/internal/reader-lifecycle"
            ))
        || (method == "POST"
            && matches!(
                path,
                "/api/internal/usb-at/open"
                    | "/api/internal/usb-at/close"
                    | "/api/internal/usb-at/acquire"
                    | "/api/internal/usb-at/release"
                    | "/api/internal/usb-at/command"
                    | "/api/internal/usb-at/configure"
                    | "/api/internal/usb-at/status"
                    | "/api/internal/usb-at/device-status"
                    | "/api/internal/usb-at/sim-identity"
                    | "/api/internal/usb-at/sim-profile-check"
                    | "/api/internal/usb-at/sim-aka"
            ))
        || path.starts_with(crate::modem_actor::PREFIX)
        || path.starts_with(crate::qmi_enhancement::PREFIX)
        || path.starts_with(crate::device_send_store::PREFIX)
    {
        return RouteOwner::Rust;
    }
    if (method == "POST"
        && matches!(
            path,
            "/api/multistandby/start"
                | "/api/multistandby/stop"
                | "/api/multistandby/retry"
                | "/api/multistandby/control-intents"
                | "/api/multistandby/control-intents/ack"
        ))
        || (method == "GET"
            && path.starts_with("/api/multistandby/operations/")
            && !path["/api/multistandby/operations/".len()..].contains('/'))
    {
        return RouteOwner::Rust;
    }
    // Selected USB/PCSC eSIM profile operations are executed by Rust. The
    // dispatcher retains Go as the compatibility path for BLE.
    if method == "PATCH" && matches!(path, "/api/reader/esim/profile") {
        return RouteOwner::Rust;
    }
    if method == "DELETE" && matches!(path, "/api/reader/esim/profile") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/reader/esim/switch") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/network/check-4g") {
        return RouteOwner::Rust;
    }
    if method == "PUT" && matches!(path, "/api/network/services-order") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/voice/enable") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/reader/esim/notifications/retry") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/call-mode/backups/import") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/call-mode/backups/delete") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/internal/call-mode/backups/create") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/internal/call-mode/backups/restore-data") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/internal/call-mode/backups/validate-restore") {
        return RouteOwner::Rust;
    }
    if method == "GET" && matches!(path, "/api/network") {
        return RouteOwner::Rust;
    }
    if method == "GET" && matches!(path, "/api/internal/call-mode/runtime/verify") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/internal/call-mode/adb/probe") {
        return RouteOwner::Rust;
    }
    if method == "POST" && matches!(path, "/api/internal/call-mode/adb/deploy-runtime") {
        return RouteOwner::Rust;
    }
    if method == "POST"
        && matches!(
            path,
            "/api/internal/call-mode/adb/voice-route/start"
                | "/api/internal/call-mode/adb/voice-route/stop"
                | "/api/internal/call-mode/adb/voice-route/status"
                | "/api/internal/call-mode/adb/voice-runtime/status"
                | "/api/internal/call-mode/adb/voice-runtime/generation"
        )
    {
        return RouteOwner::Rust;
    }
    if method == "POST"
        && matches!(
            path,
            "/api/reader/connect"
                | "/api/reader/disconnect"
                | "/api/reader/refresh"
                | "/api/esim/operation/acknowledge"
        )
    {
        return RouteOwner::Rust;
    }
    // Rust-owned GET routes
    if method == "GET"
        && matches!(
            path,
            "/api/sms"
                | "/api/sms/devices"
                | "/api/sms/status"
                | "/api/network/services"
                | "/api/network/traffic"
                | "/api/reader/esim"
                | "/api/esim/health"
                | "/api/esim/operation"
                | "/api/esim/mutation-intent"
                | "/api/reader/esim/operation"
                | "/api/reader/esim/mutation-intent"
                | "/api/reader/esim/notifications"
                | "/api/reader/info"
                | "/api/v2/runtime"
                | "/api/v2/device-snapshot"
                | "/api/v2/migration-readiness"
                | "/api/v2/sms-archive"
                | "/api/v2/sms/forwarding/handoff"
                | "/api/reader/list"
                | "/api/reader/imei"
                | "/api/calls"
                | "/api/call-mode/backups"
                | "/api/call-mode/backups/export"
                | "/api/voice/enabled"
                | "/api/multistandby/report"
        )
    {
        return RouteOwner::Rust;
    }
    // Selected USB/PCSC and BLE eSIM download and confirmation are Rust-owned;
    // missing or unsupported reader selections retain Go compatibility.
    if method == "POST"
        && matches!(
            path,
            "/api/reader/esim/download" | "/api/reader/esim/download/confirm"
        )
    {
        return RouteOwner::Rust;
    }
    if method == "POST"
        && matches!(
            path,
            "/api/internal/qmi/open"
                | "/api/internal/qmi/close"
                | "/api/internal/qmi/probe"
                | "/api/internal/qmi/set-ims"
                | "/api/internal/qmi/operator"
        )
    {
        return RouteOwner::Rust;
    }
    // Rust-owned POST routes
    if method == "POST"
        && matches!(
            path,
            "/api/at"
                | "/api/sms/refresh"
                | "/api/sms/clear"
                | "/api/sms/delete"
                | "/api/sms/delete-sender"
                | "/api/reader/vowifi/proxy-check"
                | "/api/v2/carrier/resolve"
                | "/api/v2/call/dtmf"
                | "/api/v2/call/negotiate-sdp"
                | "/api/v2/call/build-local-sdp"
                | "/api/v2/call/invite-policy"
                | "/api/v2/ims/validate-register-response"
                | "/api/v2/reader/authenticate"
                | "/api/v2/reader/eap-aka"
                | "/api/v2/reader/identity"
                | "/api/v2/vowifi/eap-identity"
                | "/api/v2/vowifi/eap-exchange/release"
                | "/api/v2/vowifi/proxy-policy"
                | "/api/v2/vowifi/proxy-probe"
                | "/api/v2/vowifi/select-transport"
                | "/api/v2/vowifi/session-policy"
                | "/api/v2/vowifi/native-session-plan"
                | "/api/v2/sms/encode-rpdu"
                | "/api/v2/sms/decode-rpdu"
                | "/api/v2/sms/prepare-tpdu"
                | "/api/v2/sms/decode-incoming-tpdu"
                | "/api/v2/sms/decode-stored-snapshot"
                | "/api/v2/sms/forwarding/handoff"
                | "/api/multistandby/messages/send"
                | "/api/calls/clear"
                | "/api/calls/delete"
                | "/api/v2/sms-archive/clear"
                | "/api/v2/sms-archive/delete"
                | "/api/v2/sms-archive/delete-sender"
        )
    {
        return RouteOwner::Rust;
    }
    if is_go_compatibility_route(method, path) {
        return RouteOwner::GoCompatibility;
    }
    RouteOwner::Unknown
}

/// Production compatibility dispatch is retired. Keep this explicit empty
/// table for the contract scanner; future routes require a native owner.
const GO_COMPATIBILITY_ROUTES: &[(&str, &str)] = &[];

fn is_go_compatibility_route(method: &str, path: &str) -> bool {
    let method = if method == "HEAD" { "GET" } else { method };
    GO_COMPATIBILITY_ROUTES.contains(&(method, path))
}

#[cfg(test)]
mod tests {
    use super::{COMPONENTS, GO_COMPATIBILITY_ROUTES, RouteOwner, readiness, route_owner};
    use std::collections::HashSet;

    #[test]
    fn reader_imei_configuration_is_rust_owned_for_all_supported_methods() {
        for method in ["GET", "HEAD", "PUT", "DELETE"] {
            assert_eq!(
                route_owner(method, "/api/reader/imei"),
                RouteOwner::Rust,
                "{method} /api/reader/imei"
            );
        }
    }

    #[test]
    fn retired_go_routes_have_no_compatibility_owner() {
        assert!(GO_COMPATIBILITY_ROUTES.is_empty());
        for path in [
            "/api/internal/device-sms-route",
            "/api/internal/esim/health-snapshot",
            "/api/internal/esim/overview-snapshot",
            "/api/internal/runtime/health-snapshot",
            "/api/internal/runtime/status-snapshot",
            "/api/internal/sms/devices/facts",
        ] {
            for method in ["GET", "HEAD", "POST"] {
                assert_eq!(
                    route_owner(method, path),
                    RouteOwner::Unknown,
                    "{method} {path}"
                );
            }
        }
        for (method, path) in [
            ("GET", "/api/not-registered"),
            ("POST", "/api/internal/future-action"),
            ("POST", "/api/internal/ordinary-vowifi/stop"),
            ("POST", "/api/internal/reader/business-runtime"),
            ("GET", "/api/internal/usb-at/future"),
            ("POST", "/api/internal/usb-sms/snapshot"),
            ("POST", "/api/internal/sms/clear"),
            ("POST", "/api/internal/sms/delete"),
            ("POST", "/api/internal/sms/delete-sender/physical"),
            ("POST", "/api/internal/sms/inbox/prune"),
            ("POST", "/api/internal/call-mode/enable/start"),
            ("POST", "/api/internal/call-mode/restore/start"),
            ("POST", "/api/internal/call-mode/retry/observe"),
            ("POST", "/api/internal/call-mode/retry/start"),
            ("POST", "/api/internal/call-mode/runtime/download/admit"),
            ("POST", "/api/internal/call-mode/runtime/download/progress"),
            ("POST", "/api/internal/call-mode/status/observe"),
            ("POST", "/api/internal/call-control/state"),
            ("POST", "/api/internal/call-control/dial"),
            ("POST", "/api/internal/call-control/answer"),
            ("POST", "/api/internal/call-control/hangup"),
            ("POST", "/api/internal/call-control/dtmf"),
        ] {
            assert_eq!(
                route_owner(method, path),
                RouteOwner::Unknown,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn public_call_audio_route_lifecycle_is_rust_owned() {
        for path in ["/api/call/audio/start", "/api/call/audio/stop"] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust, "POST {path}");
            assert_eq!(route_owner("GET", path), RouteOwner::Rust, "GET {path}");
        }
    }

    #[test]
    fn selected_reader_authentication_is_dispatched_through_rust() {
        assert_eq!(route_owner("POST", "/api/reader/auth"), RouteOwner::Rust);
        assert_eq!(route_owner("GET", "/api/reader/auth"), RouteOwner::Rust);
    }

    #[test]
    fn reader_vowifi_proxy_check_is_rust_owned() {
        assert_eq!(
            route_owner("POST", "/api/reader/vowifi/proxy-check"),
            RouteOwner::Rust
        );
        assert_eq!(
            crate::public_http::admission(&hyper::Method::POST, "/api/reader/vowifi/proxy-check"),
            crate::public_http::Admission::Allowed
        );
    }

    #[test]
    fn diagnostic_timeline_read_and_private_append_are_rust_owned() {
        for method in ["GET", "HEAD"] {
            assert_eq!(
                route_owner(method, "/api/diagnostics/events"),
                RouteOwner::Rust,
                "{method} /api/diagnostics/events"
            );
        }
        assert_eq!(
            route_owner("POST", "/api/internal/diagnostics/events/append"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("GET", "/api/internal/diagnostics/events/append"),
            RouteOwner::Unknown
        );
    }

    #[test]
    fn reader_ble_status_is_rust_owned_and_keeps_get_head_contract() {
        for method in ["GET", "HEAD"] {
            assert_eq!(
                route_owner(method, "/api/reader/ble/status"),
                RouteOwner::Rust,
                "{method} /api/reader/ble/status"
            );
        }
        assert_eq!(
            crate::public_http::admission(&hyper::Method::GET, "/api/reader/ble/status"),
            crate::public_http::Admission::Allowed
        );
        assert_eq!(
            crate::public_http::admission(&hyper::Method::HEAD, "/api/reader/ble/status"),
            crate::public_http::Admission::HeadAsGet
        );
        assert_eq!(
            crate::public_http::admission(&hyper::Method::POST, "/api/reader/ble/status"),
            crate::public_http::Admission::MethodNotAllowed("GET, HEAD")
        );
    }

    #[test]
    fn reader_ble_controls_are_rust_owned_post_routes() {
        for path in [
            "/api/reader/ble/scan",
            "/api/reader/ble/connect",
            "/api/reader/ble/disconnect",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust, "POST {path}");
            assert_eq!(
                crate::public_http::admission(&hyper::Method::POST, path),
                crate::public_http::Admission::Allowed,
                "POST {path}"
            );
            assert_eq!(
                crate::public_http::admission(&hyper::Method::GET, path),
                crate::public_http::Admission::MethodNotAllowed("POST"),
                "GET {path}"
            );
        }
    }

    #[test]
    fn network_service_inventory_and_order_update_are_rust_owned() {
        for method in ["GET", "HEAD"] {
            assert_eq!(
                route_owner(method, "/api/network/services"),
                RouteOwner::Rust,
                "{method} /api/network/services"
            );
        }
        assert_eq!(
            route_owner("PUT", "/api/network/services"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("PUT", "/api/network/services-order"),
            RouteOwner::Rust
        );
    }

    #[test]
    fn network_traffic_sampling_is_rust_owned_as_a_read_only_route() {
        for method in ["GET", "HEAD"] {
            assert_eq!(
                route_owner(method, "/api/network/traffic"),
                RouteOwner::Rust,
                "{method} /api/network/traffic"
            );
        }
    }

    #[test]
    fn module_reboot_is_owned_by_the_rust_radio_actor() {
        assert_eq!(
            route_owner("POST", "/api/network/reboot-module"),
            RouteOwner::Rust
        );
    }

    #[test]
    fn usbnet_mode_is_owned_by_the_rust_radio_actor() {
        assert_eq!(route_owner("POST", "/api/network/usbnet"), RouteOwner::Rust);
        for method in ["GET", "HEAD", "PUT", "DELETE"] {
            assert_eq!(
                route_owner(method, "/api/network/usbnet"),
                RouteOwner::Rust,
                "{method} /api/network/usbnet"
            );
            assert_eq!(
                crate::public_http::admission(&method.parse().unwrap(), "/api/network/usbnet"),
                crate::public_http::Admission::MethodNotAllowed("POST")
            );
        }
        for method in ["GET", "HEAD"] {
            assert_eq!(
                route_owner(method, "/api/network"),
                RouteOwner::Rust,
                "{method} /api/network"
            );
        }
    }

    #[test]
    fn four_g_route_check_is_a_rust_owned_read_only_post() {
        assert_eq!(
            route_owner("POST", "/api/network/check-4g"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("GET", "/api/network/check-4g"),
            RouteOwner::Rust
        );
    }

    #[test]
    fn legacy_voice_enabled_api_is_owned_by_the_rust_modem_actor() {
        for method in ["GET", "HEAD"] {
            assert_eq!(
                route_owner(method, "/api/voice/enabled"),
                RouteOwner::Rust,
                "{method} /api/voice/enabled"
            );
        }
        assert_eq!(route_owner("POST", "/api/voice/enable"), RouteOwner::Rust);
    }

    #[test]
    fn multistandby_archive_and_send_preparation_registry_matches_native_routes() {
        for path in [
            "/api/multistandby/start",
            "/api/multistandby/stop",
            "/api/multistandby/retry",
            "/api/multistandby/control-intents",
            "/api/multistandby/control-intents/ack",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust, "POST {path}");
        }
        assert_eq!(
            route_owner("GET", "/api/multistandby/operations/opaque-id"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("HEAD", "/api/multistandby/operations/opaque-id"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("GET", "/api/multistandby/operations/a/b"),
            RouteOwner::Unknown
        );
        assert_eq!(
            route_owner("POST", crate::multistandby_operations::INTERNAL_CLAIM_PATH),
            RouteOwner::Unknown
        );
        for path in [
            "/api/internal/sms/archive/ingest",
            "/api/internal/sms/archive/delete",
            "/api/internal/sms/archive/delete-sender",
            "/api/internal/sms/archive/clear",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
        }
        for (method, path) in [
            ("GET", "/api/sms/forwarding/status"),
            ("POST", "/api/sms/forwarding/configure"),
            ("POST", "/api/sms/forwarding/enqueue"),
            ("POST", "/api/sms/forwarding/device-snapshot"),
            ("POST", "/api/sms/forwarding/test"),
            ("POST", "/api/sms/forwarding/queue-action"),
        ] {
            assert_eq!(route_owner(method, path), RouteOwner::Rust);
        }
        assert_eq!(
            route_owner("POST", "/api/v2/sms/forwarding/handoff"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("GET", "/api/v2/sms/forwarding/handoff"),
            RouteOwner::Rust
        );
        for path in [
            "/api/multistandby/messages",
            "/api/multistandby/changes",
            "/api/multistandby/history",
            "/api/multistandby/conversations",
            "/api/multistandby/cards",
            "/api/multistandby/profiles",
            "/api/multistandby/send-intents",
        ] {
            assert_eq!(route_owner("GET", path), RouteOwner::Rust);
            assert!(crate::multistandby_store::is_public_route(
                &hyper::Method::GET,
                path
            ));
        }
        assert_eq!(route_owner("GET", "/api/sms/status"), RouteOwner::Rust);
        assert_eq!(route_owner("HEAD", "/api/sms/status"), RouteOwner::Rust);
        assert_eq!(route_owner("GET", "/api/sms/storage"), RouteOwner::Rust);
        assert_eq!(route_owner("HEAD", "/api/sms/storage"), RouteOwner::Rust);
        assert_eq!(route_owner("GET", "/api/sms/devices"), RouteOwner::Rust);
        assert_eq!(route_owner("HEAD", "/api/sms/devices"), RouteOwner::Rust);
        assert_eq!(
            route_owner("POST", "/api/internal/sms/devices/select"),
            RouteOwner::Rust
        );
        for path in [
            "/api/multistandby/messages/send",
            "/api/multistandby/send-intents",
            "/api/multistandby/send-intents/submit",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
            assert!(crate::multistandby_store::is_public_route(
                &hyper::Method::POST,
                path
            ));
        }
        assert_eq!(
            route_owner("POST", "/api/multistandby/messages/send"),
            RouteOwner::Rust
        );
    }

    #[test]
    fn usb_sms_native_service_does_not_claim_removed_raw_snapshot_route() {
        assert_eq!(
            route_owner("GET", "/api/internal/usb-sms/inbox"),
            RouteOwner::Rust
        );
        for path in [
            "/api/internal/usb-sms/refresh",
            "/api/internal/usb-sms/prune",
            "/api/internal/usb-sms/storage-operation",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
        }
        assert_eq!(
            route_owner("GET", "/api/internal/usb-sms/storage-operation"),
            RouteOwner::Unknown
        );
        assert_eq!(
            route_owner("POST", "/api/internal/usb-sms/snapshot"),
            RouteOwner::Unknown
        );
    }

    #[test]
    fn private_call_archive_event_route_is_rust_owned() {
        assert_eq!(
            route_owner("POST", "/api/internal/call-archive/ingest"),
            RouteOwner::Rust
        );
    }

    #[test]
    fn private_ims_sms_transaction_route_is_rust_owned_only_for_post() {
        assert_eq!(
            route_owner("POST", crate::ims_sms_transactions::INTERNAL_PATH),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("GET", crate::ims_sms_transactions::INTERNAL_PATH),
            RouteOwner::Unknown
        );
    }

    #[test]
    fn private_usb_owner_routes_are_exact_and_do_not_claim_future_actions() {
        for path in [
            "/api/internal/usb-at/open",
            "/api/internal/usb-at/close",
            "/api/internal/usb-at/acquire",
            "/api/internal/usb-at/release",
            "/api/internal/usb-at/command",
            "/api/internal/usb-at/configure",
            "/api/internal/usb-at/status",
            "/api/internal/usb-at/device-status",
            "/api/internal/usb-at/sim-identity",
            "/api/internal/usb-at/sim-profile-check",
            "/api/internal/usb-at/sim-aka",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
            assert_eq!(route_owner("GET", path), RouteOwner::Unknown);
        }
        assert_eq!(
            route_owner("POST", "/api/internal/usb-at/future"),
            RouteOwner::Unknown
        );
    }

    #[test]
    fn radio_owner_routes_do_not_claim_other_methods_or_future_actions() {
        for path in [
            "/api/internal/module-radio/configure",
            "/api/internal/module-radio/query",
            "/api/internal/module-radio/bootstrap",
            "/api/internal/module-radio/refresh",
            "/api/internal/module-radio/reservation",
            "/api/internal/module-radio/vowifi",
            "/api/internal/module-radio/action",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
            assert_eq!(route_owner("GET", path), RouteOwner::Unknown);
        }
        for method in ["GET", "POST"] {
            assert_eq!(
                route_owner(method, "/api/module/flight-mode"),
                RouteOwner::Rust
            );
        }
        assert_eq!(
            route_owner("PATCH", "/api/module/flight-mode"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("POST", "/api/internal/module-radio/future"),
            RouteOwner::Unknown
        );
    }

    #[test]
    fn private_modem_actor_routes_are_rust_owned() {
        for path in [
            "/api/internal/modem/discover-at-port",
            "/api/internal/modem/open",
            "/api/internal/modem/initialize",
            "/api/internal/modem/read",
            "/api/internal/modem/write",
            "/api/internal/modem/execute",
            "/api/internal/modem/call-command",
            "/api/internal/modem/delete-archived-sms",
            "/api/internal/modem/read-stored-sms",
            "/api/internal/modem/ingest-stored-sms",
            "/api/internal/modem/project-call-state",
            "/api/internal/modem/sms-receive",
            "/api/internal/modem/close",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
        }
    }

    #[test]
    fn private_qmi_owner_routes_are_rust_owned() {
        for path in [
            "/api/internal/qmi/open",
            "/api/internal/qmi/close",
            "/api/internal/qmi/probe",
            "/api/internal/qmi/set-ims",
            "/api/internal/qmi/operator",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
        }
    }

    #[test]
    fn manifest_ids_are_unique_and_go_removal_is_fail_closed() {
        let ids = COMPONENTS
            .iter()
            .map(|component| component.id)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), COMPONENTS.len());
        assert!(!readiness().ready_to_remove_go);
        let progress = readiness();
        assert!(
            progress.rust_owned_components + progress.go_blocking_components <= COMPONENTS.len()
        );
        assert!(progress.rust_ownership_percent > 0);
        assert!(progress.rust_ownership_percent < 100);
        // Iteration510 removes runtime Go dependencies while retaining all
        // twelve acceptance/source-cleanup blockers. The coarse tally counts
        // unblocked components, not runtime owners or weighted project progress.
        assert_eq!(progress.rust_owned_components, 9);
        assert_eq!(progress.go_blocking_components, 12);
        assert_eq!(progress.rust_ownership_percent, 40);
        assert!(
            COMPONENTS
                .iter()
                .all(|component| !component.requires_go_runtime)
        );
        let pending = HashSet::from([
            "device_snapshot",
            "dji_module",
            "module_qmi_control",
            "modem_serial_port",
            "usb_at_transport",
            "module_radio_policy",
            "reader_session",
            "reader_vowifi",
            "message_runtime",
            "ims_heartbeat_supervisor",
            "call_archive",
            "outgoing_call_policy",
        ]);
        let actual = COMPONENTS
            .iter()
            .filter(|component| component.blocks_go_removal)
            .map(|component| {
                assert_eq!(component.owner, "rust");
                assert!(component.state.ends_with("acceptance-pending"));
                assert!(component.detail.contains("acceptance"));
                assert!(!component.detail.contains("remaining Go source cleanup"));
                component.id
            })
            .collect::<HashSet<_>>();
        assert_eq!(actual, pending);
        assert!(COMPONENTS.iter().any(|component| {
            component.id == "message_archive"
                && component.owner == "rust"
                && !component.requires_go_runtime
                && !component.blocks_go_removal
        }));
    }

    #[test]
    fn native_reads_and_selected_reader_esim_routes_are_rust_owned() {
        for path in [
            "/api/sms",
            "/api/esim",
            "/api/esim/health",
            "/api/reader/esim",
            "/api/reader/esim/health",
            "/api/reader/esim/operation",
            "/api/esim/operation",
            "/api/v2/runtime",
            "/api/v2/device-snapshot",
            "/api/v2/migration-readiness",
            "/api/v2/sms-archive",
            "/api/reader/list",
            "/api/calls",
            "/api/call-mode/backups",
            "/api/call-mode/backups/export",
        ] {
            assert_eq!(route_owner("GET", path), RouteOwner::Rust);
        }
        assert_eq!(
            route_owner("POST", "/api/call-mode/backups/import"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("POST", "/api/call-mode/download"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("POST", "/api/call-mode/enable"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("POST", "/api/call-mode/restore"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("POST", "/api/call-mode/retry"),
            RouteOwner::Rust
        );
        assert_eq!(route_owner("GET", "/api/call-mode/retry"), RouteOwner::Rust);
        assert_eq!(
            route_owner("POST", "/api/internal/call-mode/backups/create"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("POST", "/api/internal/call-mode/backups/restore-data"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("POST", "/api/internal/call-mode/backups/validate-restore"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("GET", "/api/internal/call-mode/runtime/verify"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("GET", "/api/internal/call-mode/runtime/download"),
            RouteOwner::Unknown
        );
        assert_eq!(
            route_owner("POST", "/api/internal/call-mode/runtime/download"),
            RouteOwner::Unknown
        );
        assert_eq!(
            route_owner("POST", "/api/internal/call-mode/adb/probe"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("POST", "/api/internal/call-mode/adb/deploy-runtime"),
            RouteOwner::Rust
        );
        for path in [
            "/api/internal/call-mode/adb/voice-route/start",
            "/api/internal/call-mode/adb/voice-route/stop",
            "/api/internal/call-mode/adb/voice-route/status",
            "/api/internal/call-mode/adb/voice-runtime/status",
            "/api/internal/call-mode/adb/voice-runtime/generation",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
        }
        assert_eq!(
            route_owner("GET", "/api/internal/call-mode/adb/probe"),
            RouteOwner::Unknown
        );
        assert_eq!(route_owner("GET", "/api/sms/adopt"), RouteOwner::Rust);
        assert_eq!(route_owner("POST", "/api/sms/adopt"), RouteOwner::Rust);
        assert_eq!(
            route_owner("POST", "/api/v2/migration-readiness"),
            RouteOwner::Unknown
        );
        for path in [
            "/api/calls/clear",
            "/api/v2/carrier/resolve",
            "/api/v2/call/dtmf",
            "/api/v2/call/negotiate-sdp",
            "/api/v2/call/build-local-sdp",
            "/api/v2/call/invite-policy",
            "/api/v2/ims/validate-register-response",
            "/api/v2/reader/authenticate",
            "/api/v2/reader/eap-aka",
            "/api/v2/reader/identity",
            "/api/v2/vowifi/eap-identity",
            "/api/v2/vowifi/eap-exchange/release",
            "/api/v2/vowifi/proxy-policy",
            "/api/v2/vowifi/proxy-probe",
            "/api/v2/vowifi/select-transport",
            "/api/v2/sms/encode-rpdu",
            "/api/v2/sms/decode-rpdu",
            "/api/v2/sms/prepare-tpdu",
            "/api/v2/sms/decode-incoming-tpdu",
            "/api/v2/sms/decode-stored-snapshot",
            "/api/calls/delete",
            "/api/call/dial",
            "/api/call/dtmf",
            "/api/call/answer",
            "/api/call/hangup",
            "/api/v2/sms-archive/clear",
            "/api/v2/sms-archive/delete",
            "/api/v2/sms-archive/delete-sender",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
        }
        for path in [
            "/api/vowifi",
            "/api/reader/vowifi",
            "/api/reader/vowifi/reconcile",
            "/api/vowifi/reconcile",
            "/api/reader/vowifi/calls/dial",
            "/api/reader/vowifi/calls/answer",
            "/api/reader/vowifi/calls/reject",
            "/api/reader/vowifi/calls/hangup",
            "/api/reader/vowifi/calls/dtmf",
            "/api/sms/send",
            "/api/esim/switch",
            "/api/reader/esim/switch",
            "/api/reader/esim/profile",
        ] {
            let expected = match crate::public_http::admission(&hyper::Method::GET, path) {
                crate::public_http::Admission::MethodNotAllowed(_) => RouteOwner::Rust,
                _ if matches!(path, "/api/reader/vowifi" | "/api/vowifi") => RouteOwner::Rust,
                _ if GO_COMPATIBILITY_ROUTES.contains(&("GET", path)) => {
                    RouteOwner::GoCompatibility
                }
                _ => RouteOwner::Unknown,
            };
            assert_eq!(route_owner("GET", path), expected, "GET {path}");
        }
        assert_eq!(route_owner("GET", "/api/vowifi/result"), RouteOwner::Rust);
        assert_eq!(route_owner("POST", "/api/reader/vowifi"), RouteOwner::Rust);
        for path in [
            "/api/vowifi",
            "/api/reader/vowifi/calls/dial",
            "/api/reader/vowifi/calls/answer",
            "/api/reader/vowifi/calls/reject",
            "/api/reader/vowifi/calls/hangup",
            "/api/reader/vowifi/calls/dtmf",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
        }
        assert_eq!(route_owner("POST", "/api/sms/send"), RouteOwner::Rust);
        assert_eq!(
            route_owner("POST", "/api/sms/send-intents"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("GET", "/api/sms/send-intents"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("POST", "/api/reader/esim/switch"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("POST", "/api/reader/esim/notifications/retry"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("GET", "/api/reader/esim/notifications"),
            RouteOwner::Rust
        );
        for path in [
            "/api/reader/esim/download",
            "/api/reader/esim/download/confirm",
        ] {
            assert_eq!(route_owner("POST", path), RouteOwner::Rust);
        }
        assert_eq!(
            route_owner("PATCH", "/api/reader/esim/profile"),
            RouteOwner::Rust
        );
        assert_eq!(
            route_owner("DELETE", "/api/reader/esim/profile"),
            RouteOwner::Rust
        );
    }

    #[test]
    fn public_health_is_rust_owned_and_private_go_observation_is_retired() {
        assert_eq!(route_owner("GET", "/api/health"), RouteOwner::Rust);
        assert_eq!(route_owner("HEAD", "/api/health"), RouteOwner::Rust);
        assert_eq!(route_owner("POST", "/api/health"), RouteOwner::Rust);
        assert_eq!(
            route_owner("POST", "/api/internal/runtime/health-snapshot"),
            RouteOwner::Unknown
        );
    }

    #[test]
    fn public_sms_clear_has_rust_route_owner() {
        assert_eq!(route_owner("POST", "/api/sms/clear"), RouteOwner::Rust);
    }

    #[test]
    fn public_sms_delete_has_rust_route_owner() {
        assert_eq!(route_owner("POST", "/api/sms/delete"), RouteOwner::Rust);
    }

    #[test]
    fn public_device_status_is_rust_owned_and_private_go_observation_is_retired() {
        assert_eq!(route_owner("GET", "/api/status"), RouteOwner::Rust);
        assert_eq!(route_owner("GET", "/api/call/status"), RouteOwner::Rust);
        assert_eq!(
            route_owner("GET", "/api/call-mode/status"),
            RouteOwner::Rust
        );
        assert_eq!(route_owner("HEAD", "/api/status"), RouteOwner::Rust);
        assert_eq!(
            route_owner("HEAD", "/api/call-mode/status"),
            RouteOwner::Rust
        );
        assert_eq!(route_owner("POST", "/api/status"), RouteOwner::Rust);
        assert_eq!(
            route_owner("POST", "/api/internal/runtime/status-snapshot"),
            RouteOwner::Unknown
        );
    }

    #[test]
    fn vowifi_call_status_aliases_are_rust_owned() {
        for path in [
            "/api/reader/vowifi/calls/status",
            "/api/vowifi/calls/status",
        ] {
            assert_eq!(route_owner("GET", path), RouteOwner::Rust, "GET {path}");
            assert_eq!(route_owner("HEAD", path), RouteOwner::Rust, "HEAD {path}");
        }
    }

    #[test]
    fn reader_info_projection_is_rust_owned() {
        assert_eq!(route_owner("GET", "/api/reader/info"), RouteOwner::Rust);
        assert_eq!(route_owner("HEAD", "/api/reader/info"), RouteOwner::Rust);
        assert_eq!(route_owner("POST", "/api/reader/info"), RouteOwner::Rust);
    }

    #[test]
    fn module_profile_notes_public_api_is_owned_by_the_shared_rust_radio_actor() {
        for method in ["GET", "HEAD", "PUT"] {
            assert_eq!(
                route_owner(method, "/api/esim/module-notes"),
                RouteOwner::Rust,
                "{method} /api/esim/module-notes"
            );
        }
        assert_eq!(
            route_owner("POST", "/api/esim/phonebook/probe"),
            RouteOwner::Rust
        );
    }

    #[test]
    fn module_profile_rename_and_delete_are_rust_owned() {
        assert_eq!(route_owner("PATCH", "/api/esim/profile"), RouteOwner::Rust);
        assert_eq!(route_owner("DELETE", "/api/esim/profile"), RouteOwner::Rust);
        assert_eq!(route_owner("POST", "/api/esim/switch"), RouteOwner::Rust);
        assert_eq!(route_owner("POST", "/api/esim/download"), RouteOwner::Rust);
        assert_eq!(
            route_owner("POST", "/api/esim/download/confirm"),
            RouteOwner::Rust
        );
    }

    #[test]
    fn component_owners_are_explicit() {
        let valid = HashSet::from(["rust", "go", "swift", "deprecated"]);
        assert!(
            COMPONENTS
                .iter()
                .all(|component| valid.contains(component.owner))
        );
    }
}
