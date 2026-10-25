use super::*;
use crate::multistandby_card::production::NativeFixture;
use crate::multistandby_native_runtime::ConnectionOptions;
fn root() -> std::path::PathBuf {
    let root = std::path::PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
        "runtime/native-multi-facade-497-{}",
        crate::multistandby_store::random_token().unwrap()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}
fn create(fixture: &NativeFixture, root: &std::path::Path) -> Arc<Service> {
    let (owner, gate, card_service) = fixture.inventory_resources();
    Service::new(Resources {
        owner,
        gate,
        card_service,
        data_root: root.to_owned(),
        store: crate::multistandby_store::Store::open(root).unwrap(),
        journal: Arc::new(Mutex::new(
            crate::multistandby_runtime_report::Journal::default(),
        )),
        transactions: Default::default(),
        connection: ConnectionOptions::default(),
    })
    .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn facade_inventory_uses_native_workflow_and_stable_store_identity_with_cache() {
    let physical = NativeFixture::new();
    physical.pool.shutdown().await.unwrap();
    let root = root();
    let facade = create(&physical, &root);
    let cards = facade.snapshot("", true, false).await.unwrap();
    assert_eq!(cards["available"], true, "{cards}");
    assert_eq!(cards["card_profiles"].as_array().unwrap().len(), 2);
    let profiles = facade.snapshot("", false, false).await.unwrap();
    let rows = profiles["profiles"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0]["profile_id"], rows[1]["profile_id"]);
    assert_eq!(rows[0]["card_id"], rows[1]["card_id"]);
    let revision = facade.store.current_revision().unwrap();
    let (release, entered) = physical.hold_next_apdu();
    let cached = facade.snapshot("", true, true).await.unwrap();
    assert_eq!(cached["card_profiles"], cards["card_profiles"]);
    assert!(!entered.load(Ordering::Acquire));
    release.send(()).unwrap();
    assert_eq!(facade.store.current_revision().unwrap(), revision);
    assert_eq!(physical.auth_count(), 0);
    assert_eq!(physical.switch_count(), 0);
    facade.shutdown().await.unwrap();
    let next = create(&physical, &root);
    assert_eq!(
        next.snapshot("", true, false).await.unwrap()["card_profiles"],
        cards["card_profiles"]
    );
    assert_eq!(
        next.snapshot("", false, false).await.unwrap()["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["profile_id"].clone())
            .collect::<Vec<_>>(),
        rows.iter()
            .map(|p| p["profile_id"].clone())
            .collect::<Vec<_>>()
    );
    next.shutdown().await.unwrap();
    physical.finish().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_snapshot_waiter_cannot_release_inventory_before_shutdown_join() {
    let physical = NativeFixture::new();
    physical.pool.shutdown().await.unwrap();
    let facade = create(&physical, &root());
    let (release, entered) = physical.hold_next_apdu();
    let service = facade.clone();
    let waiter = tokio::spawn(async move { service.snapshot("", true, false).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let stopped = facade.shutdown();
    tokio::pin!(stopped);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut stopped)
            .await
            .is_err()
    );
    assert!(
        physical
            .gate
            .acquire_native_multi("Workflow Reader", 1)
            .is_err()
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), &mut stopped)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(physical.transaction_count(), 0);
    assert_eq!(physical.switch_count(), 0);
    assert!(facade.snapshot("", true, false).await.is_err());
    physical.finish().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_reader_retirement_clears_cache_but_stale_retirement_does_not() {
    let physical = NativeFixture::new();
    physical.pool.shutdown().await.unwrap();
    let facade = create(&physical, &root());
    assert_eq!(
        facade.snapshot("", true, false).await.unwrap()["available"],
        true
    );
    facade
        .retire_generation("Workflow Reader", 2)
        .await
        .unwrap();
    assert!(facade.cache.lock().unwrap().inventory.is_some());
    facade.stop_and_drain("Workflow Reader", 1).await.unwrap();
    assert!(facade.cache.lock().unwrap().inventory.is_none());
    assert_eq!(
        facade.snapshot("", true, false).await.unwrap()["available"],
        true
    );
    facade.shutdown().await.unwrap();
    physical.finish().await;
}
