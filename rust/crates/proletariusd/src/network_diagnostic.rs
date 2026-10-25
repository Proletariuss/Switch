//! Read-only network and modem diagnostics previously assembled by the Go API.
//! AT commands are supplied by the module-radio actor; this module owns the
//! stable legacy projection and bounded macOS observations.

use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::network_traffic;

const HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HOST_OUTPUT_BYTES: usize = 1024 * 1024;
const ROUTE_PATH: &str = "/sbin/route";
const IOREG_PATH: &str = "/usr/sbin/ioreg";

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct Diagnostic {
    captured_at_ms: i64,
    duration_ms: i64,
    partial: bool,
    completed_commands: usize,
    usbnet_mode: String,
    usbcfg: String,
    pdp_contexts: Option<Vec<PdpContext>>,
    active_contexts: Option<Vec<i32>>,
    pdp_addresses: Option<Vec<String>>,
    mac_interfaces: Option<Vec<MacInterface>>,
    default_route: DefaultRoute,
    usb_network_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    usb_device: Option<UsbDevice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw: Option<std::collections::BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
struct PdpContext {
    id: i32,
    pdn: String,
    apn: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
struct MacInterface {
    name: String,
    status: String,
    ipv4: String,
    kind: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
struct DefaultRoute {
    interface: String,
    gateway: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
struct UsbInterface {
    number: i64,
    class: i64,
    subclass: i64,
    protocol: i64,
    endpoints: i64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
struct UsbDevice {
    product: String,
    vendor: String,
    vendor_id: String,
    product_id: String,
    location_id: String,
    speed: String,
    mode: String,
    interfaces: Vec<UsbInterface>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct HostSnapshot {
    mac_interfaces: Option<Vec<MacInterface>>,
    default_route: DefaultRoute,
    usb_device: Option<UsbDevice>,
}

pub(crate) async fn capture_host() -> HostSnapshot {
    let interfaces = network_traffic::read_interfaces_checked();
    let route = run_command(ROUTE_PATH, &["-n", "get", "default"]);
    let usb = discover_dji_usb_device();
    let (interfaces, route, usb_device) = tokio::join!(interfaces, route, usb);
    HostSnapshot {
        mac_interfaces: interfaces.ok().map(|items| {
            items
                .into_iter()
                .map(|item| MacInterface {
                    name: item.name,
                    status: item.status,
                    ipv4: item.ipv4,
                    kind: item.kind,
                })
                .collect()
        }),
        default_route: route
            .map(|output| parse_default_route(&output))
            .unwrap_or_default(),
        usb_device: usb_device.ok().flatten(),
    }
}

/// Read the currently attached supported USB module locally for the network
/// service/traffic projections. This keeps those read-only routes independent
/// of the Go `/api/health` endpoint while preserving its optional identity.
pub(crate) async fn discover_module_product(
    active_usb_at_description: Option<String>,
) -> Option<String> {
    if let Some(description) = active_usb_at_description {
        return Some(description);
    }
    discover_dji_usb_device()
        .await
        .ok()
        .flatten()
        .map(|device| device.product)
}

pub(crate) fn project(
    host: HostSnapshot,
    raw: std::collections::BTreeMap<String, String>,
    errors: std::collections::BTreeMap<String, String>,
    completed_commands: usize,
    captured_at_ms: i64,
    duration_ms: i64,
) -> Diagnostic {
    let usbnet = raw.get("usbnet").map(String::as_str).unwrap_or_default();
    let usbcfg = raw.get("usbcfg").map(String::as_str).unwrap_or_default();
    let cgdcont = raw.get("cgdcont").map(String::as_str).unwrap_or_default();
    let cgact = raw.get("cgact").map(String::as_str).unwrap_or_default();
    let cgpaddr = raw.get("cgpaddr").map(String::as_str).unwrap_or_default();
    let usb_network_present = host.mac_interfaces.as_ref().is_some_and(|interfaces| {
        interfaces
            .iter()
            .any(|item| item.kind == "ethernet" && item.name != "en0" && item.status == "active")
    });
    Diagnostic {
        captured_at_ms,
        duration_ms,
        partial: !errors.is_empty(),
        completed_commands,
        usbnet_mode: parse_usbnet_mode(usbnet),
        usbcfg: parse_prefixed(usbcfg, "+QCFG:"),
        pdp_contexts: parse_pdp_contexts(cgdcont),
        active_contexts: parse_active_contexts(cgact),
        pdp_addresses: parse_pdp_addresses(cgpaddr),
        mac_interfaces: host.mac_interfaces,
        default_route: host.default_route,
        usb_network_present,
        usb_device: host.usb_device,
        raw: (!raw.is_empty()).then_some(raw),
        errors: (!errors.is_empty()).then_some(errors),
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub(crate) fn elapsed_ms(started: Instant) -> i64 {
    started.elapsed().as_millis().min(i64::MAX as u128) as i64
}

pub(crate) fn parse_usbnet_mode(response: &str) -> String {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\+QCFG:\s*"usbnet",(\d+)"#).unwrap());
    PATTERN
        .captures(response)
        .and_then(|capture| capture.get(1))
        .map(|value| value.as_str().to_owned())
        .unwrap_or_default()
}

fn parse_prefixed(response: &str, prefix: &str) -> String {
    response
        .split(['\r', '\n'])
        .map(str::trim)
        .find_map(|line| line.strip_prefix(prefix).map(str::trim).map(str::to_owned))
        .unwrap_or_default()
}

fn parse_pdp_contexts(response: &str) -> Option<Vec<PdpContext>> {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\+CGDCONT:\s*(\d+),"([^"]*)","([^"]*)""#).unwrap());
    let values: Vec<_> = PATTERN
        .captures_iter(response)
        .filter_map(|capture| {
            Some(PdpContext {
                id: capture.get(1)?.as_str().parse().ok()?,
                pdn: capture.get(2)?.as_str().to_owned(),
                apn: capture.get(3)?.as_str().to_owned(),
            })
        })
        .collect();
    (!values.is_empty()).then_some(values)
}

fn parse_active_contexts(response: &str) -> Option<Vec<i32>> {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\+CGACT:\s*(\d+),1").unwrap());
    let values: Vec<_> = PATTERN
        .captures_iter(response)
        .filter_map(|capture| capture.get(1)?.as_str().parse().ok())
        .collect();
    (!values.is_empty()).then_some(values)
}

fn parse_pdp_addresses(response: &str) -> Option<Vec<String>> {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\+CGPADDR:\s*\d+,"([^"]*)""#).unwrap());
    let value = PATTERN.captures(response)?.get(1)?.as_str();
    let addresses: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    (!addresses.is_empty()).then_some(addresses)
}

fn parse_default_route(output: &str) -> DefaultRoute {
    let mut route = DefaultRoute::default();
    for line in output.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("gateway:") {
            route.gateway = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("interface:") {
            route.interface = value.trim().to_owned();
        }
    }
    route
}

async fn discover_dji_usb_device() -> Result<Option<UsbDevice>, String> {
    let output = run_command(
        IOREG_PATH,
        &["-r", "-c", "IOUSBHostInterface", "-l", "-w", "0"],
    )
    .await?;
    Ok(parse_dji_usb_device(&output))
}

fn parse_dji_usb_device(output: &str) -> Option<UsbDevice> {
    // `ioreg -c IOUSBHostInterface` may report several physical devices. Keep
    // one identity (including its location when exposed) rather than attaching
    // another device's interfaces to the first matching product. A genuine
    // DJI ID wins over the documented Quectel fallback independent of output
    // order; within one class a stable location breaks ties.
    let mut selected: Option<((u8, i64, i64, bool, i64), UsbDevice)> = None;
    for block in output.split("\n\n") {
        let vendor_id = numeric_property(block, "idVendor");
        let product_id = numeric_property(block, "idProduct");
        let (Some(vendor_id), Some(product_id)) = (vendor_id, product_id) else {
            continue;
        };
        // The DJI QDC507 may retain its vendor ID or have the documented
        // Quectel fallback ID after a USB composition change. Do not treat
        // every Quectel product as the DJI module.
        let rank = match (vendor_id, product_id) {
            (0x2ca3, 0x4006) => 0,
            (0x2c7c, 0x0125) => 1,
            (0x2ca3, _) => 2, // Preserve the existing DJI vendor observation.
            _ => continue,
        };
        let location = numeric_property(block, "locationID");
        let key = (
            rank,
            vendor_id,
            product_id,
            location.is_none(),
            location.unwrap_or_default(),
        );
        if selected
            .as_ref()
            .is_some_and(|(previous, _)| key > *previous)
        {
            continue;
        }
        if selected
            .as_ref()
            .is_none_or(|(previous, _)| key < *previous)
        {
            let mut product = string_property(block, "USB Product Name");
            let mut vendor = string_property(block, "USB Vendor Name");
            if product.trim().is_empty() {
                product = if vendor_id == 0x2ca3 {
                    "DJI 4G Module"
                } else {
                    "兼容 USB 模块"
                }
                .into();
            }
            if vendor.trim().is_empty() {
                vendor = if vendor_id == 0x2ca3 {
                    "DJI"
                } else {
                    "Quectel"
                }
                .into();
            }
            selected = Some((
                key,
                UsbDevice {
                    product,
                    vendor,
                    vendor_id: format!("{vendor_id:04x}"),
                    product_id: format!("{product_id:04x}"),
                    location_id: location
                        .map(|value| format!("0x{value:x}"))
                        .unwrap_or_default(),
                    speed: numeric_property(block, "USBSpeed")
                        .map(usb_speed_name)
                        .unwrap_or_default(),
                    mode: "vendor-specific USB mode".into(),
                    interfaces: Vec::new(),
                },
            ));
        }
        let current = &mut selected.as_mut().expect("selected USB identity").1;
        if let Some(number) = numeric_property(block, "bInterfaceNumber") {
            current.interfaces.push(UsbInterface {
                number,
                class: numeric_property(block, "bInterfaceClass").unwrap_or_default(),
                subclass: numeric_property(block, "bInterfaceSubClass").unwrap_or_default(),
                protocol: numeric_property(block, "bInterfaceProtocol").unwrap_or_default(),
                endpoints: numeric_property(block, "bNumEndpoints").unwrap_or_default(),
            });
        }
    }
    let mut device = selected.map(|(_, device)| device);
    if let Some(device) = device.as_mut() {
        device.interfaces.sort_by_key(|interface| interface.number);
        if !device.interfaces.is_empty() && device.interfaces.iter().all(|item| item.class == 255) {
            device.mode = "vendor-specific USB interface".into();
        }
    }
    device
}

/// Shared legacy-compatible USB discovery DTO. Parsing does not open a device
/// or establish transport/bootstrap readiness.
pub(crate) fn parse_health_usb_device(
    output: &str,
) -> Option<proletarius_api_contract::HealthUsbDevice> {
    use proletarius_api_contract::{HealthUsbDevice, HealthUsbInterface};
    parse_dji_usb_device(output).map(|device| HealthUsbDevice {
        product: device.product,
        vendor: device.vendor,
        vendor_id: device.vendor_id,
        product_id: device.product_id,
        location_id: device.location_id,
        speed: device.speed,
        mode: device.mode,
        interfaces: (!device.interfaces.is_empty()).then(|| {
            device
                .interfaces
                .into_iter()
                .map(|interface| HealthUsbInterface {
                    number: interface.number,
                    class_id: interface.class,
                    subclass: interface.subclass,
                    protocol_id: interface.protocol,
                    endpoints: interface.endpoints,
                })
                .collect()
        }),
    })
}

fn numeric_property(block: &str, name: &str) -> Option<i64> {
    // Legacy Go regexp captures the first non-negative decimal prefix and
    // strconv.Atoi rejects signed-64-bit overflow (without truncation).
    let pattern = Regex::new(&format!(
        r#""{}"[\t\n\f\r ]*=[\t\n\f\r ]*([0-9]+)"#,
        regex::escape(name)
    ))
    .ok()?;
    pattern.captures(block)?.get(1)?.as_str().parse().ok()
}

fn string_property(block: &str, name: &str) -> String {
    let pattern = Regex::new(&format!(
        r#""{}"[\t\n\f\r ]*=[\t\n\f\r ]*"([^"]*)""#,
        regex::escape(name)
    ));
    pattern
        .ok()
        .and_then(|pattern| {
            pattern
                .captures(block)
                .and_then(|c| c.get(1).map(|v| v.as_str().to_owned()))
        })
        .unwrap_or_default()
}

fn usb_speed_name(speed: i64) -> String {
    match speed {
        0 => String::new(),
        1 => "low-speed".into(),
        2 => "full-speed".into(),
        3 => "high-speed".into(),
        4 => "super-speed".into(),
        value => format!("speed-{value}"),
    }
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
            .take((MAX_HOST_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_HOST_OUTPUT_BYTES {
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
    timeout(HOST_COMMAND_TIMEOUT, operation)
        .await
        .map_err(|_| "command timed out".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::{HostSnapshot, parse_dji_usb_device, parse_health_usb_device, project};
    use std::collections::BTreeMap;

    #[test]
    fn parses_dji_usb_product_for_local_network_identity_and_ignores_other_devices() {
        let output = concat!(
            "+-o Generic USB Device\n",
            "  \"idVendor\" = 1234\n",
            "  \"idProduct\" = 42\n\n",
            "+-o DJI Module Interface\n",
            "  \"idVendor\" = 11427\n",
            "  \"idProduct\" = 513\n",
            "  \"USB Product Name\" = \"DJI 4G Module\"\n",
            "  \"USB Vendor Name\" = \"DJI\"\n",
            "  \"locationID\" = 305397760\n",
            "  \"USBSpeed\" = 3\n",
            "  \"bInterfaceNumber\" = 0\n",
            "  \"bInterfaceClass\" = 255\n",
            "  \"bInterfaceSubClass\" = 255\n",
            "  \"bInterfaceProtocol\" = 255\n",
            "  \"bNumEndpoints\" = 3\n"
        );

        let device = parse_dji_usb_device(output).expect("DJI device should be found");
        assert_eq!(device.product, "DJI 4G Module");
        assert_eq!(device.vendor, "DJI");
        assert_eq!(device.vendor_id, "2ca3");
        assert_eq!(device.product_id, "0201");
        assert_eq!(device.location_id, "0x12340000");
        assert_eq!(device.speed, "high-speed");
        assert_eq!(device.mode, "vendor-specific USB interface");
        assert_eq!(device.interfaces.len(), 1);
        assert_eq!(device.interfaces[0].number, 0);

        assert!(parse_dji_usb_device("+-o Generic USB Device\n  \"idVendor\" = 1234\n").is_none());
    }

    #[test]
    fn dji_usb_parser_preserves_legacy_fallback_product_and_vendor() {
        let output = concat!(
            "+-o DJI Module Interface\n",
            "  \"idVendor\" = 11427\n",
            "  \"idProduct\" = 513\n",
            "  \"bInterfaceNumber\" = 0\n",
            "  \"bInterfaceClass\" = 2\n"
        );
        let device = parse_dji_usb_device(output).expect("DJI device should be found");
        assert_eq!(device.product, "DJI 4G Module");
        assert_eq!(device.vendor, "DJI");
        assert_eq!(device.mode, "vendor-specific USB mode");
    }

    #[test]
    fn documented_fallback_id_is_visible_without_claiming_every_quectel_device_is_dji() {
        let fallback = concat!(
            "+-o USB cellular interface\n",
            "  \"idVendor\" = 11388\n",
            "  \"idProduct\" = 293\n",
            "  \"bInterfaceNumber\" = 2\n",
            "  \"bInterfaceClass\" = 255\n"
        );
        let device = parse_dji_usb_device(fallback).expect("known fallback ID must be visible");
        assert_eq!(device.vendor_id, "2c7c");
        assert_eq!(device.product_id, "0125");
        assert_eq!(device.product, "兼容 USB 模块");
        assert_eq!(device.vendor, "Quectel");
        let health = parse_health_usb_device(fallback).unwrap();
        assert_eq!(health.vendor_id, "2c7c");
        assert_eq!(health.product_id, "0125");
        for (vendor, product) in [(11388, 292), (11388, 294), (4660, 293)] {
            let unrelated = format!(
                "+-o unrelated USB interface\n  \"idVendor\" = {vendor}\n  \"idProduct\" = {product}\n"
            );
            assert!(parse_health_usb_device(&unrelated).is_none());
        }
    }

    #[test]
    fn native_dji_candidate_wins_without_importing_fallback_interfaces() {
        let native = concat!(
            "\"idVendor\" = 11427\n\"idProduct\" = 16390\n",
            "\"locationID\" = 16777216\n\"bInterfaceNumber\" = 2\n",
            "\"bInterfaceClass\" = 255\n"
        );
        let fallback = concat!(
            "\"idVendor\" = 11388\n\"idProduct\" = 293\n",
            "\"locationID\" = 33554432\n\"bInterfaceNumber\" = 7\n",
            "\"bInterfaceClass\" = 255\n"
        );
        for blocks in [[fallback, native], [native, fallback]] {
            let observed = parse_health_usb_device(&blocks.join("\n\n")).unwrap();
            assert_eq!(
                (observed.vendor_id.as_str(), observed.product_id.as_str()),
                ("2ca3", "4006")
            );
            assert_eq!(observed.location_id, "0x1000000");
            let interfaces = observed.interfaces.unwrap();
            assert_eq!(interfaces.len(), 1);
            assert_eq!(interfaces[0].number, 2);
        }
    }

    #[test]
    fn identical_vid_pid_on_different_locations_never_merge_interfaces() {
        let low = concat!(
            "\"idVendor\" = 11427\n\"idProduct\" = 16390\n",
            "\"locationID\" = 16777216\n\"bInterfaceNumber\" = 2\n"
        );
        let low_second_interface = concat!(
            "\"idVendor\" = 11427\n\"idProduct\" = 16390\n",
            "\"locationID\" = 16777216\n\"bInterfaceNumber\" = 3\n"
        );
        let high = concat!(
            "\"idVendor\" = 11427\n\"idProduct\" = 16390\n",
            "\"locationID\" = 33554432\n\"bInterfaceNumber\" = 8\n"
        );
        for blocks in [
            [high, low, low_second_interface],
            [low_second_interface, high, low],
        ] {
            let observed = parse_health_usb_device(&blocks.join("\n\n")).unwrap();
            assert_eq!(observed.location_id, "0x1000000");
            let numbers: Vec<_> = observed
                .interfaces
                .unwrap()
                .into_iter()
                .map(|item| item.number)
                .collect();
            assert_eq!(numbers, [2, 3]);
        }
    }

    #[test]
    fn network_diagnostic_projection_matches_legacy_fields_and_null_empty_arrays() {
        let raw = BTreeMap::from([
            ("usbnet".into(), "+QCFG: \"usbnet\",2\r\nOK".into()),
            (
                "usbcfg".into(),
                "+QCFG: \"USBCFG\",0x2c7c,0x0800,1,1,1\r\nOK".into(),
            ),
            (
                "cgdcont".into(),
                "+CGDCONT: 1,\"IPV4V6\",\"internet.example\"\r\nOK".into(),
            ),
            ("cgact".into(), "+CGACT: 1,1\r\nOK".into()),
            (
                "cgpaddr".into(),
                "+CGPADDR: 1,\"192.0.2.4,2001:db8::4\"\r\nOK".into(),
            ),
        ]);
        let value = project(HostSnapshot::default(), raw, BTreeMap::new(), 5, 10, 20);
        assert_eq!(value.usbnet_mode, "2");
        assert_eq!(value.usbcfg, "\"USBCFG\",0x2c7c,0x0800,1,1,1");
        assert_eq!(value.pdp_contexts.as_ref().unwrap()[0].id, 1);
        assert_eq!(value.active_contexts, Some(vec![1]));
        assert_eq!(
            value.pdp_addresses,
            Some(vec!["192.0.2.4".into(), "2001:db8::4".into()])
        );
        assert!(!value.partial);
        let json = serde_json::to_value(value).unwrap();
        assert_eq!(json["errors"], serde_json::Value::Null);
        assert_eq!(json["mac_interfaces"], serde_json::Value::Null);
        assert_eq!(json["usb_network_present"], false);

        let empty = project(
            HostSnapshot::default(),
            BTreeMap::new(),
            BTreeMap::new(),
            0,
            1,
            0,
        );
        let json = serde_json::to_value(empty).unwrap();
        assert_eq!(json["pdp_contexts"], serde_json::Value::Null);
        assert_eq!(json["active_contexts"], serde_json::Value::Null);
        assert_eq!(json["pdp_addresses"], serde_json::Value::Null);
    }

    #[test]
    fn failures_keep_partial_raw_data_and_usb_interface_projection() {
        let host = HostSnapshot {
            mac_interfaces: Some(vec![super::MacInterface {
                name: "en7".into(),
                status: "active".into(),
                ipv4: "192.0.2.1".into(),
                kind: "ethernet".into(),
            }]),
            ..HostSnapshot::default()
        };
        let mut raw = BTreeMap::new();
        raw.insert("usbnet".into(), "+QCFG: \"usbnet\",0".into());
        let mut errors = BTreeMap::new();
        errors.insert("cgdcont".into(), "timeout".into());
        let value = project(host, raw, errors, 4, 10, 30);
        assert!(value.partial);
        assert_eq!(value.usbnet_mode, "0");
        assert!(value.usb_network_present);
        let json = serde_json::to_value(value).unwrap();
        assert_eq!(json["raw"]["usbnet"], "+QCFG: \"usbnet\",0");
        assert_eq!(json["errors"]["cgdcont"], "timeout");
    }
}
