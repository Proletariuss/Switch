//! Generation-bound cross-process admission plus native physical card access.
//! Rust owns admission, immutable identity, expiry and physical cleanup.
//! Swift remains the narrow CoreBluetooth transport bridge.

use std::{path::Path, sync::Arc, time::Duration};

use proletarius_domain::BackendFailure;
use proletarius_vowifi::SimAkaResult;
use serde::{Deserialize, Serialize};
use tokio::{sync::watch, task::JoinHandle, time::sleep};
use zeroize::Zeroizing;

mod coordinator;

pub(crate) type PreAdmissionFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64, String>> + Send>>;
pub(crate) type AdmissionFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
pub(crate) type SupplierFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<EsimSwitchFinalizeResponse, String>> + Send>,
>;
/// Native lifecycle facts and finalization, with no compatibility RPC loop.
/// snapshot is a short metadata operation; finalize owns its actual physical
/// work until the future returns, including after its public waiter cancels.
pub(crate) trait NativeSessionSupplier: Send + Sync {
    fn capture_reader_maintenance(
        &self,
        _reader: &str,
        _generation: u64,
        _policy: crate::reader_reservation::NativePinPolicy,
        _ordinary: Option<&Arc<crate::ordinary_vowifi_admission::Claim>>,
    ) -> Result<Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>, BackendFailure> {
        Ok(None)
    }
    fn drain_reader_maintenance(
        self: Arc<Self>,
        _maintenance: Arc<crate::ordinary_vowifi_admission::Maintenance>,
    ) -> AdmissionFuture {
        Box::pin(async { Err("reader maintenance drain is unavailable".into()) })
    }
    /// Validate only; submitted work belongs to drain_reader_maintenance.
    fn pre_admit(
        self: Arc<Self>,
        reader: String,
        policy: crate::reader_reservation::NativePinPolicy,
        ordinary: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
        _maintenance: Option<Arc<crate::ordinary_vowifi_admission::Maintenance>>,
    ) -> PreAdmissionFuture;
    fn admit(
        self: Arc<Self>,
        binding: crate::reader_reservation::NativePinBinding,
    ) -> AdmissionFuture;
    fn snapshot(
        &self,
        binding: &crate::reader_reservation::NativePinBinding,
    ) -> Result<serde_json::Value, String>;
    fn finalize(
        self: Arc<Self>,
        binding: crate::reader_reservation::NativePinBinding,
        context: Option<NativeCardContext>,
        target: String,
        aid: String,
    ) -> SupplierFuture;
}

/// Tracks only the caller's actual preparation/lease, not unrelated card owners.
#[derive(Clone)]
pub(crate) struct AcquisitionCleanup(Arc<coordinator::AcquisitionReceipt>);
impl AcquisitionCleanup {
    pub(crate) fn new(gate: Arc<crate::reader_reservation::Gate>) -> Self {
        Self(coordinator::AcquisitionReceipt::new(gate))
    }
    pub(crate) fn confirmed(&self) -> bool {
        self.0.confirmed()
    }
}

const LEASE_TIMEOUT: Duration = Duration::from_secs(16);
// The compatibility PC/SC owner permits up to 130 seconds for release.
// Do not time out before it can acknowledge its ordered physical cleanup.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(135);
const ESIM_SWITCH_FINALIZE_TIMEOUT: Duration = Duration::from_secs(100);
const AKA_TIMEOUT: Duration = Duration::from_secs(40);
const LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(30);
const LEASE_RENEW_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const LEASE_RENEW_RETRIES: usize = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct EsimSwitchFinalizeResponse {
    pub(crate) target_identity_confirmed: bool,
    #[serde(default)]
    pub(crate) reader_refresh_warning: String,
    #[serde(default)]
    pub(crate) reader_refresh_warning_code: String,
    pub(crate) reader_info: serde_json::Value,
}

#[derive(Deserialize)]
struct AcquireResponse {
    #[serde(default)]
    lease_id: String,
    reader: String,
    reader_generation: u64,
    #[serde(default)]
    esim_manager_available: Option<bool>,
    identity: LeaseIdentityWire,
    #[serde(default)]
    notification_aids: Vec<String>,
    #[serde(default)]
    // The coordinator retains the native binding before parsing the snapshot,
    // so malformed facts cannot erase the cleanup authority.
    pcsc: Option<serde_json::Value>,
    #[serde(default)]
    session: Option<crate::reader_pcsc::LifecycleBinding>,
}

#[derive(Deserialize)]
struct LeaseIdentityWire {
    iccid: String,
    imsi: String,
    mcc: String,
    mnc: String,
    #[serde(default)]
    gid1: String,
    #[serde(default)]
    gid2: String,
    #[serde(default)]
    spn: String,
    #[serde(default)]
    private_identity: Zeroizing<String>,
    #[serde(default)]
    home_domain: Zeroizing<String>,
    #[serde(default)]
    phone_number: Zeroizing<String>,
    #[serde(default)]
    smsc: Zeroizing<String>,
    #[serde(default)]
    smsc_psi: Zeroizing<String>,
    #[serde(default)]
    smsc_target: Zeroizing<String>,
    #[serde(default)]
    public_identity: Zeroizing<String>,
}

pub(crate) struct LeaseIdentity {
    pub(crate) iccid: Zeroizing<String>,
    pub(crate) imsi: Zeroizing<String>,
    pub(crate) mcc: String,
    pub(crate) mnc: String,
    pub(crate) gid1: String,
    pub(crate) gid2: String,
    pub(crate) spn: String,
    // Prepared as sensitive, generation-bound inputs for the native IMS
    // session owner. Keep zeroizing custody across this boundary; the live
    // session route is not switched yet, so these are intentionally unused.
    #[allow(dead_code)]
    pub(crate) private_identity: Zeroizing<String>,
    #[allow(dead_code)]
    pub(crate) public_identity: Zeroizing<String>,
    pub(crate) home_domain: Zeroizing<String>,
    pub(crate) phone_number: Zeroizing<String>,
    pub(crate) smsc: Zeroizing<String>,
    pub(crate) smsc_psi: Zeroizing<String>,
    pub(crate) smsc_target: Zeroizing<String>,
}

impl LeaseIdentity {
    pub(crate) fn from_metadata(value: serde_json::Value) -> Result<Self, String> {
        let v: LeaseIdentityWire = serde_json::from_value(value)
            .map_err(|_| "invalid native subscriber identity".to_owned())?;
        if v.iccid.is_empty()
            || v.imsi.is_empty()
            || v.mcc.len() != 3
            || !(2..=3).contains(&v.mnc.len())
        {
            return Err("incomplete native subscriber identity".into());
        }
        Ok(Self {
            iccid: Zeroizing::new(v.iccid),
            imsi: Zeroizing::new(v.imsi),
            mcc: v.mcc,
            mnc: v.mnc,
            gid1: v.gid1,
            gid2: v.gid2,
            spn: v.spn,
            private_identity: v.private_identity,
            public_identity: v.public_identity,
            home_domain: v.home_domain,
            phone_number: v.phone_number,
            smsc: v.smsc,
            smsc_psi: v.smsc_psi,
            smsc_target: v.smsc_target,
        })
    }
}

/// Immutable caller view of a lease managed by the retained Rust coordinator.
/// Dropping this view requests drain; it never drops the authoritative Gate owner.
pub(crate) struct NativeReaderSessionLease {
    coordinator: Arc<coordinator::Coordinator>,
    app_token: Option<Zeroizing<String>>,
    pub(crate) reader: String,
    pub(crate) reader_generation: u64,
    pub(crate) esim_manager_available: Option<bool>,
    pub(crate) identity: LeaseIdentity,
    pub(crate) notification_aids: Vec<String>,
    pcsc: Option<crate::reader_pcsc::Binding>,
    native_owner: Option<crate::reader_pcsc::Owner>,
}
impl Drop for NativeReaderSessionLease {
    fn drop(&mut self) {
        self.coordinator.drain();
    }
}

/// Borrowed physical authority returned by the native PC/SC owner. The Go
/// session token coordinates application state only; it is never substituted
/// for this generation-bound physical lease.
#[derive(Clone)]
pub(crate) struct NativeCardContext {
    owner: crate::reader_pcsc::Owner,
    binding: crate::reader_pcsc::Binding,
}

impl NativeCardContext {
    pub(crate) fn reset(
        &self,
        action: crate::reader_pcsc::ResetAction,
    ) -> Result<serde_json::Value, crate::reader_pcsc::Failure> {
        self.owner.native_reset(&self.binding, action)
    }
    pub(crate) fn binding(&self) -> &crate::reader_pcsc::Binding {
        &self.binding
    }
    pub(crate) fn with_transaction<T>(
        &self,
        operation: impl FnOnce(&mut crate::reader_pcsc::NativeTransport) -> T,
    ) -> Result<T, crate::reader_pcsc::Failure> {
        self.owner
            .with_transaction(Some(&self.binding), &self.binding.reader, operation)
    }
}

impl NativeReaderSessionLease {
    fn physical_work_guard(
        &self,
    ) -> Result<crate::reader_reservation::NativeWorkerPermit, crate::reader_task::ReaderTaskError>
    {
        self.coordinator.work_guard()
    }

    async fn queued_physical_work_guard(
        &self,
    ) -> Result<
        (
            crate::reader_reservation::NativeWorkerPermit,
            coordinator::PhysicalTransactionPermit,
        ),
        crate::reader_task::ReaderTaskError,
    > {
        // Wait before registering physical work, then revalidate the lease.
        // A drain or expiry that occurs while queued must prevent dispatch.
        let serial = self.coordinator.transaction_guard().await?;
        let physical = self.physical_work_guard()?;
        // The worker marker is the first tuple member so it is dropped before
        // the serial mutex guard. The next queued transaction must not acquire
        // the FIFO permit while the previous worker still appears active.
        Ok((physical, serial))
    }

    pub(crate) async fn run_card_worker<F, T>(
        &self,
        pool: &crate::blocking_pool::BlockingPool,
        operation: F,
    ) -> Result<T, crate::reader_task::ReaderTaskError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (physical, serial) = self.queued_physical_work_guard().await?;
        crate::reader_task::run_in_pool(pool, Some((physical, serial)), operation).await
    }

    /// Registration metadata uses the existing FIFO/physical worker fence.
    /// Only waiting for admission is bounded; an actual filesystem commit is
    /// awaited to completion and continues to block lease retirement.
    pub(crate) async fn run_registration_metadata_worker<F, T>(
        &self,
        pool: &crate::blocking_pool::BlockingPool,
        operation: F,
    ) -> Result<T, crate::reader_task::ReaderTaskError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (physical, serial) =
            tokio::time::timeout(Duration::from_secs(3), self.queued_physical_work_guard())
                .await
                .map_err(|_| crate::reader_task::ReaderTaskError::Busy)??;
        crate::reader_task::run_in_pool(pool, Some((physical, serial)), operation).await
    }

    pub(crate) async fn acquire(
        socket_path: &Path,
        app_token: Option<&str>,
        reader: &str,
        gate: Arc<crate::reader_reservation::Gate>,
        owner: crate::reader_pcsc::Owner,
    ) -> Result<Self, BackendFailure> {
        Self::acquire_with_policy(
            socket_path,
            app_token,
            reader,
            gate,
            owner,
            crate::reader_reservation::NativePinPolicy::Identity,
            None,
            None,
        )
        .await
    }
    pub(crate) async fn acquire_for_esim_mutation(
        socket_path: &Path,
        app_token: Option<&str>,
        reader: &str,
        gate: Arc<crate::reader_reservation::Gate>,
        owner: crate::reader_pcsc::Owner,
    ) -> Result<Self, BackendFailure> {
        Self::acquire_with_policy(
            socket_path,
            app_token,
            reader,
            gate,
            owner,
            crate::reader_reservation::NativePinPolicy::Mutation,
            None,
            None,
        )
        .await
    }
    pub(crate) async fn acquire_for_card_operation(
        socket_path: &Path,
        app_token: Option<&str>,
        reader: &str,
        gate: Arc<crate::reader_reservation::Gate>,
        owner: crate::reader_pcsc::Owner,
    ) -> Result<Self, BackendFailure> {
        Self::acquire_with_policy(
            socket_path,
            app_token,
            reader,
            gate,
            owner,
            crate::reader_reservation::NativePinPolicy::CardOnly,
            None,
            None,
        )
        .await
    }
    pub(crate) async fn acquire_for_ordinary(
        socket_path: &Path,
        app_token: Option<&str>,
        reader: &str,
        gate: Arc<crate::reader_reservation::Gate>,
        owner: crate::reader_pcsc::Owner,
        claim: Arc<crate::ordinary_vowifi_admission::Claim>,
        cleanup: AcquisitionCleanup,
    ) -> Result<Self, BackendFailure> {
        Self::acquire_with_policy(
            socket_path,
            app_token,
            reader,
            gate,
            owner,
            crate::reader_reservation::NativePinPolicy::Identity,
            Some(claim),
            Some(cleanup),
        )
        .await
    }
    async fn acquire_with_policy(
        socket_path: &Path,
        app_token: Option<&str>,
        reader: &str,
        gate: Arc<crate::reader_reservation::Gate>,
        owner: crate::reader_pcsc::Owner,
        policy: crate::reader_reservation::NativePinPolicy,
        ordinary: Option<Arc<crate::ordinary_vowifi_admission::Claim>>,
        cleanup: Option<AcquisitionCleanup>,
    ) -> Result<Self, BackendFailure> {
        let (coordinator, response) = coordinator::Coordinator::acquire(
            socket_path,
            app_token,
            reader,
            gate,
            owner.clone(),
            policy,
            ordinary,
            cleanup.map(|cleanup| cleanup.0),
        )
        .await?;
        let pcsc = response
            .pcsc
            .clone()
            .map(serde_json::from_value::<crate::reader_pcsc::Binding>)
            .transpose()
            .expect("coordinator validated physical binding before activation");
        Ok(Self {
            coordinator,
            app_token: app_token.map(|v| Zeroizing::new(v.to_owned())),
            reader: response.reader.clone(),
            reader_generation: response.reader_generation,
            esim_manager_available: response.esim_manager_available,
            notification_aids: response.notification_aids.clone(),
            pcsc,
            native_owner: Some(owner),
            identity: LeaseIdentity {
                iccid: Zeroizing::new(response.identity.iccid.clone()),
                imsi: Zeroizing::new(response.identity.imsi.clone()),
                mcc: response.identity.mcc.clone(),
                mnc: response.identity.mnc.clone(),
                gid1: response.identity.gid1.clone(),
                gid2: response.identity.gid2.clone(),
                spn: response.identity.spn.clone(),
                private_identity: response.identity.private_identity.clone(),
                public_identity: response.identity.public_identity.clone(),
                home_domain: response.identity.home_domain.clone(),
                phone_number: response.identity.phone_number.clone(),
                smsc: response.identity.smsc.clone(),
                smsc_psi: response.identity.smsc_psi.clone(),
                smsc_target: response.identity.smsc_target.clone(),
            },
        })
    }

    pub(crate) fn pcsc_context(
        &self,
        _owner: crate::reader_pcsc::Owner,
    ) -> Result<NativeCardContext, BackendFailure> {
        let binding = self.pcsc.as_ref().ok_or_else(|| {
            BackendFailure::GoBackendUnavailable("reader lease lacks native PC/SC authority".into())
        })?;
        binding.validate_for_reader(&self.reader).map_err(|_| {
            BackendFailure::GoBackendUnavailable(
                "reader lease has invalid native PC/SC authority".into(),
            )
        })?;
        let owner = self.native_owner.clone().ok_or_else(|| {
            BackendFailure::GoBackendUnavailable("native physical owner is unavailable".into())
        })?;
        Ok(NativeCardContext {
            owner,
            binding: binding.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn set_release_timeout(&self, value: Duration) {
        self.coordinator.set_release_timeout(value);
    }
    pub(crate) async fn release(&self) -> Result<(), BackendFailure> {
        self.coordinator.release().await
    }
    pub(crate) async fn release_confirmed(&self) -> Result<bool, BackendFailure> {
        Ok(self.coordinator.released())
    }
    pub(crate) async fn release_with_confirmation(&self) -> Result<(), BackendFailure> {
        self.coordinator.release().await
    }
    pub(crate) async fn finalize_esim_switch(
        &self,
        target_iccid: &str,
        aid: &str,
    ) -> Result<EsimSwitchFinalizeResponse, BackendFailure> {
        self.coordinator.finalize(target_iccid, aid).await
    }
    pub(crate) async fn renew(&self) -> Result<(), BackendFailure> {
        self.coordinator.renew()
    }

    /// Run one AKA transaction under the generation-bound reader lease.
    /// PC/SC borrows the Rust physical owner; BLE submits APDUs through the
    /// authenticated Swift CoreBluetooth bridge. Identity and physical
    /// authority come directly from the native lifecycle supplier.
    pub(crate) async fn authenticate(
        &self,
        application: ReaderAkaApplication,
        rand: &[u8; 16],
        autn: &[u8; 16],
    ) -> Result<SimAkaResult, BackendFailure> {
        if self.pcsc.is_none() && self.reader.starts_with("BLE:") {
            let (serial, physical) = self.queued_physical_work_guard().await.map_err(|_| {
                BackendFailure::GoBackendUnavailable(
                    "reader lease authority is no longer current".into(),
                )
            })?;
            return authenticate_ble_with_bridge(
                Some((serial, physical)),
                std::env::var_os("PROLETARIUS_BLE_SOCKET").map(std::path::PathBuf::from),
                self.app_token.as_deref().map(|value| value.as_str()),
                &self.reader,
                &self.identity.iccid,
                &self.identity.imsi,
                match application {
                    ReaderAkaApplication::Usim => proletarius_reader::AkaApplication::Usim,
                    ReaderAkaApplication::Isim => proletarius_reader::AkaApplication::Isim,
                },
                rand,
                autn,
            )
            .await;
        }
        let owner = self.native_owner.clone().ok_or_else(|| {
            BackendFailure::GoBackendUnavailable("native PC/SC lease has no owner binding".into())
        })?;
        let context = self.pcsc_context(owner)?;
        let iccid = self.identity.iccid.clone();
        let imsi = self.identity.imsi.clone();
        let rand = Zeroizing::new(*rand);
        let autn = Zeroizing::new(*autn);
        let application = match application {
            ReaderAkaApplication::Usim => proletarius_reader::AkaApplication::Usim,
            ReaderAkaApplication::Isim => proletarius_reader::AkaApplication::Isim,
        };
        tokio::time::timeout(
            AKA_TIMEOUT,
            self.run_card_worker(native_auth_pool(), move || {
                context
                    .with_transaction(|transport| {
                        proletarius_reader::authenticate_bound_with_transport(
                            transport,
                            application,
                            &iccid,
                            &imsi,
                            &*rand,
                            &*autn,
                        )
                        .map(|result| match result {
                            proletarius_sim::AkaResult::Success { res, ck, ik } => {
                                SimAkaResult::Success { res, ck, ik }
                            }
                            proletarius_sim::AkaResult::SynchronizationFailure { auts } => {
                                SimAkaResult::SynchronizationFailure { auts }
                            }
                            proletarius_sim::AkaResult::AuthenticationReject => {
                                SimAkaResult::AuthenticationRejected
                            }
                        })
                    })
                    .map_err(|error| BackendFailure::GoBackendUnavailable(error.code.into()))?
                    .map_err(|error| {
                        BackendFailure::GoBackendUnavailable(error.failure_code().into())
                    })
            }),
        )
        .await
        .map_err(|_| {
            BackendFailure::GoBackendUnavailable("native reader authentication timed out".into())
        })?
        .map_err(|_| {
            BackendFailure::GoBackendUnavailable(
                "native reader authentication task did not complete".into(),
            )
        })?
    }

    #[allow(
        dead_code,
        reason = "used when the gated native IMS runtime takes reader ownership"
    )]
    pub(crate) async fn authenticate_ims(
        &self,
        rand: &[u8; 16],
        autn: &[u8; 16],
    ) -> Result<SimAkaResult, BackendFailure> {
        self.authenticate(ReaderAkaApplication::Isim, rand, autn)
            .await
    }

    #[allow(
        dead_code,
        reason = "used by the gated native IKE actor after it takes session ownership"
    )]
    pub(crate) async fn authenticate_ike(
        &self,
        rand: &[u8; 16],
        autn: &[u8; 16],
    ) -> Result<SimAkaResult, BackendFailure> {
        self.authenticate(ReaderAkaApplication::Usim, rand, autn)
            .await
    }
}

fn native_auth_pool() -> &'static crate::blocking_pool::BlockingPool {
    static POOL: std::sync::OnceLock<crate::blocking_pool::BlockingPool> =
        std::sync::OnceLock::new();
    POOL.get_or_init(|| crate::blocking_pool::BlockingPool::new(8))
}

async fn authenticate_ble_with_bridge<P: Send + 'static>(
    physical: Option<P>,
    socket: Option<std::path::PathBuf>,
    token: Option<&str>,
    reader: &str,
    iccid: &str,
    imsi: &str,
    application: proletarius_reader::AkaApplication,
    rand: &[u8; 16],
    autn: &[u8; 16],
) -> Result<SimAkaResult, BackendFailure> {
    let socket = socket;
    let token = token.map(|value| Zeroizing::new(value.to_owned()));
    let reader = reader.to_owned();
    let iccid = Zeroizing::new(iccid.to_owned());
    let imsi = Zeroizing::new(imsi.to_owned());
    let rand = Zeroizing::new(*rand);
    let autn = Zeroizing::new(*autn);
    static POOL: std::sync::OnceLock<crate::blocking_pool::BlockingPool> =
        std::sync::OnceLock::new();
    tokio::time::timeout(
        AKA_TIMEOUT,
        crate::reader_task::run_in_pool(
            POOL.get_or_init(|| crate::blocking_pool::BlockingPool::new(8)),
            physical,
            move || {
                let mut transport = crate::reader_ble_apdu::BleApduTransport::connect(
                    socket.as_deref(),
                    token.as_deref().map(|value| value.as_str()),
                    &reader,
                )
                .map_err(|error| BackendFailure::Io(format!("BLE AKA bridge: {error}")))?;
                proletarius_reader::authenticate_bound_with_transport(
                    &mut transport,
                    application,
                    &iccid,
                    &imsi,
                    &*rand,
                    &*autn,
                )
                .map(|result| match result {
                    proletarius_sim::AkaResult::Success { res, ck, ik } => {
                        SimAkaResult::Success { res, ck, ik }
                    }
                    proletarius_sim::AkaResult::SynchronizationFailure { auts } => {
                        SimAkaResult::SynchronizationFailure { auts }
                    }
                    proletarius_sim::AkaResult::AuthenticationReject => {
                        SimAkaResult::AuthenticationRejected
                    }
                })
                .map_err(|error| BackendFailure::Io(format!("BLE AKA failed: {error}")))
            },
        ),
    )
    .await
    .map_err(|_| {
        BackendFailure::Io(
            "BLE AKA worker timed out; card ownership is retained until physical completion".into(),
        )
    })?
    .map_err(|error| BackendFailure::Io(format!("BLE AKA worker did not complete: {error:?}")))?
}

/// Keeps the generation-bound Rust lease alive for the lifetime of a native
/// tunnel without a Go renewal RPC. Once local renewal is rejected, `lost`
/// becomes terminal so the session owner can tear down IKE/IMS instead
/// of presenting a stale online state. The caller must retain this guard for
/// the entire session, not merely through initial AKA.
pub(crate) struct NativeReaderSessionHeartbeat {
    stop: watch::Sender<bool>,
    lost: watch::Receiver<bool>,
    task: Option<JoinHandle<()>>,
}

impl NativeReaderSessionHeartbeat {
    pub(crate) fn start(lease: Arc<NativeReaderSessionLease>) -> Self {
        Self::start_with_timing(
            lease,
            LEASE_RENEW_INTERVAL,
            LEASE_RENEW_RETRY_INTERVAL,
            LEASE_RENEW_RETRIES,
        )
    }

    fn start_with_timing(
        lease: Arc<NativeReaderSessionLease>,
        interval: Duration,
        retry_interval: Duration,
        retries: usize,
    ) -> Self {
        let (stop, mut stop_rx) = watch::channel(false);
        let (lost_tx, lost) = watch::channel(false);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sleep(interval) => {}
                    changed = stop_rx.changed() => {
                        if changed.is_err() || *stop_rx.borrow() {
                            return;
                        }
                    }
                }
                if *stop_rx.borrow() {
                    return;
                }

                let mut renewed = false;
                for attempt in 0..=retries {
                    let result = tokio::select! {
                        changed = stop_rx.changed() => {
                            if changed.is_err() || *stop_rx.borrow() {
                                return;
                            }
                            continue;
                        }
                        result = lease.renew() => result,
                    };
                    if result.is_ok() {
                        renewed = true;
                        break;
                    }
                    if attempt < retries {
                        tokio::select! {
                            _ = sleep(retry_interval) => {}
                            changed = stop_rx.changed() => {
                                if changed.is_err() || *stop_rx.borrow() {
                                    return;
                                }
                            }
                        }
                        if *stop_rx.borrow() {
                            return;
                        }
                    }
                }
                if !renewed {
                    let _ = lost_tx.send(true);
                    return;
                }
            }
        });
        Self {
            stop,
            lost,
            task: Some(task),
        }
    }

    #[allow(dead_code, reason = "observed by the keyed native session actor")]
    pub(crate) fn lost_receiver(&self) -> watch::Receiver<bool> {
        self.lost.clone()
    }

    pub(crate) fn is_lost(&self) -> bool {
        *self.lost.borrow()
    }

    pub(crate) async fn wait_lost(&mut self) {
        while !self.is_lost() {
            if self.lost.changed().await.is_err() {
                return;
            }
        }
    }

    pub(crate) async fn stop(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for NativeReaderSessionHeartbeat {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderAkaApplication {
    Usim,
    Isim,
}

fn valid_digits(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_notification_aids(aids: &[String]) -> bool {
    aids.len() <= 256
        && aids.iter().all(|aid| {
            (2..=510).contains(&aid.len())
                && aid.len().is_multiple_of(2)
                && aid.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener as StdUnixListener,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    const BLE_READER: &str = "BLE:12345678-1234-1234-1234-123456789abc";

    fn fake_ble_aka_responses() -> Vec<Vec<u8>> {
        let aid = [0xa0, 0, 0, 0, 0x87, 0x10, 2, 0xff, 1];
        let imsi = [0x08, 0x29, 0x43, 0x01, 0x80, 0x69, 0x93, 0x92, 0x68];
        let iccid = [0x98, 0x10, 0x32, 0x54, 0x76, 0x98, 0x10, 0x32, 0x54, 0xf6];
        let mut dir = vec![0x61, 11, 0x4f, 9];
        dir.extend_from_slice(&aid);
        dir.extend_from_slice(&[0x90, 0]);
        let mut success = vec![0xdb, 8];
        success.extend([1; 8]);
        success.push(16);
        success.extend([2; 16]);
        success.push(16);
        success.extend([3; 16]);
        success.extend([0x90, 0]);
        vec![
            vec![0x90, 0],
            vec![0x90, 0],
            dir.clone(),
            vec![0x90, 0],
            vec![0x90, 0],
            vec![8, 0x90, 0],
            [imsi.as_slice(), &[0x90, 0]].concat(),
            vec![0x6a, 0x82],
            vec![0x90, 0],
            vec![0x90, 0],
            vec![0x90, 0],
            [iccid.as_slice(), &[0x90, 0]].concat(),
            vec![0x90, 0],
            vec![0x90, 0],
            dir,
            vec![0x90, 0],
            success,
        ]
    }

    fn fake_ble_socket_path() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("sbl-{}-{unique:x}.sock", std::process::id()))
    }

    fn start_fake_ble_bridge(
        path: &Path,
        apdu_responses: Vec<Vec<u8>>,
    ) -> thread::JoinHandle<Vec<serde_json::Value>> {
        let listener = StdUnixListener::bind(path).unwrap();
        thread::spawn(move || {
            let mut requests = Vec::with_capacity(apdu_responses.len() + 1);
            for response in std::iter::once(None).chain(apdu_responses.iter().map(Some)) {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let operation_id = request["operation_id"].clone();
                let bridge_id = request["bridge_id"].clone();
                requests.push(request);
                let result = match response {
                    None => serde_json::json!({
                        "ok": true,
                        "reader": BLE_READER,
                        "state": "connected",
                        "operation_tracking": true, "bridge_id": "fixture-bridge"
                    }),
                    Some(bytes) => serde_json::json!({
                        "ok": true,
                        "hex": bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
                        "operation_id": operation_id, "bridge_id": bridge_id, "terminal": true
                    }),
                };
                serde_json::to_writer(&mut stream, &result).unwrap();
                stream.write_all(b"\n").unwrap();
            }
            requests
        })
    }

    #[tokio::test]
    async fn leased_ble_aka_runs_through_rust_apdu_bridge() {
        let socket_path = fake_ble_socket_path();
        let bridge = start_fake_ble_bridge(&socket_path, fake_ble_aka_responses());
        let rand = [0x11; 16];
        let autn = [0x22; 16];
        let result = authenticate_ble_with_bridge(
            Some(
                Arc::new(crate::reader_reservation::Gate::default())
                    .track_ble_io(BLE_READER)
                    .unwrap(),
            ),
            Some(socket_path.clone()),
            Some("0123456789abcdef0123456789abcdef"),
            BLE_READER,
            "8901234567890123456",
            "234100896392986",
            proletarius_reader::AkaApplication::Usim,
            &rand,
            &autn,
        )
        .await
        .unwrap();
        assert!(matches!(
            result,
            SimAkaResult::Success { ref res, ref ck, ref ik }
                if res.as_slice() == &[1; 8] && ck.as_slice() == &[2; 16] && ik.as_slice() == &[3; 16]
        ));
        let requests = bridge.join().unwrap();
        assert_eq!(requests.len(), 18, "one status request plus 17 APDUs");
        assert_eq!(requests[0]["action"], "status");
        assert!(requests[0]["reader"].is_null());
        assert_eq!(requests[0]["token"], "0123456789abcdef0123456789abcdef");
        assert!(
            requests[1..]
                .iter()
                .all(|request| request["action"] == "apdu" && request["reader"] == BLE_READER)
        );
        assert!(
            requests[1..]
                .iter()
                .all(|request| request["token"] == "0123456789abcdef0123456789abcdef")
        );
        assert!(requests[1..].iter().any(|request| {
            request["hex"]
                .as_str()
                .is_some_and(|apdu| apdu.starts_with("0088") || apdu.contains("008800812210"))
        }));
        std::fs::remove_file(socket_path).unwrap();
    }

    #[test]
    fn malformed_card_identity_is_rejected() {
        assert!(!valid_digits("234x", 3, 3));
        assert!(!valid_digits("", 5, 15));
        assert!(valid_digits("234100896392986", 5, 15));
    }

    #[test]
    fn acquire_contract_distinguishes_manager_available_unavailable_and_legacy() {
        let base = serde_json::json!({
            "lease_id": "a".repeat(64),
            "reader": "Reader A",
            "reader_generation": 1,
            "identity": {"iccid":"", "imsi":"", "mcc":"", "mnc":""}
        });
        for (field, expected) in [
            (Some(true), Some(true)),
            (Some(false), Some(false)),
            (None, None),
        ] {
            let mut response = base.clone();
            if let Some(value) = field {
                response["esim_manager_available"] = serde_json::Value::Bool(value);
            }
            let decoded: AcquireResponse = serde_json::from_value(response).unwrap();
            assert_eq!(decoded.esim_manager_available, expected);
        }
    }
}
