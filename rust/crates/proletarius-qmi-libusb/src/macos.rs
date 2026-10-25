use crate::selection::{
    EndpointType, QmiInterfaceCandidate, UsbDeviceId, UsbEndpoint, UsbSelector,
    qmi_interface_candidate,
};
use proletarius_qmi_core::{CTL_SYNC, Client, ClientOptions, Transport};
use rusb::{Context, DeviceHandle, TransferType, UsbContext};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const QMI_READ_TIMEOUT: Duration = Duration::from_millis(100);
const QMI_WRITE_TIMEOUT: Duration = Duration::from_secs(3);
const QMI_SYNC_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_PROBE_ERRORS: usize = 32;

static QMI_USB_OWNER: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct OwnerLease {
    _inner: Arc<OwnerLeaseInner>,
}

struct OwnerLeaseInner;

impl OwnerLease {
    fn acquire() -> io::Result<Self> {
        QMI_USB_OWNER
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self {
                _inner: Arc::new(OwnerLeaseInner),
            })
            .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "QMI USB owner is active"))
    }
}

impl Drop for OwnerLeaseInner {
    fn drop(&mut self) {
        QMI_USB_OWNER.store(false, Ordering::Release);
    }
}

pub struct QmiUsbClient {
    pub client: Client,
    pub info: QmiUsbInfo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QmiUsbInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub interface_number: u8,
    pub alternate_setting: u8,
    pub endpoint_in: u8,
    pub endpoint_out: u8,
}

struct ClaimedDevice {
    handle: DeviceHandle<Context>,
    interface_number: u8,
    _owner: OwnerLease,
}

#[derive(Clone)]
struct LibusbTransport {
    state: Arc<RwLock<Option<ClaimedDevice>>>,
    endpoint_in: u8,
    endpoint_out: u8,
    closed: Arc<AtomicBool>,
}

/// Finds a supported USB QMI interface, claims it, and validates it by
/// completing CTL_SYNC before returning the only client/transport owner.
///
/// The function is macOS-only because Switch's production QMI path is a
/// libusb bulk interface rather than a `/dev/cdc-wdm*` node.
pub fn open_qmi_client(path: &str, options: ClientOptions) -> io::Result<QmiUsbClient> {
    let selector = UsbSelector::parse(path)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let owner = OwnerLease::acquire()?;
    let context = Context::new().map_err(libusb_error)?;
    let devices = context.devices().map_err(libusb_error)?;
    let mut found_matching_device = false;
    let mut probe_errors = Vec::new();

    for id in selector.device_ids() {
        for device in devices.iter() {
            let descriptor = match device.device_descriptor() {
                Ok(descriptor) => descriptor,
                Err(error) => {
                    push_probe_error(&mut probe_errors, format!("read USB descriptor: {error}"));
                    continue;
                }
            };
            if descriptor.vendor_id() != id.vendor_id || descriptor.product_id() != id.product_id {
                continue;
            }
            found_matching_device = true;

            let config = match device.active_config_descriptor() {
                Ok(config) => config,
                Err(error) => {
                    push_probe_error(
                        &mut probe_errors,
                        format!(
                            "device {:04x}:{:04x} active configuration: {error}",
                            id.vendor_id, id.product_id
                        ),
                    );
                    continue;
                }
            };

            let candidates = collect_candidates(*id, &config);
            for candidate in candidates {
                let handle = match device.open() {
                    Ok(handle) => handle,
                    Err(error) => {
                        push_probe_error(
                            &mut probe_errors,
                            format!(
                                "open device {:04x}:{:04x}: {error}",
                                id.vendor_id, id.product_id
                            ),
                        );
                        continue;
                    }
                };
                if let Err(error) = handle.claim_interface(candidate.interface_number) {
                    push_probe_error(
                        &mut probe_errors,
                        format!(
                            "claim interface {} ({:02x}/{:02x}/{:02x}): {error}",
                            candidate.interface_number,
                            candidate.class,
                            candidate.subclass,
                            candidate.protocol
                        ),
                    );
                    continue;
                }
                if candidate.alternate_setting != 0
                    && let Err(error) = handle.set_alternate_setting(
                        candidate.interface_number,
                        candidate.alternate_setting,
                    )
                {
                    let probe_failure = format!(
                        "select alternate setting {} on interface {}: {error}",
                        candidate.alternate_setting, candidate.interface_number
                    );
                    confirm_failed_interface_setup(handle, probe_failure.clone(), |handle| {
                        handle.release_interface(candidate.interface_number)
                    })?;
                    push_probe_error(&mut probe_errors, probe_failure);
                    continue;
                }

                let info = QmiUsbInfo {
                    vendor_id: id.vendor_id,
                    product_id: id.product_id,
                    interface_number: candidate.interface_number,
                    alternate_setting: candidate.alternate_setting,
                    endpoint_in: candidate.endpoint_in,
                    endpoint_out: candidate.endpoint_out,
                };
                let transport = LibusbTransport {
                    state: Arc::new(RwLock::new(Some(ClaimedDevice {
                        handle,
                        interface_number: candidate.interface_number,
                        _owner: owner.clone(),
                    }))),
                    endpoint_in: candidate.endpoint_in,
                    endpoint_out: candidate.endpoint_out,
                    closed: Arc::new(AtomicBool::new(false)),
                };
                let client = match start_client_with_cleanup(
                    transport,
                    options.clone(),
                    Client::from_transport,
                )? {
                    Ok(client) => client,
                    Err(error) => {
                        push_probe_error(
                            &mut probe_errors,
                            format!("start QMI transport reader: {error}"),
                        );
                        continue;
                    }
                };
                let sync = client
                    .send_request_with_timeout(0, 0, CTL_SYNC, &[], QMI_SYNC_TIMEOUT)
                    .and_then(|response| response.check_result());
                match sync {
                    Ok(()) => return Ok(QmiUsbClient { client, info }),
                    Err(error) => {
                        crate::confirm_failed_probe_cleanup(
                            format!(
                                "QMI CTL_SYNC failed on interface {}: {error}",
                                candidate.interface_number
                            ),
                            client.close(),
                        )?;
                        push_probe_error(
                            &mut probe_errors,
                            format!(
                                "QMI CTL_SYNC failed on interface {} ({:02x}/{:02x}/{:02x}, out 0x{:02x} in 0x{:02x}): {error}",
                                candidate.interface_number,
                                candidate.class,
                                candidate.subclass,
                                candidate.protocol,
                                candidate.endpoint_out,
                                candidate.endpoint_in
                            ),
                        );
                    }
                }
            }
        }
    }

    drop(owner);
    if !probe_errors.is_empty() {
        return Err(io::Error::other(format!(
            "QMI USB interface probe failed: {}",
            probe_errors.join("; ")
        )));
    }
    if found_matching_device {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no QMI bulk interface found",
        ))
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "QMI USB device not found",
        ))
    }
}

fn collect_candidates(
    id: UsbDeviceId,
    config: &rusb::ConfigDescriptor,
) -> Vec<QmiInterfaceCandidate> {
    let mut candidates = Vec::new();
    for interface in config.interfaces() {
        for descriptor in interface.descriptors() {
            let endpoints = descriptor
                .endpoint_descriptors()
                .map(|endpoint| UsbEndpoint {
                    address: endpoint.address(),
                    transfer_type: match endpoint.transfer_type() {
                        TransferType::Bulk => EndpointType::Bulk,
                        TransferType::Interrupt => EndpointType::Interrupt,
                        _ => EndpointType::Other,
                    },
                })
                .collect::<Vec<_>>();
            if let Some(candidate) = qmi_interface_candidate(
                id,
                descriptor.interface_number(),
                descriptor.setting_number(),
                descriptor.class_code(),
                descriptor.sub_class_code(),
                descriptor.protocol_code(),
                &endpoints,
            ) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

fn push_probe_error(errors: &mut Vec<String>, message: String) {
    if errors.len() < MAX_PROBE_ERRORS {
        errors.push(message);
    }
}

/// Retain the same claimed transport until reader construction succeeds.
/// The outer error is an unconfirmed cleanup receipt and must stop probing;
/// a confirmed cleanup leaves the inner construction error safe to report
/// before trying the next candidate.
fn start_client_with_cleanup<T: Transport + Clone + 'static>(
    transport: T,
    options: ClientOptions,
    start: impl FnOnce(
        Box<dyn Transport>,
        ClientOptions,
    ) -> Result<Client, proletarius_qmi_core::ClientError>,
) -> io::Result<Result<Client, proletarius_qmi_core::ClientError>> {
    let result = start(Box::new(transport.clone()), options);
    if let Err(error) = &result {
        crate::confirm_failed_probe_cleanup(
            format!("start QMI transport reader: {error}"),
            transport
                .shutdown()
                .map_err(proletarius_qmi_core::ClientError::Io),
        )?;
    }
    Ok(result)
}

impl Transport for LibusbTransport {
    fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "QMI transport closed",
            ));
        }
        let state = self
            .state
            .read()
            .map_err(|_| io::Error::other("QMI USB transport state poisoned"))?;
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "QMI transport closed",
            ));
        }
        let device = state
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Interrupted, "QMI transport closed"))?;
        match device
            .handle
            .read_bulk(self.endpoint_in, buffer, QMI_READ_TIMEOUT)
        {
            Ok(0) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "empty QMI USB read",
            )),
            Ok(length) => Ok(length),
            Err(rusb::Error::Timeout) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "QMI USB read timed out",
            )),
            Err(rusb::Error::Interrupted) => Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "QMI USB read interrupted",
            )),
            Err(error) => Err(libusb_error(error)),
        }
    }

    fn write_all(&self, data: &[u8]) -> io::Result<()> {
        self.write_all_timeout(data, QMI_WRITE_TIMEOUT)
    }

    fn write_all_timeout(&self, data: &[u8], timeout: Duration) -> io::Result<()> {
        let timeout = timeout.min(QMI_WRITE_TIMEOUT);
        let started = Instant::now();
        let mut offset = 0;
        while offset < data.len() {
            if self.closed.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "QMI transport closed",
                ));
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "QMI USB write timed out",
                ));
            }
            // libusb treats a zero millisecond timeout as infinite.
            let transfer_timeout = remaining.max(Duration::from_millis(1));
            let state = self
                .state
                .read()
                .map_err(|_| io::Error::other("QMI USB transport state poisoned"))?;
            let device = state.as_ref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Interrupted, "QMI transport closed")
            })?;
            let written =
                match device
                    .handle
                    .write_bulk(self.endpoint_out, &data[offset..], transfer_timeout)
                {
                    Ok(written) => written,
                    Err(rusb::Error::Timeout) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "QMI USB write timed out",
                        ));
                    }
                    Err(error) => return Err(libusb_error(error)),
                };
            if written == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "QMI USB write made no progress",
                ));
            }
            offset += written;
        }
        Ok(())
    }

    fn shutdown(&self) -> io::Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut state = self
            .state
            .write()
            .map_err(|_| io::Error::other("QMI USB transport state poisoned"))?;
        release_device(&mut state)
    }
}

fn release_device(state: &mut Option<ClaimedDevice>) -> io::Result<()> {
    let Some(device) = state.take() else {
        return Ok(());
    };
    // The handle also releases the interface during Drop if libusb reports a
    // release error here. Keeping the single-owner lease alive until after
    // that attempt prevents another in-process opener from racing teardown.
    let release_result = device.handle.release_interface(device.interface_number);
    drop(device);
    release_after_handle_drop(release_result)
}

/// A disconnected USB device cannot acknowledge RELEASE_INTERFACE. Once the
/// handle has actually been dropped, NoDevice confirms this old connection is
/// gone and cannot contend with a newly enumerated device. Other errors keep
/// physical cleanup unconfirmed and fence automatic reopen.
fn release_after_handle_drop(result: Result<(), rusb::Error>) -> io::Result<()> {
    match result {
        Ok(()) | Err(rusb::Error::NoDevice) => Ok(()),
        Err(error) => Err(libusb_error(error)),
    }
}

/// Failed setup already owns a claimed interface. Drop the handle before
/// classifying its release receipt, and fence all later candidates if that
/// receipt leaves physical cleanup unconfirmed.
fn confirm_failed_interface_setup<H>(
    handle: H,
    probe_failure: String,
    release: impl FnOnce(&H) -> Result<(), rusb::Error>,
) -> io::Result<()> {
    let released = release(&handle);
    drop(handle);
    crate::confirm_failed_probe_cleanup(
        probe_failure,
        release_after_handle_drop(released).map_err(proletarius_qmi_core::ClientError::Io),
    )
}

#[cfg(test)]
mod release_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Clone)]
    struct FakeTransport {
        shutdowns: Arc<AtomicUsize>,
        cleanup_failed: bool,
    }

    impl Transport for FakeTransport {
        fn read(&self, _: &mut [u8]) -> io::Result<usize> {
            panic!("reader constructor fixture must not read")
        }

        fn write_all(&self, _: &[u8]) -> io::Result<()> {
            panic!("reader constructor fixture must not write")
        }

        fn shutdown(&self) -> io::Result<()> {
            assert_eq!(self.shutdowns.fetch_add(1, Ordering::SeqCst), 0);
            if self.cleanup_failed {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected release failure",
                ))
            } else {
                Ok(())
            }
        }
    }

    fn failed_reader_fixture(cleanup_failed: bool) -> (usize, io::Result<()>) {
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let mut candidates = 0;
        let result = (|| {
            candidates += 1;
            let started = start_client_with_cleanup(
                FakeTransport {
                    shutdowns: shutdowns.clone(),
                    cleanup_failed,
                },
                ClientOptions::default(),
                |transport, _| {
                    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
                    drop(transport);
                    Err(proletarius_qmi_core::ClientError::Io(io::Error::other(
                        "injected reader spawn failure",
                    )))
                },
            )?;
            assert!(
                started
                    .err()
                    .expect("injected reader constructor must fail")
                    .to_string()
                    .contains("injected reader spawn failure")
            );
            candidates += 1;
            Ok(())
        })();
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
        (candidates, result)
    }

    #[test]
    fn failed_reader_constructor_confirmed_cleanup_allows_next_candidate() {
        let (candidates, result) = failed_reader_fixture(false);
        assert!(result.is_ok());
        assert_eq!(candidates, 2);
    }

    #[test]
    fn failed_reader_constructor_unknown_cleanup_stops_candidate_scan() {
        let (candidates, result) = failed_reader_fixture(true);
        let error = result.unwrap_err();
        let receipt = error
            .get_ref()
            .unwrap()
            .downcast_ref::<crate::ProbeCleanupFailure>()
            .unwrap();
        assert!(
            receipt
                .probe_failure
                .contains("injected reader spawn failure")
        );
        assert!(receipt.cleanup_failure.contains("injected release failure"));
        assert_eq!(candidates, 1);
    }

    struct FakeClaimedHandle(Arc<AtomicUsize>);

    impl Drop for FakeClaimedHandle {
        fn drop(&mut self) {
            assert_eq!(self.0.fetch_add(1, Ordering::SeqCst), 1);
        }
    }

    fn failed_setup_fixture(released: Result<(), rusb::Error>) -> (usize, io::Result<()>) {
        let phase = Arc::new(AtomicUsize::new(0));
        let mut candidates = 0;
        let result = (|| {
            candidates += 1;
            confirm_failed_interface_setup(
                FakeClaimedHandle(phase.clone()),
                "select alternate setting 1 on interface 4: Pipe".into(),
                |handle| {
                    assert_eq!(handle.0.fetch_add(1, Ordering::SeqCst), 0);
                    released
                },
            )?;
            candidates += 1;
            Ok(())
        })();
        assert_eq!(phase.load(Ordering::SeqCst), 2, "claimed handle must drop");
        (candidates, result)
    }

    #[test]
    fn failed_alternate_setting_confirmed_release_allows_next_candidate() {
        for released in [Ok(()), Err(rusb::Error::NoDevice)] {
            let (candidates, result) = failed_setup_fixture(released);
            assert!(result.is_ok());
            assert_eq!(candidates, 2);
        }
    }

    #[test]
    fn failed_alternate_setting_unknown_release_stops_candidate_scan() {
        for released in [rusb::Error::Access, rusb::Error::Other] {
            let (candidates, result) = failed_setup_fixture(Err(released));
            let error = result.unwrap_err();
            let receipt = error
                .get_ref()
                .unwrap()
                .downcast_ref::<crate::ProbeCleanupFailure>()
                .unwrap();
            assert!(receipt.probe_failure.contains("alternate setting 1"));
            assert!(receipt.cleanup_failure.contains("libusb:"));
            assert_eq!(candidates, 1);
        }
    }

    #[test]
    fn only_disconnected_no_device_is_confirmed_after_handle_drop() {
        assert!(release_after_handle_drop(Ok(())).is_ok());
        assert!(release_after_handle_drop(Err(rusb::Error::NoDevice)).is_ok());
        assert_eq!(
            release_after_handle_drop(Err(rusb::Error::Access))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(release_after_handle_drop(Err(rusb::Error::Other)).is_err());
    }
}

fn libusb_error(error: rusb::Error) -> io::Error {
    let kind = match error {
        rusb::Error::Access => io::ErrorKind::PermissionDenied,
        rusb::Error::NoDevice => io::ErrorKind::NotConnected,
        rusb::Error::Timeout => io::ErrorKind::TimedOut,
        rusb::Error::Interrupted => io::ErrorKind::Interrupted,
        rusb::Error::InvalidParam => io::ErrorKind::InvalidInput,
        rusb::Error::Overflow => io::ErrorKind::InvalidData,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("libusb: {error}"))
}
