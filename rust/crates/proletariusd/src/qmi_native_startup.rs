//! Production selection and lifetime of the optional Rust QMI owner.
//!
//! The former Go entry point always supplied an empty selector; it did not
//! expose a QMI device path or attach its QMI callbacks to a public handler.
//! Keep that exact selection and optional-start behavior in the native daemon.

use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::QmiEnhancement;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_INITIAL: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);
const READER_CHECK_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum NativeQmiConfig {
    #[default]
    At,
    Qmi,
}

impl NativeQmiConfig {
    pub(crate) fn from_backend_mode(mode: &str) -> Result<Self, String> {
        let mode = mode.trim().to_lowercase();
        match mode.as_str() {
            "" | "at" => Ok(Self::At),
            "qmi" => Ok(Self::Qmi),
            _ => Err(format!("unsupported -backend {mode:?} (want at|qmi)")),
        }
    }

    pub(crate) fn from_backend_args(
        arguments: &crate::daemon_args::DaemonArgs,
    ) -> Result<Self, String> {
        let mode = arguments
            .backend
            .as_deref()
            .unwrap_or(std::ffi::OsStr::new("at"))
            .to_str()
            .ok_or_else(|| "-backend must be valid UTF-8 (want at|qmi)".to_owned())?;
        Self::from_backend_mode(mode)
    }
}

/// Sole production start/stop guard. It must outlive native request handling.
/// Start and shutdown are blocking and belong on the daemon's blocking pool.
pub(crate) struct NativeQmiLifecycle {
    session: Option<OwnedSession>,
    startup_worker: Option<JoinHandle<Result<(), String>>>,
    auto_epoch: Option<u64>,
    startup_error: Option<String>,
    shutdown_result: Option<Result<(), String>>,
}

struct OwnedSession(Option<QmiEnhancement>, u64);

impl OwnedSession {
    fn close(&mut self) -> Result<(), String> {
        self.0.take().map_or(Ok(()), |owner| {
            // Ordinary operations still reject a poisoned owner. Shutdown
            // alone must recover its contents to release any acquired USB
            // session and forget the recovery selector after a worker panic.
            owner
                .close_auto_epoch(self.1)
                .map_err(|error| format!("QMI close failed: {error}"))
        })
    }
}

impl Drop for OwnedSession {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            crate::diagnostics::write_line(format_args!(
                "switchd-rs: QMI enhancement cleanup failed: {error}"
            ));
        }
    }
}

impl NativeQmiLifecycle {
    pub(crate) fn start(config: NativeQmiConfig, owner: QmiEnhancement) -> Self {
        Self::start_with_timeout(config, owner, STARTUP_TIMEOUT)
    }

    pub(super) fn start_with_timeout(
        config: NativeQmiConfig,
        owner: QmiEnhancement,
        timeout: Duration,
    ) -> Self {
        let mut lifecycle = Self {
            session: None,
            startup_worker: None,
            auto_epoch: None,
            startup_error: None,
            shutdown_result: None,
        };
        if config == NativeQmiConfig::At {
            return lifecycle;
        }
        let (epoch, control_path) = match owner.enable() {
            Ok(selection) => selection,
            Err(error) => {
                lifecycle.startup_error = Some(error);
                return lifecycle;
            }
        };
        lifecycle.auto_epoch = Some(epoch);

        // A rendezvous channel transfers ownership only while startup is
        // actually waiting. If startup times out, a late successful open is
        // closed on the worker, never installed as an orphaned USB session.
        let (sender, receiver) = mpsc::sync_channel(0);
        let worker = thread::Builder::new()
            .name("switch-qmi-startup".to_owned())
            .spawn(move || {
                let mut session = OwnedSession(Some(owner), epoch);
                let opened = session
                    .0
                    .as_ref()
                    .expect("owned QMI session")
                    .open_auto(control_path, epoch)
                    .and_then(|_| {
                        // A bounded open accepted before revocation still owns
                        // its acquired resources. Revalidate the startup handoff;
                        // rejection takes the same explicit cleanup path below.
                        session
                            .0
                            .as_ref()
                            .expect("owned QMI session")
                            .confirm_startup_admission(epoch)
                    });
                match opened {
                    Ok(_) => match sender.send(Ok(session)) {
                        Ok(()) => Ok(()),
                        // No waiter accepted ownership. Close explicitly so a
                        // late cleanup error is returned by the retained join.
                        Err(mpsc::SendError(Ok(mut session))) => session.close(),
                        Err(mpsc::SendError(Err(_))) => unreachable!("sent successful session"),
                    },
                    Err(error) => {
                        // Match the former failed-start cleanup, which also
                        // clears the selected path and its retry backoff.
                        let cleanup = session.close();
                        let message = match &cleanup {
                            Ok(()) => error.error,
                            Err(cleanup) => format!("{}; cleanup failed: {cleanup}", error.error),
                        };
                        let _ = sender.send(Err(message));
                        cleanup
                    }
                }
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                lifecycle.startup_error = Some(format!("QMI startup worker failed: {error}"));
                return lifecycle;
            }
        };
        lifecycle.startup_worker = Some(worker);
        match receiver.recv_timeout(timeout) {
            Ok(Ok(session)) => lifecycle.session = Some(session),
            Ok(Err(error)) => lifecycle.startup_error = Some(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lifecycle.startup_error = Some(format!(
                    "QMI enhancement startup timed out after {} ms; late open will be closed",
                    timeout.as_millis()
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                lifecycle.startup_error = Some("QMI startup worker stopped".to_owned());
            }
        }
        lifecycle
    }

    pub(crate) fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    /// Joining also waits for late-start cleanup; no worker is abandoned when
    /// the daemon shuts down. Explicit close clears recovery configuration.
    pub(crate) fn shutdown(&mut self) -> Result<(), String> {
        if let Some(result) = &self.shutdown_result {
            return result.clone();
        }
        self.shutdown_result = Some(Err(
            "QMI shutdown did not reach a confirmed final result".to_owned()
        ));
        let result = close_and_join(self.session.take(), self.startup_worker.take());
        // Consumed handles are not proof of success. Preserve an unknown or
        // failed close permanently; no generic retry may replay physical I/O.
        self.shutdown_result = Some(result.clone());
        result
    }
}

fn combine_cleanup_results(
    closed: Result<(), String>,
    joined: Result<(), String>,
) -> Result<(), String> {
    match (closed, joined) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(closed), Err(joined)) => Err(format!("{closed}; {joined}")),
    }
}

fn close_and_join(
    session: Option<OwnedSession>,
    startup_worker: Option<JoinHandle<Result<(), String>>>,
) -> Result<(), String> {
    let closed = session.map_or(Ok(()), |mut session| {
        // This boundary owns the cleanup obligation. Preserve an explicit
        // unknown result on panic while still joining the startup worker.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.close())).unwrap_or_else(
            |_| Err("QMI session close panicked; physical release unconfirmed".into()),
        )
    });
    let joined = startup_worker.map_or(Ok(()), |worker| {
        worker
            .join()
            .map_err(|_| "QMI startup worker panicked".to_owned())
            .and_then(|result| result)
    });
    combine_cleanup_results(closed, joined)
}

impl Drop for NativeQmiLifecycle {
    fn drop(&mut self) {
        if self.session.is_none() && self.startup_worker.is_none() {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            // A cancelled async startup waiter or an early daemon error can
            // drop this guard on a runtime worker. Keep blocking cleanup off
            // that worker; Tokio retains blocking jobs through shutdown.
            let session = self.session.take();
            let startup_worker = self.startup_worker.take();
            runtime.spawn_blocking(move || {
                if let Err(error) = close_and_join(session, startup_worker) {
                    crate::diagnostics::write_line(format_args!(
                        "switchd-rs: QMI enhancement shutdown failed: {error}"
                    ));
                }
            });
            return;
        }
        if let Err(error) = self.shutdown() {
            crate::diagnostics::write_line(format_args!(
                "switchd-rs: QMI enhancement shutdown failed: {error}"
            ));
        }
    }
}

/// One retained, serialized QMI owner across late USB attach and reader loss.
/// It never starts a second open until the previous lifecycle has joined its
/// startup worker and confirmed physical cleanup. AT/serial discovery owns a
/// separate interface and is neither polled nor reconfigured here.
pub(crate) struct NativeQmiRetryLifecycle {
    control: Arc<RetryControl>,
    worker: Option<JoinHandle<Result<(), String>>>,
    startup_error: Option<String>,
    shutdown_result: Option<Result<(), String>>,
}

#[derive(Default)]
struct RetryControl {
    state: Mutex<RetryState>,
    changed: Condvar,
}

#[derive(Default)]
struct RetryState {
    stopped: bool,
    stop_epoch: u64,
    wake_epoch: u64,
    #[cfg(test)]
    paused_count: u64,
    #[cfg(test)]
    connected_count: u64,
}

impl RetryControl {
    fn snapshot(&self) -> (bool, u64, u64) {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        (state.stopped, state.stop_epoch, state.wake_epoch)
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.stopped = true;
        state.stop_epoch = state.stop_epoch.wrapping_add(1);
        state.wake_epoch = state.wake_epoch.wrapping_add(1);
        self.changed.notify_all();
    }

    fn wait_or_stop(&self, delay: Duration, observed_wake: &mut u64) -> bool {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let (state, _) = self
            .changed
            .wait_timeout_while(state, delay, |state| {
                !state.stopped && state.wake_epoch == *observed_wake
            })
            .unwrap_or_else(|error| error.into_inner());
        *observed_wake = state.wake_epoch;
        state.stopped
    }

    #[cfg(test)]
    fn wake_for_tests(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.wake_epoch = state.wake_epoch.wrapping_add(1);
        self.changed.notify_all();
    }

    #[cfg(test)]
    fn note_paused_for_tests(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.paused_count += 1;
        self.changed.notify_all();
    }

    #[cfg(test)]
    fn note_connected_for_tests(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.connected_count += 1;
        self.changed.notify_all();
    }

    #[cfg(test)]
    fn wait_for_phase_for_tests(&self, expected: u64, connected: bool) {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                let count = if connected {
                    state.connected_count
                } else {
                    state.paused_count
                };
                count < expected
            })
            .unwrap_or_else(|error| error.into_inner());
        let count = if connected {
            state.connected_count
        } else {
            state.paused_count
        };
        assert!(
            count >= expected,
            "QMI retry did not reach requested fake-owner phase"
        );
    }
}

fn retry_delay(failures: u32, initial: Duration, maximum: Duration) -> Duration {
    let shift = failures.saturating_sub(1).min(6);
    initial.saturating_mul(1_u32 << shift).min(maximum)
}

impl NativeQmiRetryLifecycle {
    pub(crate) fn start(config: NativeQmiConfig, owner: QmiEnhancement) -> Self {
        match Self::start_with_timing(
            config,
            owner,
            STARTUP_TIMEOUT,
            RETRY_INITIAL,
            RETRY_MAX,
            READER_CHECK_INTERVAL,
        ) {
            Ok(lifecycle) => lifecycle,
            Err(error) => Self {
                control: Arc::new(RetryControl::default()),
                worker: None,
                startup_error: Some(error.clone()),
                shutdown_result: Some(Err(error)),
            },
        }
    }

    pub(crate) fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    pub(super) fn start_with_timing(
        config: NativeQmiConfig,
        owner: QmiEnhancement,
        startup_timeout: Duration,
        retry_initial: Duration,
        retry_max: Duration,
        reader_check_interval: Duration,
    ) -> Result<Self, String> {
        let control = Arc::new(RetryControl::default());
        let worker = if config == NativeQmiConfig::At {
            None
        } else {
            let retained = control.clone();
            let reporting_owner = owner.clone();
            Some(
                thread::Builder::new()
                    .name("switch-qmi-retry".to_owned())
                    .spawn(move || {
                        let result = run_retry_loop(
                            owner,
                            retained,
                            startup_timeout,
                            retry_initial,
                            retry_max,
                            reader_check_interval,
                        );
                        let result =
                            combine_cleanup_results(result, reporting_owner.close_for_shutdown());
                        if let Err(error) = &result {
                            reporting_owner.record_retry_failure(error);
                            crate::diagnostics::write_line(format_args!(
                                "switchd-rs: QMI retry stopped: {error}"
                            ));
                        }
                        result
                    })
                    .map_err(|error| format!("QMI retry worker failed: {error}"))?,
            )
        };
        Ok(Self {
            control,
            worker,
            startup_error: None,
            shutdown_result: None,
        })
    }

    #[cfg(test)]
    pub(super) fn recheck_for_tests(&self) {
        self.control.wake_for_tests();
    }

    #[cfg(test)]
    pub(super) fn wait_for_paused_for_tests(&self, expected: u64) {
        self.control.wait_for_phase_for_tests(expected, false);
    }

    #[cfg(test)]
    pub(super) fn wait_for_connected_for_tests(&self, expected: u64) {
        self.control.wait_for_phase_for_tests(expected, true);
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), String> {
        if let Some(result) = &self.shutdown_result {
            return result.clone();
        }
        self.shutdown_result = Some(Err("QMI retry shutdown unconfirmed".to_owned()));
        self.control.stop();
        let result = self.worker.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| "QMI retry worker panicked".to_owned())?
        });
        self.shutdown_result = Some(result.clone());
        result
    }
}

impl Drop for NativeQmiRetryLifecycle {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            crate::diagnostics::write_line(format_args!(
                "switchd-rs: QMI retry cleanup failed: {error}"
            ));
        }
    }
}

fn run_retry_loop(
    owner: QmiEnhancement,
    control: Arc<RetryControl>,
    startup_timeout: Duration,
    retry_initial: Duration,
    retry_max: Duration,
    reader_check_interval: Duration,
) -> Result<(), String> {
    let mut failures = 0_u32;
    loop {
        let (stopped, attempt_epoch, mut wake_epoch) = control.snapshot();
        if stopped {
            return Ok(());
        }
        if !owner.auto_reconnect_desired()? {
            #[cfg(test)]
            control.note_paused_for_tests();
            if control.wait_or_stop(reader_check_interval, &mut wake_epoch) {
                return Ok(());
            }
            continue;
        }
        let mut lifecycle = NativeQmiLifecycle::start_with_timeout(
            NativeQmiConfig::Qmi,
            owner.clone(),
            startup_timeout,
        );
        let (stopped, current_epoch, _) = control.snapshot();
        if stopped || current_epoch != attempt_epoch {
            return lifecycle.shutdown();
        }
        let auto_epoch = lifecycle
            .auto_epoch
            .ok_or("QMI automatic epoch was not captured")?;
        if !owner.auto_epoch_current(auto_epoch)? {
            lifecycle.shutdown()?;
            continue;
        }
        if let Some(error) = lifecycle.startup_error() {
            crate::diagnostics::write_line(format_args!(
                "switchd-rs: optional QMI enhancement unavailable: {error}"
            ));
            lifecycle.shutdown()?; // a failed/unconfirmed close fences all future opens
            if owner.transport_status().is_none() {
                return Err("QMI owner state is unavailable after failed startup".to_owned());
            }
            failures = failures.saturating_add(1);
            if control.wait_or_stop(
                retry_delay(failures, retry_initial, retry_max),
                &mut wake_epoch,
            ) {
                return Ok(());
            }
            continue;
        }

        failures = 0;
        #[cfg(test)]
        control.note_connected_for_tests();
        loop {
            if control.wait_or_stop(reader_check_interval, &mut wake_epoch) {
                return lifecycle.shutdown();
            }
            if !owner.auto_epoch_current(auto_epoch)? {
                break;
            }
            let reader_lost = owner.observed_transport_closed();
            let initialized = owner
                .transport_status()
                .is_some_and(|status| status.session_initialized);
            if reader_lost || !initialized {
                crate::diagnostics::write_line(format_args!(
                    "switchd-rs: QMI transport ended; awaiting confirmed close before reconnect"
                ));
                break;
            }
        }
        lifecycle.shutdown()?;
        if !owner.auto_reconnect_desired()? {
            continue;
        }
        if control.wait_or_stop(retry_initial, &mut wake_epoch) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_panic_retains_startup_join_and_cached_unconfirmed_result() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct PanicSession {
            calls: Arc<AtomicUsize>,
            entered: mpsc::Sender<()>,
        }
        impl super::super::QmiSessionOps for PanicSession {
            fn probe(&mut self) -> Result<serde_json::Value, super::super::QmiReadFailure> {
                unreachable!("no QMI probe")
            }
            fn operator(&mut self) -> Result<serde_json::Value, super::super::QmiReadFailure> {
                unreachable!("no QMI operator request")
            }
            fn set_ims(
                &mut self,
                _: super::super::ImsSettingsUpdate,
            ) -> Result<(), super::super::QmiServiceClientError> {
                unreachable!("no QMI mutation")
            }
            fn close(&mut self) -> Result<(), String> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.entered.send(()).unwrap();
                panic!("injected QMI session close panic");
            }
            fn abort(&mut self) -> Result<(), String> {
                unreachable!("no QMI recovery abort")
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let (entered, closing) = mpsc::channel();
        let owner = QmiEnhancement::default();
        owner.state.lock().unwrap().session = Some(Box::new(PanicSession {
            calls: calls.clone(),
            entered,
        }));
        let (release, blocked) = mpsc::channel();
        let startup_worker = thread::spawn(move || {
            blocked.recv_timeout(Duration::from_secs(3)).unwrap();
            Err("startup cleanup failure".to_owned())
        });
        let mut lifecycle = NativeQmiLifecycle {
            session: Some(OwnedSession(Some(owner), 0)),
            startup_worker: Some(startup_worker),
            auto_epoch: Some(0),
            startup_error: None,
            shutdown_result: None,
        };
        let (finished, observed) = mpsc::channel();
        let finalizer = thread::spawn(move || {
            let first = lifecycle.shutdown().unwrap_err();
            let repeated = lifecycle.shutdown().unwrap_err();
            finished.send((first, repeated)).unwrap();
        });
        closing.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            observed.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        let (first, repeated) = observed.recv_timeout(Duration::from_secs(1)).unwrap();
        finalizer.join().unwrap();
        assert!(first.contains("session close panicked; physical release unconfirmed"));
        assert!(first.contains("startup cleanup failure"));
        assert_eq!(first, repeated);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn final_cleanup_preserves_close_and_join_failures_in_order() {
        assert_eq!(
            combine_cleanup_results(Err("close failure".into()), Err("join failure".into())),
            Err("close failure; join failure".into())
        );
    }

    #[test]
    fn final_join_receipt_is_cached_after_worker_handle_is_consumed() {
        let mut lifecycle = NativeQmiLifecycle {
            session: None,
            startup_worker: Some(thread::spawn(|| Err("late close failed".into()))),
            auto_epoch: None,
            startup_error: Some("startup timed out".into()),
            shutdown_result: None,
        };
        let first = lifecycle.shutdown().unwrap_err();
        assert_eq!(first, "late close failed");
        assert!(lifecycle.startup_worker.is_none());
        assert_eq!(lifecycle.shutdown().unwrap_err(), first);
    }

    #[test]
    fn panicked_startup_worker_never_becomes_success_on_repeated_shutdown() {
        let mut lifecycle = NativeQmiLifecycle {
            session: None,
            startup_worker: Some(thread::spawn(|| panic!("injected QMI startup panic"))),
            auto_epoch: None,
            startup_error: None,
            shutdown_result: None,
        };
        let first = lifecycle.shutdown().unwrap_err();
        assert!(first.contains("panicked"));
        assert_eq!(lifecycle.shutdown().unwrap_err(), first);
    }

    fn parse(arguments: &[&str]) -> Result<NativeQmiConfig, String> {
        NativeQmiConfig::from_backend_args(&crate::daemon_args::parse(
            arguments.iter().map(std::ffi::OsString::from),
        )?)
    }

    #[test]
    fn backend_flag_preserves_go_defaults_spelling_and_last_value() {
        for args in [vec![], vec!["-backend="], vec!["--backend", " AT "]] {
            assert_eq!(parse(&args).unwrap(), NativeQmiConfig::At);
        }
        for args in [
            vec!["-backend=qmi"],
            vec!["--backend= QMI "],
            vec!["-backend", "qmi"],
            vec!["--backend", "qmi"],
            vec!["-backend=at", "-backend=qmi"],
            vec!["-backend=invalid", "-backend=qmi"],
        ] {
            assert_eq!(parse(&args).unwrap(), NativeQmiConfig::Qmi);
        }
        assert_eq!(
            parse(&["-backend=qmi", "-backend=at"]).unwrap(),
            NativeQmiConfig::At
        );
        assert!(
            parse(&["-backend=mbim"])
                .unwrap_err()
                .contains("want at|qmi")
        );
        assert!(parse(&["-backend"]).is_err());
    }

    #[test]
    fn backend_flag_does_not_parse_path_values_or_positional_arguments() {
        for args in [
            vec!["--", "-backend=qmi"],
            vec!["positional", "-backend=qmi"],
            vec!["-port", "-backend=qmi"],
            vec!["--vowifi-config", "-backend=qmi"],
            vec!["--listen", "-backend=qmi"],
        ] {
            assert_eq!(parse(&args).unwrap(), NativeQmiConfig::At);
        }
        assert_eq!(
            parse(&["-port", "/dev/fake", "-backend=qmi"]).unwrap(),
            NativeQmiConfig::Qmi
        );
    }
}
