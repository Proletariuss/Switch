//! Carrier policy for initial REGISTER rejection fallback.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitialRegisterVariant {
    pub initial_auth: String,
    pub include_pani: bool,
    pub include_cellular: bool,
}

#[derive(Clone, Debug)]
pub struct InitialRegisterFallbackInput<'a> {
    pub initial_auth: &'a str,
    pub include_pani: bool,
    pub include_cellular: bool,
    pub configured_enabled: bool,
    pub environment_enabled: bool,
    /// None preserves the historical default of 403; Some([]) disables it.
    pub status_codes: Option<&'a [u16]>,
    pub status_code: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitialRegisterFallbackDecision {
    pub variants: Vec<InitialRegisterVariant>,
    pub retry: bool,
}

pub fn evaluate_initial_register_fallback(
    input: InitialRegisterFallbackInput<'_>,
) -> InitialRegisterFallbackDecision {
    let enabled = input.configured_enabled || input.environment_enabled;
    let base = InitialRegisterVariant {
        initial_auth: input.initial_auth.trim().to_owned(),
        include_pani: input.include_pani,
        include_cellular: input.include_cellular,
    };
    let variants = if enabled {
        vec![
            base,
            InitialRegisterVariant {
                initial_auth: "aka_empty_uri_first".to_owned(),
                include_pani: true,
                include_cellular: true,
            },
            InitialRegisterVariant {
                initial_auth: "aka_empty".to_owned(),
                include_pani: true,
                include_cellular: true,
            },
            InitialRegisterVariant {
                initial_auth: "aka_zero_response_uri_first".to_owned(),
                include_pani: true,
                include_cellular: true,
            },
            InitialRegisterVariant {
                initial_auth: "none".to_owned(),
                include_pani: false,
                include_cellular: false,
            },
        ]
    } else {
        vec![base]
    };
    let retry = enabled
        && input
            .status_code
            .is_some_and(|status| match input.status_codes {
                Some(codes) => codes.contains(&status),
                None => status == 403,
            });
    InitialRegisterFallbackDecision { variants, retry }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        codes: Option<&'a [u16]>,
        status: Option<u16>,
    ) -> InitialRegisterFallbackInput<'a> {
        InitialRegisterFallbackInput {
            initial_auth: " eap_direct ",
            include_pani: false,
            include_cellular: false,
            configured_enabled: false,
            environment_enabled: false,
            status_codes: codes,
            status_code: status,
        }
    }

    #[test]
    fn disabled_returns_only_trimmed_base_and_never_retries() {
        let result = evaluate_initial_register_fallback(input(None, Some(403)));
        assert_eq!(result.variants.len(), 1);
        assert_eq!(result.variants[0].initial_auth, "eap_direct");
        assert!(!result.retry);
    }

    #[test]
    fn enabled_uses_exact_fallback_sequence_and_default_403() {
        let mut request = input(None, Some(403));
        request.environment_enabled = true;
        let result = evaluate_initial_register_fallback(request);
        assert_eq!(result.variants.len(), 5);
        assert_eq!(result.variants[1].initial_auth, "aka_empty_uri_first");
        assert_eq!(result.variants[2].initial_auth, "aka_empty");
        assert_eq!(
            result.variants[3].initial_auth,
            "aka_zero_response_uri_first"
        );
        assert_eq!(result.variants[4].initial_auth, "none");
        assert!(result.retry);
    }

    #[test]
    fn configured_status_list_replaces_default_and_empty_stays_empty() {
        let mut request = input(Some(&[407]), Some(403));
        request.configured_enabled = true;
        assert!(!evaluate_initial_register_fallback(request.clone()).retry);
        request.status_code = Some(407);
        assert!(evaluate_initial_register_fallback(request.clone()).retry);
        request.status_codes = Some(&[]);
        assert!(!evaluate_initial_register_fallback(request).retry);
    }
}
