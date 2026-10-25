//! Read-only module-interface byte sampling with per-backend session totals.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::network_services::{self, NetworkService};

const IFCONFIG_PATH: &str = "/sbin/ifconfig";
const NETSTAT_PATH: &str = "/usr/sbin/netstat";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Default)]
pub(crate) struct Service {
    baselines: Arc<Mutex<BaselineState>>,
    pub(crate) recovery: crate::network_recovery::Service,
}

#[derive(Default)]
struct BaselineState {
    backend_generation: Option<u64>,
    interfaces: HashMap<String, ByteCounters>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ByteCounters {
    rx: u64,
    tx: u64,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TrafficSnapshot {
    pub(crate) available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    interface: Option<String>,
    rx_bytes: u64,
    tx_bytes: u64,
    session_rx_bytes: u64,
    session_tx_bytes: u64,
    session_total_bytes: u64,
    sampled_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Stable machine reason; display strings never authorize recovery.
    reason: Option<&'static str>,
    pub(crate) recovery_retryable: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct NetworkInterface {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) ipv4: String,
    pub(crate) kind: String,
}

impl Service {
    pub(crate) fn new(data_root: &Path) -> Self {
        Self {
            baselines: Arc::new(Mutex::new(BaselineState::default())),
            recovery: crate::network_recovery::Service::new(data_root),
        }
    }

    fn session_counters(
        &self,
        backend_generation: u64,
        interface: &str,
        current: ByteCounters,
    ) -> ByteCounters {
        let mut state = self
            .baselines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.backend_generation != Some(backend_generation) {
            state.backend_generation = Some(backend_generation);
            state.interfaces.clear();
        }
        let baseline = state
            .interfaces
            .entry(interface.to_owned())
            .or_insert(current);
        if current.rx < baseline.rx || current.tx < baseline.tx {
            *baseline = current;
        }
        ByteCounters {
            rx: current.rx - baseline.rx,
            tx: current.tx - baseline.tx,
        }
    }
}

pub(crate) async fn handle(
    service: &Service,
    module_product: Option<&str>,
    backend_generation: u64,
) -> Response<Full<Bytes>> {
    crate::json_response(
        StatusCode::OK,
        &capture(service, module_product, backend_generation).await,
    )
}

pub(crate) async fn capture(
    service: &Service,
    module_product: Option<&str>,
    backend_generation: u64,
) -> TrafficSnapshot {
    let mut snapshot = TrafficSnapshot {
        sampled_at_ms: now_ms(),
        ..TrafficSnapshot::default()
    };
    let module_product = module_product.unwrap_or_default().trim();
    if module_product.is_empty() {
        snapshot.error = Some("未识别到 4G 模块".to_owned());
        snapshot.reason = Some("module_missing");
        return snapshot;
    }

    let services = match network_services::list_network_services(module_product).await {
        Ok(services) => services,
        Err(error) => {
            snapshot.error = Some(format!("networksetup: {error}"));
            snapshot.reason = Some("service_inventory_failed");
            return snapshot;
        }
    };
    let interfaces = read_interfaces().await;
    let (interface, active) = select_module_interface(&interfaces, &services);
    let Some(interface) = interface else {
        snapshot.error = Some("未识别到 4G 模块网卡".to_owned());
        snapshot.reason = Some("interface_missing");
        return snapshot;
    };
    snapshot.interface = Some(interface.clone());
    if !active {
        snapshot.error = Some("4G 模块网卡未连接".to_owned());
        snapshot.reason = Some("interface_disconnected");
        snapshot.recovery_retryable = true;
        return snapshot;
    }

    let output = match run_command(NETSTAT_PATH, &["-ibn"]).await {
        Ok(output) => output,
        Err(error) => {
            snapshot.error = Some(error);
            snapshot.reason = Some("counters_unavailable");
            return snapshot;
        }
    };
    let counters = parse_counters(&output);
    let Some(current) = counters.get(&interface).copied() else {
        snapshot.error = Some("未读取到网卡计数".to_owned());
        snapshot.reason = Some("counters_unavailable");
        return snapshot;
    };

    let session = service.session_counters(backend_generation, &interface, current);
    snapshot.available = true;
    snapshot.rx_bytes = current.rx;
    snapshot.tx_bytes = current.tx;
    snapshot.session_rx_bytes = session.rx;
    snapshot.session_tx_bytes = session.tx;
    snapshot.session_total_bytes = session.rx.wrapping_add(session.tx);
    snapshot
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub(crate) async fn read_interfaces() -> Vec<NetworkInterface> {
    read_interfaces_checked().await.unwrap_or_default()
}

pub(crate) async fn read_interfaces_checked() -> Result<Vec<NetworkInterface>, String> {
    run_command(IFCONFIG_PATH, &[])
        .await
        .map(|output| parse_interfaces(&output))
}

async fn run_command(program: &str, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "missing command output".to_owned())?;
    let operation = async {
        let mut bytes = Vec::new();
        stdout
            .take((MAX_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_OUTPUT_BYTES {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err("command output exceeds size limit".to_owned());
        }
        let status = child.wait().await.map_err(|error| error.to_string())?;
        if !status.success() {
            return Err(match status.code() {
                Some(code) => format!("exit status {code}"),
                None => "signal terminated command".to_owned(),
            });
        }
        String::from_utf8(bytes).map_err(|_| "command output is not UTF-8".to_owned())
    };
    timeout(COMMAND_TIMEOUT, operation)
        .await
        .map_err(|_| "command timed out".to_owned())?
}

pub(crate) fn parse_interfaces(output: &str) -> Vec<NetworkInterface> {
    let mut interfaces = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    let flush = |block: &mut Vec<&str>, interfaces: &mut Vec<NetworkInterface>| {
        if block.is_empty() {
            return;
        }
        let fields: Vec<_> = block[0].split_whitespace().collect();
        let Some(name) = fields.first().map(|value| value.trim_end_matches(':')) else {
            block.clear();
            return;
        };
        if name.is_empty() || name.starts_with("lo") || name.starts_with("utun") {
            block.clear();
            return;
        }
        let status = block
            .iter()
            .map(|line| line.trim())
            .find_map(|line| line.strip_prefix("status:"))
            .map(str::trim)
            .unwrap_or("unknown");
        let ipv4 = block
            .iter()
            .map(|line| line.trim())
            .find_map(|line| line.strip_prefix("inet "))
            .and_then(|value| value.split_whitespace().next())
            .unwrap_or("");
        interfaces.push(NetworkInterface {
            name: (*name).to_owned(),
            status: status.to_owned(),
            ipv4: ipv4.to_owned(),
            kind: classify_interface(name),
        });
        block.clear();
    };

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t']) && line.contains(':') {
            flush(&mut block, &mut interfaces);
        }
        if !block.is_empty() || (!line.starts_with([' ', '\t']) && line.contains(':')) {
            block.push(line);
        }
    }
    flush(&mut block, &mut interfaces);
    interfaces
}

fn classify_interface(name: &str) -> String {
    if name.starts_with("en") {
        "ethernet".to_owned()
    } else if name.starts_with("bridge") {
        "bridge".to_owned()
    } else if ["awdl", "llw", "ap"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        "apple-wireless".to_owned()
    } else {
        "other".to_owned()
    }
}

fn select_module_interface(
    interfaces: &[NetworkInterface],
    services: &[NetworkService],
) -> (Option<String>, bool) {
    let mut detected = None;
    for service in services {
        if !service.module || service.device.is_empty() {
            continue;
        }
        if detected.is_none() {
            detected = Some(service.device.clone());
        }
        if interfaces.iter().any(|interface| {
            interface.name == service.device
                && interface.kind == "ethernet"
                && interface.status == "active"
        }) {
            return (Some(service.device.clone()), true);
        }
    }
    (detected, false)
}

fn parse_counters(output: &str) -> HashMap<String, ByteCounters> {
    let mut counters = HashMap::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 10 || !fields[2].starts_with("<Link#") {
            continue;
        }
        let name = fields[0].strip_suffix('*').unwrap_or(fields[0]);
        let (Ok(rx), Ok(tx)) = (fields[6].parse::<u64>(), fields[9].parse::<u64>()) else {
            continue;
        };
        if !name.is_empty() {
            counters.insert(name.to_owned(), ByteCounters { rx, tx });
        }
    }
    counters
}

#[cfg(test)]
mod tests {
    use super::{
        ByteCounters, NetworkInterface, Service, parse_counters, parse_interfaces,
        select_module_interface,
    };
    use crate::network_services::NetworkService;

    #[test]
    fn parses_ifconfig_and_filters_tunnel_interfaces() {
        let parsed = parse_interfaces(
            "en0: flags=8863<UP,BROADCAST>\n\tinet 192.0.2.2\n\tstatus: active\n\n\
             en8: flags=8863<UP,BROADCAST>\n\tstatus: active\n\n\
             utun4: flags=8051<UP,POINTOPOINT>\n\tstatus: active\n",
        );
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].ipv4, "192.0.2.2");
        assert_eq!(parsed[1].name, "en8");
        assert_eq!(parsed[1].kind, "ethernet");
        assert_eq!(parsed[1].status, "active");
    }

    #[test]
    fn module_service_is_selected_instead_of_an_earlier_usb_adapter() {
        let interfaces = [
            NetworkInterface {
                name: "en7".into(),
                status: "active".into(),
                ipv4: "192.0.2.7".into(),
                kind: "ethernet".into(),
            },
            NetworkInterface {
                name: "en8".into(),
                status: "active".into(),
                ipv4: "192.0.2.8".into(),
                kind: "ethernet".into(),
            },
        ];
        let services = [
            NetworkService {
                name: "Dock".into(),
                device: "en7".into(),
                port: "Dock".into(),
                usb: true,
                module: false,
            },
            NetworkService {
                name: "Baiwang".into(),
                device: "en8".into(),
                port: "Baiwang".into(),
                usb: true,
                module: true,
            },
        ];
        assert_eq!(
            select_module_interface(&interfaces, &services),
            (Some("en8".into()), true)
        );
    }

    #[test]
    fn module_interface_is_retained_when_inactive_and_does_not_fallback() {
        let interfaces = [NetworkInterface {
            name: "en7".into(),
            status: "active".into(),
            ipv4: "192.0.2.7".into(),
            kind: "ethernet".into(),
        }];
        let services = [
            NetworkService {
                name: "Dock".into(),
                device: "en7".into(),
                port: "Dock".into(),
                usb: true,
                module: false,
            },
            NetworkService {
                name: "Baiwang".into(),
                device: "en8".into(),
                port: "Baiwang".into(),
                usb: true,
                module: true,
            },
        ];
        assert_eq!(
            select_module_interface(&interfaces, &services),
            (Some("en8".into()), false)
        );
    }

    #[test]
    fn parses_link_byte_counters_and_ignores_incomplete_rows() {
        let output = "Name Mtu Network Address Ipkts Ierrs Ibytes Opkts Oerrs Obytes Coll\n\
                      en8 1500 <Link#11> 00:11 10 0 4096 8 0 2048 0\n\
                      en8* 1500 <Link#11> 00:11 10 0 invalid 8 0 2 0\n\
                      lo0 16384 <Link#1> 00:00 1 0 64 1 0 64 0\n";
        assert_eq!(
            parse_counters(output).get("en8"),
            Some(&ByteCounters { rx: 4096, tx: 2048 })
        );
        assert_eq!(parse_counters("short row").len(), 0);
    }

    #[test]
    fn session_baseline_resets_on_counter_reset_and_backend_generation() {
        let service = Service::default();
        assert_eq!(
            service.session_counters(1, "en8", ByteCounters { rx: 100, tx: 50 }),
            ByteCounters::default()
        );
        assert_eq!(
            service.session_counters(1, "en8", ByteCounters { rx: 160, tx: 80 }),
            ByteCounters { rx: 60, tx: 30 }
        );
        assert_eq!(
            service.session_counters(1, "en8", ByteCounters { rx: 2, tx: 3 }),
            ByteCounters::default()
        );
        assert_eq!(
            service.session_counters(2, "en8", ByteCounters { rx: 9, tx: 4 }),
            ByteCounters::default()
        );
    }
}
