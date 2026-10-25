//! Startup control-plane recovery. Cancellation joins every spawned CLI before
//! returning the card to its owner; it never rolls back the user's proxy state.
use super::{PlanError, SelectedTransport, SessionSelectionError};
use std::{future::Future, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, sync::watch, time::Instant};

const CLI: &str = "/Applications/Surge.app/Contents/Applications/surge-cli";
const CANCELLED: &str = "native_vowifi_start_cancelled";
const OUTPUT_LIMIT: u64 = 16 * 1024;

fn error(code: &'static str) -> PlanError {
    PlanError::SurgeRecovery(code)
}

struct Control {
    stop: Option<watch::Receiver<bool>>,
    deadline: Option<Instant>,
    #[cfg(test)]
    reap_ack: Option<std::sync::Arc<ReapAck>>,
}

#[cfg(test)]
#[derive(Default)]
struct ReapAck {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Control {
    fn new(stop: Option<watch::Receiver<bool>>) -> Self {
        Self {
            stop,
            deadline: None,
            #[cfg(test)]
            reap_ack: None,
        }
    }
    fn checkpoint(&self) -> Result<(), PlanError> {
        if self
            .stop
            .as_ref()
            .is_some_and(|s| *s.borrow() || s.has_changed().is_err())
        {
            return Err(error(CANCELLED));
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(error("surge_recovery_timeout"));
        }
        Ok(())
    }
    async fn observe<T>(&mut self, work: impl Future<Output = T>) -> Result<T, PlanError> {
        self.checkpoint()?;
        tokio::pin!(work);
        let cause = tokio::select! {
            biased;
            _ = cancelled(&mut self.stop) => error(CANCELLED),
            _ = deadline(self.deadline) => error("surge_recovery_timeout"),
            value = &mut work => return Ok(value),
        };
        // The selection tree receives the same stop/deadline. It must drain
        // its owned discovery/DNS/route children before we return cancellation.
        work.await;
        Err(cause)
    }
    async fn command(
        &mut self,
        cli: &Path,
        args: &[&str],
        budget: Duration,
    ) -> Result<Vec<u8>, PlanError> {
        self.checkpoint()?;
        let mut child = tokio::process::Command::new(cli)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| error("surge_cli_unavailable"))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = {
            let work = async {
                let mut stdout = stdout.take(OUTPUT_LIMIT + 1);
                let mut stderr = stderr.take(OUTPUT_LIMIT + 1);
                tokio::try_join!(
                    stdout.read_to_end(&mut out),
                    stderr.read_to_end(&mut err),
                    child.wait()
                )
            };
            tokio::select! {
                biased;
                _ = cancelled(&mut self.stop) => Err(error(CANCELLED)),
                _ = deadline(self.deadline) => Err(error("surge_recovery_timeout")),
                _ = tokio::time::sleep(budget) => Err(error("surge_control_timeout")),
                result = work => result.map_err(|_| error("surge_control_failed")),
            }
        };
        // Dropping a timeout future alone only schedules a kill. Explicitly reap
        // the real process before the session can release its physical lease.
        let (_, _, status) = match result {
            Ok(value) => value,
            Err(cause) => {
                let _ = child.start_kill();
                #[cfg(test)]
                if let Some(ack) = &self.reap_ack {
                    // Hold the actual owned Child at its post-kill wait
                    // boundary, so outer cancellation cannot fake a reap ACK.
                    ack.entered.notify_one();
                    ack.release.notified().await;
                }
                let join_failed = crate::system_proxy::reap_child_confirmed(&mut child).await;
                if join_failed {
                    return Err(error("surge_control_join_failed"));
                }
                return Err(cause);
            }
        };
        self.checkpoint()?;
        if !status.success() {
            return Err(error("surge_control_failed"));
        }
        if out.len() > OUTPUT_LIMIT as usize || err.len() > OUTPUT_LIMIT as usize {
            return Err(error("surge_control_invalid_response"));
        }
        // Go's CombinedOutput accepts feature diagnostics on either stream.
        out.push(b'\n');
        out.extend(err);
        Ok(out)
    }
    async fn feature(&mut self, cli: &Path, feature: &str) -> Result<bool, PlanError> {
        let bytes = self
            .command(cli, &["feature", "get", feature], Duration::from_secs(3))
            .await?;
        let value = String::from_utf8_lossy(&bytes).to_lowercase();
        if value.contains(&format!("{feature}: on")) {
            Ok(true)
        } else if value.contains(&format!("{feature}: off")) {
            Ok(false)
        } else {
            Err(error("surge_control_invalid_response"))
        }
    }
}
async fn cancelled(stop: &mut Option<watch::Receiver<bool>>) {
    let Some(stop) = stop else {
        return std::future::pending().await;
    };
    loop {
        if *stop.borrow() || stop.changed().await.is_err() {
            return;
        }
    }
}
async fn deadline(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
fn source(result: &Result<SelectedTransport, SessionSelectionError>) -> Option<&'static str> {
    match result {
        Ok(value) => value.proxy_source,
        Err(SessionSelectionError::Policy { proxy_source, .. }) => *proxy_source,
        _ => None,
    }
}
fn retry(automatic: bool, result: &Result<SelectedTransport, SessionSelectionError>) -> bool {
    if !automatic || source(result) != Some("surge-effective-profile") {
        return false;
    }
    match result {
        Ok(value) => matches!(value.mode, proletarius_vowifi::ProxyMode::Direct(_)),
        Err(SessionSelectionError::Policy { error, .. }) => matches!(
            error,
            proletarius_vowifi::ProxyPolicyError::Socks5UdpUnavailable
                | proletarius_vowifi::ProxyPolicyError::UnverifiedPublicRoute
        ),
        _ => false,
    }
}
pub(super) async fn select(
    proxy: &str,
    epdg: &str,
    stop: Option<watch::Receiver<bool>>,
) -> Result<SelectedTransport, PlanError> {
    let proxy = proxy
        .trim()
        .strip_prefix("socks5://")
        .or_else(|| proxy.trim().strip_prefix("socks5h://"))
        .unwrap_or(proxy.trim());
    let selection_stop = stop.clone();
    select_with_control(
        proxy.is_empty(),
        Control::new(stop),
        Path::new(CLI),
        |deadline| {
            crate::transport_selection::select_for_session_with_stop(
                proxy,
                epdg,
                selection_stop.clone(),
                deadline,
            )
        },
    )
    .await
}
#[cfg(test)]
async fn select_with<F: Future<Output = Result<SelectedTransport, SessionSelectionError>>>(
    automatic: bool,
    stop: Option<watch::Receiver<bool>>,
    cli: &Path,
    mut selection: impl FnMut() -> F,
) -> Result<SelectedTransport, PlanError> {
    select_with_control(automatic, Control::new(stop), cli, |_| selection()).await
}
async fn select_with_control<
    F: Future<Output = Result<SelectedTransport, SessionSelectionError>>,
>(
    automatic: bool,
    mut control: Control,
    cli: &Path,
    mut selection: impl FnMut(Option<Instant>) -> F,
) -> Result<SelectedTransport, PlanError> {
    let work = selection(control.deadline);
    let first = control.observe(work).await?;
    if !retry(automatic, &first) {
        return first.map_err(PlanError::Transport);
    }
    control.deadline = Some(Instant::now() + Duration::from_secs(35));
    if !control.feature(cli, "system-proxy").await? {
        return Err(error("surge_system_proxy_unconfirmed"));
    }
    if !control.feature(cli, "enhanced-mode").await? {
        control
            .command(
                cli,
                &["feature", "set", "enhanced-mode", "on"],
                Duration::from_secs(20),
            )
            .await?;
        let mut enabled = false;
        for attempt in 0..10 {
            if attempt > 0 {
                control
                    .observe(tokio::time::sleep(Duration::from_millis(500)))
                    .await?;
            }
            if matches!(control.feature(cli, "enhanced-mode").await, Ok(true)) {
                enabled = true;
                break;
            }
            control.checkpoint()?;
        }
        if !enabled {
            return Err(error("surge_enhanced_mode_unconfirmed"));
        }
    }
    let work = selection(control.deadline);
    let selected = control.observe(work).await?;
    if source(&selected) != Some("surge-effective-profile") {
        return Err(error("surge_proxy_owner_changed"));
    }
    selected.map_err(PlanError::Transport)
}

#[cfg(test)]
pub(super) mod tests;
