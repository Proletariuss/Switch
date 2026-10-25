//! REGISTER registrar retry delay policy. Waiting remains with the transport
//! owner; Rust decides the exact delay from status and carrier policy.

pub const DEFAULT_CANDIDATE_GAP_MS: u64 = 150;
const MAX_RETRY_SECONDS: i64 = i64::MAX / 1_000_000_000;
const DEFAULT_TEMPORARY_STATUSES: &[u16] = &[408, 500, 502, 503, 504, 480];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDelayError {
    InvalidStatus,
    InvalidRetrySeconds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateRetryError {
    InvalidStatus,
    InvalidPolicyStatus,
    InvalidFailureKind,
}

const DEFAULT_FORBIDDEN_STATUSES: &[u16] = &[403];
const ADVANCING_FAILURE_KINDS: &[&str] = &[
    "deadline",
    "canceled",
    "partial_sip",
    "timeout_text",
    "transaction_ended",
    "response_timeout",
    "connection_closed",
];

pub fn should_advance_register_candidate(
    status: u16,
    failure_kind: &str,
    has_more: bool,
    forbidden_statuses: Option<&[u16]>,
    temporary_statuses: Option<&[u16]>,
) -> Result<bool, CandidateRetryError> {
    if status > 699 {
        return Err(CandidateRetryError::InvalidStatus);
    }
    if !failure_kind.is_empty() && !ADVANCING_FAILURE_KINDS.contains(&failure_kind) {
        return Err(CandidateRetryError::InvalidFailureKind);
    }
    if forbidden_statuses
        .into_iter()
        .chain(temporary_statuses)
        .flatten()
        .any(|code| *code > 699)
    {
        return Err(CandidateRetryError::InvalidPolicyStatus);
    }
    if !has_more {
        return Ok(false);
    }
    if !failure_kind.is_empty() {
        return Ok(true);
    }
    let forbidden = forbidden_statuses.unwrap_or(DEFAULT_FORBIDDEN_STATUSES);
    let temporary = temporary_statuses.unwrap_or(DEFAULT_TEMPORARY_STATUSES);
    Ok(forbidden.contains(&status) || temporary.contains(&status))
}

pub fn resolve_register_retry_delay_ms(
    status: u16,
    temporary_statuses: Option<&[u16]>,
    temporary_retry_seconds: i64,
) -> Result<u64, RetryDelayError> {
    if status > 699 {
        return Err(RetryDelayError::InvalidStatus);
    }
    if temporary_retry_seconds < 0 || temporary_retry_seconds > MAX_RETRY_SECONDS {
        return Err(RetryDelayError::InvalidRetrySeconds);
    }
    let statuses = temporary_statuses.unwrap_or(DEFAULT_TEMPORARY_STATUSES);
    if statuses.contains(&status) && temporary_retry_seconds > 0 {
        return Ok((temporary_retry_seconds as u64) * 1000);
    }
    Ok(DEFAULT_CANDIDATE_GAP_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_legacy_temporary_status_set() {
        for status in [408, 500, 502, 503, 504, 480] {
            assert_eq!(
                resolve_register_retry_delay_ms(status, None, 12),
                Ok(12_000)
            );
        }
        for status in [403, 401, 429, 0] {
            assert_eq!(resolve_register_retry_delay_ms(status, None, 12), Ok(150));
        }
    }

    #[test]
    fn explicit_empty_and_custom_statuses_are_preserved() {
        assert_eq!(resolve_register_retry_delay_ms(503, Some(&[]), 5), Ok(150));
        assert_eq!(
            resolve_register_retry_delay_ms(429, Some(&[429]), 5),
            Ok(5_000)
        );
        assert_eq!(
            resolve_register_retry_delay_ms(503, Some(&[429]), 5),
            Ok(150)
        );
    }

    #[test]
    fn zero_delay_and_invalid_values_fall_back_or_fail_closed() {
        assert_eq!(resolve_register_retry_delay_ms(503, None, 0), Ok(150));
        assert_eq!(
            resolve_register_retry_delay_ms(700, None, 1),
            Err(RetryDelayError::InvalidStatus)
        );
        assert_eq!(
            resolve_register_retry_delay_ms(503, None, -1),
            Err(RetryDelayError::InvalidRetrySeconds)
        );
        assert_eq!(
            resolve_register_retry_delay_ms(503, None, MAX_RETRY_SECONDS + 1),
            Err(RetryDelayError::InvalidRetrySeconds)
        );
    }

    #[test]
    fn candidate_retry_preserves_default_and_configured_response_policy() {
        assert_eq!(
            should_advance_register_candidate(403, "", true, None, None),
            Ok(true)
        );
        assert_eq!(
            should_advance_register_candidate(503, "", true, None, None),
            Ok(true)
        );
        assert_eq!(
            should_advance_register_candidate(429, "", true, None, Some(&[429])),
            Ok(true)
        );
        assert_eq!(
            should_advance_register_candidate(403, "", true, Some(&[]), Some(&[])),
            Ok(false)
        );
        assert_eq!(
            should_advance_register_candidate(403, "", false, None, None),
            Ok(false)
        );
    }

    #[test]
    fn candidate_retry_advances_only_for_known_probe_failure_classes() {
        for kind in [
            "deadline",
            "canceled",
            "partial_sip",
            "timeout_text",
            "transaction_ended",
            "response_timeout",
            "connection_closed",
        ] {
            assert_eq!(
                should_advance_register_candidate(0, kind, true, None, None),
                Ok(true),
                "{kind}"
            );
        }
        assert_eq!(
            should_advance_register_candidate(0, "other", true, None, None),
            Err(CandidateRetryError::InvalidFailureKind)
        );
        assert_eq!(
            should_advance_register_candidate(0, "deadline", false, None, None),
            Ok(false)
        );
    }
}
