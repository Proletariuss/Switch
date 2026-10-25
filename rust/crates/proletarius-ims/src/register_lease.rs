//! REGISTER binding selection and granted-lease policy.
//!
//! Production REGISTER lease evaluation accepts raw Contact header fields and
//! performs parsing, binding selection, expiry policy, and revocation handling
//! in Rust. The candidate-fact API remains for compatibility tests/callers.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterLeaseCandidate {
    /// False when an explicit +sip.instance or reg-id conflicts with our
    /// request's corresponding value.
    pub compatible: bool,
    pub instance_match: bool,
    pub uri_match: bool,
    pub invalid_params: bool,
    pub expires: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterLeaseInput<'a> {
    pub contacts_present: bool,
    pub candidates: &'a [RegisterLeaseCandidate],
    pub global_expires: &'a [String],
    pub requested_seconds: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedContact {
    uri: proletarius_sip::Uri,
    instance: Option<String>,
    reg_id: Option<String>,
    expires: Option<String>,
    invalid_params: bool,
}

/// Parse the raw Contact fields from the request and registrar response, then
/// apply the binding and expiry policy in one Rust-owned production operation.
/// The compatibility Go evaluator remains available to tests/legacy callers;
/// the reader IMS session sends raw header values here.
pub fn evaluate_register_lease_headers(
    request_contacts: &[String],
    contacts: &[String],
    global_expires: &[String],
    requested_seconds: i64,
) -> Result<i64, RegisterLeaseError> {
    if requested_seconds <= 0
        || request_contacts.is_empty()
        || request_contacts.len() > 16
        || contacts.len() > 128
        || global_expires.len() > 64
        || request_contacts
            .iter()
            .chain(contacts)
            .any(|value| value.len() > 8192)
        || global_expires.iter().any(|value| value.len() > 32)
    {
        return Err(RegisterLeaseError::LeaseInvalid);
    }
    let own =
        parse_contacts(request_contacts).map_err(|_| RegisterLeaseError::RequestContactInvalid)?;
    if own.len() != 1 || own[0].invalid_params {
        return Err(RegisterLeaseError::RequestContactInvalid);
    }
    if contacts.is_empty() {
        return evaluate_register_lease(RegisterLeaseInput {
            contacts_present: false,
            candidates: &[],
            global_expires,
            requested_seconds,
        });
    }
    let parsed = parse_contacts(contacts)?;
    let own = &own[0];
    let candidates: Vec<_> = parsed
        .iter()
        .map(|candidate| {
            let instance_match = own
                .instance
                .as_ref()
                .zip(candidate.instance.as_ref())
                .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right));
            let compatible = !(own
                .instance
                .as_ref()
                .zip(candidate.instance.as_ref())
                .is_some_and(|(left, right)| !left.eq_ignore_ascii_case(right)))
                && !(own
                    .reg_id
                    .as_ref()
                    .zip(candidate.reg_id.as_ref())
                    .is_some_and(|(left, right)| left != right));
            RegisterLeaseCandidate {
                compatible,
                instance_match,
                uri_match: same_contact_uri(&own.uri, &candidate.uri),
                invalid_params: candidate.invalid_params,
                expires: candidate.expires.clone(),
            }
        })
        .collect();
    evaluate_register_lease(RegisterLeaseInput {
        contacts_present: true,
        candidates: &candidates,
        global_expires,
        requested_seconds,
    })
}

fn parse_contacts(values: &[String]) -> Result<Vec<ParsedContact>, RegisterLeaseError> {
    let mut parsed = Vec::new();
    for value in values {
        for entry in split_contact_value(value, ',')? {
            let fields = split_contact_value(entry, ';')?;
            let uri_text = fields.first().ok_or(RegisterLeaseError::ContactInvalid)?;
            let uri_text = if let Some(start) = uri_text.find('<') {
                let end = uri_text[start + 1..]
                    .find('>')
                    .map(|offset| start + 1 + offset)
                    .ok_or(RegisterLeaseError::ContactInvalid)?;
                uri_text[start + 1..end].trim()
            } else {
                uri_text.trim()
            };
            let uri = proletarius_sip::Uri::parse(uri_text)
                .map_err(|_| RegisterLeaseError::ContactInvalid)?;
            if uri.wildcard
                || uri.host.is_empty()
                || !(uri.scheme.eq_ignore_ascii_case("sip")
                    || uri.scheme.eq_ignore_ascii_case("sips"))
            {
                return Err(RegisterLeaseError::ContactInvalid);
            }
            let mut contact = ParsedContact {
                uri,
                instance: None,
                reg_id: None,
                expires: None,
                invalid_params: false,
            };
            let mut seen = std::collections::HashSet::new();
            for field in fields.iter().skip(1) {
                let (name, value) = field.split_once('=').unwrap_or((field, ""));
                let name = name.trim().to_ascii_lowercase();
                let value = value.trim().to_owned();
                if name.is_empty() || !seen.insert(name.clone()) {
                    contact.invalid_params = true;
                    continue;
                }
                match name.as_str() {
                    "+sip.instance" => {
                        contact.instance = normalize_instance(&value);
                        if contact.instance.as_deref().unwrap_or_default().is_empty() {
                            contact.invalid_params = true;
                        }
                    }
                    "reg-id" => contact.reg_id = (!value.is_empty()).then_some(value),
                    "expires" => contact.expires = Some(value),
                    _ => {}
                }
            }
            parsed.push(contact);
            if parsed.len() > 128 {
                return Err(RegisterLeaseError::ContactInvalid);
            }
        }
    }
    Ok(parsed)
}

fn normalize_instance(raw: &str) -> Option<String> {
    let value = raw.trim();
    let value = if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        let mut decoded = String::new();
        let mut chars = value[1..value.len() - 1].chars();
        while let Some(character) = chars.next() {
            if character == '\\' {
                decoded.push(chars.next()?);
            } else {
                decoded.push(character);
            }
        }
        decoded
    } else {
        value.to_owned()
    };
    let value = value.trim();
    let value = value
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or(value)
        .trim();
    Some(value.to_owned())
}

fn split_contact_value(value: &str, separator: char) -> Result<Vec<&str>, RegisterLeaseError> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut angle = false;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if matches!(character, '\r' | '\n' | '\0') {
            return Err(RegisterLeaseError::ContactInvalid);
        }
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        match character {
            '<' if !angle => angle = true,
            '>' if angle => angle = false,
            '<' | '>' => return Err(RegisterLeaseError::ContactInvalid),
            current if current == separator && !angle => {
                let part = value[start..index].trim();
                if part.is_empty() {
                    return Err(RegisterLeaseError::ContactInvalid);
                }
                parts.push(part);
                start = index + current.len_utf8();
            }
            _ => {}
        }
    }
    if quoted || escaped || angle {
        return Err(RegisterLeaseError::ContactInvalid);
    }
    let part = value[start..].trim();
    if part.is_empty() {
        return Err(RegisterLeaseError::ContactInvalid);
    }
    parts.push(part);
    Ok(parts)
}

fn same_contact_uri(left: &proletarius_sip::Uri, right: &proletarius_sip::Uri) -> bool {
    if !left.scheme.eq_ignore_ascii_case(&right.scheme)
        || left.user != right.user
        || left.password != right.password
        || left.port != right.port
    {
        return false;
    }
    let left_ip = left.host.parse::<std::net::IpAddr>().ok();
    let right_ip = right.host.parse::<std::net::IpAddr>().ok();
    let hosts_match = match (left_ip, right_ip) {
        (Some(left), Some(right)) => left == right,
        _ => left.host.eq_ignore_ascii_case(&right.host),
    };
    hosts_match
        && same_uri_params(&left.uri_params, &right.uri_params)
        && same_uri_params(&left.headers, &right.headers)
}

fn same_uri_params(left: &proletarius_sip::Params, right: &proletarius_sip::Params) -> bool {
    let left: Vec<_> = left.iter().collect();
    let right: Vec<_> = right.iter().collect();
    if left.len() != right.len() {
        return false;
    }
    left.iter().all(|(key, value)| {
        right
            .iter()
            .find(|(other_key, _)| key.eq_ignore_ascii_case(other_key))
            .is_some_and(|(_, other_value)| {
                if ["transport", "user", "method", "maddr"]
                    .iter()
                    .any(|special| key.eq_ignore_ascii_case(special))
                {
                    value.eq_ignore_ascii_case(other_value)
                } else {
                    value == other_value
                }
            })
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterLeaseError {
    LeaseInvalid,
    RequestContactInvalid,
    ContactInvalid,
    ContactExpiryInvalid,
    GlobalExpiryInvalid,
    BindingRevoked,
    BindingAmbiguous,
    BindingUnconfirmed,
}

/// Select the lease advertised by REGISTER, preserving the established Go
/// precedence: explicit request override, then carrier template, then 3600s.
pub fn effective_register_expiry(configured: i64, template: i64) -> i64 {
    if configured > 0 {
        configured
    } else if template > 0 {
        template
    } else {
        3600
    }
}

impl RegisterLeaseError {
    pub fn code(self) -> &'static str {
        match self {
            Self::LeaseInvalid => "lease_invalid",
            Self::RequestContactInvalid => "request_contact_invalid",
            Self::ContactInvalid => "contact_invalid",
            Self::ContactExpiryInvalid => "contact_expiry_invalid",
            Self::GlobalExpiryInvalid => "global_expiry_invalid",
            Self::BindingRevoked => "binding_revoked",
            Self::BindingAmbiguous => "binding_ambiguous",
            Self::BindingUnconfirmed => "binding_unconfirmed",
        }
    }
}

pub fn evaluate_register_lease(input: RegisterLeaseInput<'_>) -> Result<i64, RegisterLeaseError> {
    if input.requested_seconds <= 0
        || input.candidates.len() > 128
        || input.global_expires.len() > 64
    {
        return Err(RegisterLeaseError::LeaseInvalid);
    }

    if !input.contacts_present {
        return global_expiry(input.global_expires, input.requested_seconds);
    }

    let eligible: Vec<(usize, &RegisterLeaseCandidate)> = input
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.compatible)
        .collect();
    let instance_matches: Vec<(usize, &RegisterLeaseCandidate)> = eligible
        .iter()
        .copied()
        .filter(|(_, candidate)| candidate.instance_match)
        .collect();

    let selected = match instance_matches.as_slice() {
        [only] => *only,
        [] => select_unique_uri(&eligible)?,
        _ => {
            let exact: Vec<_> = instance_matches
                .iter()
                .copied()
                .filter(|(_, candidate)| candidate.uri_match)
                .collect();
            match exact.as_slice() {
                [only] => *only,
                _ => return Err(RegisterLeaseError::BindingAmbiguous),
            }
        }
    };
    let (_, selected) = selected;
    if selected.invalid_params {
        return Err(RegisterLeaseError::ContactInvalid);
    }
    if let Some(raw) = selected.expires.as_deref() {
        let seconds =
            parse_delta_seconds(raw).map_err(|_| RegisterLeaseError::ContactExpiryInvalid)?;
        if seconds == 0 {
            return Err(RegisterLeaseError::BindingRevoked);
        }
        return Ok(seconds);
    }
    global_expiry(input.global_expires, input.requested_seconds)
}

fn select_unique_uri<'a>(
    candidates: &[(usize, &'a RegisterLeaseCandidate)],
) -> Result<(usize, &'a RegisterLeaseCandidate), RegisterLeaseError> {
    let matches: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|(_, candidate)| candidate.uri_match)
        .collect();
    select_unique(&matches)
}

fn select_unique<'a>(
    candidates: &[(usize, &'a RegisterLeaseCandidate)],
) -> Result<(usize, &'a RegisterLeaseCandidate), RegisterLeaseError> {
    match candidates {
        [only] => Ok(*only),
        [] => Err(RegisterLeaseError::BindingUnconfirmed),
        _ => Err(RegisterLeaseError::BindingAmbiguous),
    }
}

fn global_expiry(values: &[String], requested: i64) -> Result<i64, RegisterLeaseError> {
    let mut granted = None;
    for value in values {
        let seconds =
            parse_delta_seconds(value).map_err(|_| RegisterLeaseError::GlobalExpiryInvalid)?;
        if granted.is_some_and(|previous| previous != seconds) {
            return Err(RegisterLeaseError::GlobalExpiryInvalid);
        }
        granted = Some(seconds);
    }
    let seconds = granted.unwrap_or(requested);
    if seconds == 0 {
        return Err(RegisterLeaseError::BindingRevoked);
    }
    Ok(seconds)
}

fn parse_delta_seconds(raw: &str) -> Result<i64, ()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    trimmed.parse::<u32>().map(i64::from).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        compatible: bool,
        instance_match: bool,
        uri_match: bool,
        expires: Option<&str>,
    ) -> RegisterLeaseCandidate {
        RegisterLeaseCandidate {
            compatible,
            instance_match,
            uri_match,
            invalid_params: false,
            expires: expires.map(str::to_owned),
        }
    }

    fn eval(
        contacts_present: bool,
        candidates: &[RegisterLeaseCandidate],
        global: &[&str],
        requested: i64,
    ) -> Result<i64, RegisterLeaseError> {
        let values: Vec<String> = global.iter().map(|value| (*value).to_owned()).collect();
        evaluate_register_lease(RegisterLeaseInput {
            contacts_present,
            candidates,
            global_expires: &values,
            requested_seconds: requested,
        })
    }

    #[test]
    fn instance_binding_precedes_uri_and_unrelated_contacts() {
        let candidates = [
            candidate(true, false, true, Some("90")),
            candidate(true, true, false, Some("600")),
            candidate(false, true, true, Some("1")),
        ];
        assert_eq!(eval(true, &candidates, &["7200"], 3600), Ok(600));
    }

    #[test]
    fn duplicate_instance_uses_uri_to_disambiguate_else_ambiguous() {
        let candidates = [
            candidate(true, true, false, Some("90")),
            candidate(true, true, true, Some("600")),
        ];
        assert_eq!(eval(true, &candidates, &[], 3600), Ok(600));
        let ambiguous = [candidates[0].clone(), candidates[0].clone()];
        assert_eq!(
            eval(true, &ambiguous, &[], 3600),
            Err(RegisterLeaseError::BindingAmbiguous)
        );
    }

    #[test]
    fn uri_binding_and_missing_binding_are_distinguished() {
        assert_eq!(
            eval(
                true,
                &[candidate(true, false, true, Some("600"))],
                &[],
                3600
            ),
            Ok(600)
        );
        assert_eq!(
            eval(
                true,
                &[candidate(true, false, false, Some("600"))],
                &["1200"],
                3600
            ),
            Err(RegisterLeaseError::BindingUnconfirmed)
        );
    }

    #[test]
    fn contact_expiry_precedes_global_expiry_and_zero_revokes() {
        assert_eq!(
            eval(
                true,
                &[candidate(true, false, true, Some("600"))],
                &["bad"],
                3600
            ),
            Ok(600)
        );
        assert_eq!(
            eval(
                true,
                &[candidate(true, false, true, Some("0"))],
                &["600"],
                3600
            ),
            Err(RegisterLeaseError::BindingRevoked)
        );
    }

    #[test]
    fn global_fallback_requires_consistent_nonzero_values() {
        assert_eq!(eval(false, &[], &[], 3600), Ok(3600));
        assert_eq!(eval(false, &[], &["1200", "1200"], 3600), Ok(1200));
        assert_eq!(
            eval(false, &[], &["1200", "60"], 3600),
            Err(RegisterLeaseError::GlobalExpiryInvalid)
        );
        assert_eq!(
            eval(false, &[], &["0"], 3600),
            Err(RegisterLeaseError::BindingRevoked)
        );
    }

    #[test]
    fn malformed_expiry_and_invalid_selected_binding_fail_closed() {
        for invalid in ["", "600x", "\"600\"", "-1", "4294967296"] {
            assert_eq!(
                eval(
                    true,
                    &[candidate(true, false, true, Some(invalid))],
                    &[],
                    3600
                ),
                Err(RegisterLeaseError::ContactExpiryInvalid),
                "expiry {invalid:?}"
            );
        }
        let mut invalid = candidate(true, false, true, Some("600"));
        invalid.invalid_params = true;
        assert_eq!(
            eval(true, &[invalid], &[], 3600),
            Err(RegisterLeaseError::ContactInvalid)
        );
    }

    #[test]
    fn raw_contact_headers_select_exact_instance_and_ignore_other_leases() {
        let request = [r#"<sip:subscriber@[2001:db8::10]:5063;transport=tcp>;+sip.instance="<urn:uuid:own>";expires=7200"#.to_owned()];
        let response = [r#"<sip:rewritten@registrar.example>;+sip.instance="<urn:uuid:own>";expires=600,<sip:other@ims.example>;+sip.instance="<urn:uuid:other>";expires=10"#.to_owned()];
        assert_eq!(
            evaluate_register_lease_headers(&request, &response, &["7200".into()], 3600),
            Ok(600)
        );
    }

    #[test]
    fn raw_contact_headers_preserve_uri_matching_and_parameter_rules() {
        let request = ["<sip:User@IMS.Example.Test:5063;transport=tcp;user=phone>".into()];
        let response =
            ["<SIP:User@ims.example.test:5063;user=PHONE;TRANSPORT=TCP>;expires=600".into()];
        assert_eq!(
            evaluate_register_lease_headers(&request, &response, &[], 3600),
            Ok(600)
        );

        let other_user =
            ["<sip:user@ims.example.test:5063;transport=tcp;user=phone>;expires=600".into()];
        assert_eq!(
            evaluate_register_lease_headers(&request, &other_user, &[], 3600),
            Err(RegisterLeaseError::BindingUnconfirmed)
        );
    }

    #[test]
    fn raw_contact_headers_preserve_quoted_delimiters_and_duplicate_rejection() {
        let request = [r#"<sip:subscriber@ims.example>;+sip.instance="<urn:uuid:own>""#.into()];
        let valid = [
            r#""Own, \"phone\"" <sip:subscriber@ims.example>;expires=600;display="a;b,c""#.into(),
        ];
        assert_eq!(
            evaluate_register_lease_headers(&request, &valid, &[], 3600),
            Ok(600)
        );

        let duplicate = ["<sip:subscriber@ims.example>;expires=600;Expires=0".into()];
        assert_eq!(
            evaluate_register_lease_headers(&request, &duplicate, &[], 3600),
            Err(RegisterLeaseError::ContactInvalid)
        );
    }

    #[test]
    fn raw_contact_headers_fail_closed_for_malformed_request_and_ambiguous_binding() {
        assert_eq!(
            evaluate_register_lease_headers(&["<sip:bad".into()], &[], &[], 3600),
            Err(RegisterLeaseError::RequestContactInvalid)
        );
        let request = ["<sip:subscriber@ims.example>".into()];
        let ambiguous = [
            "<sip:subscriber@ims.example>;expires=600,<sip:subscriber@ims.example>;expires=900"
                .into(),
        ];
        assert_eq!(
            evaluate_register_lease_headers(&request, &ambiguous, &[], 3600),
            Err(RegisterLeaseError::BindingAmbiguous)
        );
    }

    #[test]
    fn effective_expiry_preserves_override_template_default_precedence() {
        assert_eq!(effective_register_expiry(7200, 600), 7200);
        assert_eq!(effective_register_expiry(0, 600), 600);
        assert_eq!(effective_register_expiry(-1, 0), 3600);
        assert_eq!(effective_register_expiry(0, -1), 3600);
    }
}
