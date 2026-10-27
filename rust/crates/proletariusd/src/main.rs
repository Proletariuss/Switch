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
    let module_recovery = module_recovery::Gate::inspect_with_lifetime(lifetime);
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
                &module_recovery, multistandby_store.data_root(), startup_cancel.clone()).await {
                Ok(Some(completion)) => {
                    let gate = module_recovery.clone();
                    let activation = tokio::task::spawn_blocking(move ||
                        gate.consume_legacy_current(completion)).await.unwrap_or(Err(()));
                    recovery_diagnostics::legacy_current(if activation.is_ok() {
                        recovery_diagnostics::LegacyCurrent::CurrentVerified
                    } else {
                        recovery_diagnostics::LegacyCurrent::PublicationUnconfirmed
                    });
                    return activation.map(StartupInspection::LegacyCurrent).map_err(|()|
                        StartupInspectionFailure::LegacyCurrent(startup_recovery::legacy_current::Failure::EvidenceChanged));
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
            recovery_diagnostics::native_inspection(recovery_diagnostics::NativeInspection::Inspected);
            recovery_diagnostics::cleanup(recovery_diagnostics::Cleanup::Confirmed);
            if !startup_stopped {
                legacy_current_seed = Some(seed);
                startup_recovery_inspection = None;
                diagnostics::write_line(format_args!(
                    "switchd-rs: legacy_current_verified; current baseline only; historical_recovery_confirmed=false"));
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
                StartupInspectionFailure::LegacyCurrent(_) | StartupInspectionFailure::Inspection(_) => recovery_diagnostics::native_inspection(
                    recovery_diagnostics::NativeInspection::Failed,
                ),
                StartupInspectionFailure::Reconciliation(_) => recovery_diagnostics::reconciliation(
                    recovery_diagnostics::Reconciliation::Failed,
                ),
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
            native_module_producer_id.clone(), initial_sms_adopt, module_recovery.clone(), seed),
        None => usb_at::Owner::new_with_adopt_and_recovery(
            native_module_producer_id.clone(), initial_sms_adopt, module_recovery.clone()),
    }.with_profile_notes(profile_notes::Store::new(multistandby_store.data_root()));
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
    if legacy_current_startup { module_radio.preserve_current_startup_cfun(); }
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
            return report.finish();
        }
    };
    let call_mode = if module_recovery.is_quarantined() {
        None
    } else {
        Some(state.call_mode())
    };
    let forwarding_admission_task = tokio::spawn(forwarding_admission::run(
        state.multistandby_store.clone(),
        state.forwarding_handoff_store.clone(),
        forwarding_admission,
        state.sms_forwarding.clone(),
    ));
    let device_sms_forwarding_task = tokio::spawn(device_sms_forwarding::run(
        state.sms_inbox.clone(),
        state.forwarding_handoff_store.clone(),
        device_sms_forwarding_store,
        state.sms_forwarding.clone(),
    ));
    let (multistandby_recovery_shutdown, multistandby_recovery_stop) =
        tokio::sync::watch::channel(false);
    let multistandby_recovery_task = tokio::spawn(multistandby_recovery::run(
        state.multistandby_store.clone(),
        native_multi.clone(),
        multistandby_recovery_stop,
    ));
    let (multistandby_maintenance_shutdown, multistandby_maintenance_stop) =
        tokio::sync::watch::channel(false);
    let multistandby_maintenance_task = tokio::spawn(multistandby_maintenance::run(
        native_multi.clone(),
        multistandby_maintenance_stop,
    ));
    let (multistandby_retry_shutdown, multistandby_retry_stop) = tokio::sync::watch::channel(false);
    let multistandby_retry_task = tokio::spawn(multistandby_retry::run(
        state.multi_runtime_report.clone(),
        native_multi.clone(),
        multistandby_retry_stop,
    ));
    let reader_presence_service = reader_service.clone();
    let (ble_presence_stop, ble_presence_stopping) = tokio::sync::watch::channel(false);
    let ble_presence_task = tokio::spawn(reader_lifecycle::run_ble_presence_observer(
        reader_service.clone(),
        ble_presence_stopping,
    ));
    let reader_hotplug_task = tokio::spawn(reader_hotplug::run(state.clone()));
    let reader_presence_task = tokio::spawn(
        reader_pcsc::PresenceMonitor::with_native_handoff(
            state.reader_pcsc.clone(),
            Arc::new(move |binding| {
                let service = reader_presence_service.clone();
                Box::pin(async move {
                    let snapshot = service.snapshot();
                    if snapshot.pcsc_session.as_ref() != Some(&binding) {
                        return Ok(());
                    }
                    match service
                        .confirmed_loss(
                            &binding.reader.clone(),
                            snapshot.selection.generation,
                            Some(binding),
                        )
                        .await
                    {
                        Ok(true) => Ok(()),
                        Ok(false) => Err("reader lifecycle is still pending".into()),
                        Err(error) => Err(error.message),
                    }
                })
            }),
        )
        .run(),
    );

    let module_startup = if module_recovery.is_quarantined() {
        crate::diagnostics::write_line(format_args!(
            "switchd-rs: {}: module isolated; historical recovery remains unconfirmed",
            module_recovery::CODE
        ));
        None
    } else {
        state.module_radio.start();
        state.usb_sms_receive.start();
        Some(if legacy_current_startup {
            module_startup::Service::start_verified(state.modem_actor.clone(), state.usb_at.clone(),
                state.module_radio.clone(), state.sms_inbox.clone(), native_module_producer_id)
        } else { module_startup::Service::start_on_port(
            state.modem_actor.clone(),
            state.usb_at.clone(),
            state.module_radio.clone(),
            state.sms_inbox.clone(),
            native_module_producer_id,
            selected_port,
        ) })
    };
    availability.mark_ready();
    let mut connection_pools = connection_pools::Driver::default();
    let mut eap_exchange_cleanup = interval(Duration::from_secs(30));
    let mut parent_monitor = interval(Duration::from_millis(500));

    // Optional QMI startup cannot gate public readiness or parent/signal
    // observation. Retain its task until shutdown, even if startup is slow.
    let qmi_owner = state.qmi_enhancement.clone();
    if legacy_current_startup {
        support_diagnostics::module::startup(
            support_diagnostics::module::Stage::TransportClaim,
            support_diagnostics::module::ResultKind::Deferred,
            support_diagnostics::module::Code::PolicyUnavailable,
            support_diagnostics::module::Transport::Unknown);
        diagnostics::write_line(format_args!(
            "switchd-rs: legacy_current_auto_qmi_deferred; independent device selection requires explicit operation"));
    }
    let mut qmi_startup = if module_recovery.is_quarantined() || legacy_current_startup {
        None
    } else {
        Some(tokio::task::spawn_blocking(move || {
            qmi_enhancement::NativeQmiRetryLifecycle::start(qmi_config, qmi_owner)
        }))
    };
    let mut qmi_lifecycle = None;
    let mut qmi_startup_failure = None;

    crate::diagnostics::write_line(format_args!(
        "switchd-rs backend={} app={} build={} source={} stage={} listen={}",
        BACKEND_VERSION,
        APP_RELEASE_VERSION,
        APP_BUILD_NUMBER,
        &SOURCE_DIGEST[..SOURCE_DIGEST.len().min(12)],
        MigrationStage::MessageArchiveCutover.as_str(),
        socket_path.display()
    ));

    let mut exit_report = shutdown::Report::default();
    if let Some(error) = startup_native_cleanup_error.take() {
        exit_report.record("startup native cleanup", error);
    }
    loop {
        tokio::select! {
            event = connection_pools.next(&listener, &callback_listener, &internal_listener) => {
                match event {
                    connection_pools::Event::Accepted(pool, accepted) => {
                        let Some(stream) = shutdown::accepted(&mut exit_report, pool.accept_owner(), accepted) else {
                            break;
                        };
                        let connection_state = state.clone();
                        connection_pools.spawn(pool, async move {
                            let result = match pool {
                                connection_pools::Pool::Public | connection_pools::Pool::Callback => {
                                    serve_connection(stream, connection_state).await
                                }
                                connection_pools::Pool::Internal => {
                                    serve_internal_connection(stream, connection_state).await
                                }
                            };
                            if let Err(error) = result
                                && !benign_connection_error_message(&error.to_string())
                            {
                                let surface = match pool {
                                    connection_pools::Pool::Public => "connection",
                                    connection_pools::Pool::Callback => "callback",
                                    connection_pools::Pool::Internal => "internal store",
                                };
                                crate::diagnostics::write_line(format_args!(
                                    "switchd-rs {surface}: {error}"
                                ));
                            }
                        });
                    }
                    connection_pools::Event::Completed(pool, completed) => {
                        if let Err(error) = completed {
                            crate::diagnostics::write_line(format_args!(
                                "switchd-rs {}: {error}", pool.task_owner()
                            ));
                        }
                    }
                }
            }
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
            _ = broken_pipe.recv() => {},
            _ = eap_exchange_cleanup.tick() => state.eap_exchanges.prune(),
            result = async { qmi_startup.as_mut().expect("guarded QMI startup").await }, if qmi_startup.is_some() => {
                qmi_startup = None;
                match result {
                    Ok(lifecycle) => {
                        if let Some(error) = lifecycle.startup_error() {
                            crate::diagnostics::write_line(format_args!("switchd-rs: optional QMI enhancement unavailable: {error}"));
                        }
                        qmi_lifecycle = Some(lifecycle);
                    }
                    Err(error) => {
                        crate::diagnostics::write_line(format_args!("switchd-rs: QMI startup worker failed: {error}"));
                        qmi_startup_failure = Some(error.to_string());
                    },
                }
            }
            _ = parent_monitor.tick() => {
                let current_parent = getppid();
                if parent_process_changed(initial_parent, current_parent) {
                    break;
                }
            }
        }
    }

    support_diagnostics::lifecycle("shutdown_started");
    // Close coordinator producers before request observers can be aborted.
    state.device_send_store.send_owner().begin_shutdown();
    let network_owners = state.network_traffic.recovery.begin_shutdown();
    diagnostics::write_line(format_args!(
        "switchd-rs: network recovery admission closed: {:?}",
        network_owners
    ));
    if let Some(startup) = &module_startup {
        startup.begin_shutdown();
    }
    exit_report.check(
        "native admission",
        state.native_vowifi_lifecycle.begin_shutdown(),
    );
    reader_esim_download::begin_shutdown();
    if let Some(service) = &module_call {
        service.begin_shutdown();
    }
    if let Some(service) = &call_mode {
        service.begin_shutdown();
    }
    reader_service.begin_shutdown();
    ble_presence_stop.send_replace(true);
    state.module_radio.begin_shutdown();
    native_multi.begin_shutdown();
    multistandby_recovery_shutdown.send_replace(true);
    multistandby_maintenance_shutdown.send_replace(true);
    multistandby_retry_shutdown.send_replace(true);

    // The single callback driver surrounds the whole coordinator. An unknown
    // module receipt cannot prevent independent reader/QMI branches from
    // draining, nor can a listener fault discard the retained cleanup future.
    let shared = async {
        let mut report = shutdown::Report::default();
        let (maintenance, retry, recovery) = tokio::join!(
            multistandby_maintenance_task,
            multistandby_retry_task,
            multistandby_recovery_task
        );
        report.check("N-wait maintenance join", maintenance);
        report.check("N-wait retry join", retry);
        report.check("N-wait recovery join", recovery);
        reader_hotplug_task.abort();
        shutdown_observer_join("reader hotplug", reader_hotplug_task, &mut report).await;
        reader_presence_task.abort();
        shutdown_observer_join("reader presence", reader_presence_task, &mut report).await;
        connection_pools.abort_all();
        connection_pools.drain(&mut report).await;
        forwarding_admission_task.abort();
        shutdown_observer_join(
            "forwarding admission",
            forwarding_admission_task,
            &mut report,
        )
        .await;
        device_sms_forwarding_task.abort();
        shutdown_observer_join("device forwarding", device_sms_forwarding_task, &mut report).await;
        report.check(
            "SMS forwarding worker",
            state.sms_forwarding.shutdown().await,
        );
        // HTTP joins do not join detached send/finalize trees. Keep native,
        // radio and reader dependencies live until every accepted SMS owner
        // and its output has retired. Unknown ownership never authorizes close.
        report.extend(
            shutdown::once(
                "device SMS producer owners",
                state.device_send_store.send_owner().finish_pending().await,
            )
            .await,
        );
        // This observer may register retained cleanup even while admission is
        // stopping. Stop and join it BEFORE the final reader jobs drain.
        report.extend(
            shutdown::once(
                "BLE loss observer join",
                ble_presence_task.await.map_err(|error| error.to_string()),
            )
            .await,
        );

        // Keep card admission and all transports live until Profile restore and
        // ordinary/module native session cleanup have confirmed completion.
        report.check(
            "N-wait sessions",
            wait_for_native_cleanup(
                || async { native_multi.shutdown().await.map_err(|error| error.code) },
                Duration::from_secs(1),
            )
            .await,
        );
        state.multistandby_operations.finish_dispatches().await;
        state.multistandby_card().begin_shutdown();
        state.reader_pcsc.begin_shutdown();
        report.check(
            "native sessions",
            wait_for_native_cleanup(
                || {
                    state
                        .native_vowifi_lifecycle
                        .finish_shutdown(tokio::time::Instant::now() + Duration::from_secs(180))
                },
                Duration::from_secs(1),
            )
            .await,
        );
        report
    };
    let module = async {
        let mut report = shutdown::Report::default();
        // Retained network runners may be sleeping or reading with zero archive
        // jobs. Drain them before module transport teardown; reader/QMI branches
        // remain independently live while this module branch waits.
        report.extend(
            shutdown::once(
                "network recovery owners",
                state.network_traffic.recovery.finish_pending().await,
            )
            .await,
        );
        // Drain every previously accepted eSIM owner before asking the module
        // radio for its final receipt; otherwise an in-flight LPA reservation
        // could be cached as permanently unconfirmed.
        report.check(
            "eSIM operation drain",
            reader_esim_download::wait_for_shutdown().await,
        );
        // This receiver owns accepted USB commands. A failed join cannot
        // authorize closing their transport, but need not fence reader/QMI.
        report.extend(
            shutdown::once(
                "USB SMS receiver",
                state
                    .usb_sms_receive
                    .shutdown()
                    .await
                    .map_err(|error| error.to_string()),
            )
            .await,
        );
        // Only omit finalizers that were never constructed under the immutable
        // startup fence. Unexpectedly constructed owners still retain their
        // original conservative finalizers. Radio/USB/serial owners below must
        // independently acknowledge this process has no accepted/unknown work.
        let module_call = module_call.clone().or_else(|| {
            state
                .module_call
                .get()
                .and_then(|value| value.as_ref().ok())
                .cloned()
        });
        if let Some(service) = module_call {
            report.extend(
                shutdown::confirmed("module call", || service.finish_shutdown_receipt()).await,
            );
        }
        let call_mode = call_mode.clone().or_else(|| state.call_mode.get().cloned());
        if let Some(service) = call_mode {
            report.extend(
                shutdown::confirmed("call mode", || service.finish_shutdown_receipt()).await,
            );
        }
        report.extend(
            shutdown::confirmed("module radio", || {
                state.module_radio.finish_shutdown_receipt()
            })
            .await,
        );
        if let Some(startup) = &module_startup {
            report.extend(
                shutdown::once(
                    "module startup",
                    startup
                        .shutdown()
                        .await
                        .map_err(|error| error.code.to_owned()),
                )
                .await,
            );
        }
        report.extend(
            shutdown::once(
                "USB AT",
                state
                    .usb_at
                    .shutdown()
                    .await
                    .map_err(|error| error.to_string()),
            )
            .await,
        );
        let modem = state.modem_actor.clone();
        let result = tokio::task::spawn_blocking(move || modem.shutdown())
            .await
            .map_err(|error| format!("close worker: {error}"))
            .and_then(|result| result.map_err(|error| error.code().to_owned()));
        report.extend(shutdown::once("modem", result).await);
        // Archive receiver has no live transport after confirmed physical close.
        report.check(
            "serial SMS archive",
            state
                .modem_actor
                .finish_sms_receiver()
                .await
                .map_err(|error| error.code()),
        );
        report
    };
    let reader = async {
        let mut report = shutdown::Report::default();
        state.multistandby_card().finish_shutdown().await;
        report.extend(
            shutdown::confirmed("reader lifecycle", || async {
                reader_service.finish_shutdown().await.map(|()| Vec::new())
            })
            .await,
        );
        // PC/SC retries are owner-aware: quarantined physical close is checked
        // again without publishing a new reader generation.
        report.extend(
            shutdown::confirmed("PC/SC", || async {
                state
                    .reader_pcsc
                    .shutdown()
                    .await
                    .map(|()| Vec::new())
                    .map_err(|error| error.to_string())
            })
            .await,
        );
        report
    };
    let qmi = async move {
        if let Some(startup) = qmi_startup {
            match startup.await {
                Ok(lifecycle) => qmi_lifecycle = Some(lifecycle),
                Err(error) => qmi_startup_failure = Some(error.to_string()),
            }
        }
        if let Some(error) = qmi_startup_failure {
            return shutdown::once("QMI startup worker", Err(error)).await;
        }
        let result = tokio::task::spawn_blocking(move || {
            qmi_lifecycle
                .as_mut()
                .map_or(Ok(()), |lifecycle| lifecycle.shutdown())
        })
        .await
        .map_err(|error| format!("shutdown worker: {error}"))
        .and_then(|result| result);
        shutdown::once("QMI", result).await
    };
    let cleanup_gate = shutdown::CleanupGate::default();
    let archive = async {
        cleanup_gate.close().await;
        let mut report = shutdown::Report::default();
        report.check("archive workers", archive_io::finish_pending().await);
        let call_facts = state.call_facts.clone();
        report.check(
            "call history drain",
            archive_io::run(move || call_facts.finish_pending()).await,
        );
        report
    };
    let (mut report, listener_report) = drive_shutdown_callbacks(
        shutdown::coordinate(exit_report, shared, module, reader, qmi, archive),
        &callback_listener,
        &internal_listener,
        |stream| serve_shutdown_callback_with_gate(stream, state.clone(), cleanup_gate.clone()),
        |stream| serve_internal_shutdown_with_gate(stream, state.clone(), cleanup_gate.clone()),
    )
    .await;
    report.extend(listener_report);
    if let Some(inspection) = startup_recovery_inspection {
        if let Err(error) = inspection.verify(&module_recovery) {
            // A changed read-only observation is not a physical close receipt
            // or a reason to replay cleanup. Reader shutdown remains separate.
            diagnostics::write_line(format_args!(
                "switchd-rs: {}; startup metadata changed, recovery unconfirmed",
                error.code()
            ));
        }
    }
    if let Some(inspection) = startup_native_inspection {
        if inspection.verify(&module_recovery).is_err() {
            // Resources ended before startup continued. Changed evidence only
            // invalidates this read interval, never authorizes recovery replay.
            diagnostics::write_line(format_args!(
                "switchd-rs: startup_native_observation_changed; recovery unconfirmed"
            ));
        }
    }
    report.finish()
}

async fn await_startup_native<T>(
    operation: impl std::future::Future<Output = T>,
    cancelled: Arc<AtomicBool>,
    stop: impl std::future::Future<Output = ()>,
) -> (T, bool) {
    tokio::pin!(operation);
    tokio::pin!(stop);
    tokio::select! {
        biased;
        _ = &mut stop => {
            cancelled.store(true, std::sync::atomic::Ordering::Release);
            (operation.await, true)
        }
        result = &mut operation => (result, false),
    }
}

enum StartupInspection {
    LegacyCurrent(startup_recovery::legacy_current::Seed),
    ReadOnly(Option<startup_recovery::execution::PhysicalInspection>),
    Reconciled(startup_recovery::reconcile::CompletedReconciliation),
}

#[derive(Debug)]
enum StartupInspectionFailure {
    LegacyCurrent(startup_recovery::legacy_current::Failure),
    Inspection(startup_recovery::execution::PhysicalFailure),
    Reconciliation(startup_recovery::reconcile::PhysicalFailure),
}
impl StartupInspectionFailure {
    fn cleanup_unconfirmed(&self) -> bool {
        match self {
            Self::LegacyCurrent(error) => error.cleanup_unconfirmed(),
            Self::Inspection(error) => error.cleanup_unconfirmed(),
            Self::Reconciliation(error) => error.cleanup_unconfirmed(),
        }
    }
    fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::LegacyCurrent(error) => error.code(),
            Self::Inspection(_) => "startup_native_inspection_unavailable",
            Self::Reconciliation(_) => "startup_reconciliation_unavailable",
        }
    }
}
impl std::fmt::Display for StartupInspectionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LegacyCurrent(error) => f.write_str(error.code()),
            Self::Inspection(error) => error.fmt(f),
            Self::Reconciliation(error) => error.fmt(f),
        }
    }
}

async fn shutdown_observer_join<T>(
    owner: &str,
    task: tokio::task::JoinHandle<T>,
    report: &mut shutdown::Report,
) {
    if let Err(error) = task.await
        && !error.is_cancelled()
    {
        report.record(owner, error);
    }
}

async fn shutdown_connection_joins(
    owner: &str,
    tasks: &mut JoinSet<()>,
    report: &mut shutdown::Report,
) {
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
        {
            report.record(owner, error);
        }
    }
}

fn shutdown_rejection<B>(
    request: &Request<B>,
    token: Option<&str>,
) -> Option<Response<Full<Bytes>>> {
    if token.is_none() || !token_matches(request, token) {
        return Some(json_response(
            StatusCode::UNAUTHORIZED,
            &ErrorResponse {
                error: "需要 Switch App 本机凭证",
                code: "authentication_required",
                retryable: false,
            },
        ));
    }
    if request.method() == Method::POST
        && request.uri().path() == "/api/v2/vowifi/eap-exchange/release"
    {
        return None;
    }
    Some(json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &ErrorResponse {
            error: "Switch 正在退出，未提交新操作",
            code: "backend_stopping",
            retryable: false,
        },
    ))
}

/// Failure is not proof of release. Preserve bridge ownership and callback
/// service until the lifecycle confirms cleanup, including after lock/task errors.
async fn wait_for_native_cleanup<F, Fut>(
    mut finish: F,
    retry: Duration,
) -> Result<(), BackendFailure>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), &'static str>>,
{
    let mut report = shutdown::Report::default();
    loop {
        match finish().await {
            Ok(()) => return report.finish(),
            Err(reason) => {
                report.record("native cleanup", reason);
                tokio::time::sleep(retry).await;
            }
        }
    }
}

/// Keep only caller-selected cleanup handlers alive while an owner drains.
async fn drive_shutdown_callbacks<T, S, C, I, CF, IF>(
    shutdown: S,
    callback: &UnixListener,
    internal: &UnixListener,
    callback_handler: C,
    internal_handler: I,
) -> (T, shutdown::Report)
where
    S: std::future::Future<Output = T>,
    C: Fn(UnixStream) -> CF,
    I: Fn(UnixStream) -> IF,
    CF: std::future::Future<Output = ()> + Send + 'static,
    IF: std::future::Future<Output = ()> + Send + 'static,
{
    shutdown::drive_callbacks(
        shutdown,
        || async { callback.accept().await.map(|(stream, _)| stream) },
        || async { internal.accept().await.map(|(stream, _)| stream) },
        callback_handler,
        internal_handler,
        SHUTDOWN_CONNECTION_LIMIT,
    )
    .await
}

#[cfg(test)]
async fn serve_shutdown_callback_connection(stream: UnixStream, state: ServerState) {
    serve_shutdown_callback_with_gate(stream, state, shutdown::CleanupGate::default()).await;
}

async fn serve_shutdown_callback_with_gate(
    stream: UnixStream,
    state: ServerState,
    gate: shutdown::CleanupGate,
) {
    let _ = http_server::serve_full_response(stream, move |request| {
        let rejection = shutdown_rejection(&request, state.app_token.as_deref());
        let state = state.clone();
        let gate = gate.clone();
        async move {
            if let Some(response) = rejection {
                return Ok::<_, Infallible>(response);
            }
            let _permit = match gate.enter().await {
                Some(permit) => permit,
                None => return Ok::<_, Infallible>(cleanup_finished_response()),
            };
            let task = tokio::spawn(async move {
                let _permit = _permit;
                eap_identity::handle(request, &state.eap_exchanges).await
            });
            Ok::<_, Infallible>(
                task.await
                    .unwrap_or_else(|_| cleanup_worker_failed_response()),
            )
        }
    })
    .await;
}

#[cfg(test)]
async fn serve_internal_shutdown_connection(stream: UnixStream, state: ServerState) {
    serve_internal_shutdown_with_gate(stream, state, shutdown::CleanupGate::default()).await;
}

async fn serve_internal_shutdown_with_gate(
    stream: UnixStream,
    state: ServerState,
    gate: shutdown::CleanupGate,
) {
    let _ = http_server::serve_full_response(stream, move |request| {
        let state = state.clone();
        let gate = gate.clone();
        async move {
            // Keep route classification and authentication in the common handler.
            // Its cleanup permit belongs to this retained task, not the HTTP waiter.
            let task = tokio::spawn(async move {
                handle_internal_request(request, &state, true, Some(&gate)).await
            });
            Ok::<_, Infallible>(
                task.await
                    .unwrap_or_else(|_| cleanup_worker_failed_response()),
            )
        }
    })
    .await;
}

fn cleanup_worker_failed_response() -> Response<Full<Bytes>> {
    json_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        &ErrorResponse {
            error: "退出清理请求执行失败",
            code: "cleanup_worker_failed",
            retryable: false,
        },
    )
}

fn cleanup_finished_response() -> Response<Full<Bytes>> {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        &ErrorResponse {
            error: "Switch 正在退出，清理操作已完成",
            code: "backend_stopping",
            retryable: false,
        },
    )
}

fn native_module_managed(state: &ServerState) -> bool {
    state.usb_at.native_managed() || state.modem_actor.native_serial_managed()
}

/// Go has no physical module capability after native selection, including
/// during shutdown. The same policy applies to both private RPC entry points.
fn managed_module_rpc_rejection(path: &str, state: &ServerState) -> Option<Response<Full<Bytes>>> {
    // A pure, bounded DTO projection has no hardware capability. It retains
    // the normal private-token checks and does not enter the serial actor.
    if path == modem_actor::PROJECT_CALL_STATE_PATH {
        return None;
    }
    if state.availability.module_recovery().is_quarantined() && module_recovery::owns_internal(path)
    {
        return Some(module_recovery::rejection());
    }
    if native_module_managed(state)
        && [
            usb_at::PREFIX,
            module_radio::PREFIX,
            usb_sms_receive::INTERNAL_PREFIX,
            modem_actor::PREFIX,
            "/api/internal/call-mode/",
        ]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        Some(json_response(
            StatusCode::FORBIDDEN,
            &json!({
                "code":"native_module_owned", "error":"模块硬件与会话由 Rust 独立管理"
            }),
        ))
    } else {
        None
    }
}

async fn handle_internal_request(
    request: Request<Incoming>,
    state: &ServerState,
    stopping: bool,
    cleanup_gate: Option<&shutdown::CleanupGate>,
) -> Response<Full<Bytes>> {
    let path = request.uri().path();
    let is_ordinary_vowifi_admission = path == ordinary_vowifi_admission::PATH;
    let is_reader_business = path == reader_reservation::BUSINESS_PATH;
    let is_reader_lifecycle = path == reader_reservation::LIFECYCLE_PATH;
    let is_reader_native_owner = path == reader_reservation::NATIVE_OWNER_PATH;
    let is_reader_device_identity = path == reader_device_identity::INTERNAL_RESOLVE_PATH;
    let is_device_send = path.starts_with(device_send_store::PREFIX);
    let is_multistandby_inventory = path == multistandby_native_inventory::PATH;
    let is_sms_archive = path.starts_with(message_archive::INTERNAL_PREFIX);
    let is_sms_device_selection = path == sms_device_service::SELECT_PATH;
    let is_call_archive = path.starts_with(call_archive::INTERNAL_PREFIX);
    let is_profile_notes = path.starts_with(profile_notes::INTERNAL_PREFIX);
    let is_safe_events = path.starts_with(safe_events::INTERNAL_PREFIX);
    let is_call_mode_backup = path == call_mode_backups::INTERNAL_CREATE_PATH
        || path == call_mode_backups::INTERNAL_RESTORE_DATA_PATH
        || path == call_mode_backups::INTERNAL_VALIDATE_RESTORE_PATH;
    let is_call_mode_runtime = path == "/api/internal/call-mode/runtime/verify";
    let is_call_mode_status = path == call_mode_status::INTERNAL_PROJECT_PATH;
    let is_call_mode_lifecycle = path.starts_with(call_mode_lifecycle::PREFIX);
    let is_call_mode_adb = matches!(
        path,
        call_mode_adb::INTERNAL_PROBE_PATH
            | call_mode_adb::INTERNAL_DEPLOY_PATH
            | call_mode_adb::INTERNAL_ROUTE_START_PATH
            | call_mode_adb::INTERNAL_ROUTE_STOP_PATH
            | call_mode_adb::INTERNAL_ROUTE_STATUS_PATH
            | call_mode_adb::INTERNAL_RUNTIME_STATUS_PATH
            | call_mode_adb::INTERNAL_RUNTIME_GENERATION_PATH
    );
    let is_modem = path.starts_with(modem_actor::PREFIX);
    let is_qmi = path.starts_with(qmi_enhancement::PREFIX);
    let is_usb_sms = path.starts_with(usb_sms_receive::INTERNAL_PREFIX);
    let is_usb_at = path.starts_with(usb_at::PREFIX);
    let is_module_radio = path.starts_with(module_radio::PREFIX);
    let is_ims_sms_transaction = path == ims_sms_transactions::INTERNAL_PATH;
    if !is_ordinary_vowifi_admission
        && !is_reader_native_owner
        && !is_reader_lifecycle
        && !is_reader_business
        && !is_reader_device_identity
        && !is_device_send
        && !is_multistandby_inventory
        && !is_sms_archive
        && !is_sms_device_selection
        && !is_call_archive
        && !is_profile_notes
        && !is_safe_events
        && !is_call_mode_backup
        && !is_call_mode_runtime
        && !is_call_mode_lifecycle
        && !is_call_mode_status
        && !is_call_mode_adb
        && !is_modem
        && !is_qmi
        && !is_usb_sms
        && !is_usb_at
        && !is_module_radio
        && !is_ims_sms_transaction
    {
        return json_response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({"code":"route_not_found","error":"route not found"}),
        );
    }
    if !multistandby_store::internal_authenticated(&request, &state.internal_token) {
        return json_response(
            StatusCode::UNAUTHORIZED,
            &serde_json::json!({"code":"internal_authentication_required","error":"需要后端内部凭证"}),
        );
    }
    if let Some(response) = managed_module_rpc_rejection(path, &state) {
        return response;
    }
    // USB owner verifies producer, lease purpose and its narrow CFUN recovery
    // allowlist during shutdown; ordinary SIM/SMS operations remain rejected.
    let cleanup_allowed = is_ordinary_vowifi_admission
        || is_call_mode_lifecycle
        || is_usb_at
        || is_reader_native_owner
        || is_reader_lifecycle
        || is_reader_business
        || is_module_radio
        || path == "/api/internal/device-send/finish"
        || path == "/api/internal/call-archive/ingest"
        || path == "/api/internal/call-archive/observe"
        || path == call_facts::HISTORY_ERROR_PATH
        || path == "/api/internal/modem/close"
        || path == "/api/internal/qmi/close"
        || is_ims_sms_transaction
        || is_safe_events;
    let cleanup_allowed = cleanup_allowed
        || path == call_mode_adb::INTERNAL_ROUTE_STOP_PATH
        || path == call_mode_adb::INTERNAL_ROUTE_STATUS_PATH;
    if stopping && !cleanup_allowed {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &ErrorResponse {
                error: "Switch 正在退出，未提交新操作",
                code: "backend_stopping",
                retryable: false,
            },
        );
    }
    // Retired routes still return 404; active routes authenticate before the gate.
    // Once accepted, the retained handler owns admission through its last effect.
    let _cleanup_permit = match cleanup_gate {
        Some(gate) => match gate.enter().await {
            Some(permit) => Some(permit),
            None => return cleanup_finished_response(),
        },
        None => None,
    };
    let _body_permit = match request_body_budget::try_reserve(&request) {
        Ok(permit) => permit,
        Err(()) => return request_body_budget::capacity_response(),
    };
    if is_ordinary_vowifi_admission {
        ordinary_vowifi_admission::handle(
            request,
            state.native_vowifi_lifecycle.ordinary_admission(),
            stopping,
        )
        .await
    } else if is_reader_native_owner {
        reader_reservation::handle_native_owner(request, stopping, &state.reader_gate).await
    } else if is_reader_lifecycle {
        reader_reservation::handle_lifecycle(request, stopping, &state.reader_pcsc).await
    } else if is_reader_business {
        reader_reservation::handle_business(request, stopping, state.multistandby_store.status())
            .await
    } else if is_reader_device_identity {
        reader_device_identity::handle_internal(request, state.multistandby_store.data_root()).await
    } else if is_device_send {
        device_send_store::handle(request, state.device_send_store.clone()).await
    } else if is_sms_archive {
        message_archive::handle_internal(request).await
    } else if is_sms_device_selection {
        sms_device_service::handle_internal_for_state(request, &state).await
    } else if is_call_archive {
        if matches!(
            path,
            call_facts::OBSERVE_PATH | call_facts::HISTORY_ERROR_PATH
        ) {
            call_facts::handle(request, state.call_facts.clone(), stopping).await
        } else {
            call_archive::handle_internal(request, state.call_archive.clone()).await
        }
    } else if is_profile_notes {
        profile_notes::handle_internal(
            request,
            profile_notes::Store::new(state.multistandby_store.data_root()),
        )
        .await
    } else if is_safe_events {
        safe_events::handle_internal(request, safe_events::global()).await
    } else if is_call_mode_backup {
        call_mode_backups::handle_internal_with_proof(
            request,
            state.multistandby_store.data_root(),
            state
                .availability
                .module_recovery()
                .root_proof_for(state.multistandby_store.data_root()),
        )
        .await
    } else if is_call_mode_runtime {
        call_mode_runtime::handle_internal(request, state.multistandby_store.data_root()).await
    } else if is_call_mode_lifecycle {
        state.call_mode().handle_internal(request).await
    } else if is_call_mode_status {
        call_mode_status::handle_internal(request).await
    } else if is_call_mode_adb {
        call_mode_adb::handle_internal(
            request,
            state.multistandby_store.data_root(),
            state.sms_inbox.clone(),
            state.usb_at.clone(),
            state.module_radio.clone(),
        )
        .await
    } else if is_module_radio {
        module_radio::handle_internal(request, state.module_radio.clone(), stopping).await
    } else if is_usb_at {
        usb_at::handle(request, state.usb_at.clone(), stopping).await
    } else if is_usb_sms {
        usb_sms_receive::handle(request, state.usb_sms_receive.clone(), stopping).await
    } else if is_modem {
        modem_actor::handle(request, state.modem_actor.clone()).await
    } else if is_qmi {
        qmi_enhancement::handle(request, state.qmi_enhancement.clone()).await
    } else if is_ims_sms_transaction {
        ims_sms_transactions::handle(
            request,
            state.ims_sms_transactions.clone(),
            state.multistandby_store.clone(),
            stopping,
        )
        .await
    } else if is_multistandby_inventory {
        multistandby_native_inventory::handle(
            request,
            state.reader_pcsc.clone(),
            state.reader_gate.clone(),
            state.multistandby_card(),
        )
        .await
    } else {
        json_response(
            StatusCode::NOT_FOUND,
            &json!({"code":"route_not_found","error":"route not found"}),
        )
    }
}

fn parent_process_changed(initial: Pid, current: Pid) -> bool {
    current.as_raw() <= 1 || current != initial
}

fn benign_connection_error_message(message: &str) -> bool {
    matches!(
        message,
        "error shutting down connection" | "error writing a body to connection"
    )
}

async fn serve_connection(stream: UnixStream, state: ServerState) -> Result<(), hyper::Error> {
    http_server::serve_full_response(stream, move |request| {
        handle_request(request, state.clone())
    })
    .await
}

async fn serve_internal_connection(
    stream: UnixStream,
    state: ServerState,
) -> Result<(), hyper::Error> {
    http_server::serve_full_response(
            stream,
            move |request| {
                let state = state.clone();
                async move {
                    Ok::<_, Infallible>(handle_internal_request(request, &state, false, None).await)
                }
            },
        )
        .await
}

async fn handle_request(
    mut request: Request<Incoming>,
    state: ServerState,
) -> Result<Response<Full<Bytes>>, Infallible> {
    // Match the HTTP parser boundary of the retained Go baseline: malformed
    // escapes in the path fail before routing or credentials, while the raw
    // query remains opaque here.
    if let Some(response) = public_http::invalid_path_response(request.uri()) {
        return Ok(response);
    }
    // Only reviewed public literal segments may acquire an equivalent spelling.
    // Raw canonical redirects/private paths remain untouched; HEAD, auth and
    // every later admission/Gate check see this same once-resolved public URI.
    if let Some(uri) = public_http::public_alias_uri(request.uri()) {
        *request.uri_mut() = uri;
    }
    let head = request.method() == Method::HEAD;
    let native_get = head && public_http::head_uses_get(request.uri().path());
    let stop_control = matches!(
        request.uri().path().trim_end_matches('/'),
        "/api/reader/vowifi/stop-control" | "/api/vowifi/stop-control"
    );
    let diagnostic_span = if token_matches(&request, state.app_token.as_deref()) {
        support_diagnostics::api_span(
            request.method().as_str(),
            request.uri().path(),
            request
                .headers()
                .get("x-switch-diagnostic-id")
                .and_then(|value| value.to_str().ok()),
        )
    } else {
        None
    };
    let support_snapshot = request.uri().path() == support_diagnostics::PATH;
    let mut response = handle_request_inner(request, state).await?;
    if let Some(span) = diagnostic_span {
        span.finish(response.status().as_u16());
    }
    if support_snapshot {
        response.headers_mut().insert(
            hyper::header::CACHE_CONTROL,
            hyper::header::HeaderValue::from_static("no-store"),
        );
    }
    if stop_control {
        // Includes authentication, method, admission and no-retained rejections.
        response.headers_mut().insert(
            hyper::header::CACHE_CONTROL,
            hyper::header::HeaderValue::from_static("no-store"),
        );
    }
    Ok(if head {
        public_http::without_head_body(response, native_get)
    } else {
        response
    })
}

async fn handle_request_inner(
    mut request: Request<Incoming>,
    state: ServerState,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = request.uri().path();
    // The public socket and its .rs sibling share this handler. Private RPCs
    // are admitted only by the distinct .in listener, regardless of token.
    if path == "/api/internal" || path.starts_with("/api/internal/") {
        return Ok(json_response(
            StatusCode::NOT_FOUND,
            &json!({"code":"route_not_found","error":"route not found"}),
        ));
    }
    if let Some(response) = public_http::canonical_path_redirect(request.method(), request.uri()) {
        return Ok(response);
    }
    if !token_matches(&request, state.app_token.as_deref()) {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            &ErrorResponse {
                error: "需要 Switch App 本机凭证",
                code: "authentication_required",
                retryable: false,
            },
        ));
    }
    // A Go-only route may reject HEAD itself.
    match public_http::admission(request.method(), request.uri().path()) {
        public_http::Admission::HeadAsGet => *request.method_mut() = Method::GET,
        public_http::Admission::MethodNotAllowed(allowed) => {
            return Ok(public_http::method_not_allowed(allowed));
        }
        public_http::Admission::Allowed | public_http::Admission::Unknown => {}
    }
    let _body_permit = match request_body_budget::try_reserve(&request) {
        Ok(permit) => permit,
        Err(()) => return Ok(request_body_budget::capacity_response()),
    };

    if state.availability.module_recovery().is_quarantined() {
        if module_recovery::owns_public(request.method(), request.uri().path()) {
            return Ok(module_recovery::rejection());
        }
        if request.method() == Method::GET {
            match request.uri().path() {
                "/api/call-mode/status" => {
                    return Ok(json_response(
                        StatusCode::OK,
                        &module_recovery::call_mode_status(state.multistandby_store.data_root()),
                    ));
                }
                "/api/voice/enabled" => {
                    return Ok(voice_enabled::quarantined_status(
                        state.multistandby_store.data_root(),
                    ));
                }
                _ => {}
            }
        }
    }

    if multistandby_operations::owns_public(request.method(), request.uri().path()) {
        if let Some(response) = sms_store_rejection(request.uri().path(), &state) {
            return Ok(response);
        }
        debug_assert_eq!(
            ownership::route_owner(request.method().as_str(), request.uri().path()),
            ownership::RouteOwner::Rust
        );
        return Ok(multistandby_operations::handle_public(
            request,
            state.multistandby_operations.clone(),
            match state.multistandby_service() {
                Ok(service) => service,
                Err(error) => {
                    return Ok(json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &json!({"code":error.code,"error":error.message,"retryable":true}),
                    ));
                }
            },
        )
        .await);
    }

    match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/diagnostics/support") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            if request.uri().query() == Some("view=live") {
                return Ok(support_diagnostics::handle_live());
            }
            return Ok(support_diagnostics::handle().await);
        }
        (&Method::GET, "/api/diagnostics/events") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(safe_events::handle_public(safe_events::global()));
        }
        (&Method::GET, "/api/multistandby/report") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(multistandby_runtime_report::handle(
                request,
                state.multistandby_store.clone(),
                state.multi_runtime_report.clone(),
            )
            .await);
        }
        (&Method::GET, "/api/reader/imei")
        | (&Method::PUT, "/api/reader/imei")
        | (&Method::DELETE, "/api/reader/imei") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(reader_device_identity::handle_public(
                request,
                reader_device_identity::Store::new(state.multistandby_store.data_root()),
            )
            .await);
        }
        (&Method::GET, "/api/reader/ble/status") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            let socket = env::var_os("PROLETARIUS_BLE_SOCKET").map(PathBuf::from);
            return Ok(reader_ble_status::handle_public(
                socket.as_deref(),
                state.app_token.as_deref(),
            )
            .await);
        }
        (&Method::POST, "/api/reader/ble/scan")
        | (&Method::POST, "/api/reader/ble/connect")
        | (&Method::POST, "/api/reader/ble/disconnect") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            let socket = env::var_os("PROLETARIUS_BLE_SOCKET").map(PathBuf::from);
            return Ok(reader_ble_control::handle_public(
                request,
                socket.as_deref(),
                state.app_token.as_deref(),
                &state,
            )
            .await);
        }
        (&Method::GET, "/api/esim/notes") | (&Method::PUT, "/api/esim/notes") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(profile_notes::handle_public(
                request,
                profile_notes::Store::new(state.multistandby_store.data_root()),
            )
            .await);
        }
        (&Method::GET, "/api/sms/adopt") | (&Method::POST, "/api/sms/adopt") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(sms_adopt::handle_public(
                request,
                state.usb_at.clone(),
                sms_adopt::Store::new(state.multistandby_store.data_root()),
            )
            .await);
        }
        (&Method::POST, "/api/at") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(module_radio::handle_at_console_public(
                request,
                state.module_radio.clone(),
                false,
            )
            .await);
        }
        (&Method::GET, "/api/module/flight-mode") | (&Method::POST, "/api/module/flight-mode") => {
            return Ok(
                module_radio::handle_public(request, state.module_radio.clone(), false).await,
            );
        }
        (&Method::GET, "/api/esim/module-notes") | (&Method::PUT, "/api/esim/module-notes") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(
                module_radio::handle_public(request, state.module_radio.clone(), false).await,
            );
        }
        (&Method::POST, "/api/esim/phonebook/probe") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(
                module_radio::handle_public(request, state.module_radio.clone(), false).await,
            );
        }
        (&Method::PATCH, "/api/esim/profile") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(module_esim::rename_profile(request, &state).await);
        }
        (&Method::POST, "/api/esim/switch") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(module_esim::switch_profile(request, &state).await);
        }
        (&Method::POST, "/api/esim/download") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(reader_esim_download::start_module_download(request, &state).await);
        }
        (&Method::DELETE, "/api/esim/profile") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(module_esim::delete_profile(request, &state).await);
        }
        (&Method::POST, "/api/network/reboot-module") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(network_recovery::handle(request, &state).await);
        }
        (&Method::GET, "/api/network") | (&Method::POST, "/api/network/usbnet") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(
                module_radio::handle_public(request, state.module_radio.clone(), false).await,
            );
        }
        (&Method::GET, "/api/sms") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(message_archive::handle_sms_list(request, state.sms_inbox.clone()).await);
        }
        (&Method::POST, "/api/sms/clear") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            let data_root = state.multistandby_store.data_root();
            return Ok(sms_clear::handle_public(
                request,
                state.sms_inbox.clone(),
                state.usb_sms_receive.clone(),
                &data_root,
            )
            .await);
        }
        (&Method::POST, "/api/sms/delete") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            let data_root = state.multistandby_store.data_root();
            return Ok(sms_delete::handle_public(
                request,
                state.sms_inbox.clone(),
                state.usb_sms_receive.clone(),
                &data_root,
            )
            .await);
        }
        (&Method::POST, "/api/sms/delete-sender") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            let data_root = state.multistandby_store.data_root();
            return Ok(sms_delete_sender::handle_public(
                request,
                state.sms_inbox.clone(),
                state.usb_sms_receive.clone(),
                &data_root,
            )
            .await);
        }
        (&Method::GET, "/api/sms/devices") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(sms_device_service::handle_public(request, &state).await);
        }
        (&Method::POST, "/api/call-mode/download") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(call_mode_runtime_download::handle_public(request, state.call_mode()).await);
        }
        (&Method::POST, "/api/call-mode/enable") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(call_mode_enable::handle_public(request, state.call_mode()).await);
        }
        (&Method::POST, "/api/call-mode/restore") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(call_mode_restore::handle_public(request, state.call_mode()).await);
        }
        (&Method::POST, "/api/call-mode/retry") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(call_mode_retry::handle_public(request, state.call_mode()).await);
        }
        (&Method::POST, "/api/call/audio/start") | (&Method::POST, "/api/call/audio/stop") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(call_audio::handle_public(
                request,
                state.multistandby_store.data_root(),
                state.sms_inbox.clone(),
                state.module_radio.clone(),
                state.usb_at.clone(),
            )
            .await);
        }
        (&Method::GET, "/api/call-mode/backups") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(call_mode_backups::handle_public_with_proof(
                request,
                state.multistandby_store.data_root(),
                state
                    .availability
                    .module_recovery()
                    .root_proof_for(state.multistandby_store.data_root()),
            )
            .await);
        }
        (&Method::GET, "/api/call-mode/backups/export") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(call_mode_backups::handle_public_with_proof(
                request,
                state.multistandby_store.data_root(),
                state
                    .availability
                    .module_recovery()
                    .root_proof_for(state.multistandby_store.data_root()),
            )
            .await);
        }
        (&Method::POST, "/api/call-mode/backups/delete") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(call_mode_backups::handle_delete_public_with_proof(
                request,
                state.multistandby_store.data_root(),
                state.module_radio.clone(),
                state
                    .availability
                    .module_recovery()
                    .root_proof_for(state.multistandby_store.data_root()),
            )
            .await);
        }
        (&Method::POST, "/api/call-mode/backups/import") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(call_mode_backups::handle_public_with_proof(
                request,
                state.multistandby_store.data_root(),
                state
                    .availability
                    .module_recovery()
                    .root_proof_for(state.multistandby_store.data_root()),
            )
            .await);
        }
        (&Method::GET, "/api/sms/status") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(sms_status::handle_public(
                state.sms_inbox.clone(),
                state.usb_sms_receive.clone(),
                state.usb_at.clone(),
            )
            .await);
        }
        (&Method::GET, "/api/sms/storage") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(state
                .usb_sms_receive
                .storage_usage_response(state.module_radio.sim_channel_busy())
                .await);
        }
        (&Method::POST, "/api/sms/refresh") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            if state.module_radio.vowifi_owns_sim_channel() {
                return Ok(json_response(
                    StatusCode::CONFLICT,
                    &json!({
                        "error":"模块 VoWiFi 正在使用 SIM 安全通道，短信已由 IMS 同步",
                        "code":"module_vowifi_owns_sim",
                        "retryable":true,
                        "recoverable":true
                    }),
                ));
            }
            let archive = match message_archive::archive() {
                Ok(archive) => archive,
                Err(_) => {
                    return Ok(json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &json!({
                            "error":"短信归档服务暂不可用",
                            "code":"sms_archive_unavailable",
                            "retryable":true,
                            "recoverable":true
                        }),
                    ));
                }
            };
            state.modem_actor.ensure_sms_receive_worker(archive);
            let modem_actor = state.modem_actor.clone();
            return Ok(
                match tokio::task::spawn_blocking(move || modem_actor.request_manual_sms_scan())
                    .await
                {
                    Ok(modem_actor::ManualSmsScanRequest::Scheduled) => {
                        json_response(StatusCode::ACCEPTED, &json!({"accepted":true}))
                    }
                    Ok(modem_actor::ManualSmsScanRequest::SerialUnavailable) => {
                        state.usb_sms_receive.public_refresh_response().await
                    }
                    Ok(modem_actor::ManualSmsScanRequest::Rejected(code)) => {
                        crate::diagnostics::write_line(format_args!(
                            "switchd-rs: manual SMS scan was not admitted ({code})"
                        ));
                        json_response(StatusCode::ACCEPTED, &json!({"accepted":true}))
                    }
                    Err(_) => json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &json!({
                            "error":"短信刷新调度服务暂不可用",
                            "code":"sms_refresh_unavailable",
                            "retryable":true,
                            "recoverable":true
                        }),
                    ),
                },
            );
        }
        (&Method::POST, "/api/network/check-4g") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(network_check::handle().await);
        }
        (&Method::PUT, "/api/network/services-order") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(network_services::handle_order(request).await);
        }
        (&Method::GET, "/api/voice/enabled") | (&Method::POST, "/api/voice/enable") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(voice_enabled::handle_public(
                request,
                state.modem_actor.clone(),
                state.multistandby_store.data_root(),
            )
            .await);
        }
        (&Method::GET, "/api/sms/forwarding/status")
        | (&Method::POST, "/api/sms/forwarding/configure")
        | (&Method::POST, "/api/sms/forwarding/enqueue")
        | (&Method::POST, "/api/sms/forwarding/device-snapshot")
        | (&Method::POST, "/api/sms/forwarding/test")
        | (&Method::POST, "/api/sms/forwarding/queue-action") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(state.sms_forwarding.handle(request).await);
        }
        _ => {}
    }

    match (request.method(), request.uri().path()) {
        (&Method::POST, "/api/reader/vowifi/proxy-check") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(reader_proxy_check::handle(request).await);
        }
        (&Method::GET, "/api/reader/info") => return Ok(reader_control::info(&state).await),
        (&Method::POST, "/api/reader/connect")
        | (&Method::POST, "/api/reader/disconnect")
        | (&Method::POST, "/api/reader/refresh") => {
            return Ok(reader_control::handle_public(request, &state).await);
        }
        (&Method::POST, "/api/reader/auth") => {
            if let Some(reader) = reader_esim_download::selected_reader(&state) {
                debug_assert_eq!(
                    ownership::route_owner(request.method().as_str(), request.uri().path()),
                    ownership::RouteOwner::Rust
                );
                return Ok(reader_auth::handle_public(request, &state, reader).await);
            }
            return Ok(reader_auth::no_selection(request).await);
        }
        (&Method::GET, "/api/reader/esim") => {
            return Ok(reader_esim_download::forward_overview(request, &state).await);
        }
        (&Method::GET, "/api/reader/esim/operation") => {
            return Ok(reader_esim_download::get_operation(&state).await);
        }
        (&Method::GET, "/api/reader/esim/mutation-intent") => {
            return Ok(esim_mutation_intent::query(
                "reader",
                request.uri().query().unwrap_or_default(),
            ));
        }
        (&Method::GET, "/api/esim/operation") => {
            return Ok(reader_esim_download::get_module_operation(&state).await);
        }
        (&Method::GET, "/api/esim/mutation-intent") => {
            return Ok(esim_mutation_intent::query(
                "module",
                request.uri().query().unwrap_or_default(),
            ));
        }
        (&Method::POST, "/api/esim/operation/acknowledge") => {
            return Ok(reader_esim_download::acknowledge_legacy_operation(request).await);
        }
        (&Method::GET, "/api/reader/esim/health") => {
            if let Some(reader) = reader_esim_download::selected_reader(&state) {
                return Ok(reader_esim_download::health_or_forward(request, &state, reader).await);
            }
            return Ok(reader_esim_download::no_selection());
        }
        (&Method::POST, "/api/reader/esim/download/confirm") => {
            return Ok(reader_esim_download::confirm(request).await);
        }
        (&Method::POST, "/api/esim/download/confirm")
            if module_esim_transport::native_writer(&state)
                || reader_esim_download::has_active_download_operation() =>
        {
            return Ok(reader_esim_download::confirm(request).await);
        }
        (&Method::POST, "/api/reader/esim/download") => {
            if let Some(reader) = reader_esim_download::selected_reader(&state) {
                return Ok(reader_esim_download::start(request, &state, reader).await);
            }
            return Ok(reader_esim_download::no_selection());
        }
        (&Method::PATCH, "/api/reader/esim/profile") => {
            return Ok(reader_esim_download::rename_or_forward(request, &state).await);
        }
        (&Method::DELETE, "/api/reader/esim/profile") => {
            return Ok(reader_esim_download::delete_or_forward(request, &state).await);
        }
        (&Method::POST, "/api/reader/esim/switch") => {
            return Ok(reader_esim_download::switch_or_forward(request, &state).await);
        }
        (&Method::GET, "/api/reader/esim/notifications") => {
            return Ok(reader_esim_download::list_notifications_or_forward(request, &state).await);
        }
        (&Method::POST, "/api/reader/esim/notifications/retry") => {
            return Ok(reader_esim_download::retry_notification_or_forward(request, &state).await);
        }
        _ => {}
    }

    if let Some(response) = sms_store_rejection(request.uri().path(), &state) {
        return Ok(response);
    }
    match (request.method(), request.uri().path()) {
        (&Method::POST, "/api/multistandby/messages/send") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(multistandby_send::handle_public(
                request,
                state.multistandby_store.clone(),
                match state.multistandby_service() {
                    Ok(service) => service,
                    Err(error) => {
                        return Ok(json_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            &json!({"code":error.code,"error":error.message,"retryable":true}),
                        ));
                    }
                },
            )
            .await);
        }
        (&Method::GET, "/api/sms/send-intents")
        | (&Method::POST, "/api/sms/send-intents")
        | (&Method::POST, "/api/sms/send") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(device_sms_send::handle(request, &state).await);
        }
        _ => {}
    }
    if sms_export::is_route(request.uri().path()) {
        return Ok(sms_export::handle(request, state.multistandby_store.clone()).await);
    }
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/multistandby/cards") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(multistandby_store::handle_cards(
                state.multistandby_store.clone(),
                match state.multistandby_service() {
                    Ok(service) => service,
                    Err(error) => {
                        return Ok(json_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            &json!({"code":error.code,"error":error.message,"retryable":true}),
                        ));
                    }
                },
            )
            .await);
        }
        (&Method::GET, "/api/multistandby/profiles") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            return Ok(multistandby_store::handle_profiles(
                request,
                state.multistandby_store.clone(),
                match state.multistandby_service() {
                    Ok(service) => service,
                    Err(error) => {
                        return Ok(json_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            &json!({"code":error.code,"error":error.message,"retryable":true}),
                        ));
                    }
                },
            )
            .await);
        }
        _ => {}
    }
    if multistandby_store::is_public_route(request.method(), request.uri().path()) {
        return Ok(
            multistandby_store::handle(request, state.multistandby_store.clone(), false).await,
        );
    }
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/reader/vowifi/stop-control")
        | (&Method::POST, "/api/reader/vowifi/stop-control") => {
            if let Some(rejection) =
                native_vowifi_session::stop_control_rejection(&request, "reader")
            {
                return Ok(rejection);
            }
            return Ok(match state.native_vowifi_lifecycle.ordinary_service() {
                Some(service) => service.handle_public(request).await,
                None => json_response(
                    StatusCode::CONFLICT,
                    &json!({"code":"vowifi_stop_target_stale","error":"vowifi_stop_target_stale"}),
                ),
            });
        }
        (&Method::GET, "/api/vowifi/stop-control")
        | (&Method::POST, "/api/vowifi/stop-control") => {
            if let Some(rejection) =
                native_vowifi_session::stop_control_rejection(&request, "module")
            {
                return Ok(rejection);
            }
            return Ok(
                match state.native_vowifi_lifecycle.retained_module_service() {
                    Some(service) => service.handle(request).await,
                    None => json_response(
                        StatusCode::CONFLICT,
                        &json!({"code":"vowifi_stop_target_stale","error":"vowifi_stop_target_stale"}),
                    ),
                },
            );
        }
        _ => {}
    }
    if matches!(
        (request.method().as_str(), request.uri().path()),
        ("GET" | "POST", "/api/vowifi")
            | ("GET", "/api/vowifi/result")
            | ("POST", "/api/vowifi/reconcile")
    ) {
        return Ok(match state.native_vowifi_lifecycle.module_service(&state) {
            Ok(service) => service.handle(request).await,
            Err(_) => json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &serde_json::json!({"error":"模块 VoWiFi 服务暂不可用"}),
            ),
        });
    }
    // Every public call/USSD mutation is admitted by the durable receipt
    // boundary before either legacy alias can reach the native dispatcher.
    // Status remains a read-only direct projection below.
    if unified_call_control::is_mutation(request.method(), request.uri().path()) {
        return Ok(unified_call_control::handle(request, &state).await);
    }
    // Both call prefixes address the one admitted ordinary IMS session. A
    // retained reader observation must never steal an active module's controls.
    if let Some(service) = state
        .native_vowifi_lifecycle
        .module_call_service(request.method(), request.uri().path())
    {
        return Ok(service.calls(request).await);
    }
    // A captured native intent continues to receive its own controls during
    // drain/recovery. Never fall through to a replacement Go session on error.
    // New native starts remain behind the explicit per-capability owner gates.
    if state
        .native_vowifi_lifecycle
        .handles_public_request(request.method(), request.uri().path())
    {
        return Ok(match state.native_vowifi_lifecycle.service(&state) {
            Ok(service) => service.handle_public(request).await,
            Err(_) => json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &serde_json::json!({"error":"原生 VoWiFi 服务暂不可用"}),
            ),
        });
    }
    // Ordinary reader IMS controls resolve above this module-only fence.
    if state.availability.module_recovery().is_quarantined()
        && matches!(
            request.uri().path(),
            "/api/call/dtmf"
                | "/api/call/status"
                | "/api/call/dial"
                | "/api/call/answer"
                | "/api/call/hangup"
        )
    {
        return Ok(module_recovery::rejection());
    }
    // The retained module owns its status even while the discovery process
    // restarts. This cache-only observation must not wait for or probe Go/AT.
    if request.method() == Method::GET
        && request.uri().path() == "/api/status"
        && let Some(status) = device_status_projection::cached_native_status_with_qmi(&state)
    {
        return Ok(json_response(StatusCode::OK, &status));
    }
    // Native device observations and eSIM remain available while the
    // compatibility child is restarting or has exhausted its retry budget.
    if request.method() == Method::GET
        && ((matches!(request.uri().path(), "/api/status" | "/api/health")
            && (state.modem_actor.native_serial_managed()
                || module_discovery::uses_native_usb(&state.module_radio, &state.usb_at)))
            || (matches!(request.uri().path(), "/api/esim" | "/api/esim/health")
                && module_esim_transport::native(&state)))
    {
        return handle_ready_request(request, &state).await;
    }
    let declared_owner = ownership::route_owner(request.method().as_str(), request.uri().path());
    let response = match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/v2/runtime") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            let readiness = ownership::readiness();
            let mut runtime = serde_json::to_value(RuntimeResponse {
                ok: true,
                backend: "rust",
                version: BACKEND_VERSION,
                app_release_version: APP_RELEASE_VERSION,
                build_number: APP_BUILD_NUMBER,
                source_digest: SOURCE_DIGEST,
                migration_stage: MigrationStage::MessageArchiveCutover.as_str(),
                hardware_ownership: true,
                hardware_owner: "rust",
                go_backend_connected: false,
                go_backend_restarting: false,
                go_backend_phase: "retired",
                go_backend_generation: 0,
                go_restart_attempts: 0,
                proxy_contract: PROXY_CONTRACT,
                sim_protocol_contract: SIM_PROTOCOL_CONTRACT,
                ike_wire_contract: IKE_WIRE_CONTRACT,
                ims_protocol_contract: IMS_PROTOCOL_CONTRACT,
                sms_protocol_contract: SMS_PROTOCOL_CONTRACT,
                call_protocol_contract: CALL_PROTOCOL_CONTRACT,
                store_contract: STORE_CONTRACT,
                snapshot_schema_version: 2,
                ownership_schema_version: ownership::OWNERSHIP_SCHEMA_VERSION,
                default_runtime: "rust-native",
                cutover_ready: readiness.ready_to_remove_go,
                sms_store_health: sms_store_health::snapshot(
                    state.device_send_store.status(),
                    state.multistandby_store.status(),
                ),
            })
            .expect("runtime DTO serialization");
            if let Some(status) = state.availability.module_recovery().status() {
                runtime["module_recovery"] =
                    serde_json::to_value(status).expect("module recovery DTO serialization");
            }
            json_response(StatusCode::OK, &runtime)
        }
        (&Method::GET, "/api/v2/migration-readiness") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            json_response(StatusCode::OK, &ownership::readiness())
        }
        (&Method::POST, "/api/v2/carrier/resolve") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(carrier::handle(request).await);
        }
        (&Method::POST, "/api/v2/call/dtmf") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_codec::handle(request).await);
        }
        (&Method::POST, "/api/v2/call/negotiate-sdp") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_codec::handle_audio_sdp(request).await);
        }
        (&Method::POST, "/api/v2/call/build-local-sdp") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_codec::handle_local_audio_sdp(request).await);
        }
        (&Method::POST, "/api/v2/call/invite-policy") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_codec::handle_invite_policy(request).await);
        }
        (&Method::POST, "/api/call/dtmf") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_control::handle_dtmf(request, state.module_call()).await);
        }
        (&Method::GET, "/api/call/status") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_control::handle_status(state.module_call()).await);
        }
        (&Method::GET, "/api/call-mode/status") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_mode_status::handle_public_status(state.call_mode()).await);
        }
        (&Method::POST, "/api/call/dial") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_control::handle_dial(request, state.module_call()).await);
        }
        (&Method::POST, "/api/call/answer") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_control::handle_answer(request, state.module_call()).await);
        }
        (&Method::POST, "/api/call/hangup") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_control::handle_hangup(request, state.module_call()).await);
        }
        (&Method::POST, "/api/v2/ims/validate-register-response") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(ims_codec::handle(request).await);
        }
        (&Method::POST, "/api/v2/reader/authenticate") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(reader_auth::handle(
                request,
                state.reader_pcsc.clone(),
                state.reader_gate.clone(),
            )
            .await);
        }
        (&Method::POST, "/api/v2/reader/eap-aka") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(reader_eap_aka::handle(
                request,
                &state.eap_exchanges,
                state.reader_pcsc.clone(),
                state.reader_gate.clone(),
            )
            .await);
        }
        (&Method::POST, "/api/v2/vowifi/eap-identity")
        | (&Method::POST, "/api/v2/vowifi/eap-exchange/release") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(eap_identity::handle(request, &state.eap_exchanges).await);
        }
        (&Method::POST, "/api/v2/reader/identity") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(reader_identity::handle(
                request,
                state.reader_pcsc.clone(),
                state.reader_gate.clone(),
            )
            .await);
        }
        (&Method::GET, "/api/reader/list") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(reader_inventory::handle(&state).await);
        }
        (&Method::POST, "/api/v2/vowifi/proxy-policy") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(proxy_policy::handle(request).await);
        }
        (&Method::POST, "/api/v2/vowifi/proxy-probe") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(proxy_probe::handle(request).await);
        }
        (&Method::POST, "/api/v2/vowifi/select-transport") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(transport_selection::handle(request).await);
        }
        (&Method::POST, "/api/v2/vowifi/session-policy") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(vowifi_session_policy::handle(request).await);
        }
        (&Method::POST, "/api/v2/vowifi/native-session-plan") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(native_vowifi_session::handle(
                request,
                &state.go_socket_path,
                state.app_token.as_deref(),
                state.reader_pcsc.clone(),
                state.reader_gate.clone(),
            )
            .await);
        }
        (
            &Method::POST,
            "/api/v2/sms/encode-rpdu"
            | "/api/v2/sms/decode-rpdu"
            | "/api/v2/sms/prepare-tpdu"
            | "/api/v2/sms/decode-incoming-tpdu"
            | "/api/v2/sms/decode-stored-snapshot",
        ) => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(sms_codec::handle(request).await);
        }
        (&Method::GET, "/api/v2/sms/forwarding/handoff")
        | (&Method::POST, "/api/v2/sms/forwarding/handoff") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(forwarding_handoff_store::handle(
                request,
                state.forwarding_handoff_store.clone(),
            )
            .await);
        }
        (&Method::GET, "/api/calls")
        | (&Method::POST, "/api/calls/clear")
        | (&Method::POST, "/api/calls/delete") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(call_archive::handle(request, state.call_archive.clone()).await);
        }
        (&Method::GET, "/api/v2/sms-archive")
        | (&Method::POST, "/api/v2/sms-archive/clear")
        | (&Method::POST, "/api/v2/sms-archive/delete")
        | (&Method::POST, "/api/v2/sms-archive/delete-sender") => {
            debug_assert_eq!(declared_owner, ownership::RouteOwner::Rust);
            return Ok(message_archive::handle(request).await);
        }
        _ => return handle_ready_request(request, &state).await,
    };
    Ok(response)
}

fn sms_store_rejection(path: &str, state: &ServerState) -> Option<Response<Full<Bytes>>> {
    if matches!(path, "/api/sms/send" | "/api/sms/send-intents") {
        return sms_store_health::rejection(state.device_send_store.status());
    }
    // Stop, the operation result, and the historical runtime report remain
    // available; stopping must still release a live card after a store fault.
    if path.starts_with("/api/multistandby/")
        && path != "/api/multistandby/stop"
        && path != "/api/multistandby/report"
        && !path.starts_with("/api/multistandby/operations/")
    {
        return sms_store_health::rejection(state.multistandby_store.status());
    }
    None
}

async fn handle_ready_request(
    request: Request<Incoming>,
    state: &ServerState,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let response = match (request.method(), request.uri().path()) {
        (&Method::GET, "/api/network/recovery") | (&Method::POST, "/api/network/recovery") => {
            return Ok(network_recovery::handle(request, state).await);
        }
        (&Method::GET, "/api/network/services") => {
            let module_product =
                network_diagnostic::discover_module_product(state.usb_at.active_description())
                    .await;
            return Ok(network_services::handle(module_product.as_deref()).await);
        }
        (&Method::GET, "/api/network/traffic") => {
            let module_product =
                network_diagnostic::discover_module_product(state.usb_at.active_description())
                    .await;
            let generation = state.availability.snapshot().generation;
            return Ok(network_traffic::handle(
                &state.network_traffic,
                module_product.as_deref(),
                generation,
            )
            .await);
        }
        (&Method::GET, "/api/v2/device-snapshot") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            let snapshot = snapshot::capture(state).await;
            let status = if snapshot.ok {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            json_response(status, &snapshot)
        }
        (&Method::GET, "/api/health") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            match health_projection::capture(state).await {
                Ok(health) => json_response(StatusCode::OK, &health),
                Err(error) => {
                    let message = error.to_string();
                    json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &ErrorResponse {
                            error: &message,
                            code: "go_backend_unavailable",
                            retryable: true,
                        },
                    )
                }
            }
        }
        (&Method::GET, "/api/esim/health") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            esim_health_projection::handle(state).await
        }
        (&Method::GET, "/api/esim") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            esim_overview_projection::handle(&request, state).await
        }
        (&Method::GET, "/api/status") => {
            debug_assert_eq!(
                ownership::route_owner(request.method().as_str(), request.uri().path()),
                ownership::RouteOwner::Rust
            );
            match device_status_projection::capture(state).await {
                Ok(status) => json_response(StatusCode::OK, &status),
                Err(error) => {
                    let message = error.to_string();
                    json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &ErrorResponse {
                            error: &message,
                            code: "go_backend_unavailable",
                            retryable: true,
                        },
                    )
                }
            }
        }
        _ => json_response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({"code":"route_not_found","error":"route not found"}),
        ),
    };
    Ok(response)
}

#[cfg(test)]
fn socket_is_live(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
}

fn json_response<T: serde::Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| {
        br#"{"error":"response serialization failed","code":"internal_error","retryable":false}"#
            .to_vec()
    });
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        hyper::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn valid_app_token(value: &str) -> bool {
    (16..=4096).contains(&value.len()) && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn required_app_token() -> Result<String, BackendFailure> {
    let value = env::var(APP_TOKEN_ENVIRONMENT)
        .map_err(|_| BackendFailure::Io("app_token_missing_or_invalid".into()))?;
    if valid_app_token(&value) {
        Ok(value)
    } else {
        Err(BackendFailure::Io("app_token_missing_or_invalid".into()))
    }
}

fn token_matches<B>(request: &Request<B>, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    let Some(provided) = request.headers().get(APP_TOKEN_HEADER) else {
        return false;
    };
    let Ok(provided) = provided.to_str() else {
        return false;
    };
    provided.len() == expected.len() && provided.as_bytes().ct_eq(expected.as_bytes()).into()
}

fn parse_listen_path(arguments: &daemon_args::DaemonArgs) -> Result<PathBuf, BackendFailure> {
    use std::os::unix::ffi::OsStrExt;
    let value = arguments
        .listen
        .as_deref()
        .ok_or(BackendFailure::InvalidListenAddress)?;
    let path = value
        .as_bytes()
        .strip_prefix(b"unix:")
        .map(std::ffi::OsStr::from_bytes)
        .filter(|path| !daemon_args::trimmed(path).is_empty())
        .ok_or(BackendFailure::InvalidListenAddress)?;
    Ok(PathBuf::from(path))
}

pub(crate) async fn prepare_socket_path(path: &Path) -> Result<(), BackendFailure> {
    use std::os::unix::fs::MetadataExt;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            // A second root may select the same listener path. Connecting in
            // nonblocking mode also protects a live listener with a saturated
            // accept queue. Darwin also returns ECONNREFUSED for a full
            // backlog, so require a separate owner query before any unlink.
            let probe = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
                .map_err(io_failure)?;
            probe.set_nonblocking(true).map_err(io_failure)?;
            let address = socket2::SockAddr::unix(path).map_err(io_failure)?;
            match probe.connect(&address) {
                Err(error) if error.raw_os_error() == Some(nix::libc::ECONNREFUSED) => {
                    // Darwin retains the attempted peer pathname on this
                    // failed probe. Close it before querying owners, or lsof
                    // would correctly find our own diagnostic descriptor.
                    drop(probe);
                    socket_lifetime::verify_stale_socket(path)
                        .await
                        .map_err(socket_claim_failure)?;
                }
                Ok(()) => return Err(BackendFailure::Io("backend_socket_in_use".into())),
                Err(error) => return Err(io_failure(error)),
            }
            let current = fs::symlink_metadata(path).map_err(io_failure)?;
            if !current.file_type().is_socket()
                || current.dev() != metadata.dev()
                || current.ino() != metadata.ino()
            {
                return Err(BackendFailure::Io("backend_socket_changed".into()));
            }
            fs::remove_file(path).map_err(io_failure)
        }
        Ok(_) => Err(BackendFailure::ExistingPathIsNotSocket),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_failure(error)),
    }
}

fn socket_claim_failure(error: socket_lifetime::Error) -> BackendFailure {
    match error {
        socket_lifetime::Error::Busy | socket_lifetime::Error::SocketInUse => {
            BackendFailure::Io("backend_socket_in_use".into())
        }
        error => BackendFailure::Io(error.to_string()),
    }
}

pub(crate) fn io_failure(error: std::io::Error) -> BackendFailure {
    BackendFailure::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn shutdown_driver_keeps_both_unix_cleanup_listeners_alive_until_owner_finishes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let root = test_directory("shutdown-driver");
        let callback_path = root.join("c.sock");
        let internal_path = root.join("i.sock");
        let callback = tokio::net::UnixListener::bind(&callback_path).unwrap();
        let internal = tokio::net::UnixListener::bind(&internal_path).unwrap();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let driver = tokio::spawn(async move {
            super::drive_shutdown_callbacks(
                async { Ok::<_, proletarius_domain::BackendFailure>(finished.await.unwrap()) },
                &callback,
                &internal,
                |mut stream| async move {
                    assert_eq!(stream.read_u8().await.unwrap(), 1);
                    stream.write_all(&[2]).await.unwrap();
                },
                |mut stream| async move {
                    assert_eq!(stream.read_u8().await.unwrap(), 3);
                    stream.write_all(&[4]).await.unwrap();
                },
            )
            .await
        });
        for (path, request, response) in [(callback_path, 1, 2), (internal_path, 3, 4)] {
            let mut stream = tokio::net::UnixStream::connect(path).await.unwrap();
            stream.write_all(&[request]).await.unwrap();
            assert_eq!(
                tokio::time::timeout(tokio::time::Duration::from_secs(1), stream.read_u8())
                    .await
                    .unwrap()
                    .unwrap(),
                response
            );
        }
        assert!(!driver.is_finished());
        finish.send(42_u32).unwrap();
        let (result, report) = driver.await.unwrap();
        assert_eq!(result.unwrap(), 42);
        report.finish().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn native_cleanup_errors_preserve_callback_service_until_confirmation() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let root = test_directory("native-cleanup-retry");
        let path = root.join("c.sock");
        let callback = tokio::net::UnixListener::bind(&path).unwrap();
        let internal = tokio::net::UnixListener::bind(root.join("i.sock")).unwrap();
        let released = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));
        let worker_attempts = attempts.clone();
        let worker_released = released.clone();
        let callback_released = released.clone();
        let driver = tokio::spawn(async move {
            super::drive_shutdown_callbacks(
                super::wait_for_native_cleanup(
                    move || {
                        let number = worker_attempts.fetch_add(1, Ordering::SeqCst);
                        let done = worker_released.load(Ordering::SeqCst);
                        async move {
                            if done {
                                Ok(())
                            } else {
                                Err(if number == 0 {
                                    "native_lifecycle_unavailable"
                                } else {
                                    "native_shutdown_unconfirmed"
                                })
                            }
                        }
                    },
                    tokio::time::Duration::from_millis(5),
                ),
                &callback,
                &internal,
                move |mut stream| {
                    let released = callback_released.clone();
                    async move {
                        assert_eq!(stream.read_u8().await.unwrap(), 1);
                        stream.write_all(&[2]).await.unwrap();
                        released.store(true, Ordering::SeqCst);
                    }
                },
                |_| async {},
            )
            .await
        });
        tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
            while attempts.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!driver.is_finished());
        let mut client = tokio::net::UnixStream::connect(path).await.unwrap();
        client.write_all(&[1]).await.unwrap();
        assert_eq!(client.read_u8().await.unwrap(), 2);
        let (result, report) = tokio::time::timeout(tokio::time::Duration::from_secs(1), driver)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("native_lifecycle_unavailable")
        );
        report.finish().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    use super::{
        benign_connection_error_message, parent_process_changed, parse_listen_path,
        prepare_socket_path, shutdown_rejection,
    };
    use hyper::Method;
    use nix::unistd::Pid;
    use std::fs;
    use std::os::unix::net::UnixListener as StdUnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn startup_stop_cancels_but_waits_for_the_actual_operation_result() {
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (finish, completion) = tokio::sync::oneshot::channel();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let operation_cancelled = cancelled.clone();
        let wait_cancelled = cancelled.clone();
        let task = tokio::spawn(super::await_startup_native(
            async move {
                started.send(()).unwrap();
                let value = completion.await.unwrap();
                assert!(operation_cancelled.load(Ordering::Acquire));
                value
            },
            wait_cancelled,
            async move { stopped.await.unwrap() },
        ));
        ready.await.unwrap();
        stop.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !cancelled.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!task.is_finished());
        finish.send(37).unwrap();
        assert_eq!(task.await.unwrap(), (37, true));
    }

    #[tokio::test]
    async fn startup_simultaneous_stop_is_not_lost_to_immediate_read_completion() {
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let checked = cancelled.clone();
        let result = super::await_startup_native(
            async move {
                assert!(checked.load(Ordering::Acquire));
                41
            },
            cancelled,
            std::future::ready(()),
        )
        .await;
        assert_eq!(result, (41, true));
    }

    #[tokio::test]
    async fn completed_startup_read_does_not_cancel_or_wait_for_a_future_stop() {
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let result = super::await_startup_native(
            std::future::ready(43),
            cancelled.clone(),
            std::future::pending(),
        )
        .await;
        assert_eq!(result, (43, false));
        assert!(!cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn sms_store_failure_does_not_prevent_gateway_initialization() {
        for damaged in ["device-send-ledger.json", "multistandby/store-v1.json"] {
            let root = test_directory("sms-store-degraded");
            let path = root.join(damaged);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = b"{damaged-fixture";
            fs::write(&path, original).unwrap();
            let initialized = super::initialize_sms_stores(&root);
            assert!(
                initialized.is_ok(),
                "{damaged} prevented gateway initialization"
            );
            assert_eq!(fs::read(path).unwrap(), original);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn production_sms_authority_failure_is_isolated_and_reopen_cannot_clear_it() {
        let root = test_directory("sms-authority-isolated");
        let path = root.join("device-send-ledger.json");
        let damaged = b"{invalid-device-ledger";
        fs::write(&path, damaged).unwrap();
        let lease = super::daemon_lifetime::Lease::acquire(&root).unwrap();
        let (multi, device, forwarding) =
            super::initialize_sms_stores_with_authority(&root, Some(lease.root_proof()))
                .expect("SMS failure must not prevent other modules starting");
        assert!(!multi.status().restart_required);
        assert!(device.status().restart_required);
        assert_eq!(
            super::sms_store_health::rejection(device.status())
                .unwrap()
                .status(),
            hyper::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(fs::read(&path).unwrap(), damaged);
        for action in ["authority", "get", "list", "prepare", "claim", "finish"] {
            assert!(matches!(device.apply(action,serde_json::json!({})),
                Err(proletarius_store::StoreError::Configuration(code)) if code=="sms_store_unavailable"));
        }
        // External bytes changing cannot reopen this lifetime's disabled lane.
        fs::write(&path, br#"[{"schema_version":1,"records":{}}]"#).unwrap();
        let reopened = super::device_send_store::Store::open(&root).unwrap();
        assert!(reopened.status().restart_required);
        assert!(
            reopened
                .apply(
                    "prepare",
                    serde_json::json!({"client_request_id":"must-not-admit"})
                )
                .is_err()
        );
        drop((multi, device, forwarding, reopened));
        drop(lease);
        fs::remove_dir_all(root).unwrap();
    }

    pub(super) fn test_directory(_name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        // Darwin's sockaddr_un has a short path limit. Keep the fixture under
        // TMPDIR so callers can contain every test artifact in their workspace.
        // Keep the leaf short for Darwin sockaddr_un.
        loop {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sw-{}-{}-{}",
                std::process::id(),
                nonce % 1_000_000_000,
                sequence
            ));
            match fs::create_dir(&path) {
                Ok(()) => return path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test directory: {error}"),
            }
        }
    }

    #[tokio::test]
    async fn call_events_are_auth_gated_durable_and_allowed_during_shutdown() {
        use bytes::Bytes;
        use http_body_util::{BodyExt, Full};
        use hyper_util::rt::TokioIo;
        use std::sync::Arc;
        use tokio::net::UnixStream;

        let root = test_directory("call-archive-ingest");
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
        let body = serde_json::to_vec(&serde_json::json!({
            "id":"call-fixture-module-out",
            "device":"module",
            "direction":"out",
            "number":"+441234",
            "answered":true,
            "started_at":"2026-10-04T12:00:00Z",
            "ended_at":"2026-10-04T12:01:00Z",
            "duration":60
        }))
        .unwrap();

        for (shutdown, token, expected, duplicate) in [
            (false, None, hyper::StatusCode::UNAUTHORIZED, false),
            (
                false,
                Some("internal-fixture"),
                hyper::StatusCode::OK,
                false,
            ),
            (false, Some("internal-fixture"), hyper::StatusCode::OK, true),
            (true, Some("internal-fixture"), hyper::StatusCode::OK, true),
        ] {
            let (client, server) = UnixStream::pair().unwrap();
            let connection_state = state.clone();
            let server_task = if shutdown {
                tokio::spawn(async move {
                    super::serve_internal_shutdown_connection(server, connection_state).await;
                })
            } else {
                tokio::spawn(async move {
                    let _ = super::serve_internal_connection(server, connection_state).await;
                })
            };
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(client))
                    .await
                    .unwrap();
            let connection_task = tokio::spawn(connection);
            let mut request = hyper::Request::builder()
                .method(Method::POST)
                .uri("/api/internal/call-archive/ingest")
                .header("host", "localhost");
            if let Some(token) = token {
                request = request.header("x-switch-internal-token", token);
            }
            let response = sender
                .send_request(request.body(Full::new(Bytes::from(body.clone()))).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            let response: serde_json::Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            if expected == hyper::StatusCode::OK {
                assert_eq!(response["already_recorded"], duplicate);
            }
            drop(sender);
            let _ = connection_task.await;
            server_task.await.unwrap();
        }

        let records = state.call_archive.load().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["id"], "call-fixture-module-out");

        // These are already-observed history facts, never physical call
        // commands. Keep all facts ordered during teardown, including answer
        // and number updates ahead of their terminal observation.
        for (action, sequence, at, token, expected) in [
            (
                "begin",
                1,
                "2026-01-01T12:00:00Z",
                None,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "begin",
                1,
                "2026-01-01T12:00:00Z",
                Some("internal-fixture"),
                hyper::StatusCode::OK,
            ),
            (
                "begin",
                1,
                "2026-01-01T12:00:00Z",
                Some("internal-fixture"),
                hyper::StatusCode::OK,
            ),
            (
                "answered",
                2,
                "2026-01-01T12:00:01Z",
                Some("internal-fixture"),
                hyper::StatusCode::OK,
            ),
            (
                "end",
                3,
                "2026-01-01T12:00:04Z",
                Some("internal-fixture"),
                hyper::StatusCode::OK,
            ),
            (
                "end",
                3,
                "2026-01-01T12:00:04Z",
                Some("internal-fixture"),
                hyper::StatusCode::OK,
            ),
        ] {
            let body = serde_json::to_vec(&serde_json::json!({
                "producer_id":"call-producer-fixture", "source":"module_ims",
                "owner_id":"module-fixture", "owner_epoch":1, "sequence":sequence,
                "action":"ims", "direction":"out", "number":"+441234", "call_id":"shutdown-call-fixture",
                "state":match action { "begin" => "outgoing", "answered" => "active", _ => "ended" },
                "observed_at":at,
            }))
            .unwrap();
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
                .uri("/api/internal/call-archive/observe")
                .header("host", "localhost");
            if let Some(token) = token {
                request = request.header("x-switch-internal-token", token);
            }
            let response = sender
                .send_request(request.body(Full::new(Bytes::from(body))).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            let response: serde_json::Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            if expected == hyper::StatusCode::OK {
                assert_eq!(response["ok"], true);
                assert_eq!(response["sequence"], sequence);
            }
            drop(sender);
            let _ = connection_task.await;
            server_task.await.unwrap();
        }
        state.call_facts.finish_pending().unwrap();
        let records = state.call_archive.load().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["duration"], 4);
        assert_eq!(records[0]["answered"], true);
        fs::remove_dir_all(root).expect("remove Switch/build test root");
    }

    #[tokio::test]
    async fn internal_sms_archive_requires_private_token_and_rejects_new_writes_on_shutdown() {
        use bytes::Bytes;
        use http_body_util::{BodyExt, Full};
        use hyper_util::rt::TokioIo;
        use std::sync::Arc;
        use tokio::net::UnixStream;

        let root = test_directory("sms-archive-internal-auth");
        let sms_inbox = crate::message_archive::InboxState::default();
        let data_root = root.join("multi");
        let lifetime = crate::daemon_lifetime::Lease::acquire(&data_root)
            .expect("hold the actual internal backup data root");
        let module_recovery = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
        assert!(!module_recovery.is_quarantined());
        let availability =
            super::BackendAvailability::with_module_recovery(module_recovery.clone());
        let modem_actor =
            super::modem_actor::ModemActor::new_with_module_recovery(module_recovery.clone())
                .unwrap();
        let usb_at = crate::usb_at::Owner::unavailable_for_tests("usb-producer-fixture".into())
            .with_recovery_gate_for_tests(module_recovery);
        let usb_sms_receive = crate::usb_sms_receive::Service::new(
            proletarius_store::ArchiveStore::new(root.join("sms-archive.json")),
            sms_inbox.clone(),
            usb_at.clone(),
            availability.clone(),
            "usb-producer-fixture".into(),
        );
        let module_radio = crate::module_radio::Service::new(
            data_root.clone(),
            usb_at.clone(),
            modem_actor.clone(),
            availability.clone(),
            "usb-producer-fixture".into(),
        );
        let state = super::ServerState {
            call_mode: Default::default(),
            module_call: Default::default(),
            reader_lifecycle: Default::default(),
            multistandby_card: Default::default(),
            multistandby_service: Default::default(),
            app_token: Some("app-fixture".to_owned()),
            internal_token: "internal-fixture".to_owned(),
            multistandby_store: super::multistandby_store::Store::open(&data_root).unwrap(),
            multistandby_operations: super::multistandby_operations::Store::open(&root).unwrap(),
            multi_runtime_report: Arc::new(std::sync::Mutex::new(
                super::multistandby_runtime_report::Journal::default(),
            )),
            device_send_store: super::device_send_store::Store::open(&root.join("device")).unwrap(),
            forwarding_handoff_store: super::forwarding_handoff_store::Store::open(&root).unwrap(),
            sms_forwarding: super::sms_forwarding::Service::default(),
            sms_inbox: sms_inbox.clone(),
            usb_sms_receive,
            call_archive: proletarius_store::ArchiveStore::new(root.join("call-log.json")),
            call_facts: crate::call_facts::CallFacts::new(
                proletarius_store::ArchiveStore::new(root.join("call-log.json")),
                "call-producer-fixture".to_owned(),
            ),
            go_socket_path: PathBuf::new(),
            availability,
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
            modem_actor: modem_actor.clone(),
            qmi_enhancement: super::qmi_enhancement::QmiEnhancement::default(),
            network_traffic: super::network_traffic::Service::default(),
            usb_at: usb_at.clone(),
            module_radio,
        };

        state
            .native_vowifi_lifecycle
            .ordinary_admission()
            .set_go_producer("call-producer-fixture")
            .unwrap();
        for (path, method, body, public_socket, shutdown, authenticated, expected) in [
            (
                super::ordinary_vowifi_admission::PATH,
                "POST",
                r#"{"action":"acquire","producer_id":"stale-producer","source":"reader","token":"intent","sequence":1}"#,
                false,
                false,
                true,
                hyper::StatusCode::CONFLICT,
            ),
            (
                super::ordinary_vowifi_admission::PATH,
                "POST",
                r#"{"action":"acquire","producer_id":"call-producer-fixture","source":"reader","token":"intent","sequence":1}"#,
                false,
                false,
                true,
                hyper::StatusCode::OK,
            ),
            (
                super::ordinary_vowifi_admission::PATH,
                "POST",
                r#"{"action":"acquire","producer_id":"call-producer-fixture","source":"reader","token":"intent","sequence":1}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                super::ordinary_vowifi_admission::PATH,
                "POST",
                r#"{"action":"release","producer_id":"call-producer-fixture","source":"reader","token":"intent","sequence":1,"finished":true}"#,
                false,
                true,
                true,
                hyper::StatusCode::OK,
            ),
            (
                super::ordinary_vowifi_admission::PATH,
                "POST",
                r#"{"action":"acquire","producer_id":"call-producer-fixture","source":"reader","token":"intent","sequence":1}"#,
                false,
                false,
                true,
                hyper::StatusCode::CONFLICT,
            ),
            (
                super::ordinary_vowifi_admission::PATH,
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                super::ordinary_vowifi_admission::PATH,
                "POST",
                "{}",
                true,
                false,
                false,
                hyper::StatusCode::NOT_FOUND,
            ),
            (
                super::ordinary_vowifi_admission::PATH,
                "POST",
                "{}",
                true,
                false,
                true,
                hyper::StatusCode::NOT_FOUND,
            ),
            (
                "/api/internal/reader-device-identity/resolve",
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/ordinary-vowifi/stop",
                "POST",
                "{}",
                true,
                false,
                true,
                hyper::StatusCode::NOT_FOUND,
            ),
            (
                "/api/internal/ordinary-vowifi/stop",
                "POST",
                "{}",
                true,
                false,
                false,
                hyper::StatusCode::NOT_FOUND,
            ),
            (
                "/api/internal/reader-device-identity/resolve",
                "POST",
                "{}",
                false,
                false,
                true,
                hyper::StatusCode::OK,
            ),
            (
                "/api/internal/reader-device-identity/resolve",
                "GET",
                "",
                false,
                false,
                true,
                hyper::StatusCode::NOT_FOUND,
            ),
            (
                "/api/internal/reader-device-identity/resolve",
                "POST",
                "{}",
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/call-mode/backups/create",
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/call-mode/backups/restore-data",
                "POST",
                r#"{"backup_id":"usb-before-call-mode-20261004-120000.000000000-2345.json"}"#,
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/call-mode/backups/create",
                "POST",
                r#"{"schema_version":3,"kind":"proletarius-usb-composition-backup","reason":"before_call_mode","saved_at":"2026-10-04T12:00:00+08:00","module":{"imei":"123456789012345","firmware":"fixture-fw"},"usb":{"vendor_id":11388,"product_id":9216,"flags":[1,1,1,1,1,1,1]},"voice":{"ims":1,"volte_capability":1,"volte_disabled":0},"restore_command":"AT+QCFG=\"USBCFG\",0x2C7C,0x2400,1,1,1,1,1,1,1"}"#,
                false,
                false,
                true,
                hyper::StatusCode::CREATED,
            ),
            (
                "/api/internal/call-mode/backups/restore-data",
                "POST",
                r#"{"backup_id":"usb-before-call-mode-20261004-120000.000000000-2345.json"}"#,
                false,
                false,
                true,
                hyper::StatusCode::OK,
            ),
            (
                "/api/internal/call-mode/backups/restore-data",
                "POST",
                r#"{"backup_id":"usb-before-call-mode-20261004-120000.000000000-2345.json"}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/call-mode/backups/restore-data",
                "GET",
                "",
                false,
                false,
                true,
                hyper::StatusCode::NOT_FOUND,
            ),
            (
                "/api/internal/call-mode/backups/create",
                "POST",
                r#"{"schema_version":3,"kind":"proletarius-usb-composition-backup","reason":"before_call_mode","saved_at":"2026-10-04T12:00:00+08:00","module":{"imei":"123456789012345","firmware":"fixture-fw"},"usb":{"vendor_id":11388,"product_id":9216,"flags":[1,1,1,1,1,1,1]},"voice":{"ims":1,"volte_capability":1,"volte_disabled":0},"restore_command":"AT+QCFG=\"USBCFG\",0x2C7C,0x2400,1,1,1,1,1,1,1"}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/call-mode/backups/create",
                "GET",
                "",
                false,
                false,
                true,
                hyper::StatusCode::NOT_FOUND,
            ),
            (
                "/api/internal/module-radio/configure",
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/configure",
                "POST",
                "{}",
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/query",
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/query",
                "POST",
                "{}",
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/bootstrap",
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/bootstrap",
                "POST",
                "{}",
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/refresh",
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/refresh",
                "POST",
                "{}",
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/reservation",
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/reservation",
                "POST",
                "{}",
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/vowifi",
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/vowifi",
                "POST",
                "{}",
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/action",
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/action",
                "POST",
                "{}",
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/module-radio/configure",
                "POST",
                "{\"producer_id\":\"usb-producer-fixture\",\"config_seq\":1,\"transport\":\"none\",\"session_id\":\"\"}",
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/module-radio/action",
                "POST",
                "[]",
                false,
                false,
                true,
                hyper::StatusCode::BAD_REQUEST,
            ),
            (
                "/api/internal/module-radio/query",
                "GET",
                "",
                false,
                false,
                true,
                hyper::StatusCode::NOT_FOUND,
            ),
            (
                "/api/module/flight-mode",
                "GET",
                "",
                true,
                false,
                false,
                hyper::StatusCode::OK,
            ),
            (
                "/api/module/flight-mode",
                "POST",
                "{\"enabled\":\"yes\",\"user_initiated\":true}",
                true,
                false,
                false,
                hyper::StatusCode::BAD_REQUEST,
            ),
            (
                "/api/internal/sms/archive/clear",
                "POST",
                "",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/sms/archive/clear",
                "POST",
                "",
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/sms/archive/delete-ids",
                "POST",
                r#"{"ids":[]}"#,
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/sms/archive/delete-ids",
                "POST",
                r#"{"ids":[]}"#,
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/sms/archive/delete-ids",
                "POST",
                r#"{"ids":[]}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/usb-sms/storage-operation",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","lease_id":"storage-lease","action":{"kind":"usage"}}"#,
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/usb-sms/storage-operation",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","lease_id":"storage-lease","action":{"kind":"usage"}}"#,
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/usb-sms/storage-operation",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","lease_id":"storage-lease","action":{"kind":"usage"}}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/sms/archive/delete-ids",
                "POST",
                r#"{"ids":["not-a-stable-id"]}"#,
                false,
                false,
                true,
                hyper::StatusCode::BAD_REQUEST,
            ),
            (
                "/api/internal/sms/archive/delete-ids",
                "GET",
                "",
                false,
                false,
                true,
                hyper::StatusCode::NOT_FOUND,
            ),
            (
                "/api/internal/usb-sms/storage-operation",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","lease_id":"storage-lease","action":{"kind":"clear","storages":["SM;AT+CFUN=1"]}}"#,
                false,
                false,
                true,
                hyper::StatusCode::BAD_REQUEST,
            ),
            (
                "/api/internal/usb-sms/storage-operation",
                "GET",
                "",
                false,
                false,
                true,
                hyper::StatusCode::NOT_FOUND,
            ),
            (
                "/api/internal/usb-sms/inbox",
                "GET",
                "",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/usb-sms/inbox",
                "GET",
                "",
                false,
                false,
                true,
                hyper::StatusCode::OK,
            ),
            (
                "/api/internal/usb-sms/inbox",
                "GET",
                "",
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/usb-sms/inbox",
                "GET",
                "",
                true,
                false,
                true,
                hyper::StatusCode::OK,
            ),
            (
                "/api/internal/usb-sms/prune",
                "POST",
                r#"{"ids":[]}"#,
                false,
                false,
                true,
                hyper::StatusCode::OK,
            ),
            (
                "/api/internal/usb-sms/refresh",
                "POST",
                "{}",
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/usb-sms/refresh",
                "POST",
                "{}",
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/usb-sms/prune",
                "POST",
                r#"{"ids":[]}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/usb-at/status",
                "POST",
                r#"{"producer_id":"usb-producer-fixture"}"#,
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/usb-at/device-status",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","lease_id":"status-lease","suppress_cellular":true}"#,
                false,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/usb-at/device-status",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","lease_id":"status-lease","suppress_cellular":true}"#,
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/usb-at/device-status",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","lease_id":"status-lease","suppress_cellular":true}"#,
                false,
                false,
                true,
                hyper::StatusCode::CONFLICT,
            ),
            (
                "/api/internal/usb-at/device-status",
                "POST",
                r#"{"producer_id":"old-producer","session_id":"not-open","lease_id":"status-lease","suppress_cellular":true}"#,
                false,
                false,
                true,
                hyper::StatusCode::CONFLICT,
            ),
            (
                "/api/internal/usb-at/device-status",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","lease_id":"status-lease"}"#,
                false,
                false,
                true,
                hyper::StatusCode::BAD_REQUEST,
            ),
            (
                "/api/internal/usb-at/device-status",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","lease_id":"status-lease","suppress_cellular":true,"command":"AT+CFUN=1"}"#,
                false,
                false,
                true,
                hyper::StatusCode::BAD_REQUEST,
            ),
            (
                "/api/internal/usb-at/device-status",
                "GET",
                "{}",
                false,
                false,
                true,
                hyper::StatusCode::METHOD_NOT_ALLOWED,
            ),
            (
                "/api/internal/usb-at/device-status",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","lease_id":"status-lease","suppress_cellular":true}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/usb-at/status",
                "POST",
                r#"{"producer_id":"usb-producer-fixture"}"#,
                true,
                false,
                false,
                hyper::StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/internal/usb-at/status",
                "POST",
                r#"{"producer_id":"usb-producer-fixture"}"#,
                true,
                false,
                true,
                hyper::StatusCode::OK,
            ),
            (
                "/api/internal/usb-at/status",
                "POST",
                r#"{"producer_id":"old-producer"}"#,
                false,
                false,
                true,
                hyper::StatusCode::CONFLICT,
            ),
            (
                "/api/internal/usb-at/status",
                "POST",
                r#"{"producer_id":"usb-producer-fixture"}"#,
                false,
                true,
                true,
                hyper::StatusCode::OK,
            ),
            (
                "/api/internal/usb-at/configure",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","action":"sms","config_seq":1,"serial":false,"poll_interval_ms":8000}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/usb-at/command",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","command":"AT+CMGS=4","timeout_ms":3000}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/usb-at/command",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","command":"AT+CGLA=1,4,0000","timeout_ms":3000}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/usb-at/command",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"not-open","command":"AT+CFUN?","timeout_ms":3000}"#,
                false,
                true,
                true,
                hyper::StatusCode::CONFLICT,
            ),
            (
                "/api/internal/usb-at/open",
                "POST",
                r#"{"producer_id":"usb-producer-fixture"}"#,
                false,
                true,
                true,
                hyper::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "/api/internal/usb-at/acquire",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"","lease_id":"voice-cleanup","lease_seq":1,"purpose":"voice"}"#,
                false,
                true,
                true,
                hyper::StatusCode::OK,
            ),
            (
                "/api/internal/usb-at/release",
                "POST",
                r#"{"producer_id":"usb-producer-fixture","session_id":"","lease_id":"voice-cleanup","lease_seq":1}"#,
                false,
                true,
                true,
                hyper::StatusCode::OK,
            ),
        ] {
            let (client, server) = UnixStream::pair().unwrap();
            let state = state.clone();
            let server_task = if shutdown {
                tokio::spawn(async move {
                    super::serve_internal_shutdown_connection(server, state).await;
                })
            } else if public_socket {
                tokio::spawn(async move {
                    let _ = super::serve_connection(server, state).await;
                })
            } else {
                tokio::spawn(async move {
                    let _ = super::serve_internal_connection(server, state).await;
                })
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
            if authenticated {
                request = request.header("x-switch-internal-token", "internal-fixture");
            }
            let response = sender
                .send_request(request.body(Full::new(Bytes::from(body))).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let response_body = response.into_body().collect().await.unwrap().to_bytes();
            let expected = if public_socket && !shutdown && path.starts_with("/api/internal/") {
                hyper::StatusCode::NOT_FOUND
            } else {
                expected
            };
            assert_eq!(
                status,
                expected,
                "internal matrix path={path} method={method} public={public_socket} shutdown={shutdown} authenticated={authenticated} body={}",
                String::from_utf8_lossy(&response_body),
            );
            drop(sender);
            let _ = connection_task.await;
            server_task.await.unwrap();
        }

        assert!(!root.join("sms-archive.json").exists());
        assert!(
            root.join(
                "multi/module-backups/usb-before-call-mode-20261004-120000.000000000-2345.json"
            )
            .is_file()
        );
        let identity: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join("multi/imei.json"))
                .expect("Rust identity owner persisted first-use IMEI"),
        )
        .expect("valid compatible IMEI config");
        assert_eq!(identity["imei"].as_str().unwrap().len(), 15);
        usb_at
            .shutdown()
            .await
            .expect("checked inert USB owner close");
        modem_actor
            .shutdown()
            .expect("actual modem Close acknowledgement");
        drop(state);
        drop(usb_at);
        drop(modem_actor);
        drop(lifetime);
        fs::remove_dir_all(root).expect("remove Switch/build test root");
    }

    #[tokio::test]
    async fn retired_multistandby_ipc_is_rejected_without_persistence() {
        use bytes::Bytes;
        use http_body_util::{BodyExt, Full};
        use hyper_util::rt::TokioIo;
        use std::sync::Arc;
        use tokio::net::UnixStream;

        let root = test_directory("multi-runtime-owner-auth");
        let sms_inbox = crate::message_archive::InboxState::default();
        let journal_path = root.join("multi-runtime-events.json");
        let multi_runtime_report = Arc::new(std::sync::Mutex::new(
            super::multistandby_runtime_report::Journal::open(&journal_path, super::unix_now_ms()),
        ));
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
            multi_runtime_report,
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
