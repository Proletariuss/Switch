//! Dial-string normalization used by the reader-backed IMS call flow.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallTargetError {
    Required,
    PhoneContextRequired,
    EmergencyCallingDisabled,
    UssdUnavailable,
}

pub fn normalize(
    peer: &str,
    call_phone_context: &str,
    home_domain: &str,
    realm: &str,
) -> Result<String, CallTargetError> {
    let target = peer.trim();
    if target.is_empty() {
        return Err(CallTargetError::Required);
    }
    if target.ends_with('#') && (target.starts_with('*') || target.starts_with('#')) {
        return Err(CallTargetError::UssdUnavailable);
    }
    let lower = target.to_ascii_lowercase();
    if lower.starts_with("sip:") || lower.starts_with("tel:") {
        return Ok(target.to_owned());
    }
    let emergency_key: String = target
        .chars()
        .filter(|ch| !matches!(ch, '+' | ' ' | '-' | '(' | ')'))
        .collect();
    if matches!(
        emergency_key.as_str(),
        "112" | "911" | "1911" | "999" | "000" | "110" | "119" | "120"
    ) {
        return Err(CallTargetError::EmergencyCallingDisabled);
    }
    let compact: String = target
        .chars()
        .filter(|ch| !matches!(ch, ' ' | '-' | '(' | ')'))
        .collect();
    let compact = if compact.starts_with("00")
        && compact.len() > 2
        && compact[2..].bytes().all(|byte| byte.is_ascii_digit())
    {
        format!("+{}", &compact[2..])
    } else {
        compact
    };
    if compact.starts_with('+') {
        return Ok(format!("tel:{compact}"));
    }
    let mut context = call_phone_context.trim();
    if context.is_empty() {
        context = home_domain.trim();
    }
    if context.is_empty() {
        context = realm.trim();
    }
    context = context
        .strip_prefix("sip:")
        .or_else(|| context.strip_prefix("SIP:"))
        .unwrap_or(context);
    if context.is_empty() {
        return Err(CallTargetError::PhoneContextRequired);
    }
    Ok(format!("tel:{compact};phone-context={context}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_global_access_prefix_and_local_target_semantics() {
        let cases = [
            (
                "+86 189-0731-3060",
                "",
                "ims.example.test",
                "",
                "tel:+8618907313060",
            ),
            (
                "00442079460000",
                "",
                "ims.example.test",
                "",
                "tel:+442079460000",
            ),
            (
                "00 44 (20) 7946-0000",
                "",
                "ims.example.test",
                "",
                "tel:+442079460000",
            ),
            (
                "1033",
                "",
                "ims.example.test",
                "",
                "tel:1033;phone-context=ims.example.test",
            ),
            (
                "00",
                "",
                "ims.example.test",
                "",
                "tel:00;phone-context=ims.example.test",
            ),
            (
                "sip:alice@ims.example",
                "",
                "ims.example.test",
                "",
                "sip:alice@ims.example",
            ),
            (" tel:888 ", "", "ims.example.test", "", "tel:888"),
            (
                "888",
                "",
                "",
                "sip:ims.example.test",
                "tel:888;phone-context=ims.example.test",
            ),
            (
                "888",
                "ims.carrier.test",
                "ims.home.test",
                "ims.realm.test",
                "tel:888;phone-context=ims.carrier.test",
            ),
        ];
        for (peer, carrier_context, home, realm, expected) in cases {
            assert_eq!(
                normalize(peer, carrier_context, home, realm).as_deref(),
                Ok(expected)
            );
        }
    }

    #[test]
    fn reports_missing_target_and_local_phone_context() {
        assert_eq!(
            normalize("  ", "", "ims.example", ""),
            Err(CallTargetError::Required)
        );
        assert_eq!(
            normalize("888", "", "", ""),
            Err(CallTargetError::PhoneContextRequired)
        );
    }

    #[test]
    fn rejects_ussd_and_emergency_voice_dialing_before_target_normalization() {
        for value in ["*100#", "##107#"] {
            assert_eq!(
                normalize(value, "", "ims.example", ""),
                Err(CallTargetError::UssdUnavailable)
            );
        }
        for value in [
            "112", "911", "1911", "+1 911", "999", "000", "110", "119", "120",
        ] {
            assert_eq!(
                normalize(value, "", "ims.example", ""),
                Err(CallTargetError::EmergencyCallingDisabled)
            );
        }
        assert_eq!(
            normalize("tel:911", "", "ims.example", "").as_deref(),
            Ok("tel:911")
        );
    }
}
