use super::*;
use std::collections::VecDeque;

const CARD: &str = "8988212345678901234";

fn cpms(stores: [&str; 3], used: u32) -> String {
    format!(
        "+CPMS: \"{}\",{used},20,\"{}\",{used},20,\"{}\",{used},20\r\nOK\r\n",
        stores[0], stores[1], stores[2]
    )
}
fn step(command: &str, response: &str) -> (String, Result<String, Failure>) {
    (command.into(), Ok(response.into()))
}
fn source(storage: Storage, index: u32) -> StoredSource {
    StoredSource {
        storage,
        index,
        pdu: "001122".into(),
    }
}
fn group(token: &str, indices: &[u32]) -> DeleteGroup {
    DeleteGroup {
        token: token.into(),
        card_identity: CARD.into(),
        sources: indices
            .iter()
            .map(|&index| source(Storage::ME, index))
            .collect(),
    }
}
fn run(
    operation: StorageOperation,
    steps: Vec<(String, Result<String, Failure>)>,
) -> (Execution, Vec<(Phase, String)>) {
    operation.validate().unwrap();
    let mut steps = VecDeque::from(steps);
    let mut calls = Vec::new();
    let execution = execute(&operation, |phase, command, timeout, limit| {
        assert!(timeout <= Duration::from_secs(20));
        assert_eq!(limit, RESPONSE_LIMIT);
        calls.push((phase, command.to_owned()));
        if command == "AT+QCCID"
            && steps
                .front()
                .is_none_or(|(expected, _)| expected != command)
        {
            return Ok(format!("+QCCID: {CARD}\r\nOK"));
        }
        // The engine deliberately rereads each already preflighted PDU just
        // before CMGD. Individual tests can override that reply explicitly.
        if command.starts_with("AT+CMGR=")
            && steps
                .front()
                .is_some_and(|(expected, _)| expected.starts_with("AT+CMGD="))
        {
            return Ok("+CMGR: 0,,2\r\n001122\r\nOK".into());
        }
        let (expected, result) = steps.pop_front().expect("unexpected physical command");
        assert_eq!(command, expected);
        result
    });
    assert!(steps.is_empty(), "unused physical replies: {}", steps.len());
    (execution, calls)
}

#[test]
fn usage_restores_all_three_original_selections() {
    let initial = cpms(["MT", "SM", "ME"], 3);
    let (execution, calls) = run(
        StorageOperation::Usage,
        vec![
            step("AT+CPMS?", &initial),
            step("AT+CPMS=\"SM\",\"SM\",\"SM\"", "OK"),
            step("AT+CPMS?", &cpms(["SM"; 3], 2)),
            step("AT+CPMS=\"ME\",\"ME\",\"ME\"", "OK"),
            step("AT+CPMS?", &cpms(["ME"; 3], 4)),
            step("AT+CPMS=\"MT\",\"SM\",\"ME\"", "OK"),
            step("AT+CPMS?", &initial),
        ],
    );
    assert!(execution.outcome.complete && execution.outcome.storage_restored);
    assert_eq!(execution.outcome.usage[&Storage::SM].used, 2);
    assert_eq!(execution.outcome.usage[&Storage::ME].used, 4);
    assert!(calls[..5].iter().all(|(phase, _)| *phase == Phase::Active));
    assert!(calls[5..].iter().all(|(phase, _)| *phase == Phase::Cleanup));
}

#[test]
fn failed_selection_never_submits_delete_and_still_restores() {
    let initial = cpms(["ME"; 3], 3);
    let (execution, calls) = run(
        StorageOperation::Clear {
            storages: vec![Storage::SM],
        },
        vec![
            step("AT+CPMS?", &initial),
            step("AT+CPMS=\"SM\",\"SM\",\"SM\"", "ERROR"),
            step("AT+CPMS=\"ME\",\"ME\",\"ME\"", "OK"),
            step("AT+CPMS?", &initial),
        ],
    );
    assert!(!execution.outcome.complete);
    assert!(execution.outcome.storage_restored && !execution.outcome.uncertain);
    assert_eq!(
        execution.outcome.error_code,
        "usb_sms_storage_select_failed"
    );
    assert!(
        !calls
            .iter()
            .any(|(_, command)| command.starts_with("AT+CMGD"))
    );
}

#[test]
fn apparent_select_ok_requires_matching_readback_before_mutation() {
    let initial = cpms(["ME"; 3], 3);
    let (execution, calls) = run(
        StorageOperation::Clear {
            storages: vec![Storage::SM],
        },
        vec![
            step("AT+CPMS?", &initial),
            step("AT+CPMS=\"SM\",\"SM\",\"SM\"", "OK"),
            step("AT+CPMS?", &initial),
            step("AT+CPMS=\"ME\",\"ME\",\"ME\"", "OK"),
            step("AT+CPMS?", &initial),
        ],
    );
    assert_eq!(
        execution.outcome.error_code,
        "usb_sms_storage_selection_mismatch"
    );
    assert!(
        !calls
            .iter()
            .any(|(_, command)| command.starts_with("AT+CMGD"))
    );
}

#[test]
fn changed_or_truncated_message_refuses_entire_delete_batch() {
    for readback in [
        "+CMGR: 0,,2\r\n001133\r\nOK",
        "+CMGR: 0,,4\r\n001122\r\nOK",
        "+CMGR: 0,,2\r\n001122\r\n+CMGR: 0,,2\r\n001122\r\nOK",
    ] {
        let (execution, calls) = run(
            StorageOperation::Delete {
                groups: vec![group("first", &[1]), group("second", &[2])],
            },
            vec![
                step("AT+CPMS?", &cpms(["ME"; 3], 2)),
                step("AT+CMGF=0", "OK"),
                step("AT+CMGR=1", "+CMGR: 0,,2\r\n001122\r\nOK"),
                step("AT+CMGR=2", readback),
            ],
        );
        assert_eq!(
            execution.outcome.error_code,
            "usb_sms_storage_message_changed"
        );
        assert!(execution.outcome.deleted_tokens.is_empty());
        assert!(
            !calls
                .iter()
                .any(|(_, command)| command.starts_with("AT+CMGD"))
        );
    }
}

#[test]
fn multipart_and_shared_sources_are_verified_then_deleted_once_in_reverse_order() {
    let (execution, calls) = run(
        StorageOperation::Delete {
            groups: vec![group("multipart", &[2, 8]), group("same-source", &[8])],
        },
        vec![
            step("AT+CPMS?", &cpms(["ME"; 3], 2)),
            step("AT+CMGF=0", "OK"),
            step("AT+CMGR=2", "+CMGR: 0,,2\r\n001122FFFF\r\nOK"),
            step("AT+CMGR=8", "+CMGR: 0,,2\r\n001122\r\nOK"),
            step("AT+CMGD=8", "OK"),
            step("AT+CMGD=2", "OK"),
        ],
    );
    assert!(execution.outcome.complete);
    assert_eq!(
        execution.outcome.deleted_tokens,
        vec!["same-source", "multipart"]
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(_, command)| command.starts_with("AT+CMGD"))
            .count(),
        2
    );
}

#[test]
fn partial_deletion_confirms_only_whole_groups_and_never_replays_unknown() {
    let mut failure = Failure::new(
        StatusCode::BAD_GATEWAY,
        "usb_at_timeout",
        "USB AT transaction timed out",
    );
    failure.uncertain = true;
    let (execution, calls) = run(
        StorageOperation::Delete {
            groups: vec![group("whole", &[9]), group("partial", &[2, 8])],
        },
        vec![
            step("AT+CPMS?", &cpms(["ME"; 3], 3)),
            step("AT+CMGF=0", "OK"),
            step("AT+CMGR=2", "+CMGR: 0,,2\r\n001122\r\nOK"),
            step("AT+CMGR=8", "+CMGR: 0,,2\r\n001122\r\nOK"),
            step("AT+CMGR=9", "+CMGR: 0,,2\r\n001122\r\nOK"),
            step("AT+CMGD=9", "OK"),
            step("AT+CMGD=8", "OK"),
            ("AT+CMGD=2".into(), Err(failure)),
        ],
    );
    assert!(!execution.outcome.complete && execution.outcome.uncertain);
    assert_eq!(execution.outcome.deleted_tokens, vec!["whole"]);
    assert_eq!(
        calls
            .iter()
            .filter(|(_, command)| command == "AT+CMGD=2")
            .count(),
        1
    );
}

#[test]
fn incomplete_delete_response_is_unknown_and_stream_is_retired() {
    let (execution, calls) = run(
        StorageOperation::Clear {
            storages: vec![Storage::SM],
        },
        vec![
            step("AT+CPMS?", &cpms(["ME"; 3], 3)),
            step("AT+CPMS=\"SM\",\"SM\",\"SM\"", "OK"),
            step("AT+CPMS?", &cpms(["SM"; 3], 2)),
            step("AT+CMGD=1,4", "\r\n"),
        ],
    );
    assert!(execution.outcome.uncertain && execution.retire_transport);
    assert!(!execution.outcome.complete);
    assert!(!execution.outcome.storage_restored);
    assert!(!execution.outcome.details[&Storage::SM].delete_confirmed);
    assert_eq!(
        calls
            .iter()
            .filter(|(_, command)| command.starts_with("AT+CMGD"))
            .count(),
        1
    );
}

#[test]
fn clear_preserves_confirmed_side_effect_when_second_store_rejects() {
    let initial = cpms(["SM"; 3], 3);
    let (execution, _) = run(
        StorageOperation::Clear {
            storages: vec![Storage::SM, Storage::ME],
        },
        vec![
            step("AT+CPMS?", &initial),
            step("AT+CMGD=1,4", "OK"),
            step("AT+CPMS?", &cpms(["SM"; 3], 0)),
            step("AT+CPMS=\"ME\",\"ME\",\"ME\"", "OK"),
            step("AT+CPMS?", &cpms(["ME"; 3], 2)),
            step("AT+CMGD=1,4", "+CMS ERROR: 500"),
            step("AT+CPMS=\"SM\",\"SM\",\"SM\"", "OK"),
            step("AT+CPMS?", &cpms(["SM"; 3], 0)),
        ],
    );
    assert!(!execution.outcome.complete && !execution.outcome.uncertain);
    assert!(execution.outcome.storage_restored);
    assert!(execution.outcome.details[&Storage::SM].delete_confirmed);
    assert_eq!(execution.outcome.details[&Storage::SM].after, Some(0));
    assert!(!execution.outcome.details[&Storage::ME].delete_confirmed);
}

#[test]
fn arrivals_after_bulk_clear_are_reported_and_not_deleted_again() {
    let (execution, calls) = run(
        StorageOperation::Clear {
            storages: vec![Storage::ME],
        },
        vec![
            step("AT+CPMS?", &cpms(["ME"; 3], 3)),
            step("AT+CMGD=1,4", "OK"),
            step("AT+CPMS?", &cpms(["ME"; 3], 1)),
        ],
    );
    assert!(!execution.outcome.complete && !execution.outcome.uncertain);
    assert_eq!(execution.outcome.error_code, "usb_sms_storage_not_empty");
    assert_eq!(execution.outcome.details[&Storage::ME].before, 3);
    assert_eq!(execution.outcome.details[&Storage::ME].after, Some(1));
    assert_eq!(
        calls
            .iter()
            .filter(|(_, command)| command.starts_with("AT+CMGD"))
            .count(),
        1
    );
}

#[test]
fn restore_failure_keeps_confirmed_delete_but_prevents_success() {
    let (execution, _) = run(
        StorageOperation::Clear {
            storages: vec![Storage::SM],
        },
        vec![
            step("AT+CPMS?", &cpms(["ME"; 3], 3)),
            step("AT+CPMS=\"SM\",\"SM\",\"SM\"", "OK"),
            step("AT+CPMS?", &cpms(["SM"; 3], 2)),
            step("AT+CMGD=1,4", "OK"),
            step("AT+CPMS?", &cpms(["SM"; 3], 0)),
            step("AT+CPMS=\"ME\",\"ME\",\"ME\"", "ERROR"),
        ],
    );
    assert!(execution.outcome.details[&Storage::SM].delete_confirmed);
    assert!(execution.retire_transport);
    assert!(!execution.outcome.complete && !execution.outcome.storage_restored);
    assert_eq!(
        execution.outcome.error_code,
        "usb_sms_storage_restore_unconfirmed"
    );
}

#[test]
fn invalid_capacity_and_unknown_original_storage_are_not_zero_success() {
    for response in [
        "+CPMS: \"ME\",21,20,\"ME\",0,20,\"ME\",0,20\r\nOK",
        "+CPMS: 0,20,0,20,0,20\r\nOK",
        "+CPMS: \"EVIL\",0,20,\"ME\",0,20,\"ME\",0,20\r\nOK",
    ] {
        let (execution, calls) = run(StorageOperation::Usage, vec![step("AT+CPMS?", response)]);
        assert!(!execution.outcome.complete);
        assert!(execution.outcome.usage.is_empty());
        assert_eq!(calls.len(), 1);
    }
}

#[test]
fn validation_rejects_ambiguous_sources_and_never_accepts_command_fragments() {
    assert!(serde_json::from_str::<Storage>("\"SM;AT+CMGD=1,4\"").is_err());
    assert!(
        StorageOperation::Clear {
            storages: vec![Storage::ME, Storage::ME]
        }
        .validate()
        .is_err()
    );
    let mut invalid = group("target", &[1]);
    invalid.sources[0].index = 0;
    assert!(
        StorageOperation::Delete {
            groups: vec![invalid]
        }
        .validate()
        .is_err()
    );
    let mut changed = group("second", &[1]);
    changed.sources[0].pdu = "001133".into();
    assert!(
        StorageOperation::Delete {
            groups: vec![group("first", &[1]), changed]
        }
        .validate()
        .is_err()
    );
    let mut truncated = group("target", &[1]);
    truncated.sources[0].pdu = "04AA".into();
    assert!(
        StorageOperation::Delete {
            groups: vec![truncated]
        }
        .validate()
        .is_err()
    );
}

#[test]
fn slot_is_rechecked_immediately_before_each_delete() {
    let (execution, calls) = run(
        StorageOperation::Delete {
            groups: vec![group("target", &[1])],
        },
        vec![
            step("AT+CPMS?", &cpms(["ME"; 3], 1)),
            step("AT+CMGF=0", "OK"),
            step("AT+CMGR=1", "+CMGR: 0,,2\r\n001122\r\nOK"),
            step("AT+CMGR=1", "+CMGR: 0,,2\r\n001133\r\nOK"),
        ],
    );
    assert_eq!(
        execution.outcome.error_code,
        "usb_sms_storage_message_changed"
    );
    assert!(
        !calls
            .iter()
            .any(|(_, command)| command.starts_with("AT+CMGD"))
    );
}

#[test]
fn card_change_before_delete_cannot_authorize_matching_pdu_on_another_sim() {
    let (execution, calls) = run(
        StorageOperation::Delete {
            groups: vec![group("target", &[1])],
        },
        vec![
            step("AT+CPMS?", &cpms(["ME"; 3], 1)),
            step("AT+CMGF=0", "OK"),
            step("AT+CMGR=1", "+CMGR: 0,,2\r\n001122\r\nOK"),
            step("AT+QCCID", "+QCCID: 8988212345678901235\r\nOK"),
        ],
    );
    assert_eq!(execution.outcome.error_code, "usb_sms_storage_card_changed");
    assert!(!execution.outcome.uncertain && !execution.outcome.storage_restored);
    assert!(execution.retire_transport);
    assert!(
        !calls
            .iter()
            .any(|(_, command)| command.starts_with("AT+CMGD"))
    );
}

#[test]
fn card_unconfirmed_after_successful_delete_remains_unknown_without_group_confirmation() {
    for response in ["+QCCID: 8988212345678901235\r\nOK", "ERROR"] {
        let (execution, calls) = run(
            StorageOperation::Delete {
                groups: vec![group("target", &[1])],
            },
            vec![
                step("AT+CPMS?", &cpms(["ME"; 3], 1)),
                step("AT+CMGF=0", "OK"),
                step("AT+CMGR=1", "+CMGR: 0,,2\r\n001122\r\nOK"),
                step("AT+CMGD=1", "OK"),
                step("AT+QCCID", response),
            ],
        );
        assert!(!execution.outcome.complete && execution.outcome.uncertain);
        assert!(execution.outcome.deleted_tokens.is_empty());
        assert_eq!(
            calls
                .iter()
                .filter(|(_, command)| command.starts_with("AT+CMGD"))
                .count(),
            1
        );
    }
}

#[test]
fn logical_record_spanning_both_storages_is_confirmed_only_after_both_sources() {
    let original = cpms(["MT", "SM", "ME"], 2);
    let operation = StorageOperation::Delete {
        groups: vec![DeleteGroup {
            token: "both".into(),
            card_identity: CARD.into(),
            sources: vec![source(Storage::SM, 2), source(Storage::ME, 9)],
        }],
    };
    let (execution, calls) = run(
        operation,
        vec![
            step("AT+CPMS?", &original),
            step("AT+CMGF=0", "OK"),
            step("AT+CPMS=\"SM\",\"SM\",\"SM\"", "OK"),
            step("AT+CPMS?", &cpms(["SM"; 3], 1)),
            step("AT+CMGR=2", "+CMGR: 0,,2\r\n001122\r\nOK"),
            step("AT+CPMS=\"ME\",\"ME\",\"ME\"", "OK"),
            step("AT+CPMS?", &cpms(["ME"; 3], 1)),
            step("AT+CMGR=9", "+CMGR: 0,,2\r\n001122\r\nOK"),
            step("AT+CPMS=\"SM\",\"SM\",\"SM\"", "OK"),
            step("AT+CPMS?", &cpms(["SM"; 3], 1)),
            step("AT+CMGD=2", "OK"),
            step("AT+CPMS=\"ME\",\"ME\",\"ME\"", "OK"),
            step("AT+CPMS?", &cpms(["ME"; 3], 1)),
            step("AT+CMGD=9", "OK"),
            step("AT+CPMS=\"MT\",\"SM\",\"ME\"", "OK"),
            step("AT+CPMS?", &original),
        ],
    );
    assert!(execution.outcome.complete && execution.outcome.storage_restored);
    assert_eq!(execution.outcome.deleted_tokens, vec!["both"]);
    assert_eq!(
        calls
            .iter()
            .filter(|(_, command)| command.starts_with("AT+CMGR"))
            .count(),
        4
    );
}

#[test]
fn active_deadline_failure_still_uses_independent_cleanup_budget() {
    let original = cpms(["ME"; 3], 2);
    let timeout = Failure::new(
        StatusCode::REQUEST_TIMEOUT,
        "usb_at_wait_timeout",
        "USB AT admission deadline expired",
    );
    let (execution, calls) = run(
        StorageOperation::Usage,
        vec![
            step("AT+CPMS?", &original),
            step("AT+CPMS=\"SM\",\"SM\",\"SM\"", "OK"),
            ("AT+CPMS?".into(), Err(timeout)),
            step("AT+CPMS=\"ME\",\"ME\",\"ME\"", "OK"),
            step("AT+CPMS?", &original),
        ],
    );
    assert!(!execution.outcome.complete && execution.outcome.storage_restored);
    assert!(calls[3..].iter().all(|(phase, _)| *phase == Phase::Cleanup));
}
