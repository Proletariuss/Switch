//! Targeted reopening. Successor evidence is operational serial/topology
//! continuity, not cryptographic hardware identity or a general discovery right.
use super::*;
use std::sync::Mutex;

pub(crate) const SUCCESSOR_PENDING: &str = "usb_at_successor_pending";
const IDENTITY_TIMEOUT: Duration = Duration::from_millis(900);
const MAX_SCAN_DEVICES: usize = 256;

/// A durable observation of an already opened native connection. This data
/// carries no Device, owner, reservation, lock, right, or permission to reopen.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "DurableUsbBindingData")]
pub(crate) struct DurableUsbBinding {
    schema_version: u8,
    vendor_id: u16,
    product_id: u16,
    bus_number: u8,
    port_numbers: Vec<u8>,
    serial_language: u16,
    serial_number: String,
    interface_number: u8,
    alternate_setting: u8,
    endpoint_in: u8,
    endpoint_out: u8,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableUsbBindingData {
    schema_version: u8,
    vendor_id: u16,
    product_id: u16,
    bus_number: u8,
    port_numbers: Vec<u8>,
    serial_language: u16,
    serial_number: String,
    interface_number: u8,
    alternate_setting: u8,
    endpoint_in: u8,
    endpoint_out: u8,
}

impl fmt::Debug for DurableUsbBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DurableUsbBinding")
            .field("schema_version", &self.schema_version)
            .finish_non_exhaustive()
    }
}
impl Drop for DurableUsbBinding {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.serial_number);
    }
}
impl TryFrom<DurableUsbBindingData> for DurableUsbBinding {
    type Error = &'static str;
    fn try_from(data: DurableUsbBindingData) -> Result<Self, Self::Error> {
        // LANGID and topology are exact observations, not locale guesses or
        // a path to enumerate. Bus zero remains valid under the native policy.
        if data.schema_version != 1
            || !DEVICE_IDS.contains(&(data.vendor_id, data.product_id))
            || !(Topology {
                bus: data.bus_number,
                ports: data.port_numbers.clone(),
            })
            .usable()
            || data.serial_language == 0
            || data.serial_number.is_empty()
            || data.serial_number.encode_utf16().count() > 126
            || data.serial_number.chars().all(char::is_whitespace)
            || data.serial_number.chars().any(char::is_control)
            || !super::supports_usb_at_interface(
                (data.vendor_id, data.product_id),
                data.interface_number,
            )
            || !super::usb_at_endpoint_pair_matches(
                (data.vendor_id, data.product_id),
                data.interface_number,
                data.endpoint_in,
                data.endpoint_out,
            )
            || !(0x81..=0x8f).contains(&data.endpoint_in)
            || !(0x01..=0x0f).contains(&data.endpoint_out)
        {
            return Err("invalid durable USB binding");
        }
        Ok(Self {
            schema_version: data.schema_version,
            vendor_id: data.vendor_id,
            product_id: data.product_id,
            bus_number: data.bus_number,
            port_numbers: data.port_numbers,
            serial_language: data.serial_language,
            serial_number: data.serial_number,
            interface_number: data.interface_number,
            alternate_setting: data.alternate_setting,
            endpoint_in: data.endpoint_in,
            endpoint_out: data.endpoint_out,
        })
    }
}
impl DurableUsbBinding {
    /// Read-only observation for the separately authorized startup selector.
    /// These fields are data and cannot construct a normal reconnect anchor.
    pub(super) fn recovery_parts(&self) -> ((u16, u16), u8, &[u8], u16, &str, Candidate) {
        (
            (self.vendor_id, self.product_id),
            self.bus_number,
            &self.port_numbers,
            self.serial_language,
            &self.serial_number,
            Candidate {
                interface: self.interface_number,
                alternate: self.alternate_setting,
                endpoint_in: self.endpoint_in,
                endpoint_out: self.endpoint_out,
            },
        )
    }

    fn from_observation(
        model: (u16, u16),
        topology: &Topology,
        language: u16,
        serial: &str,
        candidate: Candidate,
    ) -> Result<Self, TransportError> {
        DurableUsbBindingData {
            schema_version: 1,
            vendor_id: model.0,
            product_id: model.1,
            bus_number: topology.bus,
            port_numbers: topology.ports.clone(),
            serial_language: language,
            serial_number: serial.to_owned(),
            interface_number: candidate.interface,
            alternate_setting: candidate.alternate,
            endpoint_in: candidate.endpoint_in,
            endpoint_out: candidate.endpoint_out,
        }
        .try_into()
        .map_err(|_| unconfirmed())
    }
}

#[derive(Clone)]
pub(crate) struct ReenumerationAnchor {
    inner: Arc<AnchorConnection>,
}
enum AnchorConnection {
    #[cfg(target_os = "macos")]
    Native(NativeAnchor),
    // Ownership-only actor fixture, never a native Device or successor proof.
    #[cfg(test)]
    Synthetic {
        source: PhysicalTarget,
        descriptor: (u16, u16),
    },
    #[cfg(not(target_os = "macos"))]
    Unsupported,
}
impl fmt::Debug for ReenumerationAnchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReenumerationAnchor")
            .finish_non_exhaustive()
    }
}
impl ReenumerationAnchor {
    pub(crate) fn durable_binding(&self) -> Result<Option<DurableUsbBinding>, TransportError> {
        match self.inner.as_ref() {
            #[cfg(target_os = "macos")]
            AnchorConnection::Native(anchor) => DurableUsbBinding::from_observation(
                anchor.core.model,
                &anchor.core.topology,
                anchor.language,
                &anchor.core.serial,
                anchor.candidate,
            )
            .map(Some),
            // Only this explicit ownership-only test variant is unbound.
            #[cfg(test)]
            AnchorConnection::Synthetic { .. } => Ok(None),
            #[cfg(not(target_os = "macos"))]
            AnchorConnection::Unsupported => Err(unconfirmed()),
        }
    }
    #[cfg(test)]
    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub(crate) fn same_source(&self, target: &PhysicalTarget) -> bool {
        match self.inner.as_ref() {
            #[cfg(target_os = "macos")]
            AnchorConnection::Native(anchor) => target
                .native_device()
                .is_ok_and(|device| device.as_raw() == anchor.core.original.as_raw()),
            #[cfg(test)]
            AnchorConnection::Synthetic { source, .. } => source.same_connection(target),
            #[cfg(not(target_os = "macos"))]
            AnchorConnection::Unsupported => false,
        }
    }
    #[cfg(test)]
    pub(crate) fn synthetic_for_tests(source: PhysicalTarget, descriptor: (u16, u16)) -> Self {
        assert!(
            DEVICE_IDS.contains(&descriptor),
            "ownership fixture needs an explicit supported descriptor"
        );
        Self {
            inner: Arc::new(AnchorConnection::Synthetic { source, descriptor }),
        }
    }
}

fn failed(code: &'static str, message: &'static str) -> TransportError {
    TransportError::new(code, message, &[], false)
}
fn pending() -> TransportError {
    failed(
        SUCCESSOR_PENDING,
        "The original USB transition has not completed",
    )
}
fn unconfirmed() -> TransportError {
    failed(
        "usb_at_successor_unconfirmed",
        "The original USB successor is unconfirmed",
    )
}
pub(super) fn check_finite(
    deadline: Instant,
    guard: &crate::module_radio::CallAdmission,
) -> Result<Duration, TransportError> {
    if !guard() {
        return Err(failed(
            "usb_at_reopen_revoked",
            "USB reopen authority is no longer active",
        ));
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining < MIN_TIMEOUT {
        return Err(failed("usb_at_reopen_timeout", "USB reopen budget expired"));
    }
    Ok(remaining.min(IDENTITY_TIMEOUT))
}

/// Reopen only the actual retained connection; never enumerate a replacement.
pub(crate) fn open_exact(
    target: &PhysicalTarget,
    deadline: Instant,
    guard: crate::module_radio::CallAdmission,
) -> Result<Box<dyn Transport>, TransportError> {
    check_finite(deadline, &guard)?;
    #[cfg(target_os = "macos")]
    {
        let device = target.native_device()?;
        let owner = OwnerLease::acquire()?;
        open_selected_device(device, None, owner, deadline, guard)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = target;
        Err(target_unconfirmed())
    }
}

/// One bounded attempt. Pending may be polled within the accepting owner's
/// finite budget; a selected Device is retained and never replaced on retry.
pub(crate) fn open_successor(
    anchor: &ReenumerationAnchor,
    deadline: Instant,
    guard: crate::module_radio::CallAdmission,
) -> Result<Box<dyn Transport>, TransportError> {
    check_finite(deadline, &guard)?;
    match anchor.inner.as_ref() {
        #[cfg(target_os = "macos")]
        AnchorConnection::Native(anchor) => execute_successor(
            &anchor.core,
            &mut NativeBackend {
                context: anchor.core.original.context().clone(),
                language: anchor.language,
                model: anchor.core.model,
                topology: anchor.core.topology.clone(),
                deadline,
                guard,
            },
        ),
        #[cfg(test)]
        AnchorConnection::Synthetic { .. } => Err(target_unconfirmed()),
        #[cfg(not(target_os = "macos"))]
        AnchorConnection::Unsupported => Err(target_unconfirmed()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Topology {
    bus: u8,
    ports: Vec<u8>,
}
impl Topology {
    fn usable(&self) -> bool {
        !self.ports.is_empty() && self.ports.len() <= 7 && self.ports.iter().all(|port| *port != 0)
    }
}
struct Observation<D> {
    device: D,
    model: (u16, u16),
    topology: Topology,
}
struct Transition<D> {
    detached: bool,
    selected: Option<D>,
    failure: Option<TransportError>,
}
struct CoreAnchor<D> {
    original: D,
    model: (u16, u16),
    topology: Topology,
    serial: Zeroizing<String>,
    transition: Mutex<Transition<D>>,
}
trait Backend {
    type Device: Clone;
    type Opened;
    type Output;
    fn fence(&mut self) -> Result<(), TransportError>;
    fn same_device(&self, a: &Self::Device, b: &Self::Device) -> bool;
    fn snapshot(&mut self) -> Result<Vec<Observation<Self::Device>>, TransportError>;
    fn open(&mut self, device: &Self::Device) -> Result<Self::Opened, TransportError>;
    fn serial(&mut self, opened: &mut Self::Opened) -> Result<Zeroizing<String>, TransportError>;
    fn claim_probe(&mut self, opened: Self::Opened) -> Result<Self::Output, TransportError>;
    fn retire_output(&mut self, output: Self::Output) -> Result<(), TransportError>;
}

// Production and token-only selector tests share this whole state machine.
// A test Device token is never converted into a native anchor/PhysicalTarget.
fn execute_successor<B: Backend>(
    anchor: &CoreAnchor<B::Device>,
    backend: &mut B,
) -> Result<B::Output, TransportError> {
    let mut state = anchor.transition.try_lock().map_err(|_| {
        failed(
            "usb_at_successor_busy",
            "The retained USB transition is already executing",
        )
    })?;
    if let Some(error) = &state.failure {
        return Err(error.clone());
    }
    if let Err(error) = backend.fence() {
        state.failure = Some(error.clone());
        return Err(error);
    }
    let selected = if let Some(selected) = &state.selected {
        selected.clone()
    } else {
        let observations = backend.snapshot()?;
        if observations.len() > MAX_SCAN_DEVICES {
            return Err(unconfirmed());
        }
        let original_present = observations
            .iter()
            .any(|item| backend.same_device(&item.device, &anchor.original));
        if original_present {
            if state.detached {
                let error = unconfirmed();
                state.failure = Some(error.clone());
                return Err(error);
            }
            return Err(pending());
        }
        // Only a complete current held-Context snapshot can establish absence.
        state.detached = true;
        let mut slot = observations
            .into_iter()
            .filter(|item| item.topology == anchor.topology);
        let Some(candidate) = slot.next() else {
            return Err(pending());
        };
        if slot.next().is_some() {
            let error = failed(
                "usb_at_successor_ambiguous",
                "Multiple USB devices occupy the expected topology",
            );
            state.failure = Some(error.clone());
            return Err(error);
        }
        // Unsupported devices count in slot uniqueness before this model gate.
        if candidate.model != anchor.model || !DEVICE_IDS.contains(&candidate.model) {
            let error = unconfirmed();
            state.failure = Some(error.clone());
            return Err(error);
        }
        state.selected = Some(candidate.device.clone());
        candidate.device
    };
    let result = (|| {
        backend.fence()?;
        let mut opened = backend.open(&selected)?;
        backend.fence()?;
        let serial = backend.serial(&mut opened)?;
        if serial.as_str() != anchor.serial.as_str() {
            return Err(unconfirmed());
        }
        backend.fence()?;
        let output = backend.claim_probe(opened)?;
        if let Err(error) = backend.fence() {
            backend.retire_output(output)?;
            return Err(error);
        }
        Ok(output)
    })();
    if let Err(error) = &result {
        // No native open occurred when only the process-wide AT owner was busy.
        // A later retry is still pinned to this same selected Device.
        if error.code != "usb_at_owner_busy" {
            state.failure = Some(error.clone());
        }
    }
    result
}

fn string_units(bytes: &[u8]) -> Result<Vec<u16>, TransportError> {
    if bytes.len() < 2
        || bytes.len() > 254
        || bytes.len() % 2 != 0
        || usize::from(bytes[0]) != bytes.len()
        || bytes[1] != 3
    {
        return Err(unconfirmed());
    }
    Ok(bytes[2..]
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect())
}
pub(super) fn languages(bytes: &[u8]) -> Result<Vec<u16>, TransportError> {
    let languages = string_units(bytes)?;
    if languages.is_empty()
        || languages.iter().any(|language| *language == 0)
        || languages
            .iter()
            .enumerate()
            .any(|(index, language)| languages[..index].contains(language))
    {
        return Err(unconfirmed());
    }
    Ok(languages)
}
pub(super) fn serial_value(bytes: &[u8]) -> Result<Zeroizing<String>, TransportError> {
    let units = Zeroizing::new(string_units(bytes)?);
    let serial = Zeroizing::new(String::from_utf16(&units).map_err(|_| unconfirmed())?);
    if serial.is_empty()
        || serial.chars().any(char::is_control)
        || serial.chars().all(char::is_whitespace)
    {
        return Err(unconfirmed());
    }
    Ok(serial)
}

#[cfg(target_os = "macos")]
struct NativeAnchor {
    core: CoreAnchor<rusb::Device<rusb::Context>>,
    language: u16,
    candidate: Candidate,
}
#[cfg(target_os = "macos")]
fn native_error(error: rusb::Error) -> TransportError {
    if error == rusb::Error::NoDevice {
        failed(
            "usb_at_target_disconnected",
            "The retained USB connection is no longer present",
        )
    } else {
        unconfirmed()
    }
}
#[cfg(target_os = "macos")]
fn topology(device: &rusb::Device<rusb::Context>) -> Result<Topology, TransportError> {
    Ok(Topology {
        bus: device.bus_number(),
        ports: device.port_numbers().map_err(native_error)?,
    })
}
#[cfg(target_os = "macos")]
fn read_string(
    handle: &rusb::DeviceHandle<rusb::Context>,
    index: u8,
    language: u16,
    deadline: Instant,
    guard: &crate::module_radio::CallAdmission,
) -> Result<Zeroizing<Vec<u8>>, TransportError> {
    let timeout = check_finite(deadline, guard)?;
    let mut bytes = Zeroizing::new([0_u8; 255]);
    let length = handle
        .read_control(
            rusb::request_type(
                rusb::Direction::In,
                rusb::RequestType::Standard,
                rusb::Recipient::Device,
            ),
            6, // GET_DESCRIPTOR, USB standard request (not AT/ADB bulk I/O).
            (3_u16 << 8) | u16::from(index),
            language,
            &mut bytes[..],
            timeout,
        )
        .map_err(native_error)?;
    check_finite(deadline, guard)?;
    if length > bytes.len() {
        return Err(unconfirmed());
    }
    Ok(Zeroizing::new(bytes[..length].to_vec()))
}
#[cfg(target_os = "macos")]
pub(super) fn capture_native(
    handle: &rusb::DeviceHandle<rusb::Context>,
    candidate: Candidate,
    deadline: Instant,
    guard: crate::module_radio::CallAdmission,
) -> Result<ReenumerationAnchor, TransportError> {
    check_finite(deadline, &guard)?;
    let device = handle.device();
    let descriptor = device.device_descriptor().map_err(native_error)?;
    let model = (descriptor.vendor_id(), descriptor.product_id());
    let topology = topology(&device)?;
    if !DEVICE_IDS.contains(&model) || !topology.usable() {
        return Err(unconfirmed());
    }
    let index = descriptor
        .serial_number_string_index()
        .ok_or_else(unconfirmed)?;
    let language = languages(&read_string(handle, 0, 0, deadline, &guard)?)?[0];
    let serial = serial_value(&read_string(handle, index, language, deadline, &guard)?)?;
    check_finite(deadline, &guard)?;
    Ok(ReenumerationAnchor {
        inner: Arc::new(AnchorConnection::Native(NativeAnchor {
            core: CoreAnchor {
                original: device,
                model,
                topology,
                serial,
                transition: Mutex::new(Transition {
                    detached: false,
                    selected: None,
                    failure: None,
                }),
            },
            language,
            candidate,
        })),
    })
}

#[cfg(target_os = "macos")]
struct Opened {
    device: rusb::Device<rusb::Context>,
    handle: rusb::DeviceHandle<rusb::Context>,
    owner: OwnerLease,
}
#[cfg(target_os = "macos")]
struct NativeBackend {
    context: rusb::Context,
    language: u16,
    model: (u16, u16),
    topology: Topology,
    deadline: Instant,
    guard: crate::module_radio::CallAdmission,
}
#[cfg(target_os = "macos")]
impl Backend for NativeBackend {
    type Device = rusb::Device<rusb::Context>;
    type Opened = Opened;
    type Output = Box<dyn Transport>;
    fn fence(&mut self) -> Result<(), TransportError> {
        check_finite(self.deadline, &self.guard).map(|_| ())
    }
    fn same_device(&self, a: &Self::Device, b: &Self::Device) -> bool {
        a.as_raw() == b.as_raw()
    }
    fn snapshot(&mut self) -> Result<Vec<Observation<Self::Device>>, TransportError> {
        use rusb::UsbContext;
        self.fence()?;
        let devices = self.context.devices().map_err(native_error)?;
        if devices.len() > MAX_SCAN_DEVICES {
            return Err(unconfirmed());
        }
        let mut observations = Vec::new();
        for device in devices.iter() {
            self.fence()?;
            let descriptor = device.device_descriptor().map_err(native_error)?;
            let topology = topology(&device)?;
            observations.push(Observation {
                device,
                model: (descriptor.vendor_id(), descriptor.product_id()),
                topology,
            });
        }
        self.fence()?;
        Ok(observations)
    }
    fn open(&mut self, device: &Self::Device) -> Result<Self::Opened, TransportError> {
        self.fence()?;
        let owner = OwnerLease::acquire()?;
        self.fence()?;
        let handle = device.open().map_err(native_error)?;
        self.fence()?;
        if handle.device().as_raw() != device.as_raw() {
            return Err(unconfirmed());
        }
        let descriptor = device.device_descriptor().map_err(native_error)?;
        if (descriptor.vendor_id(), descriptor.product_id()) != self.model
            || topology(device)? != self.topology
        {
            return Err(unconfirmed());
        }
        Ok(Opened {
            device: device.clone(),
            handle,
            owner,
        })
    }
    fn serial(&mut self, opened: &mut Self::Opened) -> Result<Zeroizing<String>, TransportError> {
        self.fence()?;
        let descriptor = opened.device.device_descriptor().map_err(native_error)?;
        if (descriptor.vendor_id(), descriptor.product_id()) != self.model
            || topology(&opened.device)? != self.topology
        {
            return Err(unconfirmed());
        }
        let index = descriptor
            .serial_number_string_index()
            .ok_or_else(unconfirmed)?;
        if !languages(&read_string(
            &opened.handle,
            0,
            0,
            self.deadline,
            &self.guard,
        )?)?
        .contains(&self.language)
        {
            return Err(unconfirmed());
        }
        serial_value(&read_string(
            &opened.handle,
            index,
            self.language,
            self.deadline,
            &self.guard,
        )?)
    }
    fn retire_output(&mut self, output: Self::Output) -> Result<(), TransportError> {
        output.checked_close().map(|_| ())
    }
    fn claim_probe(&mut self, opened: Self::Opened) -> Result<Self::Output, TransportError> {
        self.fence()?;
        let descriptor = opened.device.device_descriptor().map_err(native_error)?;
        if (descriptor.vendor_id(), descriptor.product_id()) != self.model
            || topology(&opened.device)? != self.topology
        {
            return Err(unconfirmed());
        }
        open_selected_device(
            opened.device,
            Some(opened.handle),
            opened.owner,
            self.deadline,
            self.guard.clone(),
        )
    }
}

fn probe_guarded(
    mut transport: NativeTransport,
    deadline: Instant,
    guard: crate::module_radio::CallAdmission,
) -> Result<NativeTransport, TransportError> {
    let result = (|| {
        let timeout = check_finite(deadline, &guard)?.min(PROBE_TIMEOUT);
        let actual = guard.clone();
        let result = transport.command_with_guard(
            "AT",
            None,
            timeout,
            4096,
            Arc::new(move || check_finite(deadline, &actual).is_ok()),
        );
        check_finite(deadline, &guard)?;
        match result {
            Ok(response) if probe_succeeded(&response) => Ok(()),
            Err(error) if error.code == "usb_at_disconnected" => Err(failed(
                "usb_at_target_disconnected",
                "The retained USB connection is no longer present",
            )),
            Err(error) if error.code == "module_call_admission_changed" => Err(failed(
                "usb_at_reopen_revoked",
                "USB reopen authority is no longer active",
            )),
            _ => Err(failed(
                "usb_at_probe_failed",
                "The selected USB AT interface did not confirm its probe",
            )),
        }
    })();
    match result {
        Ok(()) => Ok(transport),
        Err(error) => {
            Box::new(transport).checked_close()?;
            Err(error)
        }
    }
}

#[cfg(target_os = "macos")]
fn open_selected_device(
    device: rusb::Device<rusb::Context>,
    mut initial: Option<rusb::DeviceHandle<rusb::Context>>,
    owner: OwnerLease,
    deadline: Instant,
    guard: crate::module_radio::CallAdmission,
) -> Result<Box<dyn Transport>, TransportError> {
    use rusb::TransferType;
    check_finite(deadline, &guard)?;
    let descriptor = device.device_descriptor().map_err(native_error)?;
    let model = (descriptor.vendor_id(), descriptor.product_id());
    if !DEVICE_IDS.contains(&model) {
        return Err(unconfirmed());
    }
    let configuration = device.active_config_descriptor().map_err(native_error)?;
    let mut candidates = Vec::new();
    for interface in configuration.interfaces() {
        for alternate in interface.descriptors() {
            if let Some(value) = candidate(
                model,
                alternate.interface_number(),
                alternate.setting_number(),
                alternate.class_code(),
                alternate.sub_class_code(),
                alternate.protocol_code(),
                alternate.endpoint_descriptors().map(|endpoint| {
                    (
                        endpoint.address(),
                        endpoint.transfer_type() == TransferType::Bulk,
                    )
                }),
            ) {
                candidates.push(value);
            }
        }
    }
    candidates.sort_by_key(|candidate| usb_at_interface_priority(model, candidate.interface));
    let mut last = unconfirmed();
    for candidate in candidates {
        check_finite(deadline, &guard)?;
        let handle = match initial.take() {
            Some(handle) => handle,
            None => device.open().map_err(native_error)?,
        };
        check_finite(deadline, &guard)?;
        if handle.device().as_raw() != device.as_raw() {
            return Err(unconfirmed());
        }
        check_finite(deadline, &guard)?;
        if let Err(error) = handle.claim_interface(candidate.interface) {
            last = native_error(error);
            if error == rusb::Error::NoDevice {
                return Err(last);
            }
            continue;
        }
        let io = macos::LibusbIo {
            handle,
            candidate,
            claimed: true,
        };
        if let Err(error) = check_finite(deadline, &guard) {
            Box::new(io).checked_close()?;
            return Err(error);
        }
        if candidate.alternate != 0 {
            if let Err(error) = io
                .handle
                .set_alternate_setting(candidate.interface, candidate.alternate)
            {
                last = native_error(error);
                Box::new(io).checked_close()?;
                if error == rusb::Error::NoDevice {
                    return Err(last);
                }
                continue;
            }
        }
        let description = format!(
            "USB AT · {:04x}:{:04x} interface {} out 0x{:02x} in 0x{:02x}",
            model.0, model.1, candidate.interface, candidate.endpoint_out, candidate.endpoint_in
        );
        match probe_guarded(
            NativeTransport::new(Box::new(io), description, Some(owner.clone())),
            deadline,
            guard.clone(),
        ) {
            Ok(transport) => return Ok(Box::new(transport)),
            Err(error) => {
                if error.code != "usb_at_probe_failed" {
                    return Err(error);
                }
                last = error;
            }
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests;
