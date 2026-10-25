//! One startup-v1 native read attempt. Durable USB fields are observations;
//! only the coordinator's live, sealed scope authorizes this entry point.
//! No transport, normal reconnect anchor, or physical target escapes this read.
use super::*;
use crate::module_radio_transport::{
    Action, CallModeModuleIdentity, Outcome, Plan, Reply, UsbComposition, VoiceConfiguration,
};
use crate::startup_recovery::execution::NativeIdentityScope;

pub(crate) mod reconcile;

const MAX_SCAN_DEVICES: usize = 256;

pub(crate) struct NativeReadback {
    identity: CallModeModuleIdentity,
    usb: UsbComposition,
    voice: VoiceConfiguration,
    close: CloseDisposition,
}
impl fmt::Debug for NativeReadback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeReadback")
            .field("close", &self.close)
            .finish_non_exhaustive()
    }
}
impl NativeReadback {
    pub(crate) fn identity(&self) -> &CallModeModuleIdentity {
        &self.identity
    }
    pub(crate) fn usb_composition(&self) -> UsbComposition {
        self.usb
    }
    pub(crate) fn voice(&self) -> VoiceConfiguration {
        self.voice
    }
    pub(crate) fn close_disposition(&self) -> CloseDisposition {
        self.close
    }
}

fn failed(code: &'static str, message: &'static str) -> TransportError {
    TransportError::new(code, message, &[], false)
}
fn unconfirmed() -> TransportError {
    failed(
        "startup_native_target_unconfirmed",
        "The exact startup USB target is unconfirmed",
    )
}
fn read_failed() -> TransportError {
    failed(
        "startup_native_read_unconfirmed",
        "The startup USB identity or readback is unconfirmed",
    )
}
fn callback_panicked() -> TransportError {
    TransportError::new(
        "startup_native_callback_panicked",
        "The startup USB callback panicked",
        &[],
        true,
    )
}

struct BindingView<'a> {
    model: (u16, u16),
    bus: u8,
    ports: &'a [u8],
    language: u16,
    serial: &'a str,
    candidate: Candidate,
}
impl<'a> BindingView<'a> {
    fn new(binding: &'a DurableUsbBinding) -> Self {
        let (model, bus, ports, language, serial, candidate) = binding.recovery_parts();
        Self {
            model,
            bus,
            ports,
            language,
            serial,
            candidate,
        }
    }
}
struct Observation<D> {
    device: D,
    model: (u16, u16),
    bus: u8,
    ports: Vec<u8>,
}

// Private implementation seams only. No actor accepts a caller-implemented
// authority or a binding plus boolean. Token fixtures exercise this same code.
trait Authority {
    fn check(&self) -> Result<(), TransportError>;
    fn deadline(&self) -> Instant;
    fn admission(&self) -> crate::module_radio::CallAdmission;
    fn expected_imei(&self) -> &str;
}
impl Authority for NativeIdentityScope<'_> {
    fn check(&self) -> Result<(), TransportError> {
        NativeIdentityScope::check(self)
    }
    fn deadline(&self) -> Instant {
        NativeIdentityScope::deadline(self)
    }
    fn admission(&self) -> crate::module_radio::CallAdmission {
        NativeIdentityScope::admission(self)
    }
    fn expected_imei(&self) -> &str {
        NativeIdentityScope::expected_imei(self)
    }
}
trait Backend {
    type Device;
    fn snapshot(&mut self) -> Result<Vec<Observation<Self::Device>>, TransportError>;
    fn open_selected(
        &mut self,
        selected: &Self::Device,
        binding: &BindingView<'_>,
    ) -> Result<Box<dyn Transport>, TransportError>;
}

fn select_exact<D>(
    observations: Vec<Observation<D>>,
    binding: &BindingView<'_>,
) -> Result<D, TransportError> {
    if observations.len() > MAX_SCAN_DEVICES {
        return Err(unconfirmed());
    }
    // Every occupant counts before descriptor support, serial, or openability.
    let mut occupants = observations
        .into_iter()
        .filter(|item| item.bus == binding.bus && item.ports.as_slice() == binding.ports);
    let selected = occupants.next().ok_or_else(|| {
        failed(
            "startup_native_target_absent",
            "The recorded startup USB topology is absent",
        )
    })?;
    if occupants.next().is_some() {
        return Err(failed(
            "startup_native_target_ambiguous",
            "The recorded startup USB topology is ambiguous",
        ));
    }
    if selected.model != binding.model || !DEVICE_IDS.contains(&selected.model) {
        return Err(unconfirmed());
    }
    Ok(selected.device)
}

fn strict_candidate(
    model: (u16, u16),
    expected: Candidate,
    descriptors: impl IntoIterator<Item = (u8, u8, u8, u8, u8, Vec<(u8, bool)>)>,
) -> Result<Candidate, TransportError> {
    if !super::supports_usb_at_interface(model, expected.interface) {
        return Err(unconfirmed());
    }
    let mut matching = descriptors
        .into_iter()
        .filter(|item| item.0 == expected.interface && item.1 == expected.alternate);
    let descriptor = matching.next().ok_or_else(unconfirmed)?;
    if matching.next().is_some() || (descriptor.2, descriptor.3, descriptor.4) != (0xff, 0, 0) {
        return Err(unconfirmed());
    }
    let bulk: Vec<_> = descriptor
        .5
        .into_iter()
        .filter(|(_, bulk)| *bulk)
        .map(|(address, _)| address)
        .collect();
    if bulk.len() != 2
        || bulk
            .iter()
            .filter(|address| **address == expected.endpoint_in)
            .count()
            != 1
        || bulk
            .iter()
            .filter(|address| **address == expected.endpoint_out)
            .count()
            != 1
        || !super::usb_at_endpoint_pair_matches(
            model,
            expected.interface,
            expected.endpoint_in,
            expected.endpoint_out,
        )
    {
        return Err(unconfirmed());
    }
    Ok(expected)
}

fn remaining(authority: &impl Authority) -> Result<Duration, TransportError> {
    authority.check()?;
    let remaining = authority
        .deadline()
        .saturating_duration_since(Instant::now());
    if remaining < MIN_TIMEOUT {
        return Err(read_failed());
    }
    Ok(remaining)
}

fn run_plan(
    transport: &mut dyn Transport,
    authority: &impl Authority,
    action: Action,
) -> Result<Outcome, TransportError> {
    // A fixed allowlist precedes Plan construction; each actual step is also
    // checked, so adding a write stage cannot widen this startup read scope.
    if !matches!(
        action,
        Action::CallModeIdentityRead
            | Action::CallModeUsbCompositionRead
            | Action::CallModeVoiceRead
    ) {
        return Err(read_failed());
    }
    let mut plan = Plan::new_with_cancelled(
        action,
        remaining(authority)?,
        Arc::new(AtomicBool::new(false)),
    );
    while !plan.done() {
        remaining(authority)?;
        let Some(step) = plan.next() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        if step.write {
            return Err(read_failed());
        }
        let response = transport.command_with_guard(
            &step.command,
            None,
            step.timeout.min(remaining(authority)?),
            crate::module_radio_transport::RESPONSE_LIMIT,
            authority.admission(),
        );
        authority.check()?;
        // Raw response/error bytes never enter a startup diagnostic.
        plan.accept(Reply::response(response.map_err(|_| read_failed())?));
    }
    if plan.retire_transport() {
        return Err(read_failed());
    }
    let outcome = plan.finish();
    authority.check()?;
    if !outcome.complete || !outcome.verified || outcome.uncertain {
        return Err(read_failed());
    }
    Ok(outcome)
}

fn read_opened(
    transport: &mut dyn Transport,
    authority: &impl Authority,
) -> Result<(CallModeModuleIdentity, UsbComposition, VoiceConfiguration), TransportError> {
    let response = transport.command_with_guard(
        "AT",
        None,
        remaining(authority)?.min(PROBE_TIMEOUT),
        4096,
        authority.admission(),
    );
    authority.check()?;
    if !probe_succeeded(&response.map_err(|_| read_failed())?) {
        return Err(read_failed());
    }
    let identity = run_plan(transport, authority, Action::CallModeIdentityRead)?
        .module_identity
        .ok_or_else(read_failed)?;
    if identity.imei.len() != 15
        || !identity.imei.bytes().all(|byte| byte.is_ascii_digit())
        || identity.imei != authority.expected_imei()
    {
        return Err(read_failed());
    }
    let usb = run_plan(transport, authority, Action::CallModeUsbCompositionRead)?
        .usb_composition
        .filter(|value| value.valid())
        .ok_or_else(read_failed)?;
    let voice = run_plan(transport, authority, Action::CallModeVoiceRead)?
        .voice
        .filter(|value| value.valid())
        .ok_or_else(read_failed)?;
    Ok((identity, usb, voice))
}

fn execute<B: Backend>(
    authority: &impl Authority,
    binding: &BindingView<'_>,
    backend: &mut B,
) -> Result<NativeReadback, TransportError> {
    authority.check()?;
    let observations = backend.snapshot()?;
    authority.check()?;
    // This real Device remains retained through the entire one-shot attempt.
    // No retry path can enumerate or substitute a second connection.
    let selected = select_exact(observations, binding)?;
    authority.check()?;
    let mut transport = backend.open_selected(&selected, binding)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        authority
            .check()
            .and_then(|_| read_opened(transport.as_mut(), authority))
    }))
    .unwrap_or_else(|_| Err(callback_panicked()));
    // Always retire the real claimed handle, including revocation/read errors.
    // Close failure outranks both successful readback and the original error.
    let close =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| transport.checked_close()))
            .unwrap_or_else(|_| Err(close_unconfirmed()));
    let fence = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| authority.check()))
        .unwrap_or_else(|_| Err(callback_panicked()));
    let close = close?;
    // Retain callback-panic debt even if cancellation/evidence also changed;
    // a later fence cannot downgrade that original typed uncertainty.
    if let Err(error) = &result {
        if error.code == "startup_native_callback_panicked" {
            return Err(error.clone());
        }
    }
    fence?;
    let (identity, usb, voice) = result?;
    Ok(NativeReadback {
        identity,
        usb,
        voice,
        close,
    })
}

/// Only the original coordinator-owned sealed scope can enter native I/O.
/// The owner enforces one attempt and stays alive until this synchronous call
/// returns after checked close; cancellation cannot detach a blocking handle.
pub(crate) fn read_identity_and_state(
    scope: &NativeIdentityScope<'_>,
) -> Result<NativeReadback, TransportError> {
    scope.check()?;
    #[cfg(target_os = "macos")]
    {
        let mut backend = native::NativeBackend::new(scope)?;
        execute(scope, &BindingView::new(scope.binding()), &mut backend)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(unconfirmed())
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use rusb::{Context, Device, DeviceHandle, TransferType, UsbContext};

    pub(super) struct NativeBackend {
        context: Context,
        deadline: Instant,
        guard: crate::module_radio::CallAdmission,
    }
    impl NativeBackend {
        pub(super) fn new(scope: &NativeIdentityScope<'_>) -> Result<Self, TransportError> {
            Self::new_authority(scope)
        }
        pub(super) fn new_authority(scope: &impl Authority) -> Result<Self, TransportError> {
            scope.check()?;
            let context = Context::new().map_err(|_| unconfirmed())?;
            scope.check()?;
            Ok(Self {
                context,
                deadline: scope.deadline(),
                guard: scope.admission(),
            })
        }
        pub(super) fn fence(&self) -> Result<(), TransportError> {
            reconnect::check_finite(self.deadline, &self.guard).map(|_| ())
        }
        pub(super) fn model_topology(
            &self,
            device: &Device<Context>,
            binding: &BindingView<'_>,
        ) -> Result<(), TransportError> {
            self.fence()?;
            let descriptor = device.device_descriptor().map_err(|_| unconfirmed())?;
            self.fence()?;
            let ports = device.port_numbers().map_err(|_| unconfirmed())?;
            self.fence()?;
            if (descriptor.vendor_id(), descriptor.product_id()) != binding.model
                || device.bus_number() != binding.bus
                || ports.as_slice() != binding.ports
            {
                return Err(unconfirmed());
            }
            Ok(())
        }
        fn tuple(
            &self,
            device: &Device<Context>,
            binding: &BindingView<'_>,
        ) -> Result<Candidate, TransportError> {
            self.fence()?;
            let configuration = device
                .active_config_descriptor()
                .map_err(|_| unconfirmed())?;
            let mut descriptors = Vec::new();
            for interface in configuration.interfaces() {
                for alternate in interface.descriptors() {
                    descriptors.push((
                        alternate.interface_number(),
                        alternate.setting_number(),
                        alternate.class_code(),
                        alternate.sub_class_code(),
                        alternate.protocol_code(),
                        alternate
                            .endpoint_descriptors()
                            .map(|endpoint| {
                                (
                                    endpoint.address(),
                                    endpoint.transfer_type() == TransferType::Bulk,
                                )
                            })
                            .collect(),
                    ));
                }
            }
            self.fence()?;
            strict_candidate(binding.model, binding.candidate, descriptors)
        }
        fn read_string(
            &self,
            handle: &DeviceHandle<Context>,
            index: u8,
            language: u16,
        ) -> Result<Zeroizing<Vec<u8>>, TransportError> {
            let timeout = reconnect::check_finite(self.deadline, &self.guard)?;
            let mut bytes = Zeroizing::new([0_u8; 255]);
            let length = handle
                .read_control(
                    rusb::request_type(
                        rusb::Direction::In,
                        rusb::RequestType::Standard,
                        rusb::Recipient::Device,
                    ),
                    6,
                    (3_u16 << 8) | u16::from(index),
                    language,
                    &mut bytes[..],
                    timeout,
                )
                .map_err(|_| unconfirmed())?;
            self.fence()?;
            if length > bytes.len() {
                return Err(unconfirmed());
            }
            Ok(Zeroizing::new(bytes[..length].to_vec()))
        }
        fn serial(
            &self,
            handle: &DeviceHandle<Context>,
            binding: &BindingView<'_>,
        ) -> Result<(), TransportError> {
            self.fence()?;
            let descriptor = handle
                .device()
                .device_descriptor()
                .map_err(|_| unconfirmed())?;
            let index = descriptor
                .serial_number_string_index()
                .ok_or_else(unconfirmed)?;
            if !reconnect::languages(&self.read_string(handle, 0, 0)?)?.contains(&binding.language)
            {
                return Err(unconfirmed());
            }
            let serial =
                reconnect::serial_value(&self.read_string(handle, index, binding.language)?)?;
            if serial.as_str() != binding.serial {
                return Err(unconfirmed());
            }
            self.model_topology(&handle.device(), binding)
        }
    }
    impl NativeBackend {
        pub(super) fn open_retained(
            &mut self,
            selected: &Device<Context>,
            binding: &BindingView<'_>,
            owner: OwnerLease,
        ) -> Result<Box<dyn Transport>, TransportError> {
            self.fence()?;
            self.model_topology(selected, binding)?;
            self.tuple(selected, binding)?;
            self.open_claimed(selected, binding, owner)
        }
        fn open_claimed(
            &mut self,
            selected: &Device<Context>,
            binding: &BindingView<'_>,
            owner: OwnerLease,
        ) -> Result<Box<dyn Transport>, TransportError> {
            self.fence()?;
            let handle = selected.open().map_err(|_| unconfirmed())?;
            self.fence()?;
            if handle.device().as_raw() != selected.as_raw() {
                return Err(unconfirmed());
            }
            self.model_topology(selected, binding)?;
            self.serial(&handle, binding)?;
            let candidate = self.tuple(selected, binding)?;
            self.fence()?;
            handle
                .claim_interface(candidate.interface)
                .map_err(|_| unconfirmed())?;
            // From the first successful claim onward, every failure is routed
            // through the existing real release-and-handle-drop implementation.
            let io = macos::LibusbIo {
                handle,
                candidate,
                claimed: true,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.fence()?;
                // A descriptor's presence and a successful claim do not prove
                // the currently selected alternate, including alternate zero.
                io.handle
                    .set_alternate_setting(candidate.interface, candidate.alternate)
                    .map_err(|_| unconfirmed())?;
                self.fence()?;
                let mut actual_alternate = [0_u8; 1];
                let timeout = reconnect::check_finite(self.deadline, &self.guard)?;
                let length = io
                    .handle
                    .read_control(
                        rusb::request_type(
                            rusb::Direction::In,
                            rusb::RequestType::Standard,
                            rusb::Recipient::Interface,
                        ),
                        10,
                        0,
                        u16::from(candidate.interface),
                        &mut actual_alternate,
                        timeout,
                    )
                    .map_err(|_| unconfirmed())?;
                self.fence()?;
                if length != 1 || actual_alternate[0] != candidate.alternate {
                    return Err(unconfirmed());
                }
                self.model_topology(selected, binding)?;
                self.tuple(selected, binding)?;
                self.fence()
            }))
            .unwrap_or_else(|_| Err(callback_panicked()));
            if let Err(error) = result {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Box::new(io).checked_close()
                }))
                .unwrap_or_else(|_| Err(close_unconfirmed()))?;
                return Err(error);
            }
            Ok(Box::new(NativeTransport::new(
                Box::new(io),
                "Startup USB AT read".into(),
                Some(owner),
            )))
        }
    }
    impl Backend for NativeBackend {
        type Device = Device<Context>;
        fn snapshot(&mut self) -> Result<Vec<Observation<Self::Device>>, TransportError> {
            self.fence()?;
            let devices = self.context.devices().map_err(|_| unconfirmed())?;
            self.fence()?;
            if devices.len() > MAX_SCAN_DEVICES {
                return Err(unconfirmed());
            }
            let mut observations = Vec::with_capacity(devices.len());
            for device in devices.iter() {
                self.fence()?;
                // Errors on unrelated devices also reject the complete snapshot.
                let descriptor = device.device_descriptor().map_err(|_| unconfirmed())?;
                self.fence()?;
                let ports = device.port_numbers().map_err(|_| unconfirmed())?;
                self.fence()?;
                observations.push(Observation {
                    bus: device.bus_number(),
                    ports,
                    model: (descriptor.vendor_id(), descriptor.product_id()),
                    device,
                });
            }
            self.fence()?;
            Ok(observations)
        }
        fn open_selected(
            &mut self,
            selected: &Self::Device,
            binding: &BindingView<'_>,
        ) -> Result<Box<dyn Transport>, TransportError> {
            self.fence()?;
            // Reject an incomplete or changed persisted interface tuple before
            // opening the candidate or reading its serial control descriptor.
            self.model_topology(selected, binding)?;
            self.tuple(selected, binding)?;
            let owner = OwnerLease::acquire()?;
            self.open_claimed(selected, binding, owner)
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{OwnerFixtureScenario, scripted_read_for_owner};
