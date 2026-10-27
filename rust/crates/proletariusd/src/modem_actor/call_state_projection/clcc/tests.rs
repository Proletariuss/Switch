use super::*;
use crate::module_radio::call_snapshot;

fn snapshot(response: &str) -> crate::module_radio::CallStateSnapshot {
    call_snapshot(response.to_owned())
}

#[test]
fn official_lte_ps_rows_are_voice_idle_and_never_a_phone_number() {
    for row in [
        "+CLCC: 1,0,0,1,0,\"\",128",
        "+CLCC: 1,1,0,1,0,\"\",128",
        "+CLCC: 1,0,0,1,0",
        "+CLCC: 1,0,0,1,0,\"data\",129,\"packet context\"",
    ] {
        let observed = snapshot(&format!("AT+CLCC\r\n{row}\r\nOK"));
        assert!(observed.safe_idle, "{row}");
        assert_eq!(observed.display["state"], "idle");
        assert_eq!(observed.display["active"], false);
        assert!(observed.display.get("number").is_none());
    }
    let data = "+CLCC: 1,0,0,1,0,\"\",128";
    let observed = snapshot(&format!("{data}\r\n+CLCC: 2,1,0,1,0,\"\",128\r\nOK"));
    assert!(observed.safe_idle);
    assert!(observed.display.get("number").is_none());
}

#[test]
fn numberless_active_and_held_voice_rows_keep_the_call_protected() {
    for status in [0, 1] {
        for suffix in ["", ",\"\"", ",\"\",128", ",\"\",129,\"withheld\""] {
            let observed = snapshot(&format!("+CLCC: 1,0,{status},0,0{suffix}\r\nOK"));
            assert!(!observed.safe_idle);
            assert_eq!(observed.display["state"], "active");
            assert_eq!(observed.display["active"], true);
            assert!(observed.display.get("number").is_none());
        }
    }
}

#[test]
fn full_standard_fields_and_quoted_alpha_do_not_leak_into_number() {
    for suffix in [
        "\"123\",129",
        "\"+123\",145",
        "\"123\",129,\"Caller, alias\"",
        "\"123\",129,\"Caller \"\"alias\"\"\"",
        "123",
    ] {
        let observed = snapshot(&format!("+CLCC: 1,1,4,0,0,{suffix}\r\nOK"));
        assert!(!observed.safe_idle);
        assert_eq!(observed.display["state"], "incoming");
        assert_eq!(observed.display["incoming"], true);
        assert_eq!(
            observed.display["number"],
            if suffix.starts_with("\"+") {
                "+123"
            } else {
                "123"
            }
        );
    }
}

#[test]
fn data_rows_before_or_after_real_voice_never_override_voice_state_or_number() {
    let data = "+CLCC: 1,0,0,1,0,\"\",128";
    for (status, expected) in [
        (0, "active"),
        (1, "active"),
        (2, "dialing"),
        (3, "alerting"),
        (4, "incoming"),
        (5, "incoming"),
    ] {
        let voice = format!("+CLCC: 2,1,{status},0,0,\"123\",129");
        for lines in [format!("{data}\r\n{voice}"), format!("{voice}\r\n{data}")] {
            let observed = snapshot(&format!("{lines}\r\nOK"));
            assert!(!observed.safe_idle);
            assert_eq!(observed.display["state"], expected);
            assert_eq!(observed.display["number"], "123");
        }
    }
    let observed = snapshot(
        "+CLCC: 1,1,4,0,0,\"123\",129\r\n+CLCC: 2,0,0,0,0,\"456\",129\r\n+CLCC: 3,0,0,1,0,\"\",128\r\nOK",
    );
    assert_eq!(observed.display["state"], "active");
    assert_eq!(observed.display["number"], "456");
    assert_eq!(observed.display["incoming"], true);
    assert_eq!(observed.display["active"], true);
}

#[test]
fn only_exact_data_mode_is_excluded_from_voice_idle_proof() {
    for mode in [2, 3, 4, 9, 255, 65536] {
        let observed = snapshot(&format!("+CLCC: 1,0,0,{mode},0,\"\",128\r\nOK"));
        assert!(!observed.safe_idle);
        assert_eq!(observed.display["state"], "unknown");
    }
    // A disconnected voice row is not silently generalized into data-idle.
    let observed = snapshot("+CLCC: 1,0,6,0,0,\"123\",129\r\nOK");
    assert!(!observed.safe_idle);
}

#[test]
fn malformed_partial_and_legacy_data_rows_cannot_prove_idle() {
    for response in [
        "",
        "ERROR\r\nOK",
        "+CME ERROR: 10\r\nOK",
        "OK\r\nOK",
        "+CLCC: invalid\r\nOK",
        "+CLCC invalid\r\nOK",
        "+CLCC: 1,0,0,1\r\nOK",
        "+CLCC: 0,0,0,1,0,\"\",128\r\nOK",
        "+CLCC: 1,2,0,1,0,\"\",128\r\nOK",
        "+CLCC: 1,0,9,1,0,\"\",128\r\nOK",
        "+CLCC: 1,0,0,1,2,\"\",128\r\nOK",
        "+CLCC: \"1\",0,0,1,0,\"\",128\r\nOK",
        "+CLCC: 1,0,0,1,0,\"\"\r\nOK",
        "+CLCC: 1,0,0,1,0,\"\",bogus\r\nOK",
        "+CLCC: 1,0,0,1,0,\"\",256\r\nOK",
        "+CLCC: 1,0,0,1,0,\"\",128junk\r\nOK",
        "+CLCC: 1,0,0,1,0,\"unterminated,128\r\nOK",
        "+CLCC: 1,0,0,1,0,\"\"garbage,128\r\nOK",
        "+CLCC: 1,0,0,1,0,\"\",128,\"unterminated\r\nOK",
        "+CLCC: 1,0,0,1,0,\"\",128,\"alpha\",unexpected\r\nOK",
        "+CLCC: 1,0,0,1,0,\"\",128",
        "+CLCC: 1,0,0,1,0,\"\",128\r\nO",
        "OK\r\n+CLCC: 1,0,0,1,0,\"\",128",
    ] {
        let observed = snapshot(response);
        assert!(!observed.safe_idle, "{response}");
        assert_eq!(observed.display["state"], "unknown", "{response}");
    }
    // Keep display compatibility for the old number-without-type voice rows,
    // but their missing terminal is never idle evidence.
    let observed = snapshot("+CLCC: 1,0,0,0,0,\"123\"");
    assert!(!observed.safe_idle);
    assert_eq!(observed.display["state"], "active");
    assert_eq!(observed.display["number"], "123");
}

#[test]
fn call_urcs_and_unrecognized_tail_cannot_clear_existing_voice() {
    let data = "+CLCC: 1,0,0,1,0,\"\",128";
    for urc in [
        "RING",
        "+CLIP: \"123\",129",
        "CONNECT",
        "CONNECT 9600",
        "MO CONNECTED",
        "^DSCI: 1,0,3,0,\"123\",129,0",
        "unrecognized",
    ] {
        for text in [
            format!("{data}\r\n{urc}\r\nOK"),
            format!("{data}\r\nOK\r\n{urc}"),
        ] {
            assert!(!snapshot(&text).safe_idle, "{text}");
        }
    }
    assert!(snapshot("AT+CLCC\r\nOK").safe_idle);
    assert!(
        snapshot("OK").safe_idle,
        "the transport returns normalized terminal text"
    );
}

#[test]
fn parser_row_and_field_limits_never_turn_truncation_into_idle() {
    let data = "+CLCC: 1,0,0,1,0,\"\",128\r\n";
    assert!(snapshot(&(data.repeat(MAX_ROWS) + "OK")).safe_idle);
    assert!(!snapshot(&(data.repeat(MAX_ROWS + 1) + "OK")).safe_idle);
    assert!(!snapshot(&(" ".repeat(MAX_BYTES + 1) + "OK")).safe_idle);
    assert!(
        !snapshot(&format!(
            "+CLCC: 1,0,0,1,0,\"{}\",128\r\nOK",
            "x".repeat(MAX_FIELD_BYTES + 1)
        ))
        .safe_idle
    );
}
