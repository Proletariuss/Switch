Warning: truncated output (original token count: 59868)
Total output lines: 5900

#![forbid(unsafe_code)]

mod acceptance_capsule;
#[allow(dead_code)]
mod acceptance_phase_a;
mod archive_io;
mod availability;
mod b0_auth;
mod blocking_pool;
mod call_archive;
mod call_audio;
mod call_codec;
mod call_control;
mod call_facts;
mod call_mode_adb;
mod call_mode_backups;
mod call_mode_enable;
mod call_mode_lifecycle;
mod call_mode_restore;
mod call_mode_restore_journal;
mod call_mode_retry;
mod call_mode_runtime;
mod call_mode_runtime_download;
mod call_mode_runtime_manifest;
mod call_mode_state;
mod call_mode_status;
mod call_mutation_intent;
mod carrier;
mod connection_pools;
mod daemon_args;
mod daemon_lifetime;
mod data_root;
mod device_forwarding_store;
mod device_send_store;
mod device_sms_forwarding;
mod device_sms_send;
mod device_status_projection;
mod diagnostics;
mod dji_module;
mod eap_identity;
mod esim_health_projection;
mod esim_mutation_intent;
mod esim_overview_projection;
mod esim_refresh;
#[cfg(test)]
mod esim_test_support;
mod forwarding_admission;
mod forwarding_admission_store;
mod forwarding_handoff_store;
mod gateway;
mod health_projection;
mod http_server;
mod ims_codec;
mod ims_heartbeat_supervisor;
mod ims_sms_transactions;
mod message_archive;
mod modem_actor;
mod module_call;
mod module_call_state;
mod module_discovery;
mod module_esim;
mod module_esim_channel;
mod module_esim_transport;
mod module_profile_notes;
#[cfg(test)]
mod module_quarantine_tests;
mod module_radio;
mod module_radio_store;
mod module_radio_transport;
mod module_recovery;
mod module_sim_identity;
mod module_startup;
mod module_vowifi_card;
mod multistandby_card;
mod multistandby_maintenance;
mod multistandby_native_card;
mod multistandby_native_inventory;
mod multistandby_native_runtime;
mod multistandby_native_service;
mod multistandby_operations;
mod multistandby_recovery;
mod multistandby_retry;
mod multistandby_runtime_report;
mod multistandby_send;
mod multistandby_store;
mod native_ike_certificate;
mod native_ims_actor;
mod native_ims_call_setup;
mod native_ims_calls;
#[cfg(test)]
mod native_module_supervision_tests;
mod native_registered_session;
#[cfg(test)]
mod native_serial_module_supervision_tests;
#[cfg(test)]
mod native_serial_supervision_tests;
mod native_sms_consumer;
mod native_sms_originating;
mod native_vowifi_session;
mod native_vowifi_supervisor;
mod network_check;
mod network_diagnostic;
mod network_recovery;
mod network_services;
mod network_traffic;
mod operator_name;
mod ordinary_vowifi_admission;
mod ownership;
mod profile_notes;
mod proxy_policy;
mod proxy_probe;
mod public_http;
mod public_mutation_receipt;
mod qadbkey;
mod qmi_enhancement;
mod reader_aka_binding;
mod reader_auth;
mod reader_ble_apdu;
mod reader_ble_control;
#[cfg(test)]
mod reader_ble_gateway;
mod reader_ble_status;
mod reader_business_runtime;
#[cfg(test)]
mod reader_compat_retirement_tests;
mod reader_control;
mod reader_device_identity;
mod reader_eap_aka;
mod reader_esim_download;
mod reader_hotplug;
mod reader_identity;
mod reader_info_projection;
mod reader_inventory;
mod reader_lifecycle;
mod reader_mutation_intent;
mod reader_notification_intent;
mod reader_pcsc;
#[cfg(test)]
mod reader_pcsc_gateway_tests;
mod reader_proxy_check;
mod reader_reservation;
mod reader_session_lease;
mod reader_task;
mod request_body_budget;
mod safe_events;
#[cfg(test)]
mod serial_esim_gateway_tests;
mod serial_module_identity;
mod shutdown;
mod sms_adopt;
mod sms_clear;
mod sms_codec;
mod sms_delete;
mod sms_delete_sender;
mod sms_device_service;
mod sms_export;
mod sms_forwarding;
mod sms_forwarding_config_intent;
mod sms_status;
mod sms_store_health;
mod snapshot;
mod socket_lifetime;
mod startup_recovery;
mod support_diagnostics;
mod system_proxy;
mod transport_selection;
mod unified_call_control;
#[cfg(test)]
mod upstream;
mod usb_at;
mod usb_at_status;
mod usb_at_storage;
mod usb_at_transport;
mod usb_sms_receive;
mod voice_enabled;
mod vowifi_input;
mod vowifi_session_policy;

use std::convert::Infallible;
use std::env;
use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use nix::unistd::{Pid, getppid};
use proletarius_api_contract::{ErrorResponse, RuntimeResponse};
use proletarius_call::CALL_PROTOCOL_CONTRACT;
use proletarius_domain::{BackendFailure, MigrationStage};
use proletarius_ike::IKE_WIRE_CONTRACT;
use proletarius_ims::IMS_PROTOCOL_CONTRACT;
use proletarius_reader::ReaderDiscovery;
use proletarius_sim::SIM_PROTOCOL_CONTRACT;
use proletarius_sms::SMS_PROTOCOL_CONTRACT;
use proletarius_store::STORE_CONTRACT;
use proletarius_vowifi::PROXY_CONTRACT;
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinSet;
use tokio::time::{Duration, interval};

use availability::BackendAvailability;
#[cfg(test)]
use availability::BackendPhase;
use gateway::{callback_socket_path, internal_socket_path};
use support_diagnostics::recovery as recovery_diagnostics;

const APP_TOKEN_ENVIRONMENT: &str = "PROLETARIUS_APP_TOKEN";
const APP_TOKEN_HEADER: &str = "x-proletarius-app-token";
const BACKEND_VERSION: &str = env!("CARGO_PKG_VERSION");
const APP_RELEASE_VERSION: &str = match option_env!("SWITCH_APP_VERSION") {
    Some(value) => value,
    None => "unknown",
};
const APP_BUILD_NUMBER: &str = match option_env!("SWITCH_BUILD_NUMBER") {
    Some(value) => value,
    None => "unknown",
};
const SOURCE_DIGEST: &str = match option_env!("SWITCH_SOURCE_DIGEST") {
    Some(value) => value,
    None => "unknown",
};
const PUBLIC_CONNECTION_LIMIT: usize = 64;
const NATIVE_CALLBACK_CONNECTION_LIMIT: usize = 32;
const INTERNAL_STORE_CONNECTION_LIMIT: usize = 16;
const SHUTDOWN_CONNECTION_LIMIT: usize = 16;

#[derive(Clone)]
struct ServerState {
    call_mode: Arc<std::sync::OnceLock<call_mode_lifecycle::Service>>,
    module_call: Arc<std::sync::OnceLock<Result<module_call::Service, String>>>,
    reader_lifecycle:
        Arc<std::sync::OnceLock<Result<Arc<reader_lifecycle::Service>, reader_lifecycle::Failure>>>,
    app_token: Option<String>,
    internal_token: String,
    multistandby_card: Arc<std::sync::OnceLock<multistandby_card::Service>>,
    multistandby_service: Arc<
        std::sync::OnceLock<
            Result<Arc<multistandby_native_service::Service>, multistandby_native_service::Failure>,
        >,
    >,
    multistandby_store: multistandby_store::Store,
    multistandby_operations: multistandby_operations::Store,
    multi_runtime_report: Arc<Mutex<multistandby_runtime_report::Journal>>,
    device_send_store: device_send_store::Store,
    forwarding_handoff_store: forwarding_handoff_store::Store,
    sms_forwarding: sms_forwarding::Service,
    sms_inbox: message_archive::InboxState,
    usb_sms_receive: usb_sms_receive::Service,
    usb_at: usb_at::Owner,
    module_radio: module_radio::Service,
    call_archive: proletarius_store::ArchiveStore,
    call_facts: call_facts::CallFacts,
    modem_actor: modem_actor::ModemActor,
    qmi_enhancement: qmi_enhancement::QmiEnhancement,
    network_traffic: network_traffic::Service,
    go_socket_path: PathBuf,
    availability: BackendAvailability,
    reader_discovery: Arc<Mutex<ReaderDiscovery>>,
    reader_pcsc: reader_pcsc::Owner,
    reader_gate: Arc<reader_reservation::Gate>,
    snapshot_instance_id: String,
    snapshot_revision: Arc<AtomicU64>,
    eap_exchanges: Arc<eap_identity::EapExchangeStore>,
    native_vowifi_lifecycle: Arc<native_vowifi_session::NativeLifecycle>,
    ims_sms_transactions: ims_sms_transactions::Service,
}

impl ServerState {
    fn multistandby_card(&self) -> multistandby_card::Service {
        self.multistandby_card
            .get_or_init(|| {
                multistandby_card::Service::new(
                    self.reader_pcsc.clone(),
                    self.reader_gate.clone(),
                    env::var_os("PROLETARIUS_BLE_SOCKET").map(PathBuf::from),
                    self.app_token.clone(),
                )
            })
            .clone()
    }
    fn multistandby_service(
        &self,
    ) -> Result<Arc<multistandby_native_service::Service>, multistandby_native_service::Failure>
    {
        self.multistandby_service
            .get_or_init(|| {
                multistandby_native_service::Service::new(multistandby_native_service::Resources {
                    owner: self.reader_pcsc.clone(),
                    gate: self.reader_gate.clone(),
                    card_service: self.multistandby_card(),
                    data_root: self.multistandby_store.data_root().to_path_buf(),
                    store: self.multistandby_store.clone(),
                    journal: self.multi_runtime_report.clone(),
                    transactions: self.ims_sms_transactions.clone(),
                    connection: Default::default(),
                })
            })
            .clone()
    }
    fn reader_lifecycle(&self) -> Result<reader_lifecycle::Service, reader_lifecycle::Failure> {
        self.reader_lifecycle
            .get_or_init(|| {
                let producer = self
                    .reader_gate
                    .native_reader_producer()
                    .unwrap_or_else(|| {
                        let producer = snapshot::new_instance_id();
                        self.reader_gate
                            .initialize_native_reader_producer(&producer);
                        producer
                    });
                let service = reader_lifecycle::Service::new(
                    self.multistandby_store.data_root().to_path_buf(),
                    self.reader_gate.clone(),
                    self.reader_pcsc.clone(),
                    env::var_os("PROLETARIUS_BLE_SOCKET").map(PathBuf::from),
                    self.app_token.clone(),
                    producer,
                )?;
                service.set_business_bridge(reader_business_runtime::Bridge::new(
                    self.sms_inbox.clone(),
                ));
                service.set_ordinary_lifecycle(Arc::downgrade(&self.native_vowifi_lifecycle));
                let multi =
                    self.multistandby_service()
                        .map_err(|error| reader_lifecycle::Failure {
                            status: StatusCode::SERVICE_UNAVAILABLE,
                            code: error.code,
                            message: error.message,
                            recoverable: true,
                        })?;
                service.set_multistandby_service(Arc::downgrade(&multi));
                let service = Arc::new(service);
                let supplier: Arc<dyn reader_session_lease::NativeSessionSupplier> =
                    service.clone();
                self.reader_gate
                    .set_native_session_supplier(Arc::downgrade(&supplier));
                Ok(service)
            })
            .clone()
            .map(|service| (*service).clone())
    }
    fn module_call(&self) -> Result<module_call::Service, String> {
        self.module_call
            .get_or_init(|| {
                module_call::Service::new(
                    self.multistandby_store.data_root().to_path_buf(),
                    self.module_radio.clone(),
                    self.sms_inbox.clone(),
                    self.usb_at.clone(),
                    self.call_mode(),
                    self.call_facts.clone(),
                )
            })
            .clone()
    }
    fn call_mode(&self) -> call_mode_lifecycle::Service {
        self.call_mode
            .get_or_init(|| {
                call_mode_lifecycle::Service::new(
                    self.multistandby_store.data_root(),
                    self.module_radio.clone(),
                    self.sms_inbox.clone(),
                    self.usb_at.clone(),
                )
            })
            .clone()
    }
}

fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
fn test_usb_sms_receiver(
    inbox: message_archive::InboxState,
    archive_path: PathBuf,
) -> usb_sms_receive::Service {
    usb_sms_receive::Service::new(
        proletarius_store::ArchiveStore::new(archive_path),
        inbox,
        usb_at::Owner::unavailable_for_tests("usb-producer-fixture".into()),
        BackendAvailability::new(),
        "usb-producer-fixture".into(),
    )
}

#[cfg(test)]
fn test_module_radio(root: &Path) -> module_radio::Service {
    module_radio::Service::new(
        root.to_path_buf(),
        usb_at::Owner::unavailable_for_tests("usb-producer-fixture".into()),
        modem_actor::ModemActor::new().unwrap(),
        BackendAvailability::new(),
        "usb-producer-fixture".into(),
    )
}

struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
    _claim: socket_lifetime::Claim,
}

impl SocketGuard {
    async fn bind(path: &Path) -> Result<(UnixListener, Self), BackendFailure> {
        // Data-root leases cannot serialize two different roots choosing the
        // same IPC pathname. Retain its independent claim across stale
        // inspection, bind, publication and eventual socket retirement.
        let claim = socket_lifetime::Claim::acquire(path).map_err(socket_claim_failure)?;
        let path = claim.path().to_path_buf();
        prepare_socket_path(&path).await?;
        let listener = UnixListener::bind(&path).map_err(io_failure)?;
        let guard = Self::capture(&path, claim)?;
        Ok((listener, guard))
    }

    fn capture(path: &Path, claim: socket_lifetime::Claim) -> Result<Self, BackendFailure> {
        use std::os::unix::fs::MetadataExt;
        claim.validate().map_err(socket_claim_failure)?;
        let metadata = fs::symlink_metadata(path).map_err(io_failure)?;
        if !metadata.file_type().is_socket() {
            return Err(BackendFailure::ExistingPathIsNotSocket);
        }
        Ok(Self {
            path: path.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
            _claim: claim,
        })
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let startup_arguments: Vec<std::ffi::OsString> = env::args_os().skip(1).collect();
    // G8 proof is checked before credentials, data roots, stores and owners.
    // Even valid capsules refuse: acceptance composition has no admission yet.
    if let Err(_error) = acceptance_capsule::pre_entry(&startup_arguments) {
        eprintln!("switchd-rs: acceptance/arguments rejected; runtime admission NO-GO");
        return std::process::ExitCode::from(78);
    }
    if startup_arguments.len() == 1
        && startup_arguments[0] == std::ffi::OsStr::new(call_mode_adb::INTERNAL_PROBE_ARGUMENT)
    {
        // The isolated USB helper is a shipping-binary mode and can touch a
        // physical module. It inherits the app credential from its supervised
        // parent and must reject direct unauthenticated invocation before even
        // resolving or locking the data root.
        if let Err(error) = required_app_token() {
            crate::diagnostics::write_line(format_args!("switchd-rs: {error}"));
            crate::diagnostics::flush(Duration::from_millis(100));
            return std::process::ExitCode::FAILURE;
        }
        let data_root = match data_root::resolve() {
            Ok(path) => path,
            Err(_) => {
                crate::diagnostics::write_line(format_args!("module_adb_lock_unavailable"));
                crate::diagnostics::flush(Duration::from_millis(100));
                return std::process::ExitCode::FAILURE;
            }
        };
        if let Err(error) = call_mode_adb::run_isolated_probe(&data_root) {
            crate::diagnostics::write_line(format_args!("{}", error.code));
            crate::diagnostics::flush(Duration::from_millis(100));
            return std::process::ExitCode::FAILURE;
        }
        return std::process::ExitCode::SUCCESS;
    }
    if let Err(error) = run(startup_arguments).await {
        support_diagnostics::lifecycle("shutdown_failed");
        crate::diagnostics::write_line(format_args!("switchd-rs: {error}"));
        crate::diagnostics::flush(Duration::from_millis(100));
        return std::process::ExitCode::FAILURE;
    }
    support_diagnostics::lifecycle("shutdown_complete");
    crate::diagnostics::write_line(format_args!("switchd-rs: native shutdown complete"));
    crate::diagnostics::flush(Duration::from_millis(100));
    std::process::ExitCode::SUCCESS
}

async fn run(arguments: Vec<std::ffi::OsString>) -> Result<(), BackendFailure> {
    acceptance_capsule::pre_entry(&arguments).map_err(BackendFailure::Io)?;
    let arguments = daemon_args::parse(arguments).map_err(BackendFailure::Io)?;
    // A shipping daemon is always an authenticated child of the macOS app.
    // Refuse startup before acquiring the data-root lease, opening stores or
    // starting any hardware owner when that ephemeral credential is absent.
    let app_token = required_app_token()?;
    let data_root = data_root::resolve().map_err(BackendFailure::Io)?;
    // This lease intentionally outlives the run future and Tokio's shutdown.
    // An early startup error or cancelled waiter must not permit another daemon
    // to recover/write stores while detached native work is still exiting.
    // Static storage is released only by OS process teardown; never unlink
    // its lock entry or infer physical recovery from lease availability.
    static LIFETIME: std::sync::OnceLock<daemon_lifetime::Lease> = std::sync::OnceLock::new();
    let lifetime = daemon_lifetime::Lease::acquire(&data_root)
        .map_err(|error| BackendFailure::Io(error.to_string()))?;
    LIFETIME
        .set(lifetime)
        .map_err(|_| BackendFailure::Io("backend lifetime already initialized".into()))?;
    let lifetime = LIFETIME.get().expect("just installed backend lifetime");
    lifetime
        .validate()
        .map_err(|error| BackendFailure::Io(error.to_string()))?;
    let data_root = lifetime.root().to_path_buf();
    support_diagnostics::initialize(&data_root);
    native_vowifi_session::configuration::initialize_config_path(&arguments, &data_root)
        .map_err(|_| BackendFailure::Io("native_carrier_overrides_invalid".into()))?;
    let (multistandby_store, device_send_store, forwarding_handoff_store) =
        initialize_sms_stores_with_authority(&data_root, Some(lifetime.root_proof()))?;
    let device_sms_forwarding_store = device_forwarding_store::Store::open(&data_root)
        .map_err(|error| BackendFailure::Io(error.to_string()))?;
    let sms_forwarding = sms_forwarding::Service::new_with_admission(
        &data_root,
        forwarding_handoff_store.clone(),
        device_sms_forwarding_store.clone(),
    );
    run_with_sms_stores(
        arguments,
        lifetime,
        multistandby_store,
        device_send_store,
        forwarding_handoff_store,
        device_sms_forwarding_store,
        sms_forwarding,
        app_token,
    )
    .await
}

#[cfg(test)]
fn initialize_sms_stores(
    data_root: &Path,
) -> Result<
    (
        multistandby_store::Store,
        device_send_store::Store,
        forwarding_handoff_store::Store,
    ),
    BackendFailure,
> {
    initialize_sms_stores_with_authority(data_root, None)
}

fn initialize_sms_stores_with_authority(
    data_root: &Path,
    proof: Option<daemon_lifetime::RootProof>,
) -> Result<
    (
        multistandby_store::Store,
        device_send_store::Store,
        forwarding_handoff_store::Store,
    ),
    BackendFailure,
> {
    let multistandby_store = multistandby_store::Store::open(data_root)
        .map_err(|e| BackendFailure::Io(e.to_string()))?;
    if multistandby_store.recover().is_err() {
        crate::diagnostics::write_line(format_args!(
            "switchd-rs: Nwait store unavailable; new submissions disabled"
        ));
    }
    let device_send_store = match proof {
        Some(proof) => match device_send_store::Store::open_with_authority(data_root, proof) {
            Ok(store) => Ok(store),
            Err(_) => {
                crate::diagnostics::write_line(format_args!(
                    "switchd-rs: device SMS authority unavailable; SMS isolated without legacy fallback"
                ));
                device_send_store::Store::unavailable(data_root)
            }
        },
        None => device_send_store::Store::open(data_root),
    }.map_err(|e| BackendFailure::Io(e.to_string()))?;
    if !device_send_store.is_v2()
        && !device_send_store.status().restart_required
        && device_send_store.recover().is_err()
    {
        crate::diagnostics::write_line(format_args!(
            "switchd-rs: device send store unavailable; new submissions disabled"
        ));
    }
    let forwarding_handoff_store = forwarding_handoff_store::Store::open(data_root)
        .map_err(|e| BackendFailure::Io(e.to_string()))?;
    Ok((
        multistandby_store,
        device_send_store,
        forwarding_handoff_store,
    ))
}

async fn run_with_sms_stores(
    startup_arguments: daemon_args::DaemonArgs,
    lifetime: &'static daemon_lifetime::Lease,
    multistandby_store: multistandby_store::Store,
    device_send_store: device_send_store::Store,
    forwarding_handoff_store: forwarding_handoff_store::Store,
    device_sms_forwarding_store: device_forwarding_store::Store,
    sms_forwarding: sms_forwarding::Service,
    app_token: String,
) -> Result<(), BackendFailure> {
    // Capture the original parent and install handlers before finite startup
    // work can block. A signal requests cancellation; it never drops a native
    // worker's locks or reports completion before the actual worker returns.
    let initial_parent = getppid();
    let mut terminate = signal(SignalKind::terminate()).map_err(io_failure)?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(io_failure)?;
    let mut broken_pipe = signal(SignalKind::pipe()).map_err(io_failure)?;
    // Complete only a strictly pinned filesystem transaction before recovery
    // classification or any module producer. Failure remains quarantined by
    // the classifier; this diagnostic carries no backup contents or authority.
    if startup_recovery::reconcile::backup_lifecycle::resume(
        multistandby_store.data_root(),
        &lifetime.root_proof(),
    )
    .is_err()
    {
        eprintln!("startup_backup_lifecycle_pending");
    }
    match module_recovery::legacy_locks::migrate(
        multistandby_store.data_root(),
        &lifetime.root_proof(),
    ) {
        Ok(true) => diagnostics::write_line(format_args!(
            "switchd-rs: idle legacy lock pair provenance recorded; no physical recovery performed"
        )),
        Ok(false) => {}
        Err(_) => diagnostics::write_line(format_args!(
            "switchd-rs: idle legacy lock provenance unavailable; module quarantine retained"
        )),
    }
    let selected_port =
        module_startup::explicit_port(&startup_arguments).map_err(BackendFailure::Io)?;
    let module_recovery = dji_module::admission_gate(lifetime);
    recovery_diagnostics::bind(&module_recovery);
    // The independent metadata owner runs before any module producer starts.
    // It releases both actual locks before returning a retained read-only
    // observation. It cannot turn historical evidence into device authority.
    let mut startup_recovery_inspection = match startup_recovery::Coordinator::inspect(
        &module_recovery,
        multistandby_store.data_root(),
    ) {
        Ok(inspection) => {
            recovery_diagnostics::metadata(if module_recovery.is_quarantined() {
                recovery_diagnostics::Metadata::Inspected
            } else {
                recovery_diagnostics::Metadata::NotNeeded
            });
            if module_recovery.is_quarantined() {
                if let Ok(diagnostic) = serde_json::to_string(inspection.startup_logging()) {
                    diagnostics::write_line(format_args!(
                        "switchd-rs: startup recovery {diagnostic}"
                    ));
                }
            }
            Some(inspection)
        }
        Err(error) => {
            recovery_diagnostics::metadata(recovery_diagnostics::Metadata::Failed);
            diagnostics::write_line(format_args!(
                "switchd-rs: {}; module quarantine retained",
                error.code()
            ));
            None
        }
    };
    // Startup compensation owns only the original journal's exact device and
    // new durable write permits. Checked close and actual worker join precede
    // archival and same-Gate activation. Inapplicable recovery stays read-only.
    let startup_cancel = Arc::new(AtomicBool::new(false));
    let startup_stop = async {
        let mut monitor = interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = terminate.recv() => break,
                _ = interrupt.recv() => break,
                // Preserve the established EPIPE behavior: a closed log pipe
                // alone does not terminate the daemon or interrupt cleanup.
                _ = broken_pipe.recv() => {},
                _ = monitor.tick() => {
                    if parent_process_changed(initial_parent, getppid()) {
                        break;
                    }
                }
            }
        }
    };
    let startup_operation = async {
        if selected_port.is_none() {
            match startup_recovery::Coordinator::inspect_legacy_current(
                &module_recovery,
                multistandby_store.data_root(),
                startup_cancel.clone(),
            )
            .await
            {
                Ok(Some(completion)) => {
                    let gate = module_recovery.clone();
                    let activation = tokio::task::spawn_blocking(move || {
                        gate.consume_legacy_current(completion)
                    })
                    .await
                    .unwrap_or(Err(()));
                    recovery_diagnostics::legacy_current(if activation.is_ok() {
                        recovery_diagnostics::LegacyCurrent::CurrentVerified
                    } else {
                        recovery_diagnostics::LegacyCurrent::PublicationUnconfirmed
                    });
                    return activation
                        .map(StartupInspection::LegacyCurrent)
                        .map_err(|()| {
                            StartupInspectionFailure::LegacyCurrent(
                                startup_recovery::legacy_current::Failure::EvidenceChanged,
                            )
                        });
                }
                Ok(None) => {}
                Err(error) => return Err(StartupInspectionFailure::LegacyCurrent(error)),
            }
        }
        match startup_recovery::Coordinator::reconcile_with_cancel(
            &module_recovery,
            multistandby_store.data_root(),
            startup_cancel.clone(),
        )
        .await
        {
            Ok(Some(completion)) => Ok(StartupInspection::Reconciled(completion)),
            // Only policy inapplicability before any recovery I/O may enter
            // the old readonly path. An attempted or failed restoration must
            // never silently reopen a device as another inspection attempt.
            Ok(None) => {
                recovery_diagnostics::reconciliation(
                    recovery_diagnostics::Reconciliation::NotApplicable,
                );
                startup_recovery::Coordinator::inspect_native_with_cancel(
                    &module_recovery,
                    multistandby_store.data_root(),
                    startup_cancel.clone(),
                )
                .await
                .map(StartupInspection::ReadOnly)
                .map_err(StartupInspectionFailure::Inspection)
            }
            Err(error) => Err(StartupInspectionFailure::Reconciliation(error)),
        }
    };
    let (startup_result, startup_stopped) =
        await_startup_native(startup_operation, startup_cancel.clone(), startup_stop).await;
    let mut legacy_current_seed = None;
    let (startup_native_inspection, mut startup_native_cleanup_error) = match startup_result {
        Ok(StartupInspection::LegacyCurrent(seed)) => {
            recovery_diagnostics::native_inspection(
                recovery_diagnostics::NativeInspection::Inspected,
            );
            recovery_diagnostics::cleanup(recovery_diagnostics::Cleanup::Confirmed);
            if !startup_stopped {
                legacy_current_seed = Some(seed);
                startup_recovery_inspection = None;
                diagnostics::write_line(format_args!(
                    "switchd-rs: legacy_current_verified; current baseline only; historical_recovery_confirmed=false"
                ));
            }
            (None, None)
        }
        Ok(StartupInspection::ReadOnly(Some(inspection))) => {
            recovery_diagnostics::native_inspection(
                recovery_diagnostics::NativeInspection::Inspected,
            );
            recovery_diagnostics::cleanup(recovery_diagnostics::Cleanup::Confirmed);
            if let Ok(diagnostic) = serde_json::to_string(inspection.startup_logging()) {
                diagnostics::write_line(format_args!(
                    "switchd-rs: startup native inspection {diagnostic}"
                ));
            }
            (Some(inspection), None)
        }
        Ok(StartupInspection::ReadOnly(None)) => {
            recovery_diagnostics::native_inspection(
                recovery_diagnostics::NativeInspection::NotApplicable,
            );
            (None, None)
        }
        Ok(StartupInspection::Reconciled(completion)) => {
            // The existing completion already requires real worker join and
            // checked release; this observation grants no admission rights.
            recovery_diagnostics::cleanup(recovery_diagnostics::Cleanup::Confirmed);
            if startup_stopped {
                recovery_diagnostics::reconciliation(
                    recovery_diagnostics::Reconciliation::CompletedAfterStop,
                );
                // The actual worker and finalizer have ended. Retain its
                // durable publication for the next startup, but do not start
                // ordinary producers after an observed stop request.
                (None, None)
            } else if module_recovery.consume_reconciliation(&completion).is_ok() {
                recovery_diagnostics::reconciliation(
                    recovery_diagnostics::Reconciliation::Completed,
                );
                // The new owner verified the exact archived placement and
                // published the durable completion in this same Gate. The old
                // readonly observer is intentionally superseded, not ignored
                // after a failed unchanged-evidence assertion.
                startup_recovery_inspection = None;
                diagnostics::write_line(format_args!(
                    "switchd-rs: startup reconciliation published receipt={}; ordinary module admission restored",
                    completion.receipt_sha256(),
                ));
                (None, None)
            } else {
                recovery_diagnostics::reconciliation(
                    recovery_diagnostics::Reconciliation::PublicationUnconfirmed,
                );
                diagnostics::write_line(format_args!(
                    "switchd-rs: startup_reconciliation_publication_unconfirmed; module quarantine retained"
                ));
                (None, None)
            }
        }
        Err(error) => {
            match &error {
                StartupInspectionFailure::LegacyCurrent(_)
                | StartupInspectionFailure::Inspection(_) => {
                    recovery_diagnostics::native_inspection(
                        recovery_diagnostics::NativeInspection::Failed,
                    )
                }
                StartupInspectionFailure::Reconciliation(_) => {
                    recovery_diagnostics::reconciliation(
                        recovery_diagnostics::Reconciliation::Failed,
                    )
                }
            }
            if error.cleanup_unconfirmed() {
                recovery_diagnostics::cleanup(recovery_diagnostics::Cleanup::Unconfirmed);
            }
            diagnostics::write_line(format_args!(
                "switchd-rs: {} cleanup_unconfirmed={}; module quarantine retained",
                error.diagnostic_code(),
                error.cleanup_unconfirmed()
            ));
            let debt = error.cleanup_unconfirmed().then_some(error);
            (None, debt)
        }
    };
    if startup_stopped {
        // No module/reader/forwarding producer has been started. The finite
        // native worker has already returned, including real resource cleanup.
        // Its unconfirmed close/unlock result still prevents a clean exit.
        let forwarding_result = sms_forwarding.shutdown().await;
        let mut report = shutdown::Report::default();
        report.check("SMS forwarding worker", forwarding_result);
        if let Some(error) = startup_native_cleanup_error.take() {
            report.record("startup native cleanup", error);
        }
        report.check(
            "startup archive workers",
            archive_io::finish_pending().await,
        );
        return report.finish();
    }
    let availability = BackendAvailability::with_module_recovery(module_recovery.clone());
    let multistandby_operations =
        multistandby_operations::Store::open(multistandby_store.data_root())
            .map_err(|error| BackendFailure::Io(error.to_string()))?;
    if multistandby_operations.recover().is_err() {
        crate::diagnostics::write_line(format_args!(
            "switchd-rs: N-wait operation registry unavailable; new operations disabled"
        ));
    }
    let sms_adopt_store = sms_adopt::Store::new(multistandby_store.data_root());
    let initial_sms_adopt = sms_adopt_store.load().unwrap_or_else(|_| {
        crate::diagnostics::write_line(format_args!(
            "switchd-rs: legacy SMS adoption setting is unavailable; defaulting to disabled"
        ));
        false
    });
    reader_esim_download::initialize(multistandby_store.data_root()).map_err(|error| {
        BackendFailure::Io(format!("eSIM operation journal unavailable: {error}"))
    })?;
    esim_mutation_intent::initialize(multistandby_store.data_root()).map_err(|error| {
        BackendFailure::Io(format!("eSIM mutation intent journal unavailable: {error}"))
    })?;
    reader_mutation_intent::initialize(multistandby_store.data_root()).map_err(|error| {
        BackendFailure::Io(format!(
            "reader mutation receipt journal unavailable: {error}"
        ))
    })?;
    reader_notification_intent::initialize(multistandby_store.data_root()).map_err(|error| {
        BackendFailure::Io(format!(
            "reader notification receipt journal unavailable: {error}"
        ))
    })?;
    call_mutation_intent::initialize(multistandby_store.data_root()).map_err(|error| {
        BackendFailure::Io(format!(
            "call mutation receipt journal unavailable: {error}"
        ))
    })?;
    let internal_token =
        multistandby_store::random_token().map_err(|e| BackendFailure::Io(e.to_string()))?;
    // Keep the Rust-owned keyed-session cleanup/recovery service live for the
    // daemon lifetime. This does not open a reader or report VoWiFi readiness;
    // public session admission remains gated until the native SMS/call runtime
    // is connected to this same owner.
    let native_vowifi_lifecycle = Arc::new(native_vowifi_session::NativeLifecycle::default());
    let qmi_config = qmi_enhancement::NativeQmiConfig::from_backend_args(&startup_arguments)
        .map_err(BackendFailure::Io)?;
    let socket_path = parse_listen_path(&startup_arguments)?;
    let callback_path = callback_socket_path(&socket_path);
    let internal_path = internal_socket_path(&socket_path);
    let (listener, _guard) = SocketGuard::bind(&socket_path).await?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).map_err(io_failure)?;
    let (callback_listener, _callback_guard) = SocketGuard::bind(&callback_path).await?;
    fs::set_permissions(&callback_path, fs::Permissions::from_mode(0o600)).map_err(io_failure)?;
    let (internal_listener, _internal_guard) = SocketGuard::bind(&internal_path).await?;
    fs::set_permissions(&internal_path, fs::Permissions::from_mode(0o600)).map_err(io_failure)?;
    let forwarding_admission =
        forwarding_admission_store::Store::open(multistandby_store.data_root())
            .map_err(|error| BackendFailure::Io(error.to_string()))?;
    // Native services own their producer for the daemon lifetime. Hardware
    // readiness is reported separately from the public service being alive.
    let call_producer_id = snapshot::new_instance_id();
    native_vowifi_lifecycle
        .ordinary_admission()
        .set_go_producer(&call_producer_id)
        .map_err(|reason| BackendFailure::Io(reason.to_string()))?;
    let reader_producer_id = snapshot::new_instance_id();
    reader_reservation::gate().initialize_native_reader_producer(&reader_producer_id);
    reader_reservation::gate().initialize_business_producer(&call_producer_id);
    reader_reservation::gate()
        .set_ordinary_admission(native_vowifi_lifecycle.ordinary_admission())
        .map_err(|reason| BackendFailure::Io(reason.to_owned()))?;
    let modem_actor = modem_actor::ModemActor::new_with_module_recovery(module_recovery.clone())
        .map_err(|error| BackendFailure::Io(error.to_string()))?;
    let multi_runtime_report = Arc::new(Mutex::new(multistandby_runtime_report::Journal::open(
        &multistandby_store
            .data_root()
            .join("multi-runtime-events.json"),
        unix_now_ms(),
    )));
    // Discovery and native USB startup do not depend on a compatibility child.
    let go_socket_path = gateway::child_socket_path(&socket_path);
    let native_module_producer_id = snapshot::new_instance_id();
    let call_archive =
        proletarius_store::ArchiveStore::new(multistandby_store.data_root().join("call-log.json"));
    let sms_inbox = message_archive::InboxState::for_go_producer(call_producer_id.clone());
    let legacy_current_startup = legacy_current_seed.is_some();
    let usb_at = match legacy_current_seed {
        Some(seed) => usb_at::Owner::with_legacy_current_seed(
            native_module_producer_id.clone(),
            initial_sms_adopt,
            module_recovery.clone(),
            seed,
        ),
        None => usb_at::Owner::new_with_adopt_and_recovery(
            native_module_producer_id.clone(),
            initial_sms_adopt,
            module_recovery.clone(),
        ),
    }
    .with_profile_notes(profile_notes::Store::new(multistandby_store.data_root()));
    let usb_sms_receive = usb_sms_receive::Service::new(
        proletarius_store::ArchiveStore::new(
            multistandby_store.data_root().join("sms-archive.json"),
        ),
        sms_inbox.clone(),
        usb_at.clone(),
        availability.clone(),
        native_module_producer_id.clone(),
    );
    let module_radio = module_radio::Service::new(
        multistandby_store.data_root().to_path_buf(),
        usb_at.clone(),
        modem_actor.clone(),
        availability.clone(),
        native_module_producer_id.clone(),
    );
    if legacy_current_startup {
        module_radio.preserve_current_startup_cfun();
    }
    let network_traffic = network_traffic::Service::new(multistandby_store.data_root());
    let state = ServerState {
        call_mode: Default::default(),
        module_call: Default::default(),
        reader_lifecycle: Default::default(),
        multistandby_card: Default::default(),
        multistandby_service: Default::default(),
        app_token: Some(app_token),
        internal_token: internal_token.clone(),
        multistandby_store,
        multistandby_operations,
        multi_runtime_report,
        device_send_store,
        forwarding_handoff_store,
        sms_forwarding,
        sms_inbox,
        usb_sms_receive,
        usb_at,
        module_radio,
        call_facts: call_facts::CallFacts::new(call_archive.clone(), call_producer_id.clone()),
        call_archive,
        modem_actor,
        qmi_enhancement: qmi_enhancement::QmiEnhancement::default()
            .with_module_recovery(module_recovery.clone()),
        network_traffic,
        go_socket_path,
        availability: availability.clone(),
        reader_discovery: Arc::new(Mutex::new(ReaderDiscovery::default())),
        reader_pcsc: reader_pcsc::Owner::new(reader_producer_id),
        reader_gate: reader_reservation::gate(),
        snapshot_instance_id: snapshot::new_instance_id(),
        snapshot_revision: Arc::new(AtomicU64::new(0)),
        eap_exchanges: Arc::new(eap_identity::EapExchangeStore::default()),
        native_vowifi_lifecycle,
        ims_sms_transactions: ims_sms_transactions::Service::default(),
    };
    // No hardware producer, request listener or scheduling loop is active yet.
    // If a lazy constructor fails, drain the few already-created idle owners.
    let preflight = (|| {
        let native_multi = state
            .multistandby_service()
            .map_err(|error| BackendFailure::Io(error.to_string()))?;
        let reader_service = state
            .reader_lifecycle()
            .map_err(|error| BackendFailure::Io(error.message))?;
        let module_call = if module_recovery.is_quarantined() {
            None
        } else {
            Some(state.module_call().map_err(BackendFailure::Io)?)
        };
        state
            .native_vowifi_lifecycle
            .activate()
            .map_err(|reason| BackendFailure::Io(reason.to_owned()))?;
        Ok::<_, BackendFailure>((native_multi, reader_service, module_call))
    })();
    let (native_multi, reader_service, module_call) = match preflight {
        Ok(services) => services,
        Err(error) => {
            let mut report = shutdown::Report::default();
            report.record("startup preflight", error);
            if let Some(error) = startup_native_cleanup_error.take() {
                report.record("startup native cleanup", error);
            }
            if let Some(Ok(module_call)) = state.module_call.get() {
                report.extend(
                    shutdown::confirmed("idle module call", || {
                        module_call.finish_shutdown_receipt()
                    })
                    .await,
                );
            }
            let modem = state.modem_actor.clone();
            let closed = tokio::task::spawn_blocking(move || modem.shutdown()).await;
            match closed {
                Ok(result) => report.check("idle modem", result.map_err(|error| error.code())),
                Err(error) => report.record("idle modem join", error),
            }
            …37868 tokens truncated…dAvailability::new(),
            reader_discovery: Arc::new(std::sync::Mutex::new(super::ReaderDiscovery::default())),
            snapshot_instance_id: "test".to_owned(),
            snapshot_revision: Arc::new(super::AtomicU64::new(0)),
            eap_exchanges: Arc::new(super::eap_identity::EapExchangeStore::default()),
            native_vowifi_lifecycle: Arc::new(
                super::native_vowifi_session::NativeLifecycle::default(),
            ),
            ims_sms_transactions: super::ims_sms_transactions::Service::default(),
            reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
            reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
            modem_actor: super::modem_actor::ModemActor::new().unwrap(),
            qmi_enhancement: super::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: super::network_traffic::Service::default(),
            usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
            module_radio: crate::test_module_radio(&root),
        };
        // Use the real public/private HTTP connection drivers. Retired paths
        // are absent from routing, so even a valid capability cannot revive them.
        async fn request(
            state: &super::ServerState,
            socket: &str,
            method: &str,
            path: &str,
            token: Option<&str>,
            body: &str,
        ) -> (hyper::StatusCode, serde_json::Value) {
            let (client, server) = UnixStream::pair().unwrap();
            let state = state.clone();
            let server_task =
                match socket {
                    "public" => {
                        tokio::spawn(async move { super::serve_connection(server, state).await })
                    }
                    "private" => tokio::spawn(async move {
                        super::serve_internal_connection(server, state).await
                    }),
                    "shutdown" => tokio::spawn(async move {
                        super::serve_internal_shutdown_connection(server, state).await;
                        Ok(())
                    }),
                    _ => unreachable!(),
                };
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(client))
                    .await
                    .unwrap();
            let connection_task = tokio::spawn(connection);
            let mut request = hyper::Request::builder()
                .method(method)
                .uri(path)
                .header("host", "localhost")
                .header("x-proletarius-app-token", "app-fixture");
            if let Some(token) = token {
                request = request.header("x-switch-internal-token", token);
            }
            let response = sender
                .send_request(
                    request
                        .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            drop(sender);
            // A fully consumed response may race the peer's Unix half-close.
            // Both tasks still join; only the existing benign close error is allowed.
            for result in [connection_task.await.unwrap(), server_task.await.unwrap()] {
                if let Err(error) = result {
                    assert!(
                        super::benign_connection_error_message(&error.to_string()),
                        "{error:?}"
                    );
                }
            }
            (status, value)
        }
        let store_path = root.join("multi/multistandby/store-v1.json");
        let operations_path = root.join("multistandby/operations-v1.json");
        for path in [&store_path, &journal_path, &operations_path] {
            assert!(
                !path.exists(),
                "fixture unexpectedly persisted {}",
                path.display()
            );
        }
        let observe = r#"{"profile_id":"profile_auth_fixture","generation":4,"reason":"readiness","desired":true,"tunnel_connected":true,"ims_registered":true,"messaging_ready":true,"sim_sms_route_ready":true}"#;
        let mut retired = vec![
            (
                "POST",
                "/api/internal/multistandby/runtime/observe".to_owned(),
                observe,
            ),
            (
                "POST",
                "/api/internal/multistandby/runtime/recovery-plan".to_owned(),
                r#"{"profile_id":"profile_auth_fixture","reason":"planned_renewal"}"#,
            ),
            (
                "POST",
                "/api/internal/multistandby/runtime/retry-schedule".to_owned(),
                "{}",
            ),
            (
                "POST",
                "/api/internal/multistandby/runtime/registration-schedule".to_owned(),
                "{}",
            ),
            (
                "POST",
                "/api/internal/multistandby/operation-claim".to_owned(),
                "{}",
            ),
            (
                "POST",
                "/api/internal/multistandby/operation-transition".to_owned(),
                "{}",
            ),
            (
                "POST",
                "/api/internal/multistandby/card/submit".to_owned(),
                "{}",
            ),
            (
                "POST",
                "/api/internal/multistandby/card/receipt".to_owned(),
                "{}",
            ),
            (
                "POST",
                "/api/internal/multistandby/card/cancel".to_owned(),
                "{}",
            ),
            (
                "GET",
                "/api/internal/multistandby/store/catalog".to_owned(),
                "",
            ),
            (
                "GET",
                "/api/internal/multistandby/store/pending".to_owned(),
                "",
            ),
            (
                "POST",
                "/api/internal/multistandby/store/catalog".to_owned(),
                r#"{"event_id":"retired-catalog","reader":"Offline Reader","profiles":[]}"#,
            ),
        ];
        for action in [
            "desired",
            "queue",
            "claim",
            "claim-next",
            "outcome",
            "inbound",
            "inbound-event",
            "recovery-selection",
        ] {
            retired.push((
                "POST",
                format!("/api/internal/multistandby/store/{action}"),
                "{}",
            ));
        }
        for action in ["acquire", "status", "release", "renew", "quiesce"] {
            retired.push((
                "POST",
                format!("/api/internal/multistandby/reader-reservation/operation/{action}"),
                "{}",
            ));
        }
        for (method, path, body) in retired {
            for socket in ["public", "private", "shutdown"] {
                for token in [None, Some("wrong-token"), Some("internal-fixture")] {
                    let (status, value) = request(&state, socket, method, &path, token, body).await;
                    assert_eq!(
                        status,
                        hyper::StatusCode::NOT_FOUND,
                        "{socket} {method} {path} token={token:?}: {value}"
                    );
                    assert_eq!(value["code"], "route_not_found");
                }
            }
        }
        // The read-only module busy observer and native inventory remain routed and
        // authenticated. Invalid bodies reach their validators without card I/O.
        for path in [
            "/api/internal/reader-business",
            "/api/internal/multistandby/inventory",
        ] {
            for socket in ["private", "shutdown"] {
                for token in [None, Some("wrong-token")] {
                    let (status, value) = request(&state, socket, "POST", path, token, "{}").await;
                    assert_eq!(status, hyper::StatusCode::UNAUTHORIZED);
                    assert_eq!(value["code"], "internal_authentication_required");
                }
            }
            let (status, value) = request(
                &state,
                "private",
                "POST",
                path,
                Some("internal-fixture"),
                "{}",
            )
            .await;
            assert_eq!(status, hyper::StatusCode::BAD_REQUEST);
            assert_eq!(value["code"], "invalid_request");
        }
        for (path, expected, code) in [
            (
                "/api/internal/multistandby/inventory",
                hyper::StatusCode::SERVICE_UNAVAILABLE,
                "backend_stopping",
            ),
            (
                "/api/internal/reader-business",
                hyper::StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
        ] {
            let (status, value) = request(
                &state,
                "shutdown",
                "POST",
                path,
                Some("internal-fixture"),
                "{}",
            )
            .await;
            assert_eq!(status, expected, "{path}: {value}");
            assert_eq!(value["code"], code);
        }
        state.multistandby_operations.finish_dispatches().await;
        assert_eq!(state.multistandby_store.current_revision().unwrap(), 0);
        for path in [&store_path, &journal_path, &operations_path] {
            assert!(!path.exists(), "retired IPC persisted {}", path.display());
        }
        state.reader_pcsc.shutdown().await.unwrap();
        fs::remove_dir_all(root).expect("remove Switch/build test root");
    }

    #[tokio::test]
    async fn internal_modem_actor_requires_token_and_allows_shutdown_release_only() {
        use bytes::Bytes;
        use http_body_util::{BodyExt, Full};
        use hyper_util::rt::TokioIo;
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;
        use tokio::net::UnixStream;

        let root = test_directory("modem-actor-shutdown");
        let sms_inbox = crate::message_archive::InboxState::default();
        let state = super::ServerState {
            call_mode: Default::default(),
            module_call: Default::default(),
            reader_lifecycle: Default::default(),
            multistandby_card: Default::default(),
            multistandby_service: Default::default(),
            app_token: Some("app-fixture".to_owned()),
            internal_token: "internal-fixture".to_owned(),
            multistandby_store: super::multistandby_store::Store::open(&root.join("multi"))
                .unwrap(),
            multistandby_operations: super::multistandby_operations::Store::open(&root).unwrap(),
            multi_runtime_report: Arc::new(std::sync::Mutex::new(
                super::multistandby_runtime_report::Journal::default(),
            )),
            device_send_store: super::device_send_store::Store::open(&root.join("device")).unwrap(),
            forwarding_handoff_store: super::forwarding_handoff_store::Store::open(&root).unwrap(),
            sms_forwarding: super::sms_forwarding::Service::default(),
            sms_inbox: sms_inbox.clone(),
            usb_sms_receive: crate::test_usb_sms_receiver(
                sms_inbox.clone(),
                root.join("sms-archive.json"),
            ),
            call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
            call_facts: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                "call-producer-fixture".to_owned(),
            ),
            go_socket_path: PathBuf::new(),
            availability: super::BackendAvailability::new(),
            reader_discovery: Arc::new(std::sync::Mutex::new(super::ReaderDiscovery::default())),
            snapshot_instance_id: "test".to_owned(),
            snapshot_revision: Arc::new(AtomicU64::new(0)),
            eap_exchanges: Arc::new(super::eap_identity::EapExchangeStore::default()),
            native_vowifi_lifecycle: Arc::new(
                super::native_vowifi_session::NativeLifecycle::default(),
            ),
            ims_sms_transactions: super::ims_sms_transactions::Service::default(),
            reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
            reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
            modem_actor: super::modem_actor::ModemActor::new().unwrap(),
            qmi_enhancement: super::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: super::network_traffic::Service::default(),
            usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
            module_radio: crate::test_module_radio(&root),
        };
        for (path, token, body, expected) in [
            (
                "/api/internal/modem/discover-at-port",
                None,
                "{}",
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/modem/discover-at-port",
                Some("internal-fixture"),
                "{}",
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/modem/open",
                None,
                r#"{"port":"/dev/cu.fixture","baud_rate":115200}"#,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/modem/open",
                Some("internal-fixture"),
                r#"{"port":"/dev/cu.fixture","baud_rate":115200}"#,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/modem/execute",
                None,
                r#"{"command":"AT+CSQ","timeout_ms":1000}"#,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/modem/execute",
                Some("internal-fixture"),
                r#"{"command":"AT+CSQ","timeout_ms":1000}"#,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/modem/typed-query",
                None,
                r#"{"query":"imei"}"#,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/modem/typed-query",
                Some("internal-fixture"),
                r#"{"query":"imei"}"#,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/modem/call-command",
                None,
                r#"{"action":"answer","value":""}"#,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/modem/call-command",
                Some("internal-fixture"),
                r#"{"action":"answer","value":""}"#,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/modem/execute",
                Some("internal-fixture"),
                r#"{"command":"AT+CMGS=2","timeout_ms":20000,"high_priority":true,"follow_up":"0011^Z","expected_sms_generation":7}"#,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/modem/read-stored-sms",
                None,
                r#"{"storage":"ME","index":7,"expected_sms_generation":0}"#,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/modem/read-stored-sms",
                Some("internal-fixture"),
                r#"{"storage":"ME","index":7,"expected_sms_generation":0}"#,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/modem/initialize",
                None,
                "{}",
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/modem/initialize",
                Some("internal-fixture"),
                "{}",
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/modem/sms-receive",
                None,
                r#"{"action":"scan"}"#,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/modem/sms-receive",
                Some("internal-fixture"),
                r#"{"action":"configure","enabled":true}"#,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/modem/close",
                None,
                "{}",
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/modem/close",
                Some("internal-fixture"),
                "{}",
                hyper::StatusCode::BAD_REQUEST,
            ),
            (
                "/api/internal/modem/close",
                Some("internal-fixture"),
                r#"{"serial_session":1}"#,
                hyper::StatusCode::OK,
            ),
            (
                "/api/internal/qmi/open",
                None,
                r#"{"control_path":""}"#,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/qmi/open",
                Some("internal-fixture"),
                r#"{"control_path":""}"#,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/qmi/probe",
                Some("internal-fixture"),
                "{}",
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/qmi/close",
                None,
                "{}",
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/qmi/close",
                Some("internal-fixture"),
                "{}",
                hyper::StatusCode::OK,
            ),
        ] {
            let (client, server) = UnixStream::pair().unwrap();
            let connection_state = state.clone();
            let server_task = tokio::spawn(async move {
                super::serve_internal_shutdown_connection(server, connection_state).await;
            });
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(client))
                    .await
                    .unwrap();
            let connection_task = tokio::spawn(connection);
            let mut request = hyper::Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("host", "localhost");
            if let Some(token) = token {
                request = request.header("x-switch-internal-token", token);
            }
            let response = sender
                .send_request(request.body(Full::new(Bytes::from(body))).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "route {path}");
            response.into_body().collect().await.unwrap();
            drop(sender);
            let _ = connection_task.await;
            server_task.await.unwrap();
        }
        fs::remove_dir_all(root).expect("remove Switch/build test root");
    }

    #[test]
    fn parses_go_compatible_listen_argument() {
        let arguments =
            crate::daemon_args::parse(["-listen".into(), "unix:/tmp/proletarius.sock".into()])
                .unwrap();
        let path = parse_listen_path(&arguments).expect("listen argument");
        assert_eq!(path.to_string_lossy(), "/tmp/proletarius.sock");
    }

    #[test]
    fn app_token_contract_rejects_absent_weak_or_header_unsafe_values() {
        assert!(super::valid_app_token("0123456789abcdef"));
        assert!(super::valid_app_token(&"a".repeat(4096)));
        for value in ["", "short", "contains space token", "line\nbreak"] {
            assert!(!super::valid_app_token(value));
        }
        assert!(!super::valid_app_token(&"a".repeat(4097)));
        let no_header = hyper::Request::builder().body(()).unwrap();
        assert!(!super::token_matches(&no_header, None));
        assert!(!super::token_matches(&no_header, Some("0123456789abcdef")));
        let matching = hyper::Request::builder()
            .header(super::APP_TOKEN_HEADER, "0123456789abcdef")
            .body(())
            .unwrap();
        assert!(super::token_matches(&matching, Some("0123456789abcdef")));
        assert!(!super::token_matches(&matching, Some("fedcba9876543210")));
    }

    #[test]
    fn suppresses_only_known_client_disconnect_noise() {
        assert!(benign_connection_error_message(
            "error shutting down connection"
        ));
        assert!(benign_connection_error_message(
            "error writing a body to connection"
        ));
        assert!(!benign_connection_error_message(
            "invalid HTTP request received"
        ));
    }

    #[test]
    fn exits_when_parent_is_replaced_or_reparented_to_launchd() {
        let app = Pid::from_raw(42);
        assert!(!parent_process_changed(app, app));
        assert!(parent_process_changed(app, Pid::from_raw(43)));
        assert!(parent_process_changed(app, Pid::from_raw(1)));
    }

    #[test]
    fn rejects_tcp_or_missing_listen_arguments() {
        let arguments =
            crate::daemon_args::parse(["-listen".into(), "127.0.0.1:8080".into()]).unwrap();
        assert!(parse_listen_path(&arguments).is_err());
        assert!(parse_listen_path(&crate::daemon_args::DaemonArgs::default()).is_err());
    }

    #[test]
    fn shutdown_allows_only_authenticated_exchange_release() {
        for (method, path, token, expected) in [
            (
                Method::POST,
                "/api/v2/vowifi/eap-exchange/release",
                None,
                Some(hyper::StatusCode::UNAUTHORIZED),
            ),
            (
                Method::POST,
                "/api/v2/vowifi/eap-exchange/release",
                Some("wrong"),
                Some(hyper::StatusCode::UNAUTHORIZED),
            ),
            (
                Method::POST,
                "/api/v2/vowifi/eap-exchange/release",
                Some("fixture"),
                None,
            ),
            (
                Method::GET,
                "/api/v2/vowifi/eap-exchange/release",
                Some("fixture"),
                Some(hyper::StatusCode::SERVICE_UNAVAILABLE),
            ),
            (
                Method::POST,
                "/api/sms/send",
                Some("fixture"),
                Some(hyper::StatusCode::SERVICE_UNAVAILABLE),
            ),
        ] {
            let mut request = hyper::Request::builder().method(method).uri(path);
            if let Some(token) = token {
                request = request.header("x-proletarius-app-token", token);
            }
            let request = request.body(()).unwrap();
            assert_eq!(
                shutdown_rejection(&request, Some("fixture")).map(|response| response.status()),
                expected
            );
        }
    }

    #[tokio::test]
    async fn shutdown_listener_serves_authenticated_cleanup_over_socket() {
        use bytes::Bytes;
        use http_body_util::{BodyExt, Full};
        use hyper_util::rt::TokioIo;
        use std::sync::Arc;
        use tokio::net::UnixStream;
        let root = test_directory("shutdown-listener-cleanup");
        for authenticated in [false, true] {
            let sms_inbox = crate::message_archive::InboxState::default();
            let (client, server) = UnixStream::pair().unwrap();
            let server = tokio::spawn(super::serve_shutdown_callback_connection(
                server,
                super::ServerState {
                    call_mode: Default::default(),
                    module_call: Default::default(),
                    reader_lifecycle: Default::default(),
                    multistandby_card: Default::default(),
                    multistandby_service: Default::default(),
                    app_token: Some("fixture".to_owned()),
                    internal_token: "internal-fixture".to_owned(),
                    multistandby_store: super::multistandby_store::Store::open(&root).unwrap(),
                    multistandby_operations: super::multistandby_operations::Store::open(&root)
                        .unwrap(),
                    multi_runtime_report: Arc::new(std::sync::Mutex::new(
                        super::multistandby_runtime_report::Journal::default(),
                    )),
                    device_send_store: super::device_send_store::Store::open(&root).unwrap(),
                    forwarding_handoff_store: super::forwarding_handoff_store::Store::open(&root)
                        .unwrap(),
                    sms_forwarding: super::sms_forwarding::Service::new(&root.join("forwarding")),
                    sms_inbox: sms_inbox.clone(),
                    usb_sms_receive: crate::test_usb_sms_receiver(
                        sms_inbox.clone(),
                        root.join("sms-archive.json"),
                    ),
                    call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                    call_facts: crate::call_facts::CallFacts::new(
                        proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                        "call-producer-fixture".to_owned(),
                    ),
                    go_socket_path: PathBuf::new(),
                    availability: super::BackendAvailability::new(),
                    reader_discovery: Arc::new(std::sync::Mutex::new(
                        super::ReaderDiscovery::default(),
                    )),
                    snapshot_instance_id: "test".to_owned(),
                    snapshot_revision: Arc::new(super::AtomicU64::new(0)),
                    eap_exchanges: Arc::new(super::eap_identity::EapExchangeStore::default()),
                    native_vowifi_lifecycle: Arc::new(
                        super::native_vowifi_session::NativeLifecycle::default(),
                    ),
                    ims_sms_transactions: super::ims_sms_transactions::Service::default(),
                    reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
                    reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
                    modem_actor: super::modem_actor::ModemActor::new().unwrap(),
                    qmi_enhancement: super::qmi_enhancement::QmiEnhancement::default(),
                    network_traffic: super::network_traffic::Service::default(),
                    usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
                    module_radio: crate::test_module_radio(&root),
                },
            ));
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(client))
                    .await
                    .unwrap();
            let connection = tokio::spawn(connection);
            let mut request = hyper::Request::builder()
                .method(hyper::Method::POST)
                .uri("/api/v2/vowifi/eap-exchange/release")
                .header("host", "localhost");
            if authenticated {
                request = request.header("x-proletarius-app-token", "fixture");
            }
            let body = format!("{{\"exchange_id\":\"{}\"}}", "0".repeat(64));
            let response = sender
                .send_request(request.body(Full::new(Bytes::from(body))).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if authenticated {
                    hyper::StatusCode::OK
                } else {
                    hyper::StatusCode::UNAUTHORIZED
                }
            );
            response.into_body().collect().await.unwrap();
            drop(sender);
            server.await.unwrap();
            let _ = connection.await;
        }
        fs::remove_dir_all(&root).unwrap();
        assert!(
            !root.exists(),
            "shutdown listener test root survived cleanup"
        );
    }

    #[tokio::test]
    async fn forwarding_routes_require_app_token_and_handoff_ack_is_durable() {
        use bytes::Bytes;
        use http_body_util::{BodyExt, Full};
        use hyper_util::rt::TokioIo;
        use std::sync::Arc;
        use tokio::net::UnixStream;

        let switch_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .expect("Switch root from Cargo manifest");
        let parent = switch_root.join("build/tmp/forwarding-handoff-route-tests");
        fs::create_dir_all(&parent).expect("create test parent in Switch/build");
        let root = parent.join(crate::multistandby_store::random_token().unwrap());
        fs::create_dir(&root).expect("create test root");

        let sms_inbox = crate::message_archive::InboxState::default();
        let state = super::ServerState {
            call_mode: Default::default(),
            module_call: Default::default(),
            reader_lifecycle: Default::default(),
            multistandby_card: Default::default(),
            multistandby_service: Default::default(),
            app_token: Some("fixture".to_owned()),
            internal_token: "internal-fixture".to_owned(),
            multistandby_store: super::multistandby_store::Store::open(&root.join("multi"))
                .unwrap(),
            multistandby_operations: super::multistandby_operations::Store::open(&root).unwrap(),
            multi_runtime_report: Arc::new(std::sync::Mutex::new(
                super::multistandby_runtime_report::Journal::default(),
            )),
            device_send_store: super::device_send_store::Store::open(&root.join("device")).unwrap(),
            forwarding_handoff_store: super::forwarding_handoff_store::Store::open(&root).unwrap(),
            sms_forwarding: super::sms_forwarding::Service::new(&root.join("forwarding")),
            sms_inbox: sms_inbox.clone(),
            usb_sms_receive: crate::test_usb_sms_receiver(
                sms_inbox.clone(),
                root.join("sms-archive.json"),
            ),
            call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
            call_facts: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                "call-producer-fixture".to_owned(),
            ),
            go_socket_path: PathBuf::new(),
            availability: super::BackendAvailability::new(),
            reader_discovery: Arc::new(std::sync::Mutex::new(super::ReaderDiscovery::default())),
            snapshot_instance_id: "test".to_owned(),
            snapshot_revision: Arc::new(super::AtomicU64::new(0)),
            eap_exchanges: Arc::new(super::eap_identity::EapExchangeStore::default()),
            native_vowifi_lifecycle: Arc::new(
                super::native_vowifi_session::NativeLifecycle::default(),
            ),
            ims_sms_transactions: super::ims_sms_transactions::Service::default(),
            reader_pcsc: crate::reader_pcsc::Owner::new("reader-producer-fixture".into()),
            reader_gate: std::sync::Arc::new(crate::reader_reservation::Gate::default()),
            modem_actor: super::modem_actor::ModemActor::new().unwrap(),
            qmi_enhancement: super::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: super::network_traffic::Service::default(),
            usb_at: crate::usb_at::Owner::new("usb-producer-fixture".into()),
            module_radio: crate::test_module_radio(&root),
        };
        let body = r#"{"schema_version":1,"handoff_id":"123e4567-e89b-12d3-a456-426614174000","device_sms":{"accepted_notification_ids":[],"was_enabled":null},"nwait_sms":{"cursor":null,"seeded":false,"was_enabled":null}}"#;

        let (client, server) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(super::serve_connection(server, state.clone()));
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
            .await
            .unwrap();
        let connection_task = tokio::spawn(connection);

        let unauthorized = sender
            .send_request(
                hyper::Request::builder()
                    .method(Method::GET)
                    .uri("/api/sms/forwarding/status")
                    .header("host", "localhost")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), hyper::StatusCode::UNAUTHORIZED);
        unauthorized.into_body().collect().await.unwrap();
        assert!(!root.join("forwarding/sms-forwarding").exists());
        assert!(!root.join("sms-forwarding-handoff.json").exists());
        drop(sender);
        let _ = connection_task.await.unwrap();
        server_task.await.unwrap().unwrap();

        let (client, server) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(super::serve_connection(server, state.clone()));
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
            .await
            .unwrap();
        let connection_task = tokio::spawn(connection);
        let status = sender
            .send_request(
                hyper::Request::builder()
                    .method(Method::GET)
                    .uri("/api/sms/forwarding/status")
                    .header("host", "localhost")
                    .header("x-proletarius-app-token", "fixture")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), hyper::StatusCode::OK);
        let status: serde_json::Value =
            serde_json::from_slice(&status.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(status["configured"], false);
        assert!(!root.join("forwarding/sms-forwarding").exists());
        drop(sender);
        let _ = connection_task.await.unwrap();
        server_task.await.unwrap().unwrap();

        let (client, server) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(super::serve_connection(server, state));
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
            .await
            .unwrap();
        let connection_task = tokio::spawn(connection);
        let accepted = {
            let _fixture = crate::forwarding_handoff_store::REQUEST_TEST_LOCK
                .lock()
                .await;
            sender
                .send_request(
                    hyper::Request::builder()
                        .method(Method::POST)
                        .uri("/api/v2/sms/forwarding/handoff")
                        .header("host", "localhost")
                        .header("x-proletarius-app-token", "fixture")
                        .body(Full::new(Bytes::from(body)))
                        .unwrap(),
                )
                .await
                .unwrap()
        };
        assert_eq!(accepted.status(), hyper::StatusCode::OK);
        let ack: serde_json::Value =
            serde_json::from_slice(&accepted.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(ack["accepted"], true);
        assert_eq!(ack["already_accepted"], false);
        assert!(root.join("sms-forwarding-handoff.json").exists());

        drop(sender);
        let _ = connection_task.await.unwrap();
        server_task.await.unwrap().unwrap();
        fs::remove_dir_all(root).expect("remove Switch/build test root");
    }

    #[tokio::test]
    async fn socket_preparation_preserves_regular_files() {
        let directory = test_directory("regular-file");
        let path = directory.join("important-data");
        fs::write(&path, b"keep").expect("write fixture");
        assert!(prepare_socket_path(&path).await.is_err());
        assert_eq!(fs::read(&path).expect("fixture remains"), b"keep");
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[tokio::test]
    async fn socket_preparation_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = test_directory("symlink");
        let target = directory.join("target");
        let link = directory.join("listen.sock");
        fs::write(&target, b"keep").expect("write target");
        symlink(&target, &link).expect("create symlink");
        assert!(prepare_socket_path(&link).await.is_err());
        assert!(fs::symlink_metadata(&link).is_ok());
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[tokio::test]
    async fn socket_preparation_removes_only_stale_sockets() {
        let directory = test_directory("socket");
        let path = directory.join("listen.sock");
        // Leave a real pathname that has never accepted connections. A just
        // closed listener can briefly still accept nonblocking connect on
        // Darwin; success must continue to protect that node in production.
        let abandoned =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        abandoned
            .bind(&socket2::SockAddr::unix(&path).unwrap())
            .expect("bind abandoned fixture socket");
        drop(abandoned);
        prepare_socket_path(&path)
            .await
            .expect("remove stale socket");
        assert!(!path.exists());
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[tokio::test]
    async fn socket_preparation_preserves_live_listener_without_waiting_for_accept() {
        use std::os::unix::fs::MetadataExt;
        let directory = test_directory("live-socket");
        let path = directory.join("s");
        let listener = StdUnixListener::bind(&path).unwrap();
        let before = fs::symlink_metadata(&path).unwrap();
        assert!(prepare_socket_path(&path).await.is_err());
        let after = fs::symlink_metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        // The probe's connection is accepted by the original listener, proving
        // the rejection did not replace or disconnect that listener.
        listener.set_nonblocking(true).unwrap();
        listener.accept().unwrap();
        drop(listener);
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn socket_preparation_preserves_saturated_listener_and_is_nonblocking() {
        use std::os::unix::fs::MetadataExt;
        use std::time::{Duration, Instant};
        let directory = test_directory("full-socket");
        let path = directory.join("s");
        let address = socket2::SockAddr::unix(&path).unwrap();
        let listener =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        listener.bind(&address).unwrap();
        listener.listen(1).unwrap();
        let mut clients = Vec::new();
        let mut saturated = false;
        for _ in 0..32 {
            let client =
                socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
            match client.connect_timeout(&address, Duration::from_millis(20)) {
                Ok(()) => clients.push(client),
                Err(_) => {
                    saturated = true;
                    break;
                }
            }
        }
        assert!(saturated, "fixture did not saturate accept queue");
        let before = fs::symlink_metadata(&path).unwrap();
        let start = Instant::now();
        assert!(prepare_socket_path(&path).await.is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        let after = fs::symlink_metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        drop(clients);
        drop(listener);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn socket_guard_removes_only_its_own_socket() {
        let directory = test_directory("own-socket");
        let path = directory.join("s");
        let listener = StdUnixListener::bind(&path).unwrap();
        let guard = super::SocketGuard::capture(
            &path,
            super::socket_lifetime::Claim::acquire(&path).unwrap(),
        )
        .unwrap();
        drop(guard);
        assert!(!path.exists());
        drop(listener);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn socket_guard_preserves_replacement_file_link_and_listener() {
        use std::os::unix::fs::{MetadataExt, symlink};
        for replacement in ["file", "symlink", "socket"] {
            let directory = test_directory("replaced-socket");
            let path = directory.join("s");
            let listener = StdUnixListener::bind(&path).unwrap();
            let guard = super::SocketGuard::capture(
                &path,
                super::socket_lifetime::Claim::acquire(&path).unwrap(),
            )
            .unwrap();
            fs::remove_file(&path).unwrap();
            let replacement_listener = match replacement {
                "file" => {
                    fs::write(&path, b"preserve replacement").unwrap();
                    None
                }
                "symlink" => {
                    fs::write(directory.join("target"), b"preserve target").unwrap();
                    symlink(directory.join("target"), &path).unwrap();
                    None
                }
                "socket" => Some(StdUnixListener::bind(&path).unwrap()),
                _ => unreachable!(),
            };
            let before = fs::symlink_metadata(&path).unwrap();
            drop(guard);
            let after = fs::symlink_metadata(&path).unwrap();
            assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
            if replacement == "socket" {
                let _connection = std::os::unix::net::UnixStream::connect(&path).unwrap();
            } else {
                assert!(fs::read(&path).unwrap().starts_with(b"preserve"));
            }
            drop(replacement_listener);
            drop(listener);
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test]
    async fn socket_secondary_claim_failure_cleans_only_this_startup_socket() {
        let directory = test_directory("callback-conflict");
        let public_path = directory.join("s");
        let callback_path = directory.join("c");
        let public = StdUnixListener::bind(&public_path).unwrap();
        let own_guard = super::SocketGuard::capture(
            &public_path,
            super::socket_lifetime::Claim::acquire(&public_path).unwrap(),
        )
        .unwrap();
        let foreign = StdUnixListener::bind(&callback_path).unwrap();
        assert!(prepare_socket_path(&callback_path).await.is_err());
        drop(own_guard);
        assert!(!public_path.exists());
        assert!(callback_path.exists());
        let _connection = std::os::unix::net::UnixStream::connect(&callback_path).unwrap();
        drop(public);
        drop(foreign);
        fs::remove_dir_all(directory).unwrap();
    }
}
