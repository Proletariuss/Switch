//! Exercise the retained command/Ticket boundary with the real synthetic card gate.
use super::*;
use crate::multistandby_card::production::NativeFixture;

type Done = watch::Sender<Option<Result<(), Failure>>>;
struct Fixture {
    manager: Arc<Manager>,
    physical: NativeFixture,
    runtime: Arc<Runtime>,
    profiles: Vec<ProfileSpec>,
}
impl Fixture {
    async fn new() -> Self {
        let physical = NativeFixture::new();
        physical.pool.shutdown().await.unwrap();
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap())
            .join(format!("runtime/start-admission-753-{}", token().unwrap()));
        let store = crate::multistandby_store::Store::open(&root).unwrap();
        let card = physical.profile(true);
        let profiles = ["fresh", "healthy", "terminal"]
            .iter()
            .enumerate()
            .map(|(i, id)| ProfileSpec {
                profile_id: (*id).into(),
                card_id: "card".into(),
                eid: card.eid.clone(),
                iccid: format!("890123456789012345{i}"),
                aid: card.aid.clone(),
                display_name: (*id).into(),
                phone_number: String::new(),
                physical_enabled: false,
            })
            .collect::<Vec<_>>();
        store
            .apply(
                "catalog",
                json!({"event_id":"catalog", "profiles":profiles.iter().map(|p|json!({
            "profile_id":p.profile_id,"card_id":p.card_id,"eid":p.eid,"iccid":p.iccid,
        })).collect::<Vec<_>>()}),
            )
            .unwrap();
        for id in ["healthy", "terminal"] {
            store.apply("desired",json!({"event_id":format!("want-{id}"),"profile_id":id,"desired_state":"keep_online"})).unwrap();
        }
        let manager = Manager::new(Resources {
            owner: physical.owner.clone(),
            gate: physical.gate.clone(),
            card_service: crate::multistandby_card::Service::new(
                physical.owner.clone(),
                physical.gate.clone(),
                None,
                None,
            ),
            data_root: root,
            store,
            journal: Arc::new(Mutex::new(Journal::default())),
            transactions: Default::default(),
            connection: Default::default(),
        })
        .unwrap();
        let inventory = Inventory {
            reader: "Workflow Reader".into(),
            generation: 1,
            profiles: profiles.clone(),
        };
        manager.install_inventory(inventory.clone()).await.unwrap();
        // Retain a real Runtime and its exact native card reservation, without
        // starting any network generation or submitting any physical card job.
        let runtime = manager.runtime_for(&inventory, &profiles[0]).unwrap();
        Self {
            manager,
            physical,
            runtime,
            profiles,
        }
    }
    fn insert(
        &self,
        id: &str,
        result: Option<Result<(), Failure>>,
    ) -> (Done, mpsc::Receiver<Wake>) {
        let (stop, _) = watch::channel(false);
        let (wake, received) = mpsc::channel(4);
        let (done, done_rx) = watch::channel(result);
        self.manager.state.lock().unwrap().entries.insert(
            id.into(),
            Entry {
                spec: self
                    .profiles
                    .iter()
                    .find(|p| p.profile_id == id)
                    .unwrap()
                    .clone(),
                stop,
                wake,
                done: done_rx,
                session: None,
                generation: 7,
                reason: "starting".into(),
                error: String::new(),
                failures: 0,
                auth: "queued",
                health: "starting",
                attempt_stop: None,
                retry_requested: false,
                maintenance_pending: false,
                registration_pending: false,
                retry: None,
                installing_retry: None,
            },
        );
        (done, received)
    }
    async fn start(&self, ids: &[&str]) -> Result<Value, Failure> {
        self.manager
            .execute(
                Action::Start,
                "card".into(),
                ids.iter().map(|id| (*id).into()).collect(),
            )
            .expect("the retained command must be admitted")
            .wait()
            .await
    }
    fn retained_owner(&self) -> bool {
        Arc::ptr_eq(&self.runtime, &self.manager.runtime_for_test().unwrap())
            && self.physical.gate.selected_reader_generation()
                == Some(("Workflow Reader".into(), 1))
            && self
                .physical
                .gate
                .acquire_native_multi("Workflow Reader", 1)
                .is_err()
    }
    async fn finish(self) {
        // These injected Entry channels have no real worker to join. Remove
        // only those test records; Manager shutdown still joins/releases the
        // retained real Runtime and obtains its real no-profile-work receipt.
        self.manager.state.lock().unwrap().entries.clear();
        self.manager.shutdown().await.unwrap();
        self.physical.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_err_start_returns_cleanup_unconfirmed_and_retains_real_owner() {
    let f = Fixture::new().await;
    let _channels = f.insert("terminal", Some(Err("native_multi_cleanup_unconfirmed")));
    let catalog = f.manager.resources.store.runtime_catalog().unwrap();
    let revision = f.manager.resources.store.current_revision().unwrap();
    let result = f.start(&["terminal"]).await;
    let retained = f.retained_owner();
    let unchanged = f.manager.resources.store.current_revision().unwrap() == revision
        && f.manager.resources.store.runtime_catalog().unwrap() == catalog;
    let terminal = f.manager.state.lock().unwrap().entries["terminal"]
        .done
        .borrow()
        .clone();
    let work = (
        f.physical.auth_count(),
        f.physical.switch_count(),
        f.physical.transaction_count(),
    );
    f.finish().await;
    assert_eq!(
        result,
        Err("native_multi_cleanup_unconfirmed"),
        "an already dead worker cannot accept a new start"
    );
    assert!(
        retained,
        "rejection must retain the exact Runtime and card reservation"
    );
    assert!(
        unchanged,
        "rejection cannot rewrite durable intent or catalog"
    );
    assert_eq!(terminal, Some(Err("native_multi_cleanup_unconfirmed")));
    assert_eq!(work, (0, 0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_terminal_preflight_precedes_fresh_desired_write_and_worker_spawn() {
    let f = Fixture::new().await;
    let _healthy = f.insert("healthy", None);
    let _terminal = f.insert("terminal", Some(Err("native_multi_cleanup_unconfirmed")));
    let catalog = f.manager.resources.store.runtime_catalog().unwrap();
    let revision = f.manager.resources.store.current_revision().unwrap();
    // If the old implementation wrongly creates the fresh worker, prevent it
    // from leaving its startup queue; cancel/join it before asserting failure.
    let guard = f.manager.startup_gate.lock().await;
    let result = f.start(&["fresh", "healthy", "terminal"]).await;
    let retained = f.retained_owner();
    let unchanged = f.manager.resources.store.current_revision().unwrap() == revision
        && f.manager.resources.store.runtime_catalog().unwrap() == catalog;
    let (fresh_absent, healthy_generation, entry_count) = {
        let state = f.manager.state.lock().unwrap();
        (
            !state.entries.contains_key("fresh"),
            state.entries["healthy"].generation,
            state.entries.len(),
        )
    };
    f.manager.cancel_profile("fresh");
    drop(guard);
    f.manager.stop_profile("fresh").await.unwrap();
    let work = (
        f.physical.auth_count(),
        f.physical.switch_count(),
        f.physical.transaction_count(),
    );
    f.finish().await;
    assert_eq!(result, Err("native_multi_cleanup_unconfirmed"));
    assert!(
        unchanged,
        "batch preflight must precede every durable desired write"
    );
    assert!(
        fresh_absent,
        "no fresh worker may be spawned before all selected entries pass"
    );
    assert_eq!(entry_count, 2);
    assert_eq!(
        healthy_generation, 7,
        "a healthy sibling must keep its generation"
    );
    assert!(retained);
    assert_eq!(work, (0, 0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_queued_start_remains_idempotent_without_rewriting_intent_or_releasing_owner() {
    let f = Fixture::new().await;
    let _channels = f.insert("healthy", None);
    let catalog = f.manager.resources.store.runtime_catalog().unwrap();
    let revision = f.manager.resources.store.current_revision().unwrap();
    let first = f.start(&["healthy"]).await;
    let second = f.start(&["healthy"]).await;
    let retained = f.retained_owner();
    let unchanged = f.manager.resources.store.current_revision().unwrap() == revision
        && f.manager.resources.store.runtime_catalog().unwrap() == catalog;
    let (entries, generation) = {
        let state = f.manager.state.lock().unwrap();
        (state.entries.len(), state.entries["healthy"].generation)
    };
    let work = (
        f.physical.auth_count(),
        f.physical.switch_count(),
        f.physical.transaction_count(),
    );
    f.finish().await;
    assert_eq!(first.unwrap()["status"], "accepted");
    assert_eq!(second.unwrap()["status"], "accepted");
    assert!(retained && unchanged);
    assert_eq!((entries, generation), (1, 7));
    assert_eq!(work, (0, 0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_worker_completion_channel_cannot_accept_start() {
    let f = Fixture::new().await;
    let (done, received) = f.insert("terminal", None);
    drop(done); // The retained worker disappeared before publishing its result.
    let result = f.start(&["terminal"]).await;
    let retained = f.retained_owner();
    drop(received);
    f.finish().await;
    assert_eq!(result, Err("native_multi_cleanup_unconfirmed"));
    assert!(retained);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_or_stopping_entry_rejects_start_without_store_or_owner_changes() {
    let mut outcomes = Vec::new();
    for (label, done, stopping) in [("completed", Some(Ok(())), false), ("stopping", None, true)] {
        let f = Fixture::new().await;
        let _channels = f.insert("terminal", done);
        if stopping {
            f.manager.state.lock().unwrap().entries["terminal"]
                .stop
                .send_replace(true);
        }
        let catalog = f.manager.resources.store.runtime_catalog().unwrap();
        let revision = f.manager.resources.store.current_revision().unwrap();
        let result = f.start(&["terminal"]).await;
        let retained = f.retained_owner();
        let unchanged = f.manager.resources.store.current_revision().unwrap() == revision
            && f.manager.resources.store.runtime_catalog().unwrap() == catalog;
        let work = (
            f.physical.auth_count(),
            f.physical.switch_count(),
            f.physical.transaction_count(),
        );
        f.finish().await;
        outcomes.push((label, result, retained, unchanged, work));
    }
    assert_eq!(
        outcomes,
        vec![
            (
                "completed",
                Err("native_multi_cleanup_unconfirmed"),
                true,
                true,
                (0, 0, 0)
            ),
            (
                "stopping",
                Err("native_multi_cleanup_unconfirmed"),
                true,
                true,
                (0, 0, 0)
            ),
        ]
    );
}
