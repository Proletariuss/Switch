use super::*;
use crate::multistandby_native_runtime::DueDelivery;
use crate::multistandby_runtime_report::Journal;
use std::sync::Mutex;
struct Fixture {
    physical: crate::multistandby_card::production::NativeFixture,
    manager: Arc<Manager>,
    events: Arc<crate::safe_events::Hub>,
    store: crate::multistandby_store::Store,
    journal: Arc<Mutex<Journal>>,
    first: String,
    second: String,
    card: String,
    root: PathBuf,
}
impl Fixture {
    async fn new(root: PathBuf, proxy: String) -> Self {
        std::fs::create_dir_all(&root).unwrap();
        let physical = crate::multistandby_card::production::NativeFixture::new();
        physical.pool.shutdown().await.unwrap();
        let store = crate::multistandby_store::Store::open(&root).unwrap();
        let a = physical.profile(true);
        let b = physical.profile(false);
        let catalog=store.apply("catalog",json!({"event_id":"catalog","reader":"Workflow Reader","profiles":[{"eid":a.eid,"iccid":a.iccid,"display_name":"A","phone_number":""},{"eid":b.eid,"iccid":b.iccid,"display_name":"B","phone_number":""}]})).unwrap();
        let specs = catalog["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| ProfileSpec {
                profile_id: p["profile_id"].as_str().unwrap().into(),
                card_id: p["card_id"].as_str().unwrap().into(),
                eid: p["eid"].as_str().unwrap().into(),
                iccid: p["iccid"].as_str().unwrap().into(),
                aid: a.aid.clone(),
                display_name: "fixture".into(),
                phone_number: String::new(),
                physical_enabled: p["iccid"] == b.iccid,
            })
            .collect::<Vec<_>>();
        let first = specs
            .iter()
            .find(|p| p.iccid == a.iccid)
            .unwrap()
            .profile_id
            .clone();
        let second = specs
            .iter()
            .find(|p| p.iccid == b.iccid)
            .unwrap()
            .profile_id
            .clone();
        let card = specs[0].card_id.clone();
        let journal = Arc::new(Mutex::new(Journal::open(&root.join("journal.json"), now())));
        let events = Arc::new(crate::safe_events::Hub::new());
        let manager = Manager::new_with_events(
            Resources {
                owner: physical.owner.clone(),
                gate: physical.gate.clone(),
                card_service: crate::multistandby_card::Service::new(
                    physical.owner.clone(),
                    physical.gate.clone(),
                    None,
                    None,
                ),
                data_root: root.clone(),
                store: store.clone(),
                journal: journal.clone(),
                transactions: Default::default(),
                connection: ConnectionOptions {
                    proxy,
                    epdg: "epdg.invalid".into(),
                    epdg_port: Some(500),
                },
            },
            events.clone(),
        )
        .unwrap();
        manager
            .install_inventory(Inventory {
                reader: "Workflow Reader".into(),
                generation: 1,
                profiles: specs,
            })
            .await
            .unwrap();
        Self {
            physical,
            manager,
            events,
            store,
            journal,
            first,
            second,
            card,
            root,
        }
    }
    async fn action(
        &self,
        action: Action,
        ids: Vec<String>,
    ) -> Result<serde_json::Value, &'static str> {
        self.manager
            .execute(action, self.card.clone(), ids)
            .unwrap()
            .wait()
            .await
    }
    fn row(&self) -> serde_json::Value {
        self.manager.snapshot(&self.card)["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["profile_id"] == self.first)
            .unwrap()
            .clone()
    }
    async fn finish(self) {
        self.manager.shutdown().await.unwrap();
        self.physical.finish().await;
    }
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_recovery_configuration_and_cancel() {
    if std::env::var_os("SWITCH_MANAGER_RECOVERY_497_CHILD").is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/manager-recovery-497-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output=tokio::task::spawn_blocking(move||std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","native_vowifi_session::service::production_integration::manager_exchange::recovery::manager_recovery_configuration_and_cancel","--nocapture"]).env("SWITCH_MANAGER_RECOVERY_497_CHILD",&root).env("SWITCH_DATA_DIR",&root).env("PROLETARIUS_VOWIFI_CONFIG",config).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(65), async {
        let root = PathBuf::from(std::env::var_os("SWITCH_MANAGER_RECOVERY_497_CHILD").unwrap());
        renewal_and_eof(root.join("renewal")).await;
        permanent_configuration_and_batch_stop(root.join("configuration")).await;
        cancel_unpublished_start(root.join("cancel")).await;
        shutdown_retries_only_after_real_physical_close(root.join("close-retry")).await;
    })
    .await
    .unwrap();
}
async fn renewal_and_eof(root: PathBuf) {
    let (server, mut carrier) = renewal_peer::new_lifecycle(root.clone());
    let peer = ike_peer::Peer::with_renewal(server).await;
    let f = Fixture::new(root, peer.proxy.to_string()).await;
    assert_eq!(
        f.action(Action::Retry, vec![f.first.clone()])
            .await
            .unwrap_err(),
        "profile_stopped"
    );
    f.action(Action::Start, vec![f.first.clone()])
        .await
        .unwrap();
    ready(&f.manager, &f.first).await;
    assert_eq!(carrier.next().await, renewal_peer::Event::Registered(1));
    let first = f.row();
    assert_eq!(first["health"], "ready");
    assert_eq!(first["auth_state"], "ready");
    let expiry = first["registration_expires_at_ms"].as_i64().unwrap();
    let renewal = first["registration_renew_at_ms"].as_i64().unwrap();
    assert!(renewal < expiry);
    let generation = first["session_generation"].as_u64().unwrap();
    let revision = f.store.current_revision().unwrap();
    f.action(Action::Start, vec![f.first.clone()])
        .await
        .unwrap();
    assert_eq!(
        f.store.current_revision().unwrap(),
        revision,
        "idempotent start must not rewrite desired"
    );
    assert_eq!(carrier.next().await, renewal_peer::Event::Refreshed(1));
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.row()["registration_expires_at_ms"].as_i64().unwrap() <= expiry {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.row()["session_generation"], generation);
    assert_eq!(peer.trace.ike.load(Ordering::SeqCst), 1);
    carrier
        .command(1, renewal_peer::Command::CloseSignaling)
        .await;
    assert_eq!(
        carrier.next().await,
        renewal_peer::Event::SignalingClosed(1)
    );
    let due = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(due) = f.journal.lock().unwrap().next_due_retry(now()) {
                break due;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!f.manager.send_readiness(&f.first).channel_ready);
    assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.manager.dispatch_retry_due(due.clone()),
        DueDelivery::Accepted
    );
    assert_eq!(
        f.manager.dispatch_retry_due(due.clone()),
        DueDelivery::Stale
    );
    f.journal
        .lock()
        .unwrap()
        .complete_retry(&due, now())
        .unwrap();
    assert_eq!(carrier.next().await, renewal_peer::Event::Registered(2));
    ready(&f.manager, &f.first).await;
    assert!(f.row()["session_generation"].as_u64().unwrap() > generation);
    assert_eq!(f.physical.transaction_count(), 0);
    let stale = f
        .manager
        .reader_lost("Workflow Reader".into(), 99)
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert_eq!(stale["stale"], true);
    assert!(f.manager.send_readiness(&f.first).channel_ready);
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    f.action(Action::Stop, vec![f.first.clone()]).await.unwrap();
    assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 2);
    let events = diagnostic_events(&f.events).await;
    assert_attempts(&events, 2);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.stage == "complete" && e.code == "transport_failed")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.stage == "complete" && e.code == "cancelled")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.stage == "ready" && e.code == "ready")
            .count(),
        2
    );
    peer.finish().await;
    f.finish().await;
}
async fn permanent_configuration_and_batch_stop(root: PathBuf) {
    let config = PathBuf::from(std::env::var_os("PROLETARIUS_VOWIFI_CONFIG").unwrap());
    std::fs::write(&config, "carrier_overrides: [invalid]\n").unwrap();
    let peer = ike_peer::Peer::new().await;
    let f = Fixture::new(root, peer.proxy.to_string()).await;
    f.action(Action::Start, vec![f.first.clone()])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.row()["auth_state"] != "backoff" {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.row()["desired_state"], "keep_online");
    assert_eq!(f.row()["health"], "degraded");
    assert!(
        f.row()["last_error"]
            .as_str()
            .unwrap()
            .contains("overrides")
    );
    assert!(f.journal.lock().unwrap().next_due_retry(i64::MAX).is_none());
    assert_eq!(f.physical.auth_count(), 0);
    let events = diagnostic_events(&f.events).await;
    assert_attempts(&events, 1);
    assert!(
        !events
            .iter()
            .any(|e| e.stage == "connect" || e.stage == "ready")
    );
    assert_eq!(events.last().unwrap().code, "transport_failed");
    std::fs::write(&config,"carrier_overrides:\n  '234/33': {ip_stack: ipv4, custom_epdg: 'changed-carrier.invalid'}\n").unwrap();
    f.action(Action::Retry, vec![f.first.clone()])
        .await
        .unwrap();
    ready(&f.manager, &f.first).await;
    f.action(Action::Start, vec![f.second.clone()])
        .await
        .unwrap();
    ready(&f.manager, &f.second).await;
    // Both workers must actually retire even when every desired write fails.
    std::fs::write(f.root.join("multistandby/store-v1.json"), "invalid-json").unwrap();
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    assert_eq!(
        f.action(Action::Stop, vec![f.first.clone(), f.second.clone()])
            .await
            .unwrap_err(),
        "native_multi_store_unavailable"
    );
    assert!(!f.manager.has_active_runtime());
    assert_eq!(f.physical.transaction_count(), 0);
    assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 2);
    peer.finish().await;
    f.finish().await;
}
async fn cancel_unpublished_start(root: PathBuf) {
    let peer = ike_peer::Peer::new().await;
    let f = Fixture::new(root, peer.proxy.to_string()).await;
    let (release, entered) = f.physical.hold_next_apdu();
    f.action(Action::Start, vec![f.first.clone()])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    f.action(Action::Retry, vec![f.first.clone()])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.physical.transaction_count(), 1);
    assert_eq!(f.physical.auth_count(), 0);
    assert_eq!(peer.trace.ike.load(Ordering::SeqCst), 0);
    let pending = diagnostic_events(&f.events).await;
    assert_eq!(
        pending.len(),
        1,
        "blocked submitted APDU has no confirmed retirement"
    );
    assert_eq!(pending[0].stage, "prepare");
    release.send(()).unwrap();
    ready(&f.manager, &f.first).await;
    assert_eq!(f.physical.auth_count(), 2);
    assert_eq!(peer.trace.ike.load(Ordering::SeqCst), 1);
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    f.action(Action::Stop, vec![f.first.clone()]).await.unwrap();
    let events = diagnostic_events(&f.events).await;
    assert_attempts(&events, 2);
    assert_eq!(events.iter().filter(|e| e.stage == "connect").count(), 1);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.stage == "complete" && e.code == "manual_retry" && e.generation == 0)
            .count(),
        1
    );
    peer.finish().await;
    f.finish().await;
}

async fn shutdown_retries_only_after_real_physical_close(root: PathBuf) {
    let peer = ike_peer::Peer::new().await;
    let f = Fixture::new(root, peer.proxy.to_string()).await;
    f.action(Action::Start, vec![f.first.clone()])
        .await
        .unwrap();
    ready(&f.manager, &f.first).await;
    peer.trace.stop_requested.store(true, Ordering::SeqCst);
    let closed = f.physical.closed_count();
    let switches = f.physical.switch_count();
    let (release, entered) = f.physical.hold_close();
    let owner = f.physical.owner.clone();
    let binding = f.physical.physical_binding();
    let close_binding = binding.clone();
    let closing =
        tokio::task::spawn_blocking(move || owner.test_close_for_presence(&close_binding));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), f.manager.shutdown())
            .await
            .unwrap()
            .unwrap_err(),
        "native_multi_cleanup_unconfirmed"
    );
    assert!(f.manager.has_active_runtime());
    assert_eq!(f.physical.closed_count(), closed);
    release.send(()).unwrap();
    assert!(closing.await.unwrap().unwrap());
    let transition = f
        .physical
        .gate
        .prepare_native_transition(
            &binding.producer,
            &f.physical.gate.native_reader_snapshot().0,
            crate::reader_reservation::NativeTransitionKind::Loss,
            &binding.reader,
            crate::reader_reservation::NativeAuthority::Loss(binding.clone()),
        )
        .unwrap();
    f.physical
        .gate
        .commit_confirmed_native_presence_loss(&f.physical.owner, &transition)
        .unwrap();
    f.physical.owner.test_ack_presence_loss(&binding);
    tokio::time::timeout(Duration::from_secs(5), f.manager.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(!f.manager.has_active_runtime());
    assert_eq!(f.physical.closed_count(), closed + 1);
    assert_eq!(f.physical.switch_count(), switches);
    assert!(!f.physical.original_profile_enabled());
    assert_eq!(peer.trace.deletes.load(Ordering::SeqCst), 1);
    assert!(
        f.store.runtime_catalog().unwrap()["desired"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["profile_id"] == f.first && d["desired_state"] == "keep_online")
    );
    peer.finish().await;
    f.physical.owner.shutdown().await.unwrap();
}
