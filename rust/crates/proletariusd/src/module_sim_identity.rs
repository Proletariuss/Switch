//! Shared strict basic-channel module SIM codec and full reader identity/AKA algorithms.
//! Both physical transports execute these bodies under one retained transaction.
use crate::module_esim_channel::AtCommandTransport;
use crate::reader_session_lease::ReaderAkaApplication;
use crate::usb_at::Failure;
use hyper::StatusCode;
use proletarius_sim::ApduTransport;
use proletarius_vowifi::SimAkaResult;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;
const MAX_BYTES: usize = 1024 * 1024;
pub(crate) struct ModuleSimApduTransport<'a> {
    pub(crate) command: &'a mut dyn AtCommandTransport<Error = Failure>,
    pub(crate) deadline: Instant,
}
impl ApduTransport for ModuleSimApduTransport<'_> {
    type Error = Failure;

    fn transmit(&mut self, apdu: &[u8]) -> Result<Vec<u8>, Self::Error> {
        if apdu.is_empty() || apdu.len() > 255 {
            return Err(Failure::new(
                StatusCode::BAD_REQUEST,
                "usb_at_invalid_request",
                "USB AT request is invalid",
            ));
        }
        let mut encoded = Zeroizing::new(String::with_capacity(apdu.len() * 2));
        for byte in apdu {
            use std::fmt::Write as _;
            let _ = write!(&mut encoded, "{byte:02X}");
        }
        let timeout = identity_command_timeout(self.deadline, Duration::from_secs(12))?;
        let at_command = Zeroizing::new(format!(
            "AT+CSIM={},\"{}\"",
            encoded.len(),
            encoded.as_str()
        ));
        let at_response = Zeroizing::new(self.command.command(&at_command, timeout, MAX_BYTES)?);
        parse_module_csim_response(&at_response)
    }
}

pub(crate) fn identity_command_timeout(
    deadline: Instant,
    maximum: Duration,
) -> Result<Duration, Failure> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Failure::new(
            StatusCode::REQUEST_TIMEOUT,
            "module_sim_identity_timeout",
            "模块 SIM 身份读取超过事务期限",
        ));
    }
    Ok(remaining.min(maximum))
}

pub(crate) fn normalize_module_sim_identifier(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_end_matches(['F', 'f'])
        .to_owned()
}

pub(crate) fn valid_module_sim_identifier(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) fn parse_module_qccid(response: &str) -> Option<String> {
    let value = response
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("+QCCID:"))?;
    let identity = normalize_module_sim_identifier(value);
    valid_module_sim_identifier(&identity, 10, 22).then_some(identity)
}

pub(crate) fn parse_module_cimi(response: &str) -> Option<String> {
    response
        .lines()
        .map(str::trim)
        .find(|line| line.len() >= 5 && line.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|line| {
            line.parse::<u64>()
                .ok()
                .map(|_| normalize_module_sim_identifier(line))
        })
}

pub(crate) fn parse_module_csim_response(response: &str) -> Result<Vec<u8>, Failure> {
    let payload = response
        .split_once("+CSIM:")
        .map(|(_, payload)| payload)
        .ok_or_else(|| {
            Failure::new(
                StatusCode::BAD_GATEWAY,
                "module_csim_invalid_response",
                "模块 SIM 响应格式无效",
            )
        })?;
    let (length, encoded_tail) = payload.trim().split_once(',').ok_or_else(|| {
        Failure::new(
            StatusCode::BAD_GATEWAY,
            "module_csim_invalid_response",
            "模块 SIM 响应格式无效",
        )
    })?;
    let encoded_tail = encoded_tail.trim();
    let (encoded, tail) = if let Some(encoded_quoted) = encoded_tail.strip_prefix('"') {
        let quote = encoded_quoted.find('"').ok_or_else(|| {
            Failure::new(
                StatusCode::BAD_GATEWAY,
                "module_csim_invalid_response",
                "模块 SIM 响应格式无效",
            )
        })?;
        (&encoded_quoted[..quote], encoded_quoted[quote + 1..].trim())
    } else {
        encoded_tail
            .split_once(['\r', '\n'])
            .map_or((encoded_tail, ""), |(encoded, tail)| {
                (encoded.trim(), tail.trim())
            })
    };
    if !tail.is_empty() && tail != "OK" {
        return Err(Failure::new(
            StatusCode::BAD_GATEWAY,
            "module_csim_invalid_response",
            "模块 SIM 响应格式无效",
        ));
    }
    if !length
        .trim()
        .parse::<usize>()
        .is_ok_and(|size| size == encoded.len())
        || encoded.is_empty()
        || encoded.len() > 510
        || !encoded.len().is_multiple_of(2)
    {
        return Err(Failure::new(
            StatusCode::BAD_GATEWAY,
            "module_csim_invalid_response",
            "模块 SIM 响应长度无效",
        ));
    }
    let mut output = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0]).ok_or_else(|| {
            Failure::new(
                StatusCode::BAD_GATEWAY,
                "module_csim_invalid_response",
                "模块 SIM 响应编码无效",
            )
        })?;
        let low = hex_nibble(pair[1]).ok_or_else(|| {
            Failure::new(
                StatusCode::BAD_GATEWAY,
                "module_csim_invalid_response",
                "模块 SIM 响应编码无效",
            )
        })?;
        output.push((high << 4) | low);
    }
    Ok(output)
}

pub(crate) fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

pub(crate) fn parse_module_cnum_number(response: &str) -> Option<String> {
    let payload = response.split_once("+CNUM:")?.1;
    let mut quoted = payload.split('"').skip(1).step_by(2);
    let _alpha_tag = quoted.next()?;
    validate_module_phone(quoted.next()?)
}

pub(crate) fn parse_module_csca_number(response: &str) -> Option<String> {
    let payload = response.split_once("+CSCA:")?.1;
    let value = payload.split('"').nth(1)?;
    validate_module_phone(value)
}

/// Resolve the SMS route as part of the same Rust-owned SIM identity snapshot.
/// Card-provisioned values win; the modem's active AT+CSCA value is only a
/// fallback, and a tel: target is synthesized only when no PSI was provisioned.
pub(crate) fn resolve_module_smsc(
    card_address: &str,
    card_target: &str,
    at_address: &str,
) -> (String, String) {
    let address = normalize_module_smsc(card_address)
        .or_else(|| normalize_module_smsc(at_address))
        .unwrap_or_default();
    let target = match card_target.trim() {
        "" if !address.is_empty() => format!("tel:{address}"),
        value => value.to_owned(),
    };
    (address, target)
}

pub(crate) fn normalize_module_smsc(value: &str) -> Option<String> {
    let value = value.trim().trim_matches('"');
    let mut digits = String::with_capacity(value.len());
    let mut prefix = false;
    for character in value.chars() {
        match character {
            '0'..='9' => digits.push(character),
            '+' if digits.is_empty() && !prefix => prefix = true,
            ' ' | '\t' | '-' | '(' | ')' | '.' => {}
            _ => return None,
        }
    }
    let nonzero = digits.bytes().any(|byte| byte != b'0');
    if !(5..=15).contains(&digits.len()) || !nonzero {
        return None;
    }
    if prefix {
        digits.insert(0, '+');
    }
    Some(digits)
}

pub(crate) fn validate_module_phone(value: &str) -> Option<String> {
    let value = value.trim();
    let digits = value
        .strip_prefix('+')
        .unwrap_or(value)
        .bytes()
        .all(|byte| byte.is_ascii_digit());
    (digits && !value.is_empty()).then(|| value.to_owned())
}

pub(crate) struct IdentityRead {
    pub(crate) metadata: Value,
    pub(crate) phone_number: String,
    pub(crate) iccid: Zeroizing<String>,
    pub(crate) imsi: Zeroizing<String>,
}
pub(crate) fn read_identity(
    command: &mut dyn AtCommandTransport<Error = Failure>,
    deadline: Instant,
) -> Result<IdentityRead, Failure> {
    let cnum = command.command(
        "AT+CNUM",
        identity_command_timeout(deadline, Duration::from_secs(3))?,
        4096,
    )?;
    let metadata = {
        let mut transport = ModuleSimApduTransport { command, deadline };
        proletarius_reader::read_sim_metadata_module_with_transport(&mut transport).map_err(
            |_| {
                Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "module_sim_identity_failed",
                    "读取模块 SIM 身份信息失败",
                )
            },
        )?
    };
    let csca = command.command(
        "AT+CSCA?",
        identity_command_timeout(deadline, Duration::from_secs(3))?,
        4096,
    )?;
    let phone_number = if metadata.phone_number.trim().is_empty() {
        parse_module_cnum_number(&cnum).unwrap_or_default()
    } else {
        metadata.phone_number.clone()
    };
    let smsc_at = parse_module_csca_number(&csca).unwrap_or_default();
    let (smsc, smsc_target) = resolve_module_smsc(&metadata.smsc, &metadata.smsc_target, &smsc_at);
    let iccid = Zeroizing::new(normalize_module_sim_identifier(&metadata.iccid));
    let imsi = Zeroizing::new(normalize_module_sim_identifier(&metadata.imsi));
    if !valid_module_sim_identifier(&iccid, 10, 22) || !valid_module_sim_identifier(&imsi, 5, 20) {
        return Err(Failure::new(
            StatusCode::BAD_GATEWAY,
            "module_sim_identity_invalid",
            "模块 SIM 身份字段无效",
        ));
    }
    let mut metadata = serde_json::to_value(metadata).map_err(|_| {
        Failure::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "module_sim_identity_encode_failed",
            "模块 SIM 身份响应编码失败",
        )
    })?;
    metadata["smsc"] = json!(smsc);
    metadata["smsc_target"] = json!(smsc_target);
    Ok(IdentityRead {
        metadata,
        phone_number,
        iccid,
        imsi,
    })
}

/// A single finite basic-channel APDU. No command chaining or logical channels.
pub(crate) fn valid_module_csim_command(command: &str) -> bool {
    let Some(payload) = command.strip_prefix("AT+CSIM=") else {
        return false;
    };
    let Some((length, quoted)) = payload.split_once(',') else {
        return false;
    };
    let Some(encoded) = quoted.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return false;
    };
    !length.is_empty()
        && length.bytes().all(|b| b.is_ascii_digit())
        && length.parse::<usize>().is_ok_and(|n| n == encoded.len())
        && (2..=510).contains(&encoded.len())
        && encoded.len().is_multiple_of(2)
        && encoded
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}

pub(crate) fn authenticate(
    command: &mut dyn AtCommandTransport<Error = Failure>,
    application: ReaderAkaApplication,
    iccid: &str,
    imsi: &str,
    rand: &[u8; 16],
    autn: &[u8; 16],
) -> Result<SimAkaResult, Failure> {
    let app = match application {
        ReaderAkaApplication::Usim => proletarius_reader::AkaApplication::Usim,
        ReaderAkaApplication::Isim => proletarius_reader::AkaApplication::Isim,
    };
    let mut transport = ModuleSimApduTransport {
        command,
        deadline: Instant::now() + Duration::from_secs(60),
    };
    match proletarius_reader::authenticate_module_bound_with_transport(
        &mut transport,
        app,
        &iccid,
        &imsi,
        &rand[..],
        &autn[..],
    ) {
        Ok(proletarius_sim::AkaResult::Success { res, ck, ik }) => {
            Ok(SimAkaResult::Success { res, ck, ik })
        }
        Ok(proletarius_sim::AkaResult::SynchronizationFailure { auts }) => {
            Ok(SimAkaResult::SynchronizationFailure { auts })
        }
        Ok(proletarius_sim::AkaResult::AuthenticationReject) => {
            Ok(SimAkaResult::AuthenticationRejected)
        }
        Err(proletarius_reader::AuthenticationError::IdentityChanged) => Err(Failure::new(
            StatusCode::CONFLICT,
            "module_sim_identity_changed",
            "模块 SIM 身份已变更",
        )),
        Err(proletarius_reader::AuthenticationError::ApplicationUnavailable) => Err(Failure::new(
            StatusCode::CONFLICT,
            if application == ReaderAkaApplication::Isim {
                "reader_isim_unavailable"
            } else {
                "module_usim_unavailable"
            },
            "所选模块 SIM 应用不可用",
        )),
        Err(_) => Err(Failure::new(
            StatusCode::BAD_GATEWAY,
            "module_sim_aka_failed",
            "模块 SIM 鉴权失败",
        )),
    }
}

pub(crate) fn profile_check(
    command: &mut dyn AtCommandTransport<Error = Failure>,
    expected_iccid: &str,
    expected_imsi: &str,
    deadline: Instant,
) -> Result<Value, Failure> {
    let iccid_response = command.command(
        "AT+QCCID",
        identity_command_timeout(deadline, Duration::from_secs(4))?,
        4096,
    );
    let imsi_response = command.command(
        "AT+CIMI",
        identity_command_timeout(deadline, Duration::from_secs(4))?,
        4096,
    );
    if iccid_response.is_err() || imsi_response.is_err() {
        return Ok(json!({"outcome":"read_failed"}));
    }
    let current_iccid = iccid_response
        .as_ref()
        .ok()
        .and_then(|response| parse_module_qccid(response));
    let current_imsi = imsi_response
        .as_ref()
        .ok()
        .and_then(|response| parse_module_cimi(response));
    let matches = current_iccid.as_deref() == Some(expected_iccid)
        && current_imsi.as_deref() == Some(expected_imsi);
    Ok(json!({
        "outcome":if matches { "identity_matches" } else { "identity_changed" }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Offline codec contract used by both the USB adapter and serial actor.
    #[test]
    fn offline_basic_channel_command_admission_matches_production_encoding() {
        struct Command(Vec<String>);
        impl AtCommandTransport for Command {
            type Error = Failure;
            fn command(
                &mut self,
                value: &str,
                timeout: Duration,
                maximum: usize,
            ) -> Result<String, Failure> {
                assert!(valid_module_csim_command(value));
                assert!(timeout <= Duration::from_secs(12));
                assert_eq!(maximum, MAX_BYTES);
                self.0.push(value.to_owned());
                Ok("+CSIM: 4,\"9000\"\r\nOK".into())
            }
        }
        let mut command = Command(vec![]);
        let mut transport = ModuleSimApduTransport {
            command: &mut command,
            deadline: Instant::now() + Duration::from_secs(60),
        };
        assert_eq!(
            transport.transmit(&[0, 0xa4, 0, 4, 2, 0x3f, 0]).unwrap(),
            [0x90, 0]
        );
        assert!(transport.transmit(&[]).is_err());
        assert!(transport.transmit(&[0; 256]).is_err());
        assert_eq!(command.0, ["AT+CSIM=14,\"00A40004023F00\""]);
        for denied in [
            "AT+CSIM=4,\"00a4\"", // Retain existing USB uppercase-only admission.
            "AT+CSIM=3,\"00A4\"",
            "AT+CSIM=+4,\"00A4\"",
            "AT+CSIM=4,\"00A4\";AT+CFUN=1",
            "AT+CSIM=4,\"00A4\"\rAT+CFUN=1",
            "AT+CSIM=4,\"00A?\"",
            "AT+CSIM=4,00A4",
            "AT+CGLA=1,4,\"00A4\"",
        ] {
            assert!(!valid_module_csim_command(denied));
        }
    }

    #[test]
    fn offline_csim_response_rejects_ambiguous_or_trailing_payload() {
        for response in [
            "+CSIM: 4,\"9000\"\r\n+CSIM: 4,\"9000\"\r\nOK",
            "+CSIM: 4,\"9000\"\r\nERROR",
            "+CSIM: 4,\"9000\" extra\r\nOK",
            "+CSIM: 6,\"9000\"\r\nOK",
            "+CSIM: 3,\"900\"\r\nOK",
        ] {
            assert!(parse_module_csim_response(response).is_err());
        }
    }
}
