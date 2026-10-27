//! Finite current-device inspection for a coordinator-owned legacy baseline.
//! This never restores a historical policy, submits an AT mutation, or selects
//! a replacement after the actual native connection has been captured.
use super::*;
use crate::module_radio_transport::{Action, CallModeModuleIdentity, Outcome, Plan, Reply,
    UsbComposition, VoiceConfiguration};
use crate::startup_recovery::legacy_current::ReadScope;

#[cfg(test)]
mod tests;

pub(crate) struct VerifiedDevice {
    target: PhysicalTarget,
    facts: Facts,
    idle: crate::call_mode_adb::legacy_current::IdleEvidence,
}
impl VerifiedDevice {
    pub(crate) fn observed_cfun(&self) -> u8 { self.facts.cfun }
}
impl fmt::Debug for VerifiedDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedDevice").finish_non_exhaustive()
    }
}
struct Facts {
    identity: CallModeModuleIdentity,
    usb: UsbComposition,
    voice: VoiceConfiguration,
    cfun: u8,
}
impl Facts {
    fn same(&self, other: &Self) -> bool {
        self.identity.imei == other.identity.imei && self.identity.firmware == other.identity.firmware
            && self.usb == other.usb && self.voice == other.voice && self.cfun == other.cfun
    }
}
fn error(code: &'static str, uncertain: bool) -> TransportError {
    TransportError::new(code, "Current module baseline is unconfirmed", &[], uncertain)
}
fn device_error() -> TransportError { error("legacy_current_device_unconfirmed", false) }
fn read_error() -> TransportError { error("legacy_current_readback_unconfirmed", false) }
fn cleanup_error() -> TransportError { error("legacy_current_cleanup_unconfirmed", true) }

// This private seam permits byte-level tests, never a caller-defined authority.
trait Authority {
    fn check(&self) -> Result<(), TransportError>;
    fn deadline(&self) -> Instant;
    fn admission(&self) -> crate::module_radio::CallAdmission;
    fn expected_imei(&self) -> Option<&str>;
    fn accepts_usb(&self, usb: UsbComposition) -> bool;
}
impl Authority for ReadScope {
    fn check(&self) -> Result<(), TransportError> { ReadScope::check(self) }
    fn deadline(&self) -> Instant { ReadScope::deadline(self) }
    fn admission(&self) -> crate::module_radio::CallAdmission { ReadScope::admission(self) }
    fn expected_imei(&self) -> Option<&str> { ReadScope::expected_imei(self) }
    fn accepts_usb(&self, usb: UsbComposition) -> bool { ReadScope::accepts_usb(self, usb) }
}
fn remaining(scope: &impl Authority) -> Result<Duration, TransportError> {
    scope.check()?;
    let left = scope.deadline().saturating_duration_since(Instant::now());
    if left < MIN_TIMEOUT { return Err(read_error()); }
    Ok(left)
}
fn read_command(port: &mut NativeTransport, scope: &impl Authority,
                command: &str, timeout: Duration) -> Result<String, TransportError> {
    if !matches!(command, "AT" | "AT+CGSN" | "ATI" | "AT+CFUN?" | "AT+CLCC"
        | "AT+QCFG=\"USBCFG\"" | "AT+QCFG=\"ims\"" | "AT+QCFG=\"volte_disable\"") {
        return Err(read_error());
    }
    let timeout = timeout.min(remaining(scope)?);
    port.command_guard = Some(scope.admission());
    // Require a real wire terminal on every observation, including CFUN and
    // identity; ordinary partial-response compatibility is not baseline proof.
    let result = port.execute_command(command, None, timeout, 4096, true);
    port.command_guard = None;
    scope.check()?;
    result.map_err(|_| read_error())
}
fn read_plan(port: &mut NativeTransport, scope: &impl Authority, action: Action)
    -> Result<Outcome, TransportError> {
    if !matches!(action, Action::CallModeIdentityRead | Action::CallModeUsbCompositionRead
        | Action::CallModeVoiceRead | Action::CallModeIdleRead | Action::Observe { cellular: false }) {
        return Err(read_error());
    }
    let mut plan = Plan::new_with_cancelled(action, remaining(scope)?, Arc::new(AtomicBool::new(false)));
    while !plan.done() {
        remaining(scope)?;
        let Some(step) = plan.next() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        if step.write || step.sensitive { return Err(read_error()); }
        let reply = read_command(port, scope, &step.command, step.timeout)?;
        plan.accept(Reply::response(reply));
    }
    if plan.retire_transport() { return Err(read_error()); }
    let result = plan.finish();
    scope.check()?;
    if !result.complete || !result.verified || result.uncertain { return Err(read_error()); }
    Ok(result)
}
fn read_facts(port: &mut NativeTransport, scope: &impl Authority) -> Result<Facts, TransportError> {
    if !probe_succeeded(&read_command(port, scope, "AT", PROBE_TIMEOUT)?) { return Err(read_error()); }
    let identity = read_plan(port, scope, Action::CallModeIdentityRead)?
        .module_identity.ok_or_else(read_error)?;
    if identity.imei.len() != 15 || !identity.imei.bytes().all(|b| b.is_ascii_digit())
        || scope.expected_imei().is_some_and(|expected| expected != identity.imei) {
        return Err(read_error());
    }
    let usb = read_plan(port, scope, Action::CallModeUsbCompositionRead)?
        .usb_composition.filter(|value| value.valid() && scope.accepts_usb(*value)).ok_or_else(read_error)?;
    let voice = read_plan(port, scope, Action::CallModeVoiceRead)?
        .voice.filter(|value| value.valid()).ok_or_else(read_error)?;
    let cfun = read_plan(port, scope, Action::Observe { cellular: false })?
        .cfun.filter(|value| matches!(value, 0 | 1 | 4)).ok_or_else(read_error)?;
    read_plan(port, scope, Action::CallModeIdleRead)?;
    Ok(Facts { identity, usb, voice, cfun })
}
// The native handle and software claim peer use this same ordered boundary.
// A successful claim is recorded before GET_INTERFACE can fail or revoke.
trait InterfaceClaims {
    fn claim(&mut self, interface: u8) -> Result<(), TransportError>;
    fn alternate(&mut self, interface: u8, timeout: Duration) -> Result<u8, TransportError>;
}
fn claim_all(scope: &impl Authority, expected: &[Candidate], io: &mut impl InterfaceClaims,
    claimed: &mut Vec<u8>) -> Result<(), TransportError> {
    for candidate in expected {
        scope.check()?;
        io.claim(candidate.interface)?;
        claimed.push(candidate.interface);
        let current = io.alternate(candidate.interface, remaining(scope)?.min(Duration::from_secs(1)))?;
        scope.check()?;
        if current != candidate.alternate { return Err(device_error()); }
    }
    Ok(())
}
fn release_all(claimed: &mut Vec<u8>,
    mut release: impl FnMut(u8) -> Result<CloseDisposition, TransportError>)
    -> Result<CloseDisposition, TransportError> {
    let mut disconnected = false;
    let mut failed = false;
    for interface in claimed.drain(..).rev() {
        match release(interface) {
            Ok(CloseDisposition::ReleasedAndHandleDropped) => {},
            Ok(CloseDisposition::DisconnectedAndHandleDropped) => disconnected = true,
            Err(_) => failed = true,
        }
    }
    if failed { Err(cleanup_error()) }
    else if disconnected { Ok(CloseDisposition::DisconnectedAndHandleDropped) }
    else { Ok(CloseDisposition::ReleasedAndHandleDropped) }
}
// Perform retirement first, but never downgrade an already uncertain result
// to a definite disconnection or a later admission failure.
fn settle<T>(result: Result<T, TransportError>, closed: Result<(), TransportError>)
    -> Result<T, TransportError> {
    if closed.as_ref().err().is_some_and(|error| error.uncertain) { return Err(closed.err().expect("uncertain close")); }
    if result.as_ref().err().is_some_and(|error| error.uncertain) { return result; }
    closed?;
    result
}
fn checked_close(port: NativeTransport) -> Result<(), TransportError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Box::new(port).checked_close())) {
        Ok(Ok(CloseDisposition::ReleasedAndHandleDropped)) => Ok(()),
        Ok(Ok(CloseDisposition::DisconnectedAndHandleDropped)) => Err(device_error()),
        _ => Err(cleanup_error()),
    }
}
fn examine(port: &mut NativeTransport, scope: &ReadScope,
    idle: impl FnOnce(&ReadScope, &PhysicalTarget) -> Result<crate::call_mode_adb::legacy_current::IdleEvidence, TransportError>)
    -> Result<(PhysicalTarget, Facts, crate::call_mode_adb::legacy_current::IdleEvidence), TransportError> {
    let first = read_facts(port, scope)?;
    let target = port.physical_target().map_err(|_| device_error())?;
    match &target.connection {
        #[cfg(target_os = "macos")]
        PhysicalConnection::Native(device) => {
            let descriptor = device.device_descriptor().map_err(|_| device_error())?;
            if (descriptor.vendor_id(), descriptor.product_id()) != (first.usb.vendor_id, first.usb.product_id) {
                return Err(device_error());
            }
        }
        #[cfg(test)]
        PhysicalConnection::Synthetic(_) => {}
    }
    let idle = idle(scope, &target)?;
    let second = read_facts(port, scope)?;
    if !first.same(&second) { return Err(read_error()); }
    scope.check()?;
    Ok((target, second, idle))
}
fn inspect_port(mut port: NativeTransport, scope: &ReadScope,
    idle: impl FnOnce(&ReadScope, &PhysicalTarget) -> Result<crate::call_mode_adb::legacy_current::IdleEvidence, TransportError>)
    -> Result<VerifiedDevice, TransportError> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| examine(&mut port, scope, idle)))
        .unwrap_or_else(|_| Err(cleanup_error()));
    let (target, facts, idle) = settle(result, checked_close(port))?;
    scope.check()?;
    Ok(VerifiedDevice { target, facts, idle })
}
fn reopen_port(mut port: NativeTransport, scope: &ReadScope, verified: &VerifiedDevice,
    idle: impl FnOnce(&ReadScope, &PhysicalTarget) -> Result<crate::call_mode_adb::legacy_current::IdleEvidence, TransportError>)
    -> Result<Box<dyn Transport>, TransportError> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (target, facts, idle) = examine(&mut port, scope, idle)?;
        if !target.same_connection(&verified.target) || !facts.same(&verified.facts)
            || !idle.same_boot(&verified.idle) { return Err(read_error()); }
        scope.check()
    })).unwrap_or_else(|_| Err(cleanup_error()));
    if let Err(error) = result {
        return settle(Err(error), checked_close(port));
    }
    // Scope guards are transient at each read boundary. The ordinary owner
    // receives its native handle, not an expired startup admission closure.
    if port.command_guard.is_some() { checked_close(port)?; return Err(read_error()); }
    Ok(Box::new(port))
}
pub(crate) fn inspect(scope: &ReadScope) -> Result<VerifiedDevice, TransportError> {
    scope.check()?;
    #[cfg(target_os = "macos")]
    { inspect_port(native::open(scope, None)?, scope, crate::call_mode_adb::legacy_current::inspect) }
    #[cfg(not(target_os = "macos"))]
    { Err(device_error()) }
}
pub(crate) fn reopen(scope: &ReadScope, verified: &VerifiedDevice)
    -> Result<Box<dyn Transport>, TransportError> {
    scope.check()?;
    #[cfg(target_os = "macos")]
    { reopen_port(native::open(scope, Some(&verified.target))?, scope, verified,
        crate::call_mode_adb::legacy_current::inspect) }
    #[cfg(not(target_os = "macos"))]
    { let _ = verified; Err(device_error()) }
}
#[cfg(test)]
pub(crate) use tests::{fixture_inspect, fixture_reopen};

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use rusb::{Context, Device, DeviceHandle, TransferType, UsbContext};
    struct AllAtIo {
        handle: Option<DeviceHandle<Context>>,
        candidates: Vec<Candidate>,
        claimed: Vec<u8>,
        selected: Candidate,
    }
    impl AllAtIo {
        fn close(&mut self) -> Result<CloseDisposition, TransportError> {
            let result = match &self.handle {
                Some(handle) => release_all(&mut self.claimed, |interface| {
                    match handle.release_interface(interface) {
                        Ok(()) => Ok(CloseDisposition::ReleasedAndHandleDropped),
                        Err(rusb::Error::NoDevice) => Ok(CloseDisposition::DisconnectedAndHandleDropped),
                        Err(_) => Err(cleanup_error()),
                    }
                }),
                None => Err(cleanup_error()),
            };
            drop(self.handle.take());
            result
        }
    }
    impl Drop for AllAtIo {
        fn drop(&mut self) {
            if self.handle.is_some() { let _ = self.close(); }
        }
    }
    struct Claims<'a>(&'a DeviceHandle<Context>);
    impl InterfaceClaims for Claims<'_> {
        fn claim(&mut self, interface: u8) -> Result<(), TransportError> {
            self.0.claim_interface(interface).map_err(|_| device_error())
        }
        fn alternate(&mut self, interface: u8, timeout: Duration) -> Result<u8, TransportError> {
            let mut value = [0_u8; 1];
            let n = self.0.read_control(0x81, 10, 0, interface.into(), &mut value, timeout)
                .map_err(|_| device_error())?;
            if n != 1 { return Err(device_error()); }
            Ok(value[0])
        }
    }
    fn io_error(error: rusb::Error) -> IoError {
        match error {
            rusb::Error::Timeout => IoError::Timeout,
            rusb::Error::NoDevice => IoError::Disconnected,
            _ => IoError::Other,
        }
    }
    impl Io for AllAtIo {
        fn checked_close(mut self: Box<Self>) -> Result<CloseDisposition, TransportError> { self.close() }
        fn physical_target(&self) -> Result<PhysicalTarget, TransportError> {
            let device = self.handle.as_ref().ok_or_else(device_error)?.device();
            Ok(PhysicalTarget { connection: PhysicalConnection::Native(device) })
        }
        fn capture_reenumeration_anchor(&mut self, deadline: Instant,
            guard: crate::module_radio::CallAdmission) -> Result<ReenumerationAnchor, TransportError> {
            reconnect::capture_native(self.handle.as_ref().ok_or_else(device_error)?,
                self.selected, deadline, guard)
        }
        fn read(&mut self, bytes: &mut [u8], timeout: Duration) -> Result<usize, IoError> {
            self.handle.as_ref().ok_or(IoError::Disconnected)?
                .read_bulk(self.selected.endpoint_in, bytes, timeout).map_err(io_error)
        }
        fn write(&mut self, bytes: &[u8], timeout: Duration) -> Result<usize, IoError> {
            self.handle.as_ref().ok_or(IoError::Disconnected)?
                .write_bulk(self.selected.endpoint_out, bytes, timeout).map_err(io_error)
        }
    }
    fn candidates(device: &Device<Context>) -> Result<Vec<Candidate>, TransportError> {
        let desc = device.device_descriptor().map_err(|_| device_error())?;
        let model = (desc.vendor_id(), desc.product_id());
        if !DEVICE_IDS.contains(&model) { return Err(device_error()); }
        let config = device.active_config_descriptor().map_err(|_| device_error())?;
        let mut found = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for interface in config.interfaces() {
            for alt in interface.descriptors() {
                if !supports_usb_at_interface(model, alt.interface_number()) { continue; }
                if (alt.class_code(), alt.sub_class_code(), alt.protocol_code()) != (0xff, 0, 0) {
                    continue;
                }
                let value = candidate(model, alt.interface_number(), alt.setting_number(),
                    alt.class_code(), alt.sub_class_code(), alt.protocol_code(),
                    alt.endpoint_descriptors().map(|ep| (ep.address(), ep.transfer_type() == TransferType::Bulk)))
                    .ok_or_else(device_error)?;
                if !seen.insert(value.interface) { return Err(device_error()); }
                found.push(value);
            }
        }
        if found.is_empty() || found.len() > 4 { return Err(device_error()); }
        found.sort_by_key(|value| usb_at_interface_priority(model, value.interface));
        Ok(found)
    }
    pub(super) fn open(scope: &ReadScope, target: Option<&PhysicalTarget>) -> Result<NativeTransport, TransportError> {
        scope.check()?;
        let owner = OwnerLease::acquire().map_err(|_| device_error())?;
        let device = match target {
            Some(target) => target.native_device().map_err(|_| device_error())?,
            None => {
                let context = Context::new().map_err(|_| device_error())?;
                scope.check()?;
                let devices = context.devices().map_err(|_| device_error())?;
                if devices.len() > 256 { return Err(device_error()); }
                let mut found = Vec::new();
                for device in devices.iter() {
                    scope.check()?;
                    let descriptor = device.device_descriptor().map_err(|_| device_error())?;
                    if DEVICE_IDS.contains(&(descriptor.vendor_id(), descriptor.product_id())) { found.push(device); }
                }
                if found.len() != 1 { return Err(device_error()); }
                found.pop().ok_or_else(device_error)?
            }
        };
        let expected = candidates(&device)?;
        scope.check()?;
        let handle = device.open().map_err(|_| device_error())?;
        if handle.device().as_raw() != device.as_raw() { return Err(device_error()); }
        let mut io = AllAtIo { handle: Some(handle), selected: expected[0],
            candidates: expected.clone(), claimed: Vec::new() };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let handle = io.handle.as_ref().ok_or_else(device_error)?;
            claim_all(scope, &expected, &mut Claims(handle), &mut io.claimed)?;
            scope.check()?;
            if candidates(&device)? != io.candidates { return Err(device_error()); }
            Ok(())
        })).unwrap_or_else(|_| Err(cleanup_error()));
        if let Err(error) = result {
            let closed = io.close().and_then(|disposition| match disposition {
                CloseDisposition::ReleasedAndHandleDropped => Ok(()),
                CloseDisposition::DisconnectedAndHandleDropped => Err(device_error()),
            });
            return settle(Err(error), closed);
        }
        Ok(NativeTransport::new(Box::new(io), "Verified current USB AT owner".into(), Some(owner)))
    }
}
