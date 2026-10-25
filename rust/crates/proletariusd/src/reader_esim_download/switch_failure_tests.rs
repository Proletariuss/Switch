use super::*;

const PRIVATE_ICCID: &str = "8985200014631959072";
const PRIVATE_TRANSPORT: &str = "raw transport failure with ICCID 8985200014631959072";

#[test]
fn switch_failure_preserves_structured_enable_result_matrix_and_safe_names() {
    use ProfileOperationResult::*;
    for (result, suffix, status, recoverable, name) in [
        (
            Undefined,
            "card_rejected",
            StatusCode::BAD_GATEWAY,
            true,
            "undefinedError",
        ),
        (CatBusy, "card_busy", StatusCode::CONFLICT, true, "catBusy"),
        (
            IccidOrAidNotFound,
            "target_missing",
            StatusCode::NOT_FOUND,
            false,
            "iccidOrAidNotFound",
        ),
        (
            ProfileInWrongState,
            "state_unconfirmed",
            StatusCode::BAD_GATEWAY,
            true,
            "profileNotInDisabledState",
        ),
        (
            DisallowedByPolicy,
            "policy_denied",
            StatusCode::BAD_GATEWAY,
            false,
            "disallowedByPolicy",
        ),
        (
            WrongProfileReenabling,
            "rejected",
            StatusCode::BAD_GATEWAY,
            true,
            "wrongProfileReenabling",
        ),
        (
            Unknown(6),
            "rejected",
            StatusCode::BAD_GATEWAY,
            true,
            "unknown(6)",
        ),
        (
            DisallowedByEnterpriseRule,
            "policy_denied",
            StatusCode::BAD_GATEWAY,
            false,
            "disallowedByEnterpriseRule",
        ),
        (
            DisallowedForRpm,
            "policy_denied",
            StatusCode::BAD_GATEWAY,
            false,
            "disallowedForRpm",
        ),
        (
            NoEsimPortAvailable,
            "policy_denied",
            StatusCode::BAD_GATEWAY,
            false,
            "noEsimPortAvailable",
        ),
    ] {
        let error = classify_switch_apdu_error(EuiccError::<String>::ProfileOperation {
            operation: ProfileOperation::Enable,
            result,
        });
        let (actual_status, code, message, actual_recoverable) = switch_failure_response(&error);
        assert_eq!(actual_status, status);
        assert_eq!(code, format!("profile_switch_{suffix}"));
        assert_eq!(actual_recoverable, recoverable);
        assert!(message.contains(name), "{message}");
        assert!(message.contains(&format!("结果码 {}", result.code())));
        assert!(error.diagnostic().contains(name));
        assert!(!message.contains(PRIVATE_ICCID));
        assert!(!message.contains(PRIVATE_TRANSPORT));
    }
}

#[test]
fn switch_failure_does_not_publish_transport_text_or_classify_other_operations_as_enable() {
    for error in [
        EuiccError::Transport(PRIVATE_TRANSPORT.to_owned()),
        EuiccError::ProfileOperation {
            operation: ProfileOperation::Disable,
            result: ProfileOperationResult::ProfileInWrongState,
        },
        EuiccError::ProfileOperation {
            operation: ProfileOperation::Delete,
            result: ProfileOperationResult::Undefined,
        },
    ] {
        let error = classify_switch_apdu_error(error);
        let (status, code, message, recoverable) = switch_failure_response(&error);
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(code, "profile_switch_failed");
        assert!(recoverable);
        assert!(!message.contains(PRIVATE_ICCID));
        assert!(!message.contains("raw transport"));
        assert!(!message.contains("enableProfile"));
    }
}

struct RecordingSwitchCard {
    response: Result<Vec<u8>, String>,
    commands: Vec<Vec<u8>>,
}

impl CardTransport for RecordingSwitchCard {
    type Error = String;

    fn transmit(&mut self, command: &[u8], response: &mut Vec<u8>) -> Result<(), String> {
        self.commands.push(command.to_vec());
        *response = self.response.clone()?;
        Ok(())
    }
}

#[test]
fn reader_switch_real_apdu_result_two_is_unconfirmed_refresh_false_and_never_replayed() {
    let encoded = encode_iccid(PRIVATE_ICCID).unwrap();
    // Full command: channel 1 STORE DATA, one BF31 EnableProfile request,
    // ICCID selector and refreshFlag=false. No follow-up command is allowed.
    let mut expected = vec![0x81, 0xe2, 0x91, 0, 20, 0xbf, 0x31, 17, 0xa0, 12, 0x5a, 10];
    expected.extend_from_slice(&encoded);
    expected.extend_from_slice(&[0x81, 1, 0]);
    for (result, expected_code) in [
        (0, None),
        (1, Some("profile_switch_target_missing")),
        (2, Some("profile_switch_state_unconfirmed")),
        (3, Some("profile_switch_policy_denied")),
        (4, Some("profile_switch_rejected")),
        (6, Some("profile_switch_policy_denied")),
        (127, Some("profile_switch_card_rejected")),
        (42, Some("profile_switch_rejected")),
    ] {
        let mut card = Euicc::new(
            RecordingSwitchCard {
                response: Ok(vec![0xbf, 0x31, 3, 0x80, 1, result, 0x90, 0]),
                commands: Vec::new(),
            },
            1,
            READER_MSS,
        )
        .unwrap();
        let outcome = enable_profile_with_retry_and_wait(
            &mut card,
            ProfileIdentifier::Iccid(&encoded),
            &AtomicBool::new(false),
            || panic!("non-CAT_BUSY result must never schedule a replay"),
        );
        match expected_code {
            None => outcome.unwrap(),
            Some(code) => assert_eq!(switch_failure_response(&outcome.unwrap_err()).1, code),
        }
        assert_eq!(card.into_inner().commands, vec![expected.clone()]);
    }
}

#[test]
fn reader_switch_transport_failure_never_replays_or_leaks_identity() {
    let mut card = Euicc::new(
        RecordingSwitchCard {
            response: Err(PRIVATE_TRANSPORT.to_owned()),
            commands: Vec::new(),
        },
        1,
        READER_MSS,
    )
    .unwrap();
    let encoded = encode_iccid(PRIVATE_ICCID).unwrap();
    let error = enable_profile_with_retry_and_wait(
        &mut card,
        ProfileIdentifier::Iccid(&encoded),
        &AtomicBool::new(false),
        || panic!("transport failure must never schedule a replay"),
    )
    .unwrap_err();
    let (_, code, message, _) = switch_failure_response(&error);
    assert_eq!(code, "profile_switch_failed");
    assert!(!message.contains(PRIVATE_ICCID));
    assert!(!message.contains("raw transport"));
    assert_eq!(card.into_inner().commands.len(), 1);
}
