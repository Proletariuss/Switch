//! Fixed USB AT device-status collection. The USB owner supplies one reserved
//! transport and fences every command; this module never opens or rebinds it.
//!
//! These parsers intentionally preserve the former USB projection. The serial
//! actor has different commands, fallback rules, and unknown-value conventions.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::Serialize;

use crate::usb_at::Failure;

const STEP_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DeviceStatus {
    pub(crate) imei: String,
    pub(crate) firmware: String,
    pub(crate) iccid: String,
    pub(crate) imsi: String,
    pub(crate) phone_number: String,
    /// A COPS identifier, not a localized operator display name.
    pub(crate) operator_identifier: String,
    /// Legacy-compatible localized display value derived from the COPS identifier.
    pub(crate) operator_name: String,
    pub(crate) sim_inserted: bool,
    pub(crate) signal_dbm: i64,
    pub(crate) reg_status: i64,
    pub(crate) reg_status_text: String,
    pub(crate) network_mode: String,
    pub(crate) network_duplex: String,
    pub(crate) radio_band: String,
    pub(crate) radio_channel: u32,
    pub(crate) usbnet_mode: i64,
}

#[derive(Default)]
struct Responses {
    firmware: String,
    cpin: String,
    csq: String,
    cereg: String,
    creg: String,
    cops: String,
    qnwinfo: String,
    qccid: String,
    cimi: String,
    cnum: String,
    gsn: String,
    usbnet: String,
}

/// The supplied callback owns the actual I/O permit and applies the whole-plan
/// deadline and producer/session/radio-owner checks before and after each step.
/// An AT ERROR remains a response and follows the legacy field defaults. Any
/// callback error is a transport/admission failure: stop immediately instead of
/// mixing partial facts with a reopened device or continuing on a damaged stream.
pub(crate) fn collect<F>(suppress_cellular: bool, mut command: F) -> Result<DeviceStatus, Failure>
where
    F: FnMut(&str, Duration, usize) -> Result<String, Failure>,
{
    let cfun = command("AT+CFUN?", Duration::from_secs(4), MAX_RESPONSE_BYTES)?;
    let mut run = |text: &str| command(text, STEP_TIMEOUT, MAX_RESPONSE_BYTES);
    let suppressed = suppress_cellular || parse_cfun(&cfun) == Some(4);
    let mut responses = Responses {
        firmware: run("ATI")?,
        cpin: run("AT+CPIN?")?,
        ..Responses::default()
    };
    if !suppressed {
        responses.csq = run("AT+CSQ")?;
        responses.cereg = run("AT+CEREG?")?;
        responses.creg = run("AT+CREG?")?;
        // This changes only the COPS display format, not radio registration.
        let _ = run("AT+COPS=3,2")?;
        responses.cops = run("AT+COPS?")?;
        responses.qnwinfo = run("AT+QNWINFO")?;
    }
    responses.qccid = run("AT+QCCID")?;
    responses.cimi = run("AT+CIMI")?;
    responses.cnum = run("AT+CNUM")?;
    responses.gsn = run("AT+CGSN")?;
    responses.usbnet = run("AT+QCFG=\"usbnet\"")?;
    Ok(project(&responses, suppressed))
}

fn project(responses: &Responses, suppressed: bool) -> DeviceStatus {
    let reg_status = first_nonzero_registration(&[&responses.cereg, &responses.creg]);
    let (network_mode, network_duplex, radio_band, radio_channel) =
        parse_qnwinfo(&responses.qnwinfo);
    let mut operator_identifier = parse_operator_identifier(&responses.cops);
    // The former Go projection only displayed names from the quoted COPS
    // operator field. Keep the legacy CHN-UNICOM identifier fallback out of
    // the display projection when the modem response is unquoted/malformed.
    let operator_name = crate::operator_name::resolve_serving_operator_name(&operator_identifier);
    if operator_identifier.trim().is_empty() && responses.cops.contains("CHN-UNICOM") {
        operator_identifier = "CHN-UNICOM".into();
    }
    let mut status = DeviceStatus {
        imei: parse_imei(&responses.gsn),
        firmware: parse_firmware(&responses.firmware),
        iccid: parse_prefixed(&responses.qccid, "+QCCID:"),
        imsi: parse_imsi(&responses.cimi),
        phone_number: parse_cnum(&responses.cnum),
        operator_identifier,
        operator_name,
        sim_inserted: legacy_uppercase(&responses.cpin).contains("READY"),
        signal_dbm: parse_csq_dbm(&responses.csq),
        reg_status,
        reg_status_text: registration_text(reg_status).into(),
        network_mode,
        network_duplex,
        radio_band,
        radio_channel,
        usbnet_mode: parse_usbnet(&responses.usbnet).unwrap_or(-1),
    };
    if suppressed {
        status.operator_identifier.clear();
        status.operator_name.clear();
        status.signal_dbm = -999;
        status.reg_status = 0;
        status.reg_status_text = "飞行模式 · 蜂窝射频关闭".into();
        status.network_mode = "飞行模式".into();
        status.network_duplex.clear();
        status.radio_band.clear();
        status.radio_channel = 0;
    }
    status
}

fn lines(response: &str) -> impl Iterator<Item = &str> {
    response
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
}

// Go strings.ToUpper uses simple rune mappings. Rust's full mapping can expand
// a ligature into several ASCII letters and accidentally match an AT keyword.
fn legacy_uppercase(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            let mut upper = character.to_uppercase();
            let first = upper.next().unwrap_or(character);
            if upper.next().is_none() {
                first
            } else {
                character
            }
        })
        .collect()
}

fn parse_firmware(response: &str) -> String {
    lines(response)
        .filter(|line| {
            let upper = legacy_uppercase(line);
            !upper.starts_with("ATI") && upper != "OK"
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn parse_prefixed(response: &str, prefix: &str) -> String {
    lines(response)
        .find_map(|line| {
            line.strip_prefix(prefix)
                .map(|value| value.trim().to_owned())
        })
        .unwrap_or_default()
}

fn parse_imsi(response: &str) -> String {
    lines(response)
        .find(|line| {
            line.len() >= 5
                && line.bytes().all(|byte| byte.is_ascii_digit())
                && line.parse::<u64>().is_ok()
        })
        .unwrap_or_default()
        .to_owned()
}

// Go's regexp \d and \s are ASCII classes. Spell those classes explicitly so
// Rust regex does not start accepting Unicode digits or whitespace on this wire.
static IMEI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9]{15}").unwrap());
static CFUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\+CFUN:[\t\n\f\r ]*([0-9]+)").unwrap());
static CSQ: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\+CSQ:[\t\n\f\r ]*([0-9]+),").unwrap());
static COPS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\+COPS:[\t\n\f\r ]*[0-9]+,[0-9]+,"([^"]*)""#).unwrap());
static REGISTRATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\+(?:CE)?REG:[\t\n\f\r ]*[0-9]+,([0-9]+)").unwrap());
static QNWINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\+QNWINFO:[\t\n\f\r ]*"([^"]*)","[^"]*","([^"]*)",([0-9]+)"#).unwrap()
});
static CNUM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\+CNUM:[\t\n\f\r ]*"[^"]*"[\t\n\f\r ]*,[\t\n\f\r ]*"([^"]*)""#).unwrap()
});
static USBNET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\+QCFG:[\t\n\f\r ]*"usbnet",([0-9]+)"#).unwrap());

fn parse_imei(response: &str) -> String {
    IMEI.find(response)
        .map_or_else(String::new, |value| value.as_str().to_owned())
}

fn capture_integer(pattern: &Regex, response: &str) -> Option<i64> {
    pattern.captures(response)?.get(1)?.as_str().parse().ok()
}

fn parse_cfun(response: &str) -> Option<i64> {
    capture_integer(&CFUN, response).filter(|value| (0..=4).contains(value))
}

fn parse_csq_dbm(response: &str) -> i64 {
    match capture_integer(&CSQ, response) {
        None | Some(99) => 0,
        // Preserve Go int64 arithmetic, including malformed out-of-range RSSI.
        Some(rssi) => rssi.wrapping_mul(2).wrapping_sub(113),
    }
}

fn parse_operator_identifier(response: &str) -> String {
    COPS.captures(response)
        .and_then(|value| value.get(1))
        .map_or_else(String::new, |value| value.as_str().to_owned())
}

fn first_nonzero_registration(responses: &[&str]) -> i64 {
    responses
        .iter()
        .find_map(|response| capture_integer(&REGISTRATION, response).filter(|value| *value != 0))
        .unwrap_or(0)
}

fn registration_text(status: i64) -> &'static str {
    match status {
        1 => "已注册",
        5 => "漫游注册",
        2 => "搜索中",
        3 => "注册被拒绝",
        _ => "未注册",
    }
}

fn parse_qnwinfo(response: &str) -> (String, String, String, u32) {
    let Some(value) = QNWINFO.captures(response) else {
        return (String::new(), String::new(), String::new(), 0);
    };
    let mut mode = value[1].to_owned();
    let mut duplex = String::new();
    if legacy_uppercase(&mode).contains("FDD") {
        duplex = "FDD".into();
        mode = mode.strip_prefix("FDD").unwrap_or(&mode).trim().to_owned();
    }
    if legacy_uppercase(&mode).contains("TDD") {
        duplex = "TDD".into();
        mode = mode.strip_prefix("TDD").unwrap_or(&mode).trim().to_owned();
    }
    let band = value[2]
        .strip_prefix("LTE ")
        .unwrap_or(&value[2])
        .to_owned();
    let channel = value[3].parse::<u32>().unwrap_or(0);
    (mode, duplex, band, channel)
}

fn parse_cnum(response: &str) -> String {
    for value in CNUM.captures_iter(response) {
        let mut phone = value[1].trim();
        if phone
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("tel:"))
        {
            phone = phone[4..].trim();
        }
        let phone: String = phone
            .chars()
            .filter(|value| !matches!(value, ' ' | '-' | '(' | ')'))
            .collect();
        if phone.is_empty() || phone.eq_ignore_ascii_case("ffffffff") {
            continue;
        }
        let digits = phone.strip_prefix('+').unwrap_or(&phone);
        if (5..=15).contains(&digits.len())
            && digits.bytes().all(|value| value.is_ascii_digit())
            && digits.bytes().any(|value| value != b'0')
        {
            return phone;
        }
    }
    String::new()
}

fn parse_usbnet(response: &str) -> Option<i64> {
    capture_integer(&USBNET, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_PLAN: &[&str] = &[
        "AT+CFUN?",
        "ATI",
        "AT+CPIN?",
        "AT+CSQ",
        "AT+CEREG?",
        "AT+CREG?",
        "AT+COPS=3,2",
        "AT+COPS?",
        "AT+QNWINFO",
        "AT+QCCID",
        "AT+CIMI",
        "AT+CNUM",
        "AT+CGSN",
        "AT+QCFG=\"usbnet\"",
    ];
    const SUPPRESSED_PLAN: &[&str] = &[
        "AT+CFUN?",
        "ATI",
        "AT+CPIN?",
        "AT+QCCID",
        "AT+CIMI",
        "AT+CNUM",
        "AT+CGSN",
        "AT+QCFG=\"usbnet\"",
    ];

    fn example_response(command: &str) -> &'static str {
        match command {
            "AT+CFUN?" => "\r\n+CFUN: 1\r\nOK\r\n",
            "ATI" => "ATI\r\nQuectel\r\nModel\r\nRevision: fixture\r\nOK\r\n",
            "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n",
            "AT+CSQ" => "+CSQ: 17,99\r\nOK\r\n",
            "AT+CEREG?" => "+CEREG: 2,5\r\nOK\r\n",
            "AT+CREG?" => "+CREG: 2,1\r\nOK\r\n",
            "AT+COPS?" => "+COPS: 0,2,\"23415\",7\r\nOK\r\n",
            "AT+QNWINFO" => "+QNWINFO: \"FDD LTE\",\"23415\",\"LTE BAND 3\",1300\r\nOK\r\n",
            "AT+QCCID" => "+QCCID: \"0012345F\"\r\nOK\r\n",
            "AT+CIMI" => "AT+CIMI\r\n001234567890123\r\nOK\r\n",
            "AT+CNUM" => "+CNUM: \"Line 1\",\"tel:+372 5555 1234\",145\r\nOK\r\n",
            "AT+CGSN" => "+CGSN: \"123456789012345\"\r\nOK\r\n",
            "AT+QCFG=\"usbnet\"" => "+QCFG: \"usbnet\",2\r\nOK\r\n",
            "AT+COPS=3,2" => "OK\r\n",
            _ => panic!("unexpected fixed command"),
        }
    }

    fn failure(code: &'static str) -> Failure {
        Failure {
            status: hyper::StatusCode::BAD_GATEWAY,
            code,
            message: "fixture failure".into(),
            response: String::new(),
            uncertain: false,
        }
    }

    #[test]
    fn complete_plan_projects_usb_compatibility_fields() {
        let mut commands = Vec::new();
        let status = collect(false, |command, timeout, limit| {
            commands.push(command.to_owned());
            assert_eq!(
                timeout,
                Duration::from_secs(if command == "AT+CFUN?" { 4 } else { 3 })
            );
            assert_eq!(limit, 1024 * 1024);
            Ok(example_response(command).into())
        })
        .unwrap();
        assert_eq!(commands, FULL_PLAN);
        assert_eq!(
            status,
            DeviceStatus {
                imei: "123456789012345".into(),
                firmware: "Quectel · Model · Revision: fixture".into(),
                iccid: "\"0012345F\"".into(),
                imsi: "001234567890123".into(),
                phone_number: "+37255551234".into(),
                operator_identifier: "23415".into(),
                operator_name: "23415".into(),
                sim_inserted: true,
                signal_dbm: -79,
                reg_status: 5,
                reg_status_text: "漫游注册".into(),
                network_mode: "LTE".into(),
                network_duplex: "FDD".into(),
                radio_band: "BAND 3".into(),
                radio_channel: 1300,
                usbnet_mode: 2,
            }
        );
    }

    #[test]
    fn explicit_or_observed_rf_off_never_sends_cellular_queries() {
        for (requested, cfun) in [(true, "+CFUN: 1"), (false, "+CFUN: 4"), (true, "ERROR")] {
            let mut commands = Vec::new();
            let status = collect(requested, |command, _, _| {
                commands.push(command.to_owned());
                Ok(if command == "AT+CFUN?" {
                    cfun
                } else {
                    example_response(command)
                }
                .into())
            })
            .unwrap();
            assert_eq!(commands, SUPPRESSED_PLAN);
            assert_eq!(status.operator_identifier, "");
            assert_eq!(status.operator_name, "");
            assert_eq!(status.signal_dbm, -999);
            assert_eq!(status.reg_status, 0);
            assert_eq!(status.reg_status_text, "飞行模式 · 蜂窝射频关闭");
            assert_eq!(status.network_mode, "飞行模式");
            assert_eq!(
                (
                    status.network_duplex.as_str(),
                    status.radio_band.as_str(),
                    status.radio_channel
                ),
                ("", "", 0)
            );
            assert_eq!(status.iccid, "\"0012345F\"");
            assert!(status.sim_inserted);
        }
    }

    #[test]
    fn transport_or_owner_failure_stops_at_every_step_without_replay() {
        for code in [
            "usb_at_disconnected",
            "usb_at_timeout",
            "usb_at_stale_session",
            "usb_at_busy",
            "usb_at_stopping",
            "usb_at_response_too_large",
            "usb_at_write_failed",
        ] {
            for failed_at in 0..FULL_PLAN.len() {
                let mut commands = Vec::new();
                let result = collect(false, |command, _, _| {
                    commands.push(command.to_owned());
                    if commands.len() - 1 == failed_at {
                        Err(failure(code))
                    } else {
                        Ok(example_response(command).into())
                    }
                });
                assert_eq!(result.unwrap_err().code, code);
                assert_eq!(commands, &FULL_PLAN[..=failed_at]);
            }
        }
    }

    #[test]
    fn at_errors_remain_optional_field_defaults_without_becoming_transport_errors() {
        let mut commands = Vec::new();
        let status = collect(false, |command, _, _| {
            commands.push(command.to_owned());
            Ok("ERROR\r\n".into())
        })
        .unwrap();
        assert_eq!(commands, FULL_PLAN);
        assert!(!status.sim_inserted);
        assert_eq!(status.signal_dbm, 0);
        assert_eq!(status.usbnet_mode, -1);
        assert_eq!(status.reg_status_text, "未注册");
        // ATI historically displays any non-echo, non-OK lines, even ERROR.
        assert_eq!(status.firmware, "ERROR");
        assert_eq!(status.imei, "");
        assert_eq!(status.operator_identifier, "");
        assert_eq!(status.operator_name, "");
    }

    #[test]
    fn identity_parsers_preserve_usb_prefix_padding_and_numeric_boundaries() {
        assert_eq!(
            parse_firmware(" ati\r\n\r Model \rVersion\nOk\nATIX\n"),
            "Model · Version"
        );
        assert_eq!(
            parse_prefixed("+qccid: wrong\r\n+QCCID: \"012345F\"\r\n", "+QCCID:"),
            "\"012345F\""
        );
        assert_eq!(parse_imsi("AT+CIMI\r\n+12345\n1234\n00123\n"), "00123");
        assert_eq!(
            parse_imsi("+CIMI: 123456789012345\n18446744073709551616\n18446744073709551615\n"),
            "18446744073709551615"
        );
        assert_eq!(parse_imsi("１２３４５\n+12345\n"), "");
        assert_eq!(parse_imei("+CGSN: \"0123456789012345\""), "012345678901234");
        assert_eq!(parse_imei("１２３４５６７８９０１２３４５"), "");
        assert_eq!(parse_firmware("ATı\nﬁrmware\nOK"), "ﬁrmware");
    }

    #[test]
    fn numeric_and_registration_parsers_match_legacy_unknown_and_priority_rules() {
        for (input, expected) in [
            ("", 0),
            ("+CSQ: 99,99", 0),
            ("+CSQ: 31,0", -51),
            ("+CSQ: 100,0", 87),
            ("+CSQ: 2 ,0", 0),
            ("+CSQ:\u{a0}2,0", 0),
            ("+CSQ: 9223372036854775808,0", 0),
            ("+CSQ: 9223372036854775807,0", -115),
        ] {
            assert_eq!(parse_csq_dbm(input), expected, "{input:?}");
        }
        assert_eq!(
            first_nonzero_registration(&["+CEREG: 2,0", "+CREG: 2,5"]),
            0
        );
        assert_eq!(
            first_nonzero_registration(&["+CEREG: 2,2", "+CREG: 2,1"]),
            2
        );
        assert_eq!(
            first_nonzero_registration(&["+CEREG: 2,0\n+CEREG: 2,5", "+CREG: 2,3"]),
            0
        );
        // The legacy pattern is (?:CE)?REG: it does not match +CREG. Keep
        // that projection quirk explicit rather than changing it incidentally.
        assert_eq!(first_nonzero_registration(&["+CEREG: 2,0", "+REG: 2,5"]), 5);
        assert_eq!(
            first_nonzero_registration(&["+CGREG: 2,1", "+CREG: 2, 5"]),
            0
        );
        assert_eq!(registration_text(7), "未注册");
        assert_eq!(parse_cfun("+CFUN: 04"), Some(4));
        assert_eq!(parse_cfun("+CFUN: 9\n+CFUN: 4"), None);
        assert_eq!(parse_usbnet("+QCFG: \"usbnet\",2"), Some(2));
        assert_eq!(parse_usbnet("+QCFG: \"usbnet\", 2"), None);
    }

    #[test]
    fn radio_parser_keeps_usb_band_case_and_overflow_behavior() {
        for (input, expected) in [
            (
                r#"+QNWINFO: "TDD LTE","00101","LTE BAND 40",99"#,
                ("LTE", "TDD", "BAND 40", 99),
            ),
            (
                r#"+QNWINFO: "fdd LTE","00101","lte BAND 3",4294967296"#,
                ("fdd LTE", "FDD", "lte BAND 3", 0),
            ),
            (
                r#"+QNWINFO: "FDD TDD LTE","00101","LTE BAND 3",1"#,
                ("LTE", "TDD", "BAND 3", 1),
            ),
            (
                r#"+QNWINFO: "LTE", "00101","LTE BAND 3",1"#,
                ("", "", "", 0),
            ),
        ] {
            let actual = parse_qnwinfo(input);
            assert_eq!(
                (
                    actual.0.as_str(),
                    actual.1.as_str(),
                    actual.2.as_str(),
                    actual.3
                ),
                expected
            );
        }
        assert_eq!(
            parse_operator_identifier(r#"+COPS: 0,2,"23415",7"#),
            "23415"
        );
        assert_eq!(parse_operator_identifier(r#"+COPS: 0, 2,"23415",7"#), "");
        let responses = Responses {
            cops: "CHN-UNICOM\r\nOK\r\n".into(),
            ..Responses::default()
        };
        let status = project(&responses, false);
        assert_eq!(status.operator_identifier, "CHN-UNICOM");
        assert_eq!(status.operator_name, "");
        assert_eq!(project(&responses, true).operator_identifier, "");
    }

    #[test]
    fn cnum_uses_only_number_field_and_exact_usb_normalization() {
        for (input, expected) in [
            (r#"+CNUM: "","+447882950731",145"#, "+447882950731"),
            (
                r#"+CNUM: "Line 1","tel:+372 5555 1234",145"#,
                "+37255551234",
            ),
            (r#"+cnum: "","TEL: +(123)-45",145"#, "+12345"),
            (r#"+CNUM: "123456789","unknown",129"#, ""),
            (r#"+CNUM: "","FFFFFFFF",129"#, ""),
            (r#"+CNUM: "","0000000000",129"#, ""),
            (r#"+CNUM: "","1234",129"#, ""),
            (r#"+CNUM: "","1234567890123456",129"#, ""),
            ("+CNUM: \"\",\"12\t345\",129", ""),
            ("+CNUM:\u{a0}\"\",\"12345\",129", ""),
            (
                "+CNUM: \"\",\"00000\",129\n+CNUM: \"\",\"12345\",129",
                "12345",
            ),
        ] {
            assert_eq!(parse_cnum(input), expected, "{input:?}");
        }
    }

    #[test]
    fn wire_always_contains_every_typed_field_and_no_raw_responses() {
        let value = serde_json::to_value(project(&Responses::default(), false)).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 16);
        for key in [
            "imei",
            "firmware",
            "iccid",
            "imsi",
            "phone_number",
            "operator_identifier",
            "operator_name",
            "reg_status_text",
            "network_mode",
            "network_duplex",
            "radio_band",
        ] {
            assert!(value[key].is_string(), "{key}");
        }
        assert!(value["sim_inserted"].is_boolean());
        for key in ["signal_dbm", "reg_status", "radio_channel", "usbnet_mode"] {
            assert!(value[key].is_number(), "{key}");
        }
        assert!(value.get("operator").is_none());
        assert!(value.get("response").is_none());
    }

    /// The Go test exports expected fields from its frozen legacy projection,
    /// rather than from this implementation. The checked-in corpus runs in the
    /// default suite without an external process or fixture environment.
    #[test]
    fn go_exported_projection_parity() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Case {
            name: String,
            suppress_cellular: bool,
            responses: std::collections::BTreeMap<String, String>,
            expected: serde_json::Value,
            commands: Vec<String>,
        }
        let bytes = std::env::var("SWITCH_USB_AT_STATUS_PARITY_FIXTURE")
            .map(|path| std::fs::read(path).expect("read explicit USB status oracle"))
            .unwrap_or_else(|_| {
                include_bytes!("../../../testdata/retired-go-oracles/usb-status.json").to_vec()
            });
        let cases: Vec<Case> = serde_json::from_slice(&bytes).expect("decode frozen Go fixture");
        assert_eq!(cases.len(), 85, "complete frozen USB status corpus");
        let mut names = std::collections::BTreeSet::new();
        for case in cases {
            assert!(
                names.insert(case.name.clone()),
                "duplicate USB fixture name"
            );
            assert_eq!(case.expected.as_object().unwrap().len(), 16);
            let mut commands = Vec::new();
            let actual = collect(case.suppress_cellular, |command, _, _| {
                commands.push(command.to_owned());
                Ok(case
                    .responses
                    .get(command)
                    .unwrap_or_else(|| panic!("{}: unexpected command {}", case.name, command))
                    .clone())
            })
            .unwrap();
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                case.expected,
                "{}",
                case.name
            );
            assert_eq!(commands, case.commands, "{} fixed command plan", case.name);
        }
    }
}
