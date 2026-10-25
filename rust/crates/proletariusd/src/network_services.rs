//! macOS network-service enumeration, ordering, and compatible response projection.

use std::process::Stdio;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;

const NETWORKSETUP_PATH: &str = "/usr/sbin/networksetup";
const OSASCRIPT_PATH: &str = "/usr/bin/osascript";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const ORDER_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_ORDER_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct NetworkService {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) device: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) port: String,
    pub(crate) usb: bool,
    pub(crate) module: bool,
}

pub(crate) async fn handle(module_product: Option<&str>) -> Response<Full<Bytes>> {
    match list_network_services(module_product.unwrap_or("")).await {
        Ok(services) => crate::json_response(StatusCode::OK, &json!({"services":services})),
        Err(error) => crate::json_response(
            StatusCode::BAD_GATEWAY,
            &json!({"error":format!("networksetup: {error}")}),
        ),
    }
}

pub(crate) async fn list_network_services(
    module_product: &str,
) -> Result<Vec<NetworkService>, String> {
    let output = run_networksetup().await?;
    Ok(parse_service_order(&output, module_product))
}

/// Apply an explicitly requested service order. The public handler validates
/// every service against the current macOS inventory before invoking the
/// administrator-authenticated networksetup command.
pub(crate) async fn handle_order(request: Request<Incoming>) -> Response<Full<Bytes>> {
    if request.method() != Method::PUT {
        return crate::json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            &json!({"error":"method not allowed"}),
        );
    }
    let request = match decode_order_request(request).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let requested = request.services.unwrap_or_default();
    if requested.len() < 2 {
        return order_error(StatusCode::BAD_REQUEST, "至少需要两个网络服务");
    }

    let current = match list_network_services("").await {
        Ok(services) => services,
        Err(error) => {
            return order_error(StatusCode::BAD_GATEWAY, &format!("networksetup: {error}"));
        }
    };
    let cleaned = match validate_order(&requested, &current) {
        Ok(cleaned) => cleaned,
        Err(error) => return order_error(StatusCode::BAD_REQUEST, &error),
    };
    let script = administrator_script(&cleaned);
    match run_osascript(&script).await {
        Ok(()) => crate::json_response(
            StatusCode::OK,
            &json!({"ok":true,"message":"网络服务顺序已更新"}),
        ),
        Err(error) => order_error(StatusCode::BAD_GATEWAY, &format!("执行失败：{error}")),
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct NetworkOrderRequest {
    #[serde(default)]
    services: Option<Vec<String>>,
}

async fn decode_order_request(
    request: Request<Incoming>,
) -> Result<NetworkOrderRequest, Response<Full<Bytes>>> {
    let body = match crate::http_server::collect_body(request.into_body(), MAX_ORDER_REQUEST_BYTES)
        .await
    {
        Ok(body) => body,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return Err(crate::http_server::body_timeout_response());
        }
        Err(crate::http_server::BodyReadError::TooLarge) => {
            return Err(order_error(
                StatusCode::BAD_REQUEST,
                "http: request body too large",
            ));
        }
        Err(_) => return Err(order_error(StatusCode::BAD_REQUEST, "读取请求失败")),
    };
    parse_order_request(&body).map_err(|error| order_error(StatusCode::BAD_REQUEST, &error))
}

fn parse_order_request(body: &[u8]) -> Result<NetworkOrderRequest, String> {
    let mut decoder = serde_json::Deserializer::from_slice(body);
    let request = Option::<NetworkOrderRequest>::deserialize(&mut decoder)
        .map_err(|error| error.to_string())?;
    decoder
        .end()
        .map_err(|_| "请求只能包含一个 JSON 对象".to_owned())?;
    Ok(request.unwrap_or(NetworkOrderRequest { services: None }))
}

fn validate_order(requested: &[String], current: &[NetworkService]) -> Result<Vec<String>, String> {
    let valid: std::collections::HashSet<&str> = current
        .iter()
        .map(|service| service.name.as_str())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut cleaned = Vec::with_capacity(requested.len());
    for name in requested {
        let name = name.trim();
        if name.is_empty() || !valid.contains(name) {
            return Err(format!("存在无效的服务名称：{name}"));
        }
        if !seen.insert(name) {
            return Err(format!("服务名称重复：{name}"));
        }
        cleaned.push(name.to_owned());
    }
    Ok(cleaned)
}

fn administrator_script(services: &[String]) -> String {
    let command = format!(
        "networksetup -ordernetworkservices {}",
        services
            .iter()
            .map(|name| shell_quote(name))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let encoded = STANDARD.encode(command.as_bytes());
    format!("do shell script \"echo {encoded} | base64 -d | sh\" with administrator privileges")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn run_osascript(script: &str) -> Result<(), String> {
    let mut command = Command::new(OSASCRIPT_PATH);
    command
        .args(["-e", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "missing command output".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "missing command error output".to_owned())?;
    let operation = async {
        let (stdout, stderr) = tokio::join!(read_bounded(stdout), read_bounded(stderr));
        let stdout = stdout.map_err(|error| error.to_string())?;
        let stderr = stderr.map_err(|error| error.to_string())?;
        if stdout.len() > MAX_OUTPUT_BYTES || stderr.len() > MAX_OUTPUT_BYTES {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err("command output exceeds size limit".to_owned());
        }
        let status = child.wait().await.map_err(|error| error.to_string())?;
        if status.success() {
            return Ok(());
        }
        let code = status
            .code()
            .map(|code| format!("exit status {code}"))
            .unwrap_or_else(|| "signal terminated command".to_owned());
        let stdout = String::from_utf8_lossy(&stdout);
        let stderr = String::from_utf8_lossy(&stderr);
        let details = format!("{stdout}{stderr}").trim().to_owned();
        if details.is_empty() {
            Err(code)
        } else {
            Err(format!("{code} {details}"))
        }
    };
    timeout(ORDER_TIMEOUT, operation)
        .await
        .map_err(|_| "command timed out".to_owned())?
}

async fn read_bounded<R>(reader: R) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    reader
        .take((MAX_OUTPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    Ok(bytes)
}

fn order_error(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &json!({"error":message}))
}

async fn run_networksetup() -> Result<String, String> {
    let mut command = Command::new(NETWORKSETUP_PATH);
    command
        .args(["-listnetworkserviceorder"])
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

fn parse_service_order(output: &str, module_product: &str) -> Vec<NetworkService> {
    let mut services: Vec<NetworkService> = Vec::new();
    for line in output.lines().map(str::trim) {
        if let Some(name) = service_name(line) {
            services.push(NetworkService {
                name,
                device: String::new(),
                port: String::new(),
                usb: false,
                module: false,
            });
            continue;
        }
        if services.is_empty() || !line.contains("Hardware Port:") {
            continue;
        }
        let service = services.last_mut().expect("checked nonempty");
        if let Some(port) = field_value(line, "Hardware Port:", &[',']) {
            service.port = port.to_owned();
        }
        if let Some(device) = field_value(line, "Device:", &[',', ')']) {
            service.device = device.to_owned();
            service.usb = !device.is_empty() && device != "en0" && device.starts_with("en");
        }
        if !module_product.is_empty()
            && !service.port.is_empty()
            && service
                .port
                .trim()
                .eq_ignore_ascii_case(module_product.trim())
        {
            service.module = true;
        }
    }
    services
}

fn service_name(line: &str) -> Option<String> {
    let line = line.strip_prefix('(')?;
    let (number, name) = line.split_once(')')?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let name = name.trim_start();
    if name.is_empty() {
        return None;
    }
    Some(name.strip_prefix('*').unwrap_or(name).trim().to_owned())
}

fn field_value<'a>(line: &'a str, label: &str, delimiters: &[char]) -> Option<&'a str> {
    let value = line.split_once(label)?.1;
    let end = value
        .char_indices()
        .find_map(|(index, character)| delimiters.contains(&character).then_some(index))
        .unwrap_or(value.len());
    Some(value[..end].trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const FIXTURE: &str = "An asterisk (*) denotes that a network service is disabled.\n(1) DL-Dock\n(Hardware Port: DL-Dock, Device: en7)\n\n(2) Thunderbolt Bridge\n(Hardware Port: Thunderbolt Bridge, Device: bridge0)\n\n(3) Wi-Fi\n(Hardware Port: Wi-Fi, Device: en0)\n\n(4) Baiwang\n(Hardware Port: Baiwang, Device: en8)\n";

    #[test]
    fn parses_service_order_and_matches_legacy_usb_and_module_flags() {
        let services = parse_service_order(FIXTURE, "baiwang");
        assert_eq!(services.len(), 4);
        assert_eq!(services[0].name, "DL-Dock");
        assert_eq!(services[0].device, "en7");
        assert_eq!(services[0].port, "DL-Dock");
        assert!(services[0].usb);
        assert!(!services[0].module);
        assert!(!services[1].usb);
        assert!(!services[2].usb);
        assert!(services[3].usb);
        assert!(services[3].module);
        assert_eq!(
            serde_json::to_value(services).unwrap(),
            serde_json::json!([
                {"name":"DL-Dock","device":"en7","port":"DL-Dock","usb":true,"module":false},
                {"name":"Thunderbolt Bridge","device":"bridge0","port":"Thunderbolt Bridge","usb":false,"module":false},
                {"name":"Wi-Fi","device":"en0","port":"Wi-Fi","usb":false,"module":false},
                {"name":"Baiwang","device":"en8","port":"Baiwang","usb":true,"module":true}
            ])
        );
    }

    #[test]
    fn disabled_marker_is_removed_and_missing_fields_are_omitted_on_wire() {
        let services = parse_service_order(
            "(1) *Wi-Fi\n(Hardware Port: Wi-Fi, Device: en0)\n(2) VPN\n",
            "",
        );
        assert_eq!(services[0].name, "Wi-Fi");
        assert_eq!(services[1].name, "VPN");
        let wire = serde_json::to_value(&services).unwrap();
        assert_eq!(
            wire[1],
            Value::from(serde_json::json!({
                "name":"VPN","usb":false,"module":false
            }))
        );
    }

    #[test]
    fn service_names_and_fields_are_bound_to_their_networksetup_lines() {
        let services = parse_service_order(
            "(1) Wi-Fi\n(Hardware Port: Wi-Fi, Device: en0)\n(2) USB\n(Hardware Port: USB Ethernet, Device: en12)\n",
            "USB Ethernet",
        );
        assert_eq!(services[0].port, "Wi-Fi");
        assert_eq!(services[0].device, "en0");
        assert!(services[1].usb);
        assert!(services[1].module);
        assert!(
            parse_service_order("(x) invalid\n(Hardware Port: orphan, Device: en2)", "").is_empty()
        );
    }

    #[test]
    fn network_order_request_rejects_unknown_fields_and_trailing_json() {
        assert_eq!(
            parse_order_request(br#"{"services":["Wi-Fi","USB"]}"#)
                .unwrap()
                .services
                .unwrap(),
            vec!["Wi-Fi", "USB"]
        );
        assert!(parse_order_request(br#"{"services":["Wi-Fi"],"extra":true}"#).is_err());
        assert_eq!(
            parse_order_request(br#"{"services":["Wi-Fi"]} {}"#).unwrap_err(),
            "请求只能包含一个 JSON 对象"
        );
        assert!(parse_order_request(b"null").unwrap().services.is_none());
    }

    #[test]
    fn requested_network_order_is_trimmed_validated_and_deduplicated() {
        let current = parse_service_order(
            "(1) Wi-Fi\n(Hardware Port: Wi-Fi, Device: en0)\n(2) USB Ethernet\n(Hardware Port: USB Ethernet, Device: en7)\n",
            "",
        );
        assert_eq!(
            validate_order(&[" Wi-Fi ".into(), "USB Ethernet".into()], &current).unwrap(),
            vec!["Wi-Fi", "USB Ethernet"]
        );
        assert_eq!(
            validate_order(&["Missing".into(), "Wi-Fi".into()], &current).unwrap_err(),
            "存在无效的服务名称：Missing"
        );
        assert_eq!(
            validate_order(&["Wi-Fi".into(), "Wi-Fi".into()], &current).unwrap_err(),
            "服务名称重复：Wi-Fi"
        );
    }

    #[test]
    fn administrator_script_shell_quotes_service_names_before_encoding() {
        let script = administrator_script(&["Wi-Fi".into(), "Provider's USB".into()]);
        let encoded = script
            .strip_prefix("do shell script \"echo ")
            .unwrap()
            .strip_suffix(" | base64 -d | sh\" with administrator privileges")
            .unwrap();
        let command = STANDARD.decode(encoded).unwrap();
        assert_eq!(
            String::from_utf8(command).unwrap(),
            "networksetup -ordernetworkservices 'Wi-Fi' 'Provider'\\''s USB'"
        );
    }
}
