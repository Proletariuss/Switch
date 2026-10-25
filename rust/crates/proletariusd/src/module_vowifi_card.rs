//! Retained module SIM + RF capability. RF ownership outlives every submitted
//! identity/AKA transaction, and a failed restore remains retryable on this owner.
use crate::modem_actor::{ModemActor, SerialModuleHold};
use crate::native_vowifi_session::card::{CardFuture, OnDemandCard, SessionKind};
use crate::reader_session_lease::{LeaseIdentity, ReaderAkaApplication};
use crate::serial_module_identity::SerialModuleIdentity;
use crate::{
    module_radio::{NativeVoWiFiIntent, Service},
    usb_at::{NativeModuleIdentity, Owner},
};
use proletarius_domain::BackendFailure;
use proletarius_vowifi::SimAkaResult;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;

/// Stable physical identity; SIM identity is compared separately on reconnect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModulePhysicalIdentity {
    Usb {
        producer: String,
        session: String,
    },
    Serial {
        owner_epoch: String,
        serial_session: u64,
    },
}
enum ModuleIdentity {
    Usb(NativeModuleIdentity),
    Serial(SerialModuleIdentity),
}
impl ModuleIdentity {
    fn metadata(&self) -> serde_json::Value {
        match self {
            Self::Usb(v) => v.metadata(),
            Self::Serial(v) => v.metadata(),
        }
    }
    fn generation(&self) -> u64 {
        match self {
            Self::Usb(v) => v.generation(),
            Self::Serial(v) => v.generation(),
        }
    }
}
pub(crate) struct ModuleSessionCard {
    usb: Owner,
    radio: Service,
    intent: OnceLock<NativeVoWiFiIntent>,
    binding: OnceLock<ModuleIdentity>,
    serial: Option<ModemActor>,
    serial_hold: OnceLock<SerialModuleHold>,
    identity: OnceLock<LeaseIdentity>,
    operation: tokio::sync::Mutex<()>,
    physical_held: AtomicBool,
    prepare_rejected_stale: AtomicBool,
    prepare_not_admitted: AtomicBool,
    #[cfg(test)]
    revoke_after_hold: Mutex<Option<crate::module_recovery::Gate>>,
    #[cfg(test)]
    revoke_after_prepare: Mutex<Option<crate::module_recovery::Gate>>,
    stopping: AtomicBool,
    released: AtomicBool,
    lost: watch::Sender<bool>,
    ready: watch::Sender<Option<Result<(), &'static str>>>,
    status: Mutex<ModuleCardStatus>,
}
#[derive(Clone, Copy)]
pub(crate) struct ModuleCardStatus {
    pub(crate) phase: &'static str,
    pub(crate) ready: bool,
    pub(crate) cleanup_confirmed: bool,
    pub(crate) restoration_skipped_reader_lost: bool,
    pub(crate) code: Option<&'static str>,
    pub(crate) cfun_mode: Option<u8>,
    pub(crate) original_mode: Option<u8>,
}
fn failure(code: &'static str) -> BackendFailure {
    BackendFailure::ReaderAdmissionRejected { code }
}
impl ModuleSessionCard {
    /// Caller retains this Arc before awaiting readiness, including on failure.
    /// Transport retirement must first stop/drain the module network owner and
    /// await this capability's real cleanup. The captured RF route selects the SIM.
    pub(crate) fn begin_prepare(
        usb: Owner,
        radio: Service,
        stop: watch::Receiver<bool>,
    ) -> Arc<Self> {
        Self::begin_prepare_inner(usb, radio, None, stop, None)
    }
    pub(crate) fn begin_prepare_with_serial(
        usb: Owner,
        radio: Service,
        serial: ModemActor,
        stop: watch::Receiver<bool>,
    ) -> Arc<Self> {
        Self::begin_prepare_inner(usb, radio, Some(serial), stop, None)
    }
    /// Offline race fixture; uses the production hold, prepare and cleanup worker.
    #[cfg(test)]
    pub(crate) fn begin_prepare_with_serial_intent(
        usb: Owner,
        radio: Service,
        serial: ModemActor,
        intent: NativeVoWiFiIntent,
        stop: watch::Receiver<bool>,
    ) -> Arc<Self> {
        Self::begin_prepare_inner(usb, radio, Some(serial), stop, Some(intent))
    }
    fn begin_prepare_inner(
        usb: Owner,
        radio: Service,
        serial: Option<ModemActor>,
        stop: watch::Receiver<bool>,
        captured_intent: Option<NativeVoWiFiIntent>,
    ) -> Arc<Self> {
        let card = Arc::new(Self {
            usb,
            radio,
            serial,
            serial_hold: OnceLock::new(),
            intent: OnceLock::new(),
            binding: OnceLock::new(),
            identity: OnceLock::new(),
            operation: tokio::sync::Mutex::new(()),
            physical_held: AtomicBool::new(false),
            prepare_rejected_stale: AtomicBool::new(false),
            prepare_not_admitted: AtomicBool::new(false),
            #[cfg(test)]
            revoke_after_hold: Mutex::new(None),
            #[cfg(test)]
            revoke_after_prepare: Mutex::new(None),
            stopping: AtomicBool::new(false),
            released: AtomicBool::new(false),
            lost: watch::channel(false).0,
            ready: watch::channel(None).0,
            status: Mutex::new(ModuleCardStatus {
                phase: "preparing",
                ready: false,
                cleanup_confirmed: false,
                restoration_skipped_reader_lost: false,
                code: None,
                cfun_mode: None,
                original_mode: None,
            }),
        });
        let worker = card.clone();
        let preparation_stop = stop.clone();
        tokio::spawn(async move {
            let _operation = worker.operation.lock().await;
            let result = worker.prepare(&preparation_stop, captured_intent).await;
            if let Err(code) = result {
                worker.mark_lost();
                let cleanup = worker.restore_locked().await;
                let code = cleanup.err().unwrap_or(code);
                worker.status.lock().unwrap().code = Some(code);
                worker.ready.send_replace(Some(Err(code)));
            } else {
                worker.status.lock().unwrap().phase = "ready";
                worker.ready.send_replace(Some(Ok(())));
            }
        });
        let weak = Arc::downgrade(&card);
        tokio::spawn(async move {
            let mut stop = stop;
            loop {
                let Some(card) = weak.upgrade() else {
                    return;
                };
                if *stop.borrow()
                    || card.released.load(Ordering::Acquire)
                    || card.binding.get().is_some() && !card.binding_current()
                {
                    card.mark_lost();
                    return;
                }
                drop(card);
                tokio::select! {
                    result = stop.changed() => { if result.is_err() {
                        if let Some(card) = weak.upgrade() { card.mark_lost(); }
                        return;
                    } },
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
                }
            }
        });
        card
    }
    fn check_preparation(&self, stop: &watch::Receiver<bool>) -> Result<(), &'static str> {
        if self.stopping.load(Ordering::Acquire) || *stop.borrow() {
            Err("module_vowifi_cancelled")
        } else {
            Ok(())
        }
    }
    async fn prepare(
        &self,
        stop: &watch::Receiver<bool>,
        captured_intent: Option<NativeVoWiFiIntent>,
    ) -> Result<(), &'static str> {
        self.check_preparation(stop)?;
        let intent = match captured_intent {
            Some(intent) => intent,
            None => self.radio.native_vowifi_intent().map_err(|e| e.code)?,
        };
        // Capture RF authority first, and retain the physical hold before RF
        // prepare or any SIM transaction. Store cleanup authority before awaits.
        let _ = self.intent.set(intent);
        let intent = self.intent.get().unwrap();
        if let Some(binding) = intent.serial_binding() {
            let actor = self
                .serial
                .as_ref()
                .ok_or("module_vowifi_serial_unavailable")?;
            let hold = actor
                .retain_serial_module(&binding, intent.usb_inhibit())
                .await
                .map_err(|e| e.code)?;
            let _ = self.serial_hold.set(hold);
        } else {
            self.usb.retain_native_module(intent).map_err(|e| e.code)?;
        }
        self.physical_held.store(true, Ordering::Release);
        #[cfg(test)]
        if let Some(gate) = self.revoke_after_hold.lock().unwrap().take() {
            gate.revoke_for_tests();
        }
        let intent = self.intent.get().unwrap();
        if let Err(receipt) = intent.prepare_with_receipt().await {
            let error = match receipt {
                crate::module_radio::NativePrepareFailure::NotAdmitted(error) => {
                    // Only the owner's typed refusal of this unique preparation
                    // proves that the retained hold has no RF obligation.
                    self.prepare_not_admitted.store(true, Ordering::Release);
                    return Err(error.code);
                }
                crate::module_radio::NativePrepareFailure::AcceptedOrUnknown(error) => error,
            };
            // This settled rejection precedes metadata and enter, and prepare
            // itself has no physical RF writes. A route retired between capture
            // and retain must not strand an otherwise unused exact serial hold.
            if self.serial_hold.get().is_some()
                && error.code == "radio_stale_generation"
                && !intent.binding_current()
            {
                self.prepare_rejected_stale.store(true, Ordering::Release);
            }
            return Err(error.code);
        }
        #[cfg(test)]
        if let Some(gate) = self.revoke_after_prepare.lock().unwrap().take() {
            gate.revoke_for_tests();
        }
        self.check_preparation(stop)?;
        let binding = if let Some(hold) = self.serial_hold.get() {
            ModuleIdentity::Serial(
                SerialModuleIdentity::capture(self.serial.as_ref().unwrap(), hold)
                    .await
                    .map_err(|e| e.code)?,
            )
        } else {
            ModuleIdentity::Usb(
                self.usb
                    .native_module_identity(intent)
                    .await
                    .map_err(|e| e.code)?,
            )
        };
        let metadata = binding.metadata();
        let aid = metadata["application_aid"]
            .as_array()
            .filter(|v| (5..=32).contains(&v.len()));
        if aid.is_none_or(|v| v.iter().any(|n| n.as_u64().is_none_or(|n| n > 255))) {
            return Err("module_sim_identity_invalid");
        }
        let identity =
            LeaseIdentity::from_metadata(metadata).map_err(|_| "module_sim_identity_invalid")?;
        let _ = self.binding.set(binding);
        let _ = self.identity.set(identity);
        self.check_preparation(stop)?;
        intent.enter().await.map_err(|e| e.code)?;
        self.check_preparation(stop)?;
        if !self.binding_current() {
            return Err("module_sim_identity_changed");
        }
        Ok(())
    }
    pub(crate) async fn wait_ready(&self) -> Result<(), BackendFailure> {
        let mut ready = self.ready.subscribe();
        loop {
            if let Some(result) = *ready.borrow_and_update() {
                return result.map_err(failure);
            }
            if ready.changed().await.is_err() {
                return Err(failure("module_vowifi_worker_failed"));
            }
        }
    }
    fn mark_lost(&self) {
        self.stopping.store(true, Ordering::Release);
        self.lost.send_replace(true);
    }
    fn binding_current(&self) -> bool {
        self.intent.get().is_some_and(NativeVoWiFiIntent::current)
            && self.binding.get().is_some_and(|b| match b {
                ModuleIdentity::Usb(b) => self.usb.native_module_current(b),
                ModuleIdentity::Serial(b) => b.current(),
            })
    }
    pub(crate) fn is_current(&self) -> bool {
        !self.stopping.load(Ordering::Acquire)
            && !self.released.load(Ordering::Acquire)
            && matches!(*self.ready.borrow(), Some(Ok(())))
            && self.binding_current()
    }
    pub(crate) fn generation(&self) -> u64 {
        self.binding.get().map_or(0, ModuleIdentity::generation)
    }
    pub(crate) fn usb_session_id(&self) -> Option<&str> {
        match self.binding.get()? {
            ModuleIdentity::Usb(v) => Some(v.session_id()),
            ModuleIdentity::Serial(_) => None,
        }
    }
    pub(crate) fn physical_identity(&self) -> Option<ModulePhysicalIdentity> {
        match self.binding.get()? {
            ModuleIdentity::Usb(v) => Some(ModulePhysicalIdentity::Usb {
                producer: v.producer_id().into(),
                session: v.session_id().into(),
            }),
            ModuleIdentity::Serial(_) => {
                let binding = self.serial_hold.get()?.binding();
                Some(ModulePhysicalIdentity::Serial {
                    owner_epoch: binding.owner_epoch,
                    serial_session: binding.serial_session,
                })
            }
        }
    }
    pub(crate) fn status(&self) -> ModuleCardStatus {
        let mut status = *self.status.lock().unwrap();
        status.ready = self.is_current();
        status.cleanup_confirmed = self.released.load(Ordering::Acquire);
        if let Some(intent) = self.intent.get() {
            let radio = intent.status();
            status.cfun_mode = radio.cfun_mode.or(status.cfun_mode);
            status.original_mode = radio.original_mode.or(status.original_mode);
        }
        status
    }
    /// Status polling probes RF only. Identity APDUs remain confined to actual
    /// business admission and AKA; stopped/preparing views stay observational.
    pub(crate) async fn observe_radio(
        self: &Arc<Self>,
    ) -> Result<ModuleCardStatus, BackendFailure> {
        if !self.is_current() {
            return Ok(self.status());
        }
        let card = self.clone();
        tokio::spawn(async move {
            let _operation = card.operation.lock().await;
            if !card.is_current() {
                return Ok(card.status());
            }
            if let Err(error) = card.intent.get().unwrap().validate().await {
                card.status.lock().unwrap().code = Some(error.code);
                // An unobserved RF read does not prove a changed mode.
                if error.code == "module_vowifi_radio_changed" {
                    card.mark_lost();
                }
                return Err(failure(error.code));
            }
            Ok(card.status())
        })
        .await
        .map_err(|_| failure("module_vowifi_worker_failed"))?
    }
    /// Fresh business admission performs physical profile and CFUN checks. It
    /// never authorizes new work from cached UI facts alone.
    pub(crate) async fn validate_business(self: &Arc<Self>) -> Result<(), BackendFailure> {
        let card = self.clone();
        tokio::spawn(async move {
            let _operation = card.operation.lock().await;
            if !card.is_current() {
                return Err(failure("module_sim_identity_changed"));
            }
            let result = async {
                match card.binding.get().unwrap() {
                    ModuleIdentity::Usb(binding) => card.usb.native_module_validate(binding).await,
                    ModuleIdentity::Serial(binding) => binding.validate().await,
                }
                .map_err(|e| e.code)?;
                card.intent
                    .get()
                    .unwrap()
                    .validate()
                    .await
                    .map_err(|e| e.code)?;
                if !card.is_current() {
                    return Err("module_sim_identity_changed");
                }
                Ok(())
            }
            .await;
            if let Err(code) = result {
                if !card.binding_current()
                    || matches!(
                        code,
                        "module_sim_identity_changed"
                            | "usb_at_stale_session"
                            | "radio_stale_generation"
                            | "module_vowifi_radio_changed"
                    )
                {
                    card.mark_lost();
                }
                card.status.lock().unwrap().code = Some(code);
            }
            result.map_err(failure)
        })
        .await
        .map_err(|_| failure("module_vowifi_worker_failed"))?
    }
    async fn authenticate_retained(
        self: Arc<Self>,
        application: ReaderAkaApplication,
        rand: [u8; 16],
        autn: [u8; 16],
    ) -> Result<SimAkaResult, BackendFailure> {
        tokio::spawn(async move {
            let _operation = self.operation.lock().await;
            if !self.is_current() {
                return Err(failure("module_sim_identity_changed"));
            }
            let result = match self.binding.get().unwrap() {
                ModuleIdentity::Usb(binding) => {
                    self.usb
                        .native_module_authenticate(binding.clone(), application, rand, autn)
                        .await
                }
                ModuleIdentity::Serial(binding) => {
                    binding.authenticate(application, rand, autn).await
                }
            };
            if !self.is_current() {
                return Err(failure("module_sim_identity_changed"));
            }
            result.map_err(|e| {
                if !self.binding_current()
                    || matches!(
                        e.code,
                        "module_sim_identity_changed" | "usb_at_stale_session"
                    )
                {
                    self.mark_lost();
                }
                failure(e.code)
            })
        })
        .await
        .map_err(|_| failure("module_vowifi_worker_failed"))?
    }
    async fn restore_locked(&self) -> Result<(), &'static str> {
        if self.released.load(Ordering::Acquire) {
            return Ok(());
        }
        self.status.lock().unwrap().phase = "restoring";
        if !self.physical_held.load(Ordering::Acquire) {
            self.released.store(true, Ordering::Release);
            self.status.lock().unwrap().phase = "released";
            return Ok(());
        }
        if self.prepare_not_admitted.load(Ordering::Acquire) {
            let intent = self
                .intent
                .get()
                .ok_or("module_vowifi_cleanup_unconfirmed")?;
            if let Some(hold) = self.serial_hold.get() {
                self.serial
                    .as_ref()
                    .ok_or("module_vowifi_serial_unavailable")?
                    .release_serial_module(hold)
                    .await
                    .map_err(|e| e.code)?;
            } else {
                self.usb.release_native_module(intent).map_err(|e| e.code)?;
            }
            self.released.store(true, Ordering::Release);
            self.status.lock().unwrap().phase = "released";
            return Ok(());
        }
        if self.prepare_rejected_stale.load(Ordering::Acquire) {
            self.serial
                .as_ref()
                .unwrap()
                .release_serial_module(self.serial_hold.get().unwrap())
                .await
                .map_err(|e| e.code)?;
            self.released.store(true, Ordering::Release);
            self.status.lock().unwrap().phase = "released";
            return Ok(());
        }
        if let Some(intent) = self.intent.get() {
            let observed = intent.status();
            {
                let mut status = self.status.lock().unwrap();
                status.cfun_mode = observed.cfun_mode.or(status.cfun_mode);
                status.original_mode = observed.original_mode.or(status.original_mode);
            }
            let retired_loss = if let Some(hold) = self.serial_hold.get() {
                intent.retire_confirmed_serial_loss(hold).await
            } else if intent.serial_binding().is_none() {
                intent.retire_confirmed_usb_loss().await
            } else {
                // Admission failed before a physical hold or RF prepare.
                self.released.store(true, Ordering::Release);
                self.status.lock().unwrap().phase = "released";
                return Ok(());
            }
            .map_err(|e| e.code)?;
            if retired_loss {
                if let Some(hold) = self.serial_hold.get() {
                    self.serial
                        .as_ref()
                        .unwrap()
                        .release_serial_module(hold)
                        .await
                        .map_err(|e| e.code)?;
                }
                let mut status = self.status.lock().unwrap();
                status.restoration_skipped_reader_lost = true;
                status.phase = "released_reader_lost";
                status.cfun_mode = None;
                status.code = Some("module_vowifi_radio_lost");
                self.released.store(true, Ordering::Release);
                return Ok(());
            }
            if let Err(error) = intent.restore().await {
                let mut status = self.status.lock().unwrap();
                status.phase = "cleanup_unconfirmed";
                status.code = Some(error.code);
                return Err(error.code);
            }
        }
        if let Some(intent) = self.intent.get() {
            if let Some(hold) = self.serial_hold.get() {
                self.serial
                    .as_ref()
                    .unwrap()
                    .release_serial_module(hold)
                    .await
                    .map_err(|e| e.code)?;
            } else {
                self.usb.release_native_module(intent).map_err(|e| e.code)?;
            }
        }
        self.released.store(true, Ordering::Release);
        self.status.lock().unwrap().phase = "released";
        Ok(())
    }
    pub(crate) async fn release_with_confirmation(self: &Arc<Self>) -> Result<(), BackendFailure> {
        self.mark_lost();
        let card = self.clone();
        tokio::spawn(async move {
            let _operation = card.operation.lock().await;
            card.restore_locked().await.map_err(failure)
        })
        .await
        .map_err(|_| failure("module_vowifi_worker_failed"))?
    }
    pub(crate) fn lost_receiver(&self) -> watch::Receiver<bool> {
        self.lost.subscribe()
    }
}
impl OnDemandCard for ModuleSessionCard {
    fn identity_current(&self) -> bool {
        self.is_current()
    }
    fn kind(&self) -> SessionKind {
        SessionKind::Module
    }
    fn reader(&self) -> &str {
        "dji-module"
    }
    fn reader_generation(&self) -> u64 {
        self.generation()
    }
    fn identity(&self) -> &LeaseIdentity {
        self.identity
            .get()
            .expect("only ready module cards enter a session")
    }
    fn authenticate(
        self: Arc<Self>,
        application: ReaderAkaApplication,
        rand: [u8; 16],
        autn: [u8; 16],
    ) -> CardFuture<SimAkaResult> {
        Box::pin(self.authenticate_retained(application, rand, autn))
    }
    fn lost_receiver(&self) -> watch::Receiver<bool> {
        self.lost_receiver()
    }
    fn release_with_confirmation(self: Arc<Self>) -> CardFuture<()> {
        Box::pin(async move { ModuleSessionCard::release_with_confirmation(&self).await })
    }
}
#[cfg(test)]
pub(crate) mod tests;
