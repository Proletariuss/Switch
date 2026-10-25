use super::*;
use serde_json::json;
#[test]
fn only_live_durable_intent_requests_reconciliation() {
    for catalog in [
        json!({}),
        json!({"desired":[]}),
        json!({"desired":[{"desired_state":"stopped"}]}),
    ] {
        assert!(!has_keep_online_intent(&catalog));
    }
    assert!(has_keep_online_intent(
        &json!({"desired":[{"desired_state":"stopped"},{"desired_state":"keep_online"}]})
    ));
}
#[tokio::test]
async fn five_second_recovery_timer_can_exit_while_idle() {
    assert_eq!(POLL_INTERVAL, Duration::from_secs(5));
    let mut ticker = interval(POLL_INTERVAL);
    let (send, mut stop) = tokio::sync::watch::channel(false);
    assert!(wait_for_tick_or_shutdown(&mut ticker, &mut stop).await);
    send.send_replace(true);
    assert!(
        !tokio::time::timeout(
            Duration::from_millis(100),
            wait_for_tick_or_shutdown(&mut ticker, &mut stop)
        )
        .await
        .unwrap()
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_shutdown_waits_for_real_inventory_apdu_and_preserves_unavailable_card_intent() {
    use crate::{
        multistandby_card::production::NativeFixture,
        multistandby_native_runtime::ConnectionOptions, multistandby_native_service::Resources,
    };
    use std::sync::{Mutex, atomic::Ordering};
    let physical = NativeFixture::new();
    physical.pool.shutdown().await.unwrap();
    let root = std::path::PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
        "runtime/recovery-native-497-{}",
        multistandby_store::random_token().unwrap()
    ));
    let store = multistandby_store::Store::open(&root).unwrap();
    store.recover().unwrap();
    store.apply("catalog",json!({"event_id":"other-card","profiles":[{"profile_id":"absent","card_id":"other","eid":"other-eid","iccid":"other-iccid"}]})).unwrap();
    store
        .apply(
            "desired",
            json!({"event_id":"keep-other","profile_id":"absent","desired_state":"keep_online"}),
        )
        .unwrap();
    let (owner, gate, card_service) = physical.inventory_resources();
    let service = Service::new(Resources {
        owner,
        gate: gate.clone(),
        card_service,
        data_root: root,
        store: store.clone(),
        journal: Arc::new(Mutex::new(Default::default())),
        transactions: Default::default(),
        connection: ConnectionOptions::default(),
    })
    .unwrap();
    let (unblock, entered) = physical.hold_next_apdu();
    let (stop, signal) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(run(store.clone(), service.clone(), signal));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop.send_replace(true);
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    assert_eq!(physical.transaction_count(), 1);
    assert!(gate.acquire_native_multi("Workflow Reader", 1).is_err());
    unblock.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(physical.transaction_count(), 0);
    assert_eq!(physical.auth_count(), 0);
    assert_eq!(physical.switch_count(), 0);
    assert_eq!(
        service.snapshot("", false, false).await.unwrap()["profiles"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(has_keep_online_intent(&store.runtime_catalog().unwrap()));
    let parent = gate.acquire_native_multi("Workflow Reader", 1).unwrap();
    gate.release_native_multi(&parent).unwrap();
    service.shutdown().await.unwrap();
    physical.finish().await;
}
