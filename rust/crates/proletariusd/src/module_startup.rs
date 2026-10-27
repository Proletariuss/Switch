//! Retained serial-first selection and native physical lifecycle bootstrap. Compatibility
//! process lifetime is not the lifetime of an accepted physical operation.
use crate::support_diagnostics::module::{self as diagnostic, Code, ResultKind, Stage, Transport};
use crate::{message_archive::InboxState, modem_actor::ModemActor, module_radio, usb_at};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, watch};

mod native_serial;
#[cfg(test)]
pub(crate) use native_serial::tests::{
    Fixture as SerialTestFixture, ICCID as SERIAL_TEST_ICCID, PRODUCER as SERIAL_TEST_PRODUCER,
};

const RETRY_DELAY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    Probing,
    Serial { port: String },
    NativeUsb,
    Failed { code: &'static str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StartupError {
    pub(crate) code: &'static str,
}

#[derive(Clone)]
pub(crate) struct Service(Arc<Inner>);
struct Inner {
    selection: watch::Sender<Selection>,
    stop: watch::Sender<bool>,
    finalize: watch::Sender<bool>,
    last_error: Mutex<Option<&'static str>>,
    completion: Mutex<Option<Result<(), StartupError>>>,
    finished: Notify,
}

enum Probe {
    Native(ModemActor),
    Explicit(String),
    #[cfg(test)]
    Fixture(std::sync::Arc<dyn Fn() -> TestProbeFuture + Send + Sync>),
}
#[cfg(test)]
type TestProbeFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    crate::modem_actor::DiscoveredATPort,
                    crate::modem_actor::ActorError,
                >,
            > + Send,
    >,
>;

enum ArchiveSource {
    Native,
    #[cfg(test)]
    Fixture(proletarius_store::ArchiveStore),
}
impl ArchiveSource {
    fn store(&self) -> Result<proletarius_store::ArchiveStore, &'static str> {
        match self {
            Self::Native => {
                crate::message_archive::archive().map_err(|_| "module_startup_archive_unavailable")
            }
            #[cfg(test)]
            Self::Fixture(store) => Ok(store.clone()),
        }
    }
}

impl Probe {
    async fn discover(
        &self,
    ) -> Result<crate::modem_actor::DiscoveredATPort, crate::modem_actor::ActorError> {
        match self {
            Self::Native(actor) => actor.discover_at_port_confirmed().await,
            // An explicit selection must never scan or fall back to another
            // physical device, even if the selected port is absent.
            Self::Explicit(port) => Ok(crate::modem_actor::DiscoveredATPort { port: port.clone() }),
            #[cfg(test)]
            Self::Fixture(probe) => probe().await,
        }
    }
}

impl Service {
    pub(crate) fn start(
        modem: ModemActor,
        usb: usb_at::Owner,
        radio: module_radio::Service,
        inbox: InboxState,
        native_producer: String,
    ) -> Self {
        Self::start_on_port(modem, usb, radio, inbox, native_producer, None)
    }

    pub(crate) fn start_on_port(
        modem: ModemActor,
        usb: usb_at::Owner,
        radio: module_radio::Service,
        inbox: InboxState,
        native_producer: String,
        explicit_port: Option<String>,
    ) -> Self {
        Self::start_inner(
            explicit_port.map_or_else(|| Probe::Native(modem.clone()), Probe::Explicit),
            modem,
            usb,
            radio,
            inbox,
            native_producer,
            ArchiveSource::Native,
        )
    }

    #[cfg(test)]
    pub(crate) fn start_serial_for_test(
        modem: ModemActor,
        usb: usb_at::Owner,
        radio: module_radio::Service,
        inbox: InboxState,
        native_producer: String,
        archive: proletarius_store::ArchiveStore,
        port: String,
    ) -> Self {
        let probe = Probe::Fixture(Arc::new(move || {
            let port = port.clone();
            Box::pin(async move { Ok(crate::modem_actor::DiscoveredATPort { port }) })
        }));
        Self::start_inner(
            probe,
            modem,
            usb,
            radio,
            inbox,
            native_producer,
            ArchiveSource::Fixture(archive),
        )
    }

    fn start_inner(
        probe: Probe,
        modem: ModemActor,
        usb: usb_at::Owner,
        radio: module_radio::Service,
        inbox: InboxState,
        native_producer: String,
        archive: ArchiveSource,
    ) -> Self {
        let (selection, _) = watch::channel(Selection::Probing);
        let (stop, stopped) = watch::channel(false);
        let (finalize, finalized) = watch::channel(false);
        let service = Self(Arc::new(Inner {
            selection,
            stop,
            finalize,
            last_error: Mutex::new(None),
            completion: Mutex::new(None),
            finished: Notify::new(),
        }));
        diagnostic::startup(
            Stage::Startup,
            ResultKind::Started,
            Code::None,
            Transport::Unknown,
        );
        let worker_service = service.clone();
        let worker = tokio::spawn(async move {
            worker_service
                .run(
                    probe,
                    modem,
                    usb,
                    radio,
                    inbox,
                    native_producer,
                    archive,
                    stopped,
                    finalized,
                )
                .await
        });
        let retained = service.clone();
        // This supervisor retains the real worker even if every shutdown or
        // selection observer disappears. A panic is an error, never a receipt.
        tokio::spawn(async move {
            let result = worker.await.unwrap_or(Err(StartupError {
                code: "module_startup_worker_failed",
            }));
            if let Err(error) = result {
                retained.error(Some(error.code));
                if matches!(
                    *retained.0.selection.borrow(),
                    Selection::Probing | Selection::Failed { .. }
                ) {
                    retained
                        .0
                        .selection
                        .send_replace(Selection::Failed { code: error.code });
                }
            }
            *retained
                .0
                .completion
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(result);
            diagnostic::startup(
                Stage::Startup,
                ResultKind::WaiterEnded,
                result
                    .err()
                    .map_or(Code::None, |error| Code::from_error(error.code)),
                Transport::Unknown,
            );
            retained.0.finished.notify_waiters();
        });
        service
    }

    /// The sender remains alive after a serial selection, avoiding a closed
    /// watch being mistaken for repeated changes in the main accept loop.
    pub(crate) fn selection(&self) -> watch::Receiver<Selection> {
        self.0.selection.subscribe()
    }

    pub(crate) fn last_error(&self) -> Option<&'static str> {
        *self.0.last_error.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Freeze new startup/reconnect/metadata stages without closing the port.
    /// Existing radio, eSIM, SMS and call cleanup still needs its physical owner.
    pub(crate) fn begin_shutdown(&self) {
        diagnostic::startup(
            Stage::Shutdown,
            ResultKind::Started,
            Code::None,
            Transport::Unknown,
        );
        self.0.stop.send_replace(true);
    }

    /// Finalize after business/radio cleanup: join accepted lifecycle work,
    /// exact physical serial close and the receive/archive worker. USB close
    /// remains Owner::shutdown's job. Cancelled waiters cannot cancel this work.
    pub(crate) async fn shutdown(&self) -> Result<(), StartupError> {
        self.begin_shutdown();
        self.0.finalize.send_replace(true);
        loop {
            let finished = self.0.finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            if let Some(result) = *self.0.completion.lock().unwrap_or_else(|e| e.into_inner()) {
                return result;
            }
            finished.await;
        }
    }

    fn error(&self, error: Option<&'static str>) {
        *self.0.last_error.lock().unwrap_or_else(|e| e.into_inner()) = error;
        if let Some(code) = error {
            diagnostic::startup(
                Stage::Startup,
                ResultKind::Failed,
                Code::from_error(code),
                Transport::Unknown,
            );
        }
    }

    async fn run(
        &self,
        probe: Probe,
        modem: ModemActor,
        usb: usb_at::Owner,
        radio: module_radio::Service,
        inbox: InboxState,
        native_producer: String,
        archive: ArchiveSource,
        mut stop: watch::Receiver<bool>,
        finalize: watch::Receiver<bool>,
    ) -> Result<(), StartupError> {
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            self.0.selection.send_replace(Selection::Probing);
            let found = diagnostic::startup_step(
                Stage::SerialProbe,
                Transport::Unknown,
                probe.discover(),
                |error| error.code(),
            )
            .await;
            if *stop.borrow() {
                return Ok(());
            }
            let error = match found {
                Ok(found) => {
                    return native_serial::run(
                        self,
                        modem,
                        radio,
                        inbox,
                        native_producer,
                        found.port,
                        archive,
                        stop,
                        finalize,
                    )
                    .await;
                }
                Err(error) if error.code() == "at_port_not_found" => {
                    match diagnostic::startup_step(
                        Stage::UsbOpen,
                        Transport::Usb,
                        usb.open_native_candidate(),
                        |error| error.public_code(),
                    )
                    .await
                    {
                        Ok(_) => break,
                        Err(error) if error.code == "usb_at_not_found" && !error.uncertain => {
                            // Neither serial nor native USB has a physical AT
                            // owner yet. A later USB CDC port must be scanned
                            // rather than being trapped behind a sticky USB
                            // choice made while no device was present.
                            self.error(Some(error.code));
                            self.0
                                .selection
                                .send_replace(Selection::Failed { code: error.code });
                            retry(&mut stop).await;
                            continue;
                        }
                        Err(error) if error.uncertain => {
                            // An uncertain release cannot authorize a new
                            // serial probe or another physical USB open.
                            return Err(StartupError { code: error.code });
                        }
                        Err(error) => {
                            self.error(Some(error.code));
                            break;
                        }
                    }
                }
                Err(error) => error.code(),
            };
            if *stop.borrow() {
                // The USB owner may have completed its retained open while a
                // shutdown request was pending. Main still closes that exact
                // owner; never publish a new transport selection afterward.
                return Ok(());
            }
            self.error(Some(error));
            self.0
                .selection
                .send_replace(Selection::Failed { code: error });
            retry(&mut stop).await;
        }
        if *stop.borrow() {
            return Ok(());
        }
        // A confirmed native USB AT open fixes the transport choice. A failed
        // probe other than definite no-device stays on the USB side; ownership
        // or inbox admission may retry, but cannot select another transport
        // after USB work may have touched a physical interface.
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            let error = match usb.claim_native_management() {
                Ok(()) => match inbox.bind_native_module_producer(native_producer.clone()) {
                    Ok(()) => break,
                    Err(_) => "module_startup_inbox_unavailable",
                },
                Err(error) => error.public_code(),
            };
            self.error(Some(error));
            self.0
                .selection
                .send_replace(Selection::Failed { code: error });
            retry(&mut stop).await;
        }
        // Both physical ownership and native call-mode source identity are
        // established before main can spawn a managed compatibility process.
        self.0.selection.send_replace(Selection::NativeUsb);
        diagnostic::startup(
            Stage::TransportClaim,
            ResultKind::Succeeded,
            Code::None,
            Transport::Usb,
        );
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            if !usb.native_sms_ready() {
                let result = bootstrap_usb(&usb, &radio, &stop).await;
                if *stop.borrow() {
                    return Ok(());
                }
                self.error(result.err());
            }
            retry(&mut stop).await;
        }
    }
}

/// Preserve the explicit serial selector of the former backend. `auto` and
/// an empty value retain serial-first discovery; a concrete port is sticky.
pub(crate) fn explicit_port(
    arguments: &crate::daemon_args::DaemonArgs,
) -> Result<Option<String>, String> {
    let Some(value) = arguments.port.as_deref() else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .ok_or_else(|| "-port must be valid UTF-8".to_owned())?
        .trim();
    Ok((!value.is_empty() && value != "auto").then(|| value.to_owned()))
}

#[cfg(test)]
mod explicit_port_tests {
    use super::explicit_port;

    fn parse(arguments: &[&str]) -> Result<Option<String>, String> {
        explicit_port(&crate::daemon_args::parse(
            arguments.iter().map(std::ffi::OsString::from),
        )?)
    }

    #[test]
    fn explicit_port_preserves_forms_last_value_and_discovery_defaults() {
        for arguments in [
            vec!["-port=/dev/fake"],
            vec!["--port=/dev/fake"],
            vec!["-port", " /dev/fake "],
            vec!["--port", "/dev/fake"],
            vec!["-port=/dev/old", "-port=/dev/fake"],
        ] {
            assert_eq!(parse(&arguments).unwrap().as_deref(), Some("/dev/fake"));
        }
        for arguments in [
            vec![],
            vec!["-port="],
            vec!["-port", "   "],
            vec!["-port=auto"],
            vec!["-port=/dev/old", "--port=auto"],
        ] {
            assert_eq!(parse(&arguments).unwrap(), None);
        }
        assert!(parse(&["--port"]).is_err());
    }

    #[test]
    fn explicit_port_does_not_reinterpret_other_flags_values_or_positionals() {
        for arguments in [
            vec!["-backend", "-port=/dev/wrong"],
            vec!["--listen", "--port=/dev/wrong"],
            vec!["-vowifi-config", "-port=/dev/wrong"],
            vec!["positional", "-port=/dev/wrong"],
            vec!["--", "-port=/dev/wrong"],
            vec!["-", "-port=/dev/wrong"],
        ] {
            assert_eq!(parse(&arguments).unwrap(), None);
        }
        assert_eq!(
            parse(&[
                "-backend",
                "qmi",
                "-listen",
                "unix:/tmp/fake",
                "-port",
                "/dev/fake"
            ])
            .unwrap()
            .as_deref(),
            Some("/dev/fake")
        );
        assert_eq!(
            parse(&["-port=/dev/first", "positional", "-port=/dev/wrong"])
                .unwrap()
                .as_deref(),
            Some("/dev/first")
        );
    }
}

async fn retry(stop: &mut watch::Receiver<bool>) {
    if !*stop.borrow() {
        tokio::select! {
            _ = tokio::time::sleep(RETRY_DELAY) => {},
            _ = stop.changed() => {},
        }
    }
}

async fn bootstrap_usb(
    usb: &usb_at::Owner,
    radio: &module_radio::Service,
    stop: &watch::Receiver<bool>,
) -> Result<(), &'static str> {
    let session = diagnostic::startup_step(
        Stage::SessionOpen,
        Transport::Usb,
        usb.open_native_session(),
        |error| error.public_code(),
    )
    .await
    .map_err(|e| e.public_code())?;
    if *stop.borrow() {
        return Ok(());
    }
    diagnostic::startup_step(
        Stage::RadioConfigure,
        Transport::Usb,
        radio.configure_native_usb(&session),
        |error| error.public_code(),
    )
    .await
    .map_err(|e| e.public_code())?;
    if *stop.borrow() {
        return Ok(());
    }
    diagnostic::startup_step(
        Stage::RadioBootstrap,
        Transport::Usb,
        radio.bootstrap_native_usb(),
        |error| error.public_code(),
    )
    .await
    .map_err(|e| e.public_code())?;
    if *stop.borrow() {
        return Ok(());
    }
    if let Err(error) = diagnostic::startup_step(
        Stage::EsimWarmup,
        Transport::Usb,
        radio.warm_native_esim(),
        |error| error.public_code(),
    )
    .await
    {
        // Ordinary SIMs may not expose an eUICC. Only a confirmed release makes
        // ignoring that read failure safe for subsequent SMS admission.
        if error.public_code() == "module_startup_warm_cleanup_unconfirmed" {
            return Err(error.public_code());
        }
    }
    if *stop.borrow() {
        return Ok(());
    }
    let result = usb
        .commit_native_sms_ready(&session)
        .map_err(|e| e.public_code());
    diagnostic::startup(
        Stage::SmsReady,
        if result.is_ok() {
            ResultKind::Succeeded
        } else {
            ResultKind::Failed
        },
        result
            .as_ref()
            .err()
            .map_or(Code::None, |code| Code::from_error(code)),
        Transport::Usb,
    );
    if result.is_ok() {
        diagnostic::startup(
            Stage::Startup,
            ResultKind::Succeeded,
            Code::None,
            Transport::Usb,
        );
    }
    result
}

#[cfg(test)]
mod tests;
