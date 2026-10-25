use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::time::Instant;

const SCUTIL_PATH: &str = "/usr/sbin/scutil";
const SURGE_CLI_PATH: &str = "/Applications/Surge.app/Contents/Applications/surge-cli";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
// Surge discovery is necessarily sequential: `scutil`, feature state, then
// the effective profile. 750 ms is too short for a cold local Surge CLI and
// made automatic System Proxy selection flaky. The caller keeps the overall
// transport choice bounded and grants the remaining time to the automatic
// SOCKS probe; an explicit user-entered SOCKS endpoint still gets its full
// independent probe budget.
const SYSTEM_DISCOVERY_BUDGET: Duration = Duration::from_millis(2_400);
const SCUTIL_OUTPUT_LIMIT: usize = 64 * 1024;
const SURGE_PROFILE_OUTPUT_LIMIT: usize = 2 * 1024 * 1024;

/// Work revocation is shared down the startup selection tree. Revocation
/// prevents new work; it never cancels an already owned child's reap future.
#[derive(Clone)]
pub(crate) struct StartupCommandControl {
    stop: Option<watch::Receiver<bool>>,
    deadline: Instant,
    #[cfg(test)]
    pub(crate) reap_ack: Option<std::sync::Arc<StartupReapAck>>,
}
#[cfg(test)]
pub(crate) struct StartupReapAck {
    pub(crate) entered: tokio::sync::Notify,
    pub(crate) release: tokio::sync::Semaphore,
}
#[cfg(test)]
impl Default for StartupReapAck {
    fn default() -> Self {
        Self {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}
impl StartupCommandControl {
    pub(crate) fn new(budget: Duration, stop: Option<watch::Receiver<bool>>) -> Self {
        Self {
            stop,
            deadline: Instant::now() + budget,
            #[cfg(test)]
            reap_ack: None,
        }
    }
    pub(crate) fn child(&self, budget: Duration) -> Self {
        Self {
            stop: self.stop.clone(),
            deadline: self.deadline.min(Instant::now() + budget),
            #[cfg(test)]
            reap_ack: self.reap_ack.clone(),
        }
    }
    pub(crate) fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = self.deadline.min(deadline);
        self
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) fn is_open(&self) -> bool {
        Instant::now() < self.deadline
            && !self
                .stop
                .as_ref()
                .is_some_and(|stop| *stop.borrow() || stop.has_changed().is_err())
    }
    pub(crate) async fn revoked(&self) {
        let mut stop = self.stop.clone();
        tokio::select! {
            _ = tokio::time::sleep_until(self.deadline) => {},
            _ = async {
                let Some(stop) = stop.as_mut() else { return std::future::pending::<()>().await; };
                loop {
                    if *stop.borrow() { return; }
                    if stop.changed().await.is_err() { return; }
                }
            } => {},
        }
    }
    /// For non-process work only. Process owners use run_bounded_bytes and
    /// explicitly kill/reap before returning on this same revocation.
    pub(crate) async fn observe<T>(&self, work: impl Future<Output = T>) -> Option<T> {
        if !self.is_open() {
            return None;
        }
        tokio::select! {
            biased;
            _ = self.revoked() => None,
            value = work => Some(value),
        }
    }
}

pub(crate) async fn reap_child_confirmed(child: &mut tokio::process::Child) -> bool {
    let mut failed = false;
    loop {
        match child.wait().await {
            Ok(_) => return failed,
            Err(_) => {
                if !failed {
                    crate::diagnostics::write_line(format_args!(
                        "startup_process_join_failed; retaining child owner"
                    ));
                }
                failed = true;
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

pub(crate) async fn run_bounded_bytes(
    program: &str,
    arguments: &[&str],
    limit: usize,
    budget: Duration,
    parent: &StartupCommandControl,
) -> Option<Vec<u8>> {
    let control = parent.child(budget);
    if !control.is_open() {
        return None;
    }
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut data = Vec::new();
    let result = {
        let work = async {
            let mut stdout = stdout.take((limit as u64).saturating_add(1));
            let (_, status) = tokio::try_join!(stdout.read_to_end(&mut data), child.wait())?;
            Ok::<_, std::io::Error>(status)
        };
        tokio::select! {
            biased;
            _ = control.revoked() => None,
            value = work => Some(value),
        }
    };
    match result {
        Some(Ok(status)) => {
            (status.success() && data.len() <= limit && control.is_open()).then_some(data)
        }
        _ => {
            let _ = child.start_kill();
            #[cfg(test)]
            if let Some(ack) = &control.reap_ack {
                ack.entered.notify_one();
                ack.release
                    .acquire()
                    .await
                    .expect("test reap gate open")
                    .forget();
            }
            reap_child_confirmed(&mut child).await;
            None
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemProxy {
    pub(crate) endpoint: String,
    pub(crate) source: &'static str,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SystemProxyDiscovery {
    pub(crate) proxy: Option<SystemProxy>,
    // This is intentionally a fixed, safe-to-display code. HTTP and PAC
    // system proxy settings cannot be reinterpreted as IKE/ESP UDP transport.
    pub(crate) diagnostic: Option<&'static str>,
}

/// Reads the effective macOS proxy state without guessing a port. A verified
/// Surge System Proxy wins over a generic SOCKS setting only after its enabled
/// System Proxy and effective HTTP listener identify the same loopback port;
/// the SOCKS listener is then taken from that same bounded effective profile.
/// Otherwise an enabled system SOCKS setting is used. HTTP/PAC-only settings
/// are reported but never guessed or treated as SOCKS.
pub(crate) async fn discover() -> SystemProxyDiscovery {
    discover_with_control(&StartupCommandControl::new(SYSTEM_DISCOVERY_BUDGET, None)).await
}

pub(crate) async fn discover_with_control(parent: &StartupCommandControl) -> SystemProxyDiscovery {
    let control = parent.child(SYSTEM_DISCOVERY_BUDGET);
    discover_using(&control, SCUTIL_PATH, SURGE_CLI_PATH).await
}

async fn discover_using(
    control: &StartupCommandControl,
    scutil: &str,
    surge_cli: &str,
) -> SystemProxyDiscovery {
    let result = discover_inner(control, scutil, surge_cli).await;
    if control.is_open() {
        result
    } else {
        SystemProxyDiscovery {
            proxy: None,
            diagnostic: Some("system_proxy_discovery_timeout"),
        }
    }
}

/// A running Surge VIF can own ePDG DNS even when macOS System Proxy is off.
/// An installed but unreadable Surge must not be confused with a confirmed
/// disabled VIF, because that would silently authorize host DNS/direct UDP.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SurgeEnhancedMode {
    On,
    Off,
    Unknown,
}

pub(crate) async fn observe_surge_enhanced_mode() -> SurgeEnhancedMode {
    observe_surge_enhanced_mode_with_control(&StartupCommandControl::new(COMMAND_TIMEOUT, None))
        .await
}

pub(crate) async fn observe_surge_enhanced_mode_with_control(
    control: &StartupCommandControl,
) -> SurgeEnhancedMode {
    if !control.is_open() {
        return SurgeEnhancedMode::Unknown;
    }
    if !Path::new(SURGE_CLI_PATH).is_file() {
        return SurgeEnhancedMode::Off;
    }
    let Some(output) = run_controlled(
        SURGE_CLI_PATH,
        &["feature", "get", "enhanced-mode"],
        SCUTIL_OUTPUT_LIMIT,
        COMMAND_TIMEOUT,
        control,
    )
    .await
    else {
        return SurgeEnhancedMode::Unknown;
    };
    if feature_is_on(&output, "enhanced-mode") {
        SurgeEnhancedMode::On
    } else if feature_is_off(&output, "enhanced-mode") {
        SurgeEnhancedMode::Off
    } else {
        SurgeEnhancedMode::Unknown
    }
}

#[cfg(test)]
pub(crate) static TEST_SCUTIL_OUTPUT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

async fn discover_inner(
    control: &StartupCommandControl,
    scutil: &str,
    surge_cli: &str,
) -> SystemProxyDiscovery {
    #[cfg(test)]
    if let Some(output) = TEST_SCUTIL_OUTPUT.get().filter(|_| scutil == SCUTIL_PATH) {
        // Only the external command observation is replaced. Parsing, policy,
        // actual SOCKS UDP verification and all network/session work stay live.
        return discovery_from_fields(&parse_proxy_fields(output), None);
    }
    let output = run_controlled(
        scutil,
        &["--proxy"],
        SCUTIL_OUTPUT_LIMIT,
        COMMAND_TIMEOUT,
        control,
    )
    .await;
    let Some(output) = output else {
        return SystemProxyDiscovery {
            proxy: None,
            diagnostic: Some("system_proxy_discovery_unavailable"),
        };
    };
    let fields = parse_proxy_fields(&output);
    let surge = surge_proxy(&fields, surge_cli, control).await;
    discovery_from_fields(&fields, surge)
}

fn discovery_from_fields(
    fields: &HashMap<String, String>,
    surge_endpoint: Option<String>,
) -> SystemProxyDiscovery {
    // Prefer a positively verified Surge ownership proof. This is not a port
    // heuristic: `surge_proxy` has already checked its CLI feature state and
    // matched the effective HTTP listener to the active macOS System Proxy.
    if let Some(endpoint) = surge_endpoint {
        return SystemProxyDiscovery {
            proxy: Some(SystemProxy {
                endpoint,
                source: "surge-effective-profile",
            }),
            diagnostic: None,
        };
    }
    if let Some(endpoint) = enabled_endpoint(fields, "SOCKS") {
        return SystemProxyDiscovery {
            proxy: Some(SystemProxy {
                endpoint,
                source: "macos-socks",
            }),
            diagnostic: None,
        };
    }
    SystemProxyDiscovery {
        proxy: None,
        diagnostic: unsupported_system_proxy_diagnostic(fields),
    }
}

fn parse_proxy_fields(output: &str) -> HashMap<String, String> {
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

fn enabled_endpoint(fields: &HashMap<String, String>, kind: &str) -> Option<String> {
    (fields.get(&format!("{kind}Enable"))?.as_str() == "1").then_some(())?;
    let host = fields.get(&format!("{kind}Proxy"))?.trim();
    let port = parse_port(fields.get(&format!("{kind}Port"))?)?;
    format_endpoint(host, port)
}

fn unsupported_system_proxy_diagnostic(fields: &HashMap<String, String>) -> Option<&'static str> {
    if is_enabled(fields, "ProxyAutoConfigEnable") || is_enabled(fields, "ProxyAutoDiscoveryEnable")
    {
        return Some("system_proxy_pac_unsupported");
    }
    (enabled_endpoint(fields, "HTTP").is_some() || enabled_endpoint(fields, "HTTPS").is_some())
        .then_some("system_proxy_http_unsupported")
}

fn is_enabled(fields: &HashMap<String, String>, key: &str) -> bool {
    fields.get(key).is_some_and(|value| value == "1")
}

async fn surge_proxy(
    fields: &HashMap<String, String>,
    surge_cli: &str,
    control: &StartupCommandControl,
) -> Option<String> {
    if !control.is_open() {
        return None;
    }
    let system_http =
        enabled_endpoint(fields, "HTTP").or_else(|| enabled_endpoint(fields, "HTTPS"))?;
    if !is_loopback_endpoint(&system_http) || !Path::new(surge_cli).is_file() {
        return None;
    }
    let feature = run_controlled(
        surge_cli,
        &["feature", "get", "system-proxy"],
        SCUTIL_OUTPUT_LIMIT,
        COMMAND_TIMEOUT,
        control,
    )
    .await?;
    if !feature_is_on(&feature, "system-proxy") {
        return None;
    }
    let profile = run_controlled(
        surge_cli,
        &["dump", "profile", "effective"],
        SURGE_PROFILE_OUTPUT_LIMIT,
        COMMAND_TIMEOUT,
        control,
    )
    .await?;
    surge_socks_endpoint(&system_http, &profile)
}

fn feature_is_on(output: &str, feature: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case(&format!("{feature}: on")))
}

fn feature_is_off(output: &str, feature: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim().eq_ignore_ascii_case(&format!("{feature}: off")))
}

fn surge_socks_endpoint(system_http: &str, profile: &str) -> Option<String> {
    if !is_loopback_endpoint(system_http) {
        return None;
    }
    let system_port = endpoint_port(system_http)?;
    let mut listeners = HashMap::new();
    for line in profile.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if matches!(key.as_str(), "http-listen" | "socks5-listen") {
            listeners.insert(key, value.trim());
        }
    }
    let (http_host, http_port) = safe_local_listener(listeners.get("http-listen")?)?;
    let (socks_host, socks_port) = safe_local_listener(listeners.get("socks5-listen")?)?;
    (http_port == system_port && listener_serves_system_endpoint(system_http, http_host))
        .then(|| local_listener_endpoint(socks_host, socks_port))
}

// A matching port alone is not evidence that the effective Surge listener is
// the listener configured in macOS. In particular, 127.0.0.1:P and [::1]:P
// can be different processes. Treat `localhost` conservatively: only an
// identical `localhost` listener is accepted because its chosen family is not
// represented in scutil output.
fn listener_serves_system_endpoint(system_endpoint: &str, listener_host: &str) -> bool {
    let Some((system_host, _)) = split_host_port(system_endpoint) else {
        return false;
    };
    if system_host == "localhost" || listener_host == "localhost" {
        return system_host == listener_host;
    }
    let (Ok(system), Ok(listener)) = (
        system_host.parse::<IpAddr>(),
        listener_host.parse::<IpAddr>(),
    ) else {
        return false;
    };
    match (system, listener) {
        (IpAddr::V4(system), IpAddr::V4(listener)) => {
            listener == system || listener.is_unspecified()
        }
        (IpAddr::V6(system), IpAddr::V6(listener)) => {
            listener == system || listener.is_unspecified()
        }
        _ => false,
    }
}

fn safe_local_listener(value: &str) -> Option<(&str, u16)> {
    let (host, port) = split_host_port(value.trim())?;
    let port = parse_port(port)?;
    let local = matches!(host, "localhost" | "0.0.0.0" | "::")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    local.then_some((host, port))
}

// Preserve a listener's address family. `::` and `0.0.0.0` are bind
// wildcards, not valid peer destinations, so map each to its own loopback
// family. In particular, an IPv6-only Surge SOCKS listener must not be
// rewritten to 127.0.0.1.
fn local_listener_endpoint(host: &str, port: u16) -> String {
    match host {
        "0.0.0.0" => format!("127.0.0.1:{port}"),
        "::" => format!("[::1]:{port}"),
        _ => format_endpoint(host, port)
            .expect("safe local listener host must format as a local endpoint"),
    }
}

pub(crate) fn is_loopback_endpoint(value: &str) -> bool {
    let Some((host, _)) = split_host_port(value.trim()) else {
        return false;
    };
    host == "localhost"
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn endpoint_port(value: &str) -> Option<u16> {
    split_host_port(value).and_then(|(_, port)| parse_port(port))
}

fn split_host_port(value: &str) -> Option<(&str, &str)> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        return Some((host, port));
    }
    let (host, port) = value.rsplit_once(':')?;
    (!host.is_empty() && !host.contains(':')).then_some((host, port))
}

fn parse_port(value: &str) -> Option<u16> {
    value.trim().parse::<u16>().ok().filter(|port| *port != 0)
}

fn format_endpoint(host: &str, port: u16) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || host.contains(['\0', '/', '\\']) {
        return None;
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        return Some(SocketAddr::new(address, port).to_string());
    }
    let valid_hostname = host == "localhost"
        || (host.len() <= 253
            && host.is_ascii()
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            }));
    valid_hostname.then(|| format!("{host}:{port}"))
}

async fn run_controlled(
    program: &str,
    arguments: &[&str],
    limit: usize,
    deadline: Duration,
    control: &StartupCommandControl,
) -> Option<String> {
    String::from_utf8(run_bounded_bytes(program, arguments, limit, deadline, control).await?).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A private local executable; no system CLI, resolver or network is used.
    pub(crate) struct PrivateCli {
        root: std::path::PathBuf,
        pub(crate) path: String,
    }
    impl PrivateCli {
        pub(crate) fn new(body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = std::env::temp_dir().join(format!(
                "startup-leaf-{}",
                crate::snapshot::new_instance_id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let path = root.join("cli");
            std::fs::write(
                &path,
                format!("#!/bin/sh\ncd \"$(dirname \"$0\")\" || exit 1\n{body}\n"),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                root,
                path: path.to_str().unwrap().to_owned(),
            }
        }
        pub(crate) async fn pids(&self, count: usize) -> Vec<String> {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let pids = std::fs::read_to_string(self.root.join("pids"))
                        .unwrap_or_default()
                        .lines()
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    if pids.len() >= count {
                        break pids;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("private children published their actual PIDs")
        }
        pub(crate) async fn assert_reaped(&self, count: usize) {
            for pid in self.pids(count).await {
                let status = Command::new("/bin/kill")
                    .args(["-0", &pid])
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .unwrap();
                assert!(
                    !status.success(),
                    "private child returned without confirmed reap"
                );
            }
        }
        pub(crate) fn trace(&self) -> String {
            std::fs::read_to_string(self.root.join("trace")).unwrap_or_default()
        }
    }
    impl Drop for PrivateCli {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn scutil_discovery_stop_joins_real_child_before_returning() {
        let cli = PrivateCli::new("echo $$ >> pids; echo scutil >> trace; exec /bin/sleep 60");
        let (stop, rx) = watch::channel(false);
        let ack = std::sync::Arc::new(StartupReapAck::default());
        let mut control = StartupCommandControl::new(Duration::from_secs(30), Some(rx));
        control.reap_ack = Some(ack.clone());
        let path = cli.path.clone();
        let task = tokio::spawn(async move { discover_using(&control, &path, &path).await });
        cli.pids(1).await;
        stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(3), ack.entered.notified())
            .await
            .unwrap();
        assert!(
            !task.is_finished(),
            "discovery must hold its actual Child at reap"
        );
        ack.release.add_permits(1);
        let discovery = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(discovery.diagnostic, Some("system_proxy_discovery_timeout"));
        assert!(discovery.proxy.is_none());
        cli.assert_reaped(1).await;
        assert_eq!(
            cli.trace().lines().count(),
            1,
            "no successor Surge discovery after stop"
        );
    }

    #[tokio::test]
    async fn expired_parent_blocks_new_cli_and_child_budget_joins_real_child() {
        let cli = PrivateCli::new("echo $$ >> pids; echo started >> trace; exec /bin/sleep 60");
        let ack = std::sync::Arc::new(StartupReapAck::default());
        let mut control = StartupCommandControl::new(Duration::from_secs(30), None);
        control.reap_ack = Some(ack.clone());
        let path = cli.path.clone();
        let task = tokio::spawn(async move {
            run_bounded_bytes(
                &path,
                &[],
                SCUTIL_OUTPUT_LIMIT,
                Duration::from_millis(150),
                &control,
            )
            .await
        });
        cli.pids(1).await;
        tokio::time::timeout(Duration::from_secs(3), ack.entered.notified())
            .await
            .unwrap();
        assert!(!task.is_finished());
        ack.release.add_permits(1);
        assert!(
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        cli.assert_reaped(1).await;
        let expired = StartupCommandControl::new(Duration::ZERO, None);
        assert!(
            run_bounded_bytes(
                &cli.path,
                &[],
                SCUTIL_OUTPUT_LIMIT,
                COMMAND_TIMEOUT,
                &expired
            )
            .await
            .is_none()
        );
        assert_eq!(cli.trace().lines().count(), 1);
    }

    #[test]
    fn enhanced_mode_requires_a_positive_surge_feature_observation() {
        assert!(feature_is_on("enhanced-mode: On\n", "enhanced-mode"));
        assert!(feature_is_off("enhanced-mode: Off\n", "enhanced-mode"));
        assert!(!feature_is_on("enhanced-mode: Off\n", "enhanced-mode"));
        assert!(!feature_is_on("system-proxy: On\n", "enhanced-mode"));
        assert!(!feature_is_off("system-proxy: Off\n", "enhanced-mode"));
    }

    #[test]
    fn explicit_system_socks_requires_enabled_valid_endpoint() {
        let mut fields =
            parse_proxy_fields("SOCKSEnable : 1\nSOCKSProxy : 127.0.0.1\nSOCKSPort : 6153\n");
        assert_eq!(
            enabled_endpoint(&fields, "SOCKS").as_deref(),
            Some("127.0.0.1:6153")
        );
        fields.insert("SOCKSPort".into(), "70000".into());
        assert!(enabled_endpoint(&fields, "SOCKS").is_none());
        fields.insert("SOCKSPort".into(), "6153".into());
        fields.insert("SOCKSEnable".into(), "0".into());
        assert!(enabled_endpoint(&fields, "SOCKS").is_none());
    }

    #[test]
    fn only_verified_surge_can_claim_system_socks_ownership() {
        let fields = parse_proxy_fields(
            "SOCKSEnable : 1\nSOCKSProxy : 127.0.0.1\nSOCKSPort : 6153\nHTTPEnable : 1\nHTTPProxy : 127.0.0.1\nHTTPPort : 6152\n",
        );
        let generic = discovery_from_fields(&fields, None);
        assert_eq!(
            generic.proxy.as_ref().map(|value| value.source),
            Some("macos-socks")
        );

        let verified = discovery_from_fields(&fields, Some("[::1]:6153".into()));
        assert_eq!(
            verified.proxy.as_ref().map(|value| value.source),
            Some("surge-effective-profile")
        );
        assert_eq!(
            verified.proxy.as_ref().map(|value| value.endpoint.as_str()),
            Some("[::1]:6153")
        );
    }

    #[test]
    fn surge_listener_requires_matching_local_http_port() {
        let profile = "http-listen = 0.0.0.0:6152\nsocks5-listen = 0.0.0.0:6153\n";
        assert_eq!(
            surge_socks_endpoint("127.0.0.1:6152", profile).as_deref(),
            Some("127.0.0.1:6153")
        );
        assert!(surge_socks_endpoint("127.0.0.1:8080", profile).is_none());
        assert!(surge_socks_endpoint("192.0.2.1:6152", profile).is_none());
        assert!(
            surge_socks_endpoint(
                "127.0.0.1:6152",
                "http-listen = 0.0.0.0:6152\nsocks5-listen = 192.0.2.1:6153\n"
            )
            .is_none()
        );
    }

    #[test]
    fn surge_ipv6_only_socks_listener_keeps_ipv6_endpoint() {
        let profile = "http-listen = 127.0.0.1:6152\nsocks5-listen = [::1]:6153\n";
        assert_eq!(
            surge_socks_endpoint("127.0.0.1:6152", profile).as_deref(),
            Some("[::1]:6153")
        );
        assert_eq!(local_listener_endpoint("::", 6153), "[::1]:6153");
        assert_eq!(local_listener_endpoint("0.0.0.0", 6153), "127.0.0.1:6153");
    }

    #[test]
    fn surge_http_listener_must_serve_the_system_proxy_family() {
        let ipv6_only_http = "http-listen = [::1]:6152\nsocks5-listen = [::1]:6153\n";
        assert!(surge_socks_endpoint("127.0.0.1:6152", ipv6_only_http).is_none());

        let ipv4_wildcard_http = "http-listen = 0.0.0.0:6152\nsocks5-listen = [::1]:6153\n";
        assert_eq!(
            surge_socks_endpoint("127.0.0.1:6152", ipv4_wildcard_http).as_deref(),
            Some("[::1]:6153")
        );
    }

    #[test]
    fn http_and_pac_are_explicitly_not_socks() {
        let http = parse_proxy_fields("HTTPEnable : 1\nHTTPProxy : 127.0.0.1\nHTTPPort : 6152\n");
        assert_eq!(
            unsupported_system_proxy_diagnostic(&http),
            Some("system_proxy_http_unsupported")
        );
        let pac = parse_proxy_fields(
            "ProxyAutoConfigEnable : 1\nProxyAutoConfigURLString : https://proxy.example/pac\n",
        );
        assert_eq!(
            unsupported_system_proxy_diagnostic(&pac),
            Some("system_proxy_pac_unsupported")
        );
    }

    #[tokio::test]
    async fn live_surge_discovery_is_bounded_and_usable_when_enabled() {
        if std::env::var("PROLETARIUS_TEST_REAL_SURGE").as_deref() != Ok("1") {
            return;
        }
        let proxy = discover()
            .await
            .proxy
            .expect("running Surge System Proxy must yield its effective SOCKS endpoint");
        assert_eq!(proxy.source, "surge-effective-profile");
        crate::proxy_probe::probe_endpoint(&proxy.endpoint)
            .await
            .expect("discovered Surge SOCKS endpoint must complete a real handshake");
    }
}
