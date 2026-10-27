// Included in the existing FakeIo test module: production NativeTransport
// owns framing and terminal recognition before the shared CLCC parser runs.
const DATA_ONLY_CLCC: &str = "AT+CLCC\r\n+CLCC: 1,0,0,1,0,\"\",128\r\nOK\r\n";

fn fragmented_clcc(response: &str) -> crate::module_radio::CallStateSnapshot {
    let mut actions = vec![quiet(), write(b"AT+CLCC\r")];
    actions.extend(response.as_bytes().chunks(3).map(read));
    let (mut transport, trace) = fake(actions);
    let reply = transport
        .command("AT+CLCC", None, Duration::from_secs(3), 4096)
        .unwrap();
    let observed = crate::module_radio::call_snapshot(reply);
    let trace = trace.lock().unwrap();
    assert!(trace.actions.is_empty(), "all response fragments were consumed");
    assert_eq!(trace.writes, vec![b"AT+CLCC\r".to_vec()]);
    observed
}

#[test]
fn fragmented_standard_packet_data_clcc_is_voice_idle_after_native_transport() {
    let observed = fragmented_clcc(DATA_ONLY_CLCC);
    assert!(observed.safe_idle);
    assert_eq!(observed.display["state"], "idle");
    assert!(!observed.display["number"].as_str().is_some_and(|number| !number.is_empty()));
}

#[test]
fn fragmented_voice_mixed_and_unknown_clcc_keep_native_admission_closed() {
    for (response, expected) in [
        ("+CLCC: 1,0,0,0,0,\"\",128\r\nOK\r\n", "active"),
        ("+CLCC: 1,0,1,0,0,\"\",128\r\nOK\r\n", "active"),
        ("+CLCC: 1,1,4,0,0,\"\",128\r\nOK\r\n", "incoming"),
        (
            "+CLCC: 1,0,0,1,0,\"\",128\r\n+CLCC: 2,1,4,0,0,\"\",128\r\nOK\r\n",
            "incoming",
        ),
        ("+CLCC: 1,0,0,9,0,\"\",128\r\nOK\r\n", "unknown"),
        ("+CLCC: 1,0,0,1,0,\"\",128\r\nRING\r\nOK\r\n", "unknown"),
    ] {
        let observed = fragmented_clcc(response);
        assert!(!observed.safe_idle, "{response:?}");
        assert_eq!(observed.display["state"], expected, "{response:?}");
        assert!(!observed.display["number"].as_str().is_some_and(|number| number.contains(",128")));
    }
}

#[test]
fn incomplete_packet_data_reply_cannot_become_idle_after_native_deadline() {
    let response = "AT+CLCC\r\n+CLCC: 1,0,0,1,0,\"\",128\r\n";
    let mut actions = vec![quiet(), write(b"AT+CLCC\r")];
    actions.extend(response.as_bytes().chunks(3).map(read));
    let (mut transport, trace) = fake(actions);
    // Ordinary command compatibility returns partial text at the deadline.
    // The production CLCC classifier must still require its complete OK.
    let partial = transport
        .command("AT+CLCC", None, Duration::from_secs(3), 4096)
        .unwrap();
    assert!(!partial.ends_with("OK"));
    assert!(!crate::module_radio::call_snapshot(partial).safe_idle);
    assert_eq!(trace.lock().unwrap().writes, vec![b"AT+CLCC\r".to_vec()]);
}
