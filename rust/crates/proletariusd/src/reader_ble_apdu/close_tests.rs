use super::*;
use serde_json::{Value, json};
use std::os::unix::net::UnixListener;

pub(crate) fn proof(binding: &ConnectionBinding, mut response: Value) -> Option<CloseProof> {
    let root = std::env::temp_dir();
    let path = root.join(format!(
        "ble-close-{}.sock",
        &crate::multistandby_store::random_token().unwrap()[..8]
    ));
    let listener = UnixListener::bind(&path).unwrap();
    let expected = binding.clone();
    let peer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["action"], "operation-status");
        assert_eq!(request["operation_id"], expected.close_operation_id);
        assert_eq!(request["bridge_id"], expected.bridge_id);
        assert_eq!(request["connection_id"], expected.connection_id);
        assert_eq!(request["token"], "fixture-authenticated-app-token-32");
        let base = json!({"ok":true,"terminal":true,"closed":true,"reader":expected.reader,
            "bridge_id":expected.bridge_id,"connection_id":expected.connection_id,"operation_id":expected.close_operation_id});
        for (k, v) in base.as_object().unwrap() {
            if response.get(k).is_none() {
                response[k] = v.clone();
            }
        }
        writeln!(stream, "{response}").unwrap();
    });
    let result = confirmed_close(&path, "fixture-authenticated-app-token-32", binding).unwrap();
    peer.join().unwrap();
    std::fs::remove_file(path).unwrap();
    result
}
fn binding() -> ConnectionBinding {
    ConnectionBinding {
        reader: "BLE:12345678-1234-1234-1234-123456789abc".into(),
        bridge_id: "bridge-current".into(),
        connection_id: "physical-current".into(),
        close_operation_id: "close-current".into(),
    }
}
#[test]
fn close_receipt_requires_all_exact_identities_and_actual_successful_terminal_close() {
    let binding = binding();
    for wrong in [
        json!({"terminal":false}),
        json!({"ok":false}),
        json!({"closed":false}),
        json!({"reader":"BLE:other"}),
        json!({"bridge_id":"old-bridge"}),
        json!({"connection_id":"old-connection"}),
        json!({"operation_id":"old-operation"}),
    ] {
        assert!(proof(&binding, wrong).is_none());
    }
    assert_eq!(proof(&binding, json!({})).unwrap().binding(), &binding);
}
