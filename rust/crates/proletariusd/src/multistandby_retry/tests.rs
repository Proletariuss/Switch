use super::*;
use serde_json::json;
fn path() -> std::path::PathBuf {
    let root = std::path::PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
        "runtime/retry-native-497-{}",
        crate::multistandby_store::random_token().unwrap()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root.join("journal.json")
}
fn retry(journal: &Mutex<Journal>, attempt: char) -> RetryDue {
    journal
        .lock()
        .unwrap()
        .schedule_native_retry(
            "profile_a".into(),
            "native-owner".into(),
            attempt.to_string().repeat(48),
            1,
            now_ms() - 120_000,
        )
        .unwrap();
    journal.lock().unwrap().next_due_retry(now_ms()).unwrap()
}
#[test]
fn native_retry_busy_keeps_durable_deadline_and_terminal_delivery_fences_exact_attempt() {
    let path = path();
    let journal = Mutex::new(Journal::open(&path, now_ms()));
    let first = retry(&journal, 'a');
    complete_retry(&journal, &first, DueDelivery::Busy, now_ms());
    assert_eq!(
        Journal::open(&path, now_ms())
            .next_due_retry(now_ms())
            .unwrap()
            .attempt_id,
        first.attempt_id
    );
    complete_retry(&journal, &first, DueDelivery::Accepted, now_ms());
    assert!(
        Journal::open(&path, now_ms())
            .next_due_retry(i64::MAX)
            .is_none()
    );
    let second = retry(&journal, 'b');
    complete_retry(&journal, &first, DueDelivery::Stale, now_ms());
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .next_due_retry(now_ms())
            .unwrap()
            .attempt_id,
        second.attempt_id
    );
    complete_retry(&journal, &second, DueDelivery::Stale, now_ms());
    assert!(
        Journal::open(&path, now_ms())
            .next_due_retry(i64::MAX)
            .is_none()
    );
}
async fn registration(journal: Arc<Mutex<Journal>>, generation: u64) -> RegistrationDue {
    use crate::multistandby_runtime_report::{
        INTERNAL_REGISTRATION_SCHEDULE_PATH, handle_registration_schedule,
    };
    let request=hyper::Request::builder().method("POST").uri(INTERNAL_REGISTRATION_SCHEDULE_PATH)
        .body(http_body_util::Full::new(bytes::Bytes::from(serde_json::to_vec(&json!({"profile_id":"profile_a","owner_id":"native-owner","generation":generation,"delay_ms":1000})).unwrap()))).unwrap();
    assert_eq!(
        handle_registration_schedule(request, journal.clone())
            .await
            .status(),
        hyper::StatusCode::OK
    );
    journal
        .lock()
        .unwrap()
        .next_due_registration(i64::MAX)
        .unwrap()
}
#[tokio::test]
async fn native_registration_busy_preserves_lease_deadline_and_stale_never_erases_new_generation() {
    let path = path();
    let journal = Arc::new(Mutex::new(Journal::open(&path, now_ms())));
    let first = registration(journal.clone(), 7).await;
    complete_registration(&journal, &first, DueDelivery::Busy, now_ms());
    assert_eq!(
        Journal::open(&path, now_ms())
            .next_due_registration(i64::MAX)
            .unwrap()
            .generation,
        7
    );
    let next = registration(journal.clone(), 8).await;
    complete_registration(&journal, &first, DueDelivery::Accepted, now_ms());
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .next_due_registration(i64::MAX)
            .unwrap()
            .generation,
        8
    );
    complete_registration(&journal, &next, DueDelivery::Stale, now_ms());
    assert!(
        Journal::open(&path, now_ms())
            .next_due_registration(i64::MAX)
            .is_none()
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timer_dispatches_stale_attempt_to_real_native_facade_without_go_availability() {
    use crate::{
        multistandby_card::production::NativeFixture,
        multistandby_native_runtime::ConnectionOptions, multistandby_native_service::Resources,
    };
    let physical = NativeFixture::new();
    let path = path();
    let root = path.parent().unwrap();
    let journal = Arc::new(Mutex::new(Journal::open(&path, now_ms())));
    retry(&journal, 'a');
    let (owner, gate, card_service) = physical.inventory_resources();
    let service = Service::new(Resources {
        owner,
        gate,
        card_service,
        data_root: root.to_owned(),
        store: crate::multistandby_store::Store::open(root).unwrap(),
        journal: journal.clone(),
        transactions: Default::default(),
        connection: ConnectionOptions::default(),
    })
    .unwrap();
    let (stop, signal) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(run(journal.clone(), service.clone(), signal));
    tokio::time::timeout(Duration::from_secs(3), async {
        while journal.lock().unwrap().next_due_retry(now_ms()).is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop.send_replace(true);
    task.await.unwrap();
    assert!(
        Journal::open(&path, now_ms())
            .next_due_retry(i64::MAX)
            .is_none()
    );
    assert_eq!(physical.auth_count(), 0);
    assert_eq!(physical.switch_count(), 0);
    service.shutdown().await.unwrap();
    physical.finish().await;
}
#[tokio::test]
async fn idle_retry_scheduler_exits_promptly_when_signaled() {
    let mut ticker = interval(Duration::from_secs(60));
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
