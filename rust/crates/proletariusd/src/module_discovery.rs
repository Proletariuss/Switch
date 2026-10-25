//! Read-only USB presence observation. This never selects a transport, sends
//! AT/APDU, opens a device, or claims that radio/bootstrap completed.
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) use proletarius_api_contract::HealthUsbDevice;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObservationSource {
    NativeUsb,
    Ioreg,
    Absent,
    Unavailable,
}
#[derive(Clone, Debug)]
pub(crate) struct UsbFallback {
    pub(crate) owner_epoch: u64,
    pub(crate) device: Option<HealthUsbDevice>,
    pub(crate) port: String,
    pub(crate) discovery_error: String,
    pub(crate) source: ObservationSource,
}

pub(crate) fn uses_native_usb(
    radio: &crate::module_radio::Service,
    usb: &crate::usb_at::Owner,
) -> bool {
    if usb.native_managed() {
        return true;
    }
    match radio.configured_usb_transport() {
        Some(mode) => mode,
        None => usb.sms_status_config().is_some_and(|(serial, _)| !serial),
    }
}

type Discovery = Result<Option<HealthUsbDevice>, &'static str>;
#[derive(Clone)]
pub(crate) struct Observer(Arc<Inner>);
struct Inner {
    state: Mutex<State>,
    program: PathBuf,
    args: Vec<String>,
    timeout: Duration,
    ttl: Duration,
    max_bytes: usize,
    #[cfg(test)]
    test_result: Option<Discovery>,
}
#[derive(Default)]
struct State {
    revision: u64,
    cached: Option<(u64, Instant, Discovery)>,
    pending: Option<(u64, watch::Receiver<Option<Discovery>>)>,
}
impl Observer {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            program: "/usr/sbin/ioreg".into(),
            args: ["-r", "-c", "IOUSBHostInterface", "-l", "-w", "0"]
                .map(str::to_owned)
                .into(),
            timeout: Duration::from_secs(5),
            ttl: Duration::from_secs(2),
            max_bytes: 1024 * 1024,
            #[cfg(test)]
            test_result: None,
        }))
    }

    #[cfg(test)]
    pub(crate) fn with_program_for_tests(program: PathBuf) -> Self {
        let mut observer = Self::new();
        let inner = Arc::get_mut(&mut observer.0).expect("new unique observer");
        inner.program = program;
        inner.args.clear();
        observer
    }

    #[cfg(test)]
    pub(crate) fn absent_for_tests() -> Self {
        Self::with_test_output("")
    }

    #[cfg(test)]
    pub(crate) fn with_test_output(output: &str) -> Self {
        let mut observer = Self::new();
        Arc::get_mut(&mut observer.0)
            .expect("new unique observer")
            .test_result = Some(Ok(crate::network_diagnostic::parse_health_usb_device(
            output,
        )));
        observer
    }

    /// `observation_epoch` is the caller's captured physical-device epoch; the
    /// caller must revalidate it before publishing. Cache entries never grant
    /// hardware authority. Unknown external hotplug is only a timed historical
    /// ioreg observation until the physical owner observes the new epoch.
    pub(crate) async fn capture_usb_fallback(
        &self,
        usb_at: &crate::usb_at::Owner,
        configured_port: Option<&str>,
        observation_epoch: u64,
    ) -> UsbFallback {
        if let Some(description) = usb_at.active_description() {
            self.invalidate();
            return native_usb(description, observation_epoch);
        }
        let observation = self.observe(observation_epoch).await;
        // A device opened while ioreg was running takes precedence over that
        // older descriptor. This reads the real owner without a second probe.
        if let Some(description) = usb_at.active_description() {
            self.invalidate();
            return native_usb(description, observation_epoch);
        }
        project(observation, configured_port, observation_epoch)
    }

    fn invalidate(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.revision = state.revision.wrapping_add(1);
        state.cached = None;
    }

    async fn observe(&self, epoch: u64) -> Discovery {
        loop {
            let (pending_epoch, mut receiver) = {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some((cached_epoch, at, result)) = &state.cached
                    && *cached_epoch == epoch
                    && at.elapsed() < self.0.ttl
                {
                    return result.clone();
                }
                if let Some(pending) = &state.pending {
                    pending.clone()
                } else {
                    let (sender, receiver) = watch::channel(None);
                    state.pending = Some((epoch, receiver.clone()));
                    let revision = state.revision;
                    let owner = self.clone();
                    // The actual child belongs to this retained task, never to
                    // a UI waiter. Cancellation cannot skip kill/wait cleanup.
                    tokio::spawn(async move {
                        let result = owner.run().await;
                        {
                            let mut state = owner.0.state.lock().unwrap_or_else(|e| e.into_inner());
                            if state.revision == revision {
                                state.cached = Some((epoch, Instant::now(), result.clone()));
                            }
                            state.pending = None;
                        }
                        sender.send_replace(Some(result));
                    });
                    (epoch, receiver)
                }
            };
            let result = loop {
                if let Some(result) = receiver.borrow().clone() {
                    break result;
                }
                if receiver.changed().await.is_err() {
                    let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state
                        .pending
                        .as_ref()
                        .is_some_and(|(key, _)| *key == pending_epoch)
                    {
                        state.pending = None;
                    }
                    break Err("ioreg_worker_unavailable");
                }
            };
            if pending_epoch == epoch {
                return result;
            }
            // One retained subprocess at a time, even during rapid epoch
            // changes. A different epoch awaits old actual completion first.
        }
    }

    async fn run(&self) -> Discovery {
        #[cfg(test)]
        if let Some(result) = &self.0.test_result {
            return result.clone();
        }
        let mut command = Command::new(&self.0.program);
        command
            .args(&self.0.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| "ioreg_spawn_failed")?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.start_kill();
            child
                .wait()
                .await
                .map_err(|_| "ioreg_cleanup_unconfirmed")?;
            return Err("ioreg_output_unavailable");
        };
        let operation = async {
            let mut bytes = Vec::new();
            stdout
                .take((self.0.max_bytes + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| "ioreg_read_failed")?;
            if bytes.len() > self.0.max_bytes {
                return Err("ioreg_output_limit");
            }
            let status = child.wait().await.map_err(|_| "ioreg_wait_failed")?;
            if !status.success() {
                return Err("ioreg_failed");
            }
            let output = String::from_utf8(bytes).map_err(|_| "ioreg_invalid_utf8")?;
            Ok(crate::network_diagnostic::parse_health_usb_device(&output))
        };
        match tokio::time::timeout(self.0.timeout, operation).await {
            Ok(Ok(device)) => Ok(device),
            result => {
                // Timeout/read-limit/error is not process-exit proof. Do not
                // publish failure or permit the next worker before real wait.
                let _ = child.start_kill();
                child
                    .wait()
                    .await
                    .map_err(|_| "ioreg_cleanup_unconfirmed")?;
                match result {
                    Ok(Err(error)) => Err(error),
                    _ => Err("ioreg_timeout"),
                }
            }
        }
    }
}

fn native_usb(description: String, owner_epoch: u64) -> UsbFallback {
    UsbFallback {
        owner_epoch,
        device: Some(HealthUsbDevice {
            product: description.clone(),
            vendor: String::new(),
            vendor_id: String::new(),
            product_id: String::new(),
            location_id: String::new(),
            speed: String::new(),
            mode: "USB AT".into(),
            interfaces: None,
        }),
        port: description,
        discovery_error: String::new(),
        source: ObservationSource::NativeUsb,
    }
}
fn project(observation: Discovery, configured_port: Option<&str>, owner_epoch: u64) -> UsbFallback {
    let port = configured_port
        .filter(|port| !port.trim().is_empty())
        .unwrap_or("未发现 AT 串口")
        .to_owned();
    let (device, source, discovery_error) = match observation {
        Ok(Some(device)) => (
            Some(device),
            ObservationSource::Ioreg,
            "已检测到候选 USB 模块，AT/QMI 控制通道尚未建立".into(),
        ),
        Ok(None) => (
            None,
            ObservationSource::Absent,
            "未检测到 DJI USB 模块；USB AT 控制通道未连接".into(),
        ),
        Err(code) => (
            None,
            ObservationSource::Unavailable,
            format!("USB 设备发现失败（{code}）"),
        ),
    };
    UsbFallback {
        owner_epoch,
        device,
        port,
        discovery_error,
        source,
    }
}

#[cfg(test)]
mod tests;
