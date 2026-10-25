use super::*;
fn service() -> Service {
    Service::new(
        Owner::new("test-producer".into()),
        Arc::new(Gate::default()),
        None,
        None,
    )
}
fn submit_value() -> Value {
    json!({"operation_id":"cancel-before-submit","action":"prepare","reader":"BLE:12345678-1234-1234-1234-123456789abc","reader_generation":7,"transaction":{"kind":"operation","binding":{"producer_id":"test-producer","operation_token":"test-operation","operation_sequence":1,"reader":"BLE:12345678-1234-1234-1234-123456789abc","reader_generation":7,"purpose":"card","authority":{"kind":"idle"}}},"eid":"89049032000000000000000000000001","iccid":"8901000000000000001"})
}
#[tokio::test]
async fn unknown_cancel_fences_delayed_submit_and_never_invokes_physical_io() {
    let service = service();
    let submit = submit_value();
    let query = json!({"operation_id":submit["operation_id"],"transaction":submit["transaction"]});
    assert_eq!(
        service
            .handle("status", query.clone())
            .await
            .unwrap_err()
            .code,
        "multistandby_operation_unknown"
    );
    let cancelled = service.handle("cancel", query.clone()).await.unwrap();
    assert_eq!(cancelled["state"], "completed");
    assert_eq!(cancelled["worker_finished"], true);
    assert_eq!(
        service.handle("submit", submit.clone()).await.unwrap(),
        cancelled
    );
    assert_eq!(
        service.handle("status", query.clone()).await.unwrap(),
        cancelled
    );
    let mut wrong = query;
    wrong["transaction"]["binding"]["operation_sequence"] = json!(2);
    assert_eq!(
        service.handle("status", wrong).await.unwrap_err().code,
        "owner_mismatch"
    );
    let mut wrong = submit;
    wrong["transaction"]["binding"]["operation_sequence"] = json!(2);
    assert_eq!(
        service.handle("submit", wrong).await.unwrap_err().code,
        "operation_conflict"
    );
}
#[tokio::test]
async fn shutdown_closes_fresh_admission_but_preserves_cleanup_receipts() {
    let service = service();
    service.begin_shutdown();
    assert_eq!(
        service
            .handle("submit", submit_value())
            .await
            .unwrap_err()
            .code,
        "backend_stopping"
    );
    let submit = submit_value();
    let query = json!({"operation_id":submit["operation_id"],"transaction":submit["transaction"]});
    assert_eq!(
        service.handle("cancel", query).await.unwrap()["worker_finished"],
        true
    );
    service.finish_shutdown().await;
}
#[test]
fn physical_scope_and_challenge_fields_are_typed_before_admission() {
    let mut value = submit_value();
    value["reader"] = json!("PCSC Reader");
    value["transaction"]["binding"]["reader"] = json!("PCSC Reader");
    assert!(
        serde_json::from_value::<Submit>(value)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut value = submit_value();
    value["action"] = json!("authenticate");
    value["expected_imsi"] = json!("001010000000001");
    value["application"] = json!("isim");
    value["rand"] = json!(vec![0; 15]);
    value["autn"] = json!(vec![0; 16]);
    assert!(serde_json::from_value::<Submit>(value.clone()).is_err());
    value["rand"] = json!(vec![0; 16]);
    assert!(
        serde_json::from_value::<Submit>(value)
            .unwrap()
            .validate()
            .is_ok()
    );
}

#[tokio::test]
async fn retired_receipts_are_bounded_without_replaying_stale_work() {
    let service = service();
    let submit = submit_value();
    for i in 0..2050 {
        service
            .handle(
                "cancel",
                json!({"operation_id":format!("old-{i}"),"transaction":submit["transaction"]}),
            )
            .await
            .unwrap();
    }
    assert!(service.0.registry.lock().unwrap().entries.len() < 2048);
    let mut delayed = submit;
    delayed["operation_id"] = json!("old-0");
    // Reaped receipts never bypass the fresh capability validation.
    assert!(service.handle("submit", delayed).await.is_err());
}

#[test]
fn native_inventory_action_cannot_be_deserialized_from_wire() {
    assert!(serde_json::from_str::<super::Action>("\"inventory\"").is_err());
}
