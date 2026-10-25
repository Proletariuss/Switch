//! Read-only projection of the current default route for the 4G route check.

use std::process::Stdio;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Response, StatusCode};
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::network_traffic::{self, NetworkInterface};

const ROUTE_PATH: &str = "/sbin/route";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DefaultRoute {
    interface: String,
    gateway: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct NetworkCheckResult {
    ok: bool,
    summary: String,
    detail: String,
}

pub(crate) async fn handle() -> Response<Full<Bytes>> {
    let route = run_command(&["-n", "get", "default"])
        .await
        .map(|output| parse_default_route(&output))
        .unwrap_or_default();
    let interfaces = network_traffic::read_interfaces().await;
    let result = check_result(&route, &interfaces);
    crate::json_response(StatusCode::OK, &result)
}

fn check_result(route: &DefaultRoute, interfaces: &[NetworkInterface]) -> NetworkCheckResult {
    if route.interface.is_empty() {
        return NetworkCheckResult {
            ok: false,
            summary: "未读取到默认出口".to_owned(),
            detail: "macOS 没有返回 default route".to_owned(),
        };
    }

    let active = interfaces
        .iter()
        .find(|interface| interface.name == route.interface);
    if active.is_some_and(|interface| {
        interface.name != "en0" && interface.kind == "ethernet" && interface.status == "active"
    }) {
        return NetworkCheckResult {
            ok: true,
            summary: "当前正在走 4G 模块".to_owned(),
            detail: format!(
                "默认出口 {} -> {}，IP {}",
                route.interface,
                route.gateway,
                active
                    .map(|interface| interface.ipv4.as_str())
                    .unwrap_or_default()
            ),
        };
    }

    let mut detail = format!("默认出口 {} -> {}", route.interface, route.gateway);
    if let Some(interface) = active.filter(|interface| !interface.ipv4.is_empty()) {
        detail.push_str("，IP ");
        detail.push_str(&interface.ipv4);
    }
    NetworkCheckResult {
        ok: false,
        summary: "当前没有优先走 4G 模块".to_owned(),
        detail,
    }
}

fn parse_default_route(output: &str) -> DefaultRoute {
    let mut route = DefaultRoute::default();
    for line in output.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("gateway:") {
            route.gateway = value.trim().to_owned();
        }
        if let Some(value) = line.strip_prefix("interface:") {
            route.interface = value.trim().to_owned();
        }
    }
    route
}

async fn run_command(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(ROUTE_PATH);
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

#[cfg(test)]
mod tests {
    use super::{DefaultRoute, check_result, parse_default_route};
    use crate::network_traffic::NetworkInterface;

    #[test]
    fn parses_default_route_fields_and_ignores_unrelated_lines() {
        assert_eq!(
            parse_default_route(
                "   route to: default\n gateway: 192.0.2.1\n interface: en7\n flags: <UP>\n"
            ),
            DefaultRoute {
                interface: "en7".to_owned(),
                gateway: "192.0.2.1".to_owned(),
            }
        );
    }

    #[test]
    fn no_default_route_keeps_the_legacy_failure_projection() {
        let result = check_result(&DefaultRoute::default(), &[]);
        assert!(!result.ok);
        assert_eq!(result.summary, "未读取到默认出口");
        assert_eq!(result.detail, "macOS 没有返回 default route");
    }

    #[test]
    fn active_non_wifi_ethernet_route_keeps_the_legacy_success_projection() {
        let result = check_result(
            &DefaultRoute {
                interface: "en7".to_owned(),
                gateway: "192.0.2.1".to_owned(),
            },
            &[NetworkInterface {
                name: "en7".to_owned(),
                status: "active".to_owned(),
                ipv4: "192.0.2.7".to_owned(),
                kind: "ethernet".to_owned(),
            }],
        );
        assert!(result.ok);
        assert_eq!(result.summary, "当前正在走 4G 模块");
        assert_eq!(result.detail, "默认出口 en7 -> 192.0.2.1，IP 192.0.2.7");
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({
                "ok": true,
                "summary": "当前正在走 4G 模块",
                "detail": "默认出口 en7 -> 192.0.2.1，IP 192.0.2.7"
            })
        );
    }

    #[test]
    fn wifi_and_inactive_interfaces_keep_the_legacy_failure_projection() {
        for interface in [
            NetworkInterface {
                name: "en0".to_owned(),
                status: "active".to_owned(),
                ipv4: "192.0.2.10".to_owned(),
                kind: "ethernet".to_owned(),
            },
            NetworkInterface {
                name: "en7".to_owned(),
                status: "inactive".to_owned(),
                ipv4: "192.0.2.7".to_owned(),
                kind: "ethernet".to_owned(),
            },
        ] {
            let expected_detail = format!(
                "默认出口 {} -> 192.0.2.1，IP {}",
                interface.name, interface.ipv4
            );
            let result = check_result(
                &DefaultRoute {
                    interface: interface.name.clone(),
                    gateway: "192.0.2.1".to_owned(),
                },
                std::slice::from_ref(&interface),
            );
            assert!(!result.ok);
            assert_eq!(result.summary, "当前没有优先走 4G 模块");
            assert_eq!(result.detail, expected_detail);
        }
    }
}
