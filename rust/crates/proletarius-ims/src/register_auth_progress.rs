use crate::SipError;

pub const AUTHENTICATED_REGISTER_RETRY_LIMIT: u16 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengeProgressDecision {
    pub continue_authentication: bool,
    pub reason: &'static str,
}

/// Owns the retry/stop decision after an authenticated REGISTER response.
/// Request execution, AKA, and response handling remain with the session owner.
pub fn decide_challenge_progress(
    previous_challenge_valid: bool,
    previous_challenge_algorithm: &str,
    response_action: &str,
    completed_attempts: u16,
) -> Result<ChallengeProgressDecision, SipError> {
    decide_challenge_progress_with_policy(
        previous_challenge_valid,
        previous_challenge_algorithm,
        response_action,
        completed_attempts,
        AUTHENTICATED_REGISTER_RETRY_LIMIT,
        true,
    )
}

pub fn decide_challenge_progress_with_policy(
    previous_challenge_valid: bool,
    previous_challenge_algorithm: &str,
    response_action: &str,
    completed_attempts: u16,
    attempt_limit: u16,
    stop_plain_digest: bool,
) -> Result<ChallengeProgressDecision, SipError> {
    if attempt_limit == 0
        || attempt_limit > AUTHENTICATED_REGISTER_RETRY_LIMIT
        || completed_attempts == 0
        || completed_attempts > attempt_limit
    {
        return Err(SipError::InvalidTransition);
    }
    match response_action {
        "accepted" => Ok(ChallengeProgressDecision {
            continue_authentication: false,
            reason: "registration_accepted",
        }),
        "rejected" => Ok(ChallengeProgressDecision {
            continue_authentication: false,
            reason: "registration_rejected",
        }),
        "challenge" => {
            let algorithm = previous_challenge_algorithm.trim();
            let plain_digest = stop_plain_digest
                && previous_challenge_valid
                && (algorithm.is_empty() || algorithm.eq_ignore_ascii_case("MD5"));
            if plain_digest {
                return Ok(ChallengeProgressDecision {
                    continue_authentication: false,
                    reason: "plain_digest_rejected",
                });
            }
            if completed_attempts >= attempt_limit {
                return Ok(ChallengeProgressDecision {
                    continue_authentication: false,
                    reason: "challenge_limit_reached",
                });
            }
            Ok(ChallengeProgressDecision {
                continue_authentication: true,
                reason: "retry_challenge",
            })
        }
        _ => Err(SipError::InvalidTransition),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_plain_digest_stops_without_another_card_attempt() {
        for algorithm in ["MD5", " md5 ", ""] {
            let decision = decide_challenge_progress(true, algorithm, "challenge", 1).unwrap();
            assert!(!decision.continue_authentication);
            assert_eq!(decision.reason, "plain_digest_rejected");
        }
    }

    #[test]
    fn aka_challenges_retry_until_the_sixth_completed_attempt() {
        for attempt in 1..AUTHENTICATED_REGISTER_RETRY_LIMIT {
            let decision =
                decide_challenge_progress(true, "AKAv1-MD5", "challenge", attempt).unwrap();
            assert!(decision.continue_authentication);
            assert_eq!(decision.reason, "retry_challenge");
        }
        let final_attempt = decide_challenge_progress(
            true,
            "AKAv1-MD5",
            "challenge",
            AUTHENTICATED_REGISTER_RETRY_LIMIT,
        )
        .unwrap();
        assert!(!final_attempt.continue_authentication);
        assert_eq!(final_attempt.reason, "challenge_limit_reached");
    }

    #[test]
    fn accepted_and_rejected_responses_always_stop_and_invalid_actions_fail() {
        for (action, reason) in [
            ("accepted", "registration_accepted"),
            ("rejected", "registration_rejected"),
        ] {
            let decision = decide_challenge_progress(false, "", action, 1).unwrap();
            assert!(!decision.continue_authentication);
            assert_eq!(decision.reason, reason);
        }
        assert_eq!(
            decide_challenge_progress(false, "", "other", 1),
            Err(SipError::InvalidTransition)
        );
        assert_eq!(
            decide_challenge_progress(false, "", "challenge", 0),
            Err(SipError::InvalidTransition)
        );
    }

    #[test]
    fn legacy_voiceclient_policy_preserves_two_challenge_rounds_without_plain_digest_shortcut() {
        for algorithm in ["MD5", "", "AKAv1-MD5"] {
            let decision =
                decide_challenge_progress_with_policy(true, algorithm, "challenge", 1, 2, false)
                    .unwrap();
            assert!(decision.continue_authentication);
            assert_eq!(decision.reason, "retry_challenge");
        }
        let exhausted =
            decide_challenge_progress_with_policy(true, "AKAv1-MD5", "challenge", 2, 2, false)
                .unwrap();
        assert!(!exhausted.continue_authentication);
        assert_eq!(exhausted.reason, "challenge_limit_reached");
        assert_eq!(
            decide_challenge_progress_with_policy(true, "MD5", "challenge", 3, 2, false),
            Err(SipError::InvalidTransition)
        );
    }
}
