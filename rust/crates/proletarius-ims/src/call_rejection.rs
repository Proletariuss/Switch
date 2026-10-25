//! Stable, user-facing mapping for rejected outgoing IMS calls.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallRejectionError {
    InvalidStatus,
    InputTooLarge,
}

const MAX_REASON_LENGTH: usize = 4096;

pub fn rejection_message(
    status: u16,
    response_reason: &str,
    reason_header: &str,
    fallback_reason: &str,
) -> Result<String, CallRejectionError> {
    if !(100..=699).contains(&status) {
        return Err(CallRejectionError::InvalidStatus);
    }
    if response_reason.len() > MAX_REASON_LENGTH
        || reason_header.len() > MAX_REASON_LENGTH
        || fallback_reason.len() > MAX_REASON_LENGTH
    {
        return Err(CallRejectionError::InputTooLarge);
    }

    if status == 487 {
        let reason = if reason_header.trim().is_empty() {
            fallback_reason.trim()
        } else {
            reason_header.trim()
        };
        if reason.is_empty() {
            return Ok("呼叫请求被终止：SIP 487 Request Terminated".to_owned());
        }
        if let Some(cause) = q850_cause(reason) {
            match cause.as_str() {
                "1" => return Ok("呼叫未接通：SIP 487 / Q.850 1，号码不存在或未分配".to_owned()),
                "20" => return Ok("呼叫未接通：SIP 487 / Q.850 20，被叫暂时不可达".to_owned()),
                "127" => return Ok("呼叫未接通：SIP 487 / Q.850 127 互通失败".to_owned()),
                _ => {}
            }
        }
        return Ok(format!(
            "呼叫请求被终止：SIP 487 Request Terminated；Reason: {reason}"
        ));
    }
    Ok(format!("IMS call rejected: SIP {status} {response_reason}"))
}

fn q850_cause(value: &str) -> Option<String> {
    let mut parts = value.split(';');
    if !parts.next()?.trim().eq_ignore_ascii_case("q.850") {
        return None;
    }
    for part in parts {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("cause") {
            let cause = value.trim().trim_matches('"');
            if !cause.is_empty() {
                return Some(cause.to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_legacy_rejection_messages_and_reason_precedence() {
        let cases = [
            (
                487,
                "Request Terminated",
                "",
                "",
                "呼叫请求被终止：SIP 487 Request Terminated",
            ),
            (
                487,
                "Request Terminated",
                "Q.850;cause=1",
                "",
                "呼叫未接通：SIP 487 / Q.850 1，号码不存在或未分配",
            ),
            (
                487,
                "Request Terminated",
                "SIP;cause=487",
                "q.850;cause=20",
                "呼叫请求被终止：SIP 487 Request Terminated；Reason: SIP;cause=487",
            ),
            (
                487,
                "Request Terminated",
                "",
                "q.850;cause=127",
                "呼叫未接通：SIP 487 / Q.850 127 互通失败",
            ),
            (
                487,
                "Request Terminated",
                "Q.850;cause=;cause=20",
                "",
                "呼叫未接通：SIP 487 / Q.850 20，被叫暂时不可达",
            ),
            (
                403,
                "Forbidden",
                "",
                "",
                "IMS call rejected: SIP 403 Forbidden",
            ),
        ];
        for (status, response, header, fallback, expected) in cases {
            assert_eq!(
                rejection_message(status, response, header, fallback).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn rejects_invalid_status_and_oversized_reasons() {
        assert_eq!(
            rejection_message(700, "", "", ""),
            Err(CallRejectionError::InvalidStatus)
        );
        assert_eq!(
            rejection_message(487, "", &"x".repeat(MAX_REASON_LENGTH + 1), ""),
            Err(CallRejectionError::InputTooLarge)
        );
    }
}
