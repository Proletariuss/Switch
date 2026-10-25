// Compose actual journal/card/reservation/retained-owner/finalizer objects.
// Only AT responses and the existing radio physical-fixture channel are fake.
use super::*;
use crate::module_radio::tests::tests::Fixture as RadioFixture;
use crate::module_radio_transport::{Action, Outcome};
use std::sync::atomic::AtomicUsize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopAt {
    GetEid,
    Enable,
    Close,
    Refresh,
}

fn disabled_profiles() -> Vec<u8> {
    fn tlv(tag: &[u8], bytes: &[u8]) -> Vec<u8> {
        [tag, &[bytes.len() as u8], bytes].concat()
    }
    let fields = [
        tlv(&[0x5a], &encode_iccid(ICCID).unwrap()),
        tlv(&[0x9f, 0x70], &[0]),
    ]
    .concat();
    [
        tlv(&[0xbf, 0x2d], &tlv(&[0xa0], &tlv(&[0xe3], &fields))),
        vec![0x90, 0],
    ]
    .concat()
}

// Test observation of the actual permit's one drop, without replacing its
// ownership guard or operation-gate implementation with a fake state machine.
struct CountedPermit {
    inner: Option<SwitchPermit>,
    drops: Arc<AtomicUsize>,
}
impl Drop for CountedPermit {
    fn drop(&mut self) {
        drop(self.inner.take());
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

async fn still_pending(task: &mut tokio::task::JoinHandle<crate::shutdown::Report>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(30), task)
            .await
            .is_err()
    );
}

async fn composed_case(stop_at: StopAt, journal_fault: bool) {
    let fixture = JournalFixture::new();
    let service = Arc::new(Service::open(&fixture.0).unwrap());
    let mut physical = RadioFixture::new();
    physical.configure().await;
    let radio = physical.service.clone();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let permit_drops = Arc::new(AtomicUsize::new(0));
    let releases = Arc::new(AtomicUsize::new(0));
    let terminals = Arc::new(AtomicUsize::new(0));
    let receipts = Arc::new(AtomicUsize::new(0));
    let archives = Arc::new(AtomicUsize::new(0));
    let mut eid = apdu("BF3E", &scripted_eid_response(CARD_A));
    let mut enable = apdu("BF31", &[0xbf, 0x31, 3, 0x80, 1, 0, 0x90, 0]);
    if stop_at == StopAt::Enable {
        enable.reply = Err(crate::usb_at::Failure::new(
            StatusCode::BAD_GATEWAY,
            "synthetic_response_lost",
            "submitted response unknown",
        ));
    }
    let mut close = step("AT+CCHC=1", "OK\r\n".into());
    let protocol_barrier = match stop_at {
        StopAt::GetEid => Some(barrier(&mut eid)),
        StopAt::Enable => Some(barrier(&mut enable)),
        StopAt::Close => Some(barrier(&mut close)),
        StopAt::Refresh => None,
    };
    let mut steps = VecDeque::from([
        step("AT+CCHO=", "+CCHO: 1\r\nOK\r\n".into()),
        apdu("0070000001", &[1, 0x90, 0]),
        apdu("A40400", &[0x90, 0]),
        apdu("BF2D", &disabled_profiles()),
        eid,
    ]);
    // A failed GetEID proof commit must prevent the mutating APDU entirely.
    if !(stop_at == StopAt::GetEid && journal_fault) {
        steps.push_back(enable);
    }
    steps.extend([apdu("0070800100", &[0x90, 0]), close]);
    let mut at = BarrierAt {
        steps,
        commands: commands.clone(),
    };
    let owner_service = service.clone();
    let owner_radio = radio.clone();
    let owner_drops = permit_drops.clone();
    let owner_releases = releases.clone();
    let owner_terminals = terminals.clone();
    let (done, completed) = tokio::sync::oneshot::channel();
    let (owner_handle, owner_observed) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        let retained = spawn_retained_operation(async move {
            let permit = owner_service.begin_switch(ICCID).unwrap();
            let id = permit.id.clone();
            let permit = CountedPermit {
                inner: Some(permit),
                drops: owner_drops,
            };
            let reservation = owner_radio.reserve_esim().await.unwrap();
            let card_service = owner_service.clone();
            let card_id = id.clone();
            let transaction = tokio::task::spawn_blocking(move || {
                let mut channel =
                    ModuleEuiccChannel::open(&mut at, KNOWN_ISDR_APPLICATION_AIDS[0]).unwrap();
                let result = switch_profile_on_transport_with_proof(
                    &mut channel,
                    ICCID,
                    Some(KNOWN_ISDR_APPLICATION_AIDS[0]),
                    Arc::new(AtomicBool::new(false)),
                    Some((&card_service, &card_id)),
                );
                channel.close().unwrap();
                assert!(at.steps.is_empty());
                result
            })
            .await
            .unwrap();
            // Use the actual retained reservation's refresh job and physical
            // adapter boundary. No fake reservation or final receipt exists.
            owner_radio
                .refresh_esim_profile(&reservation, &id)
                .await
                .unwrap();
            owner_radio.release_esim(reservation).await.unwrap();
            owner_releases.fetch_add(1, Ordering::SeqCst);
            let result = transaction
                .map(|()| DownloadResult::default())
                .map_err(|_| {
                    failed(
                        "synthetic_switch_unknown",
                        "switch outcome unknown",
                        "no write replay",
                    )
                });
            owner_service.finish_switch(&id, result);
            owner_terminals.fetch_add(1, Ordering::SeqCst);
            drop(permit);
            done.send(()).unwrap();
        });
        owner_handle.send(retained.abort_handle()).unwrap();
        retained.await.unwrap();
    });

    let mut release_protocol = None;
    let mut release_refresh = None;
    if let Some((entered, release)) = protocol_barrier {
        reached(entered).await;
        release_protocol = Some(release);
    } else {
        let (action, release) = physical.next().await;
        assert!(matches!(action, Action::RefreshAfterProfileSwitch { .. }));
        release_refresh = Some(release);
    }
    let saved = journal_fault.then(|| obstruct_journal(&fixture));
    let owner_handle = owner_observed.await.unwrap();
    cancel_waiter_and_check_exclusion(waiter, &service).await;
    assert!(radio.sim_channel_busy());
    // A radio job intentionally serializes the physical adapter. At Refresh
    // a second radio request would wait behind the held adapter, so inspect
    // the live reservation instead of blocking the controller on that queue.
    if stop_at != StopAt::Refresh {
        assert!(radio.reserve_esim().await.is_err());
    }
    assert!(!owner_handle.is_finished());
    assert_eq!(permit_drops.load(Ordering::SeqCst), 0);
    assert_eq!(releases.load(Ordering::SeqCst), 0);
    assert_eq!(terminals.load(Ordering::SeqCst), 0);
    service.begin_shutdown();
    radio.begin_shutdown();
    let shutdown_service = service.clone();
    let shutdown_radio = radio.clone();
    let final_receipts = receipts.clone();
    let final_archives = archives.clone();
    let mut shutdown = tokio::spawn(crate::shutdown::coordinate(
        crate::shutdown::Report::default(),
        async { crate::shutdown::Report::default() },
        async move {
            let mut report = crate::shutdown::Report::default();
            // Same production order as main: accepted eSIM owner first, radio
            // finalizer next; the real finalizer caches its exact receipt.
            report.check(
                "eSIM operation drain",
                shutdown_service.wait_for_shutdown().await,
            );
            report.extend(
                crate::shutdown::confirmed("module radio", || {
                    final_receipts.fetch_add(1, Ordering::SeqCst);
                    shutdown_radio.finish_shutdown_receipt()
                })
                .await,
            );
            report
        },
        async { crate::shutdown::Report::default() },
        async { crate::shutdown::Report::default() },
        async move {
            final_archives.fetch_add(1, Ordering::SeqCst);
            crate::shutdown::Report::default()
        },
    ));
    still_pending(&mut shutdown).await;
    assert_eq!(receipts.load(Ordering::SeqCst), 0);
    assert_eq!(archives.load(Ordering::SeqCst), 0);
    if let Some(release) = release_protocol {
        release.send(()).unwrap();
        let (action, release) = physical.next().await;
        assert!(matches!(action, Action::RefreshAfterProfileSwitch { .. }));
        release_refresh = Some(release);
    }
    // HTTP waiter is already gone, but the real owner reached final refresh.
    still_pending(&mut shutdown).await;
    assert!(service.operation_gate.load(Ordering::Acquire));
    assert!(radio.sim_channel_busy());
    assert_eq!(permit_drops.load(Ordering::SeqCst), 0);
    assert_eq!(releases.load(Ordering::SeqCst), 0);
    assert_eq!(terminals.load(Ordering::SeqCst), 0);
    assert_eq!(receipts.load(Ordering::SeqCst), 0);
    release_refresh
        .unwrap()
        .send(Ok(Outcome {
            submitted: true,
            complete: true,
            verified: true,
            cfun: Some(1),
            ..Outcome::default()
        }))
        .unwrap();
    reached(completed).await;
    let report = tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap();
    report.finish().unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !owner_handle.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the exact detached owner returned; no orphan running task");
    for counter in [&permit_drops, &releases, &terminals, &receipts, &archives] {
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
    assert!(!service.operation_gate.load(Ordering::Acquire));
    assert!(!service.active());
    physical.assert_esim_drained();
    physical.empty();
    assert!(radio.finish_shutdown_receipt().await.unwrap().is_empty());
    physical.empty();
    let observed = commands.lock().unwrap();
    let writes = observed
        .iter()
        .filter(|command| command.contains("BF31"))
        .count();
    assert_eq!(
        writes,
        usize::from(!(stop_at == StopAt::GetEid && journal_fault))
    );
    assert_eq!(
        observed
            .iter()
            .filter(|command| command.starts_with("AT+CCHC=1"))
            .count(),
        1
    );
    assert_eq!(
        observed
            .iter()
            .filter(|command| command.contains("0070800100"))
            .count(),
        1
    );
    assert_eq!(
        observed
            .iter()
            .filter(|command| command.starts_with("AT+CCHO="))
            .count(),
        1
    );
    drop(observed);
    if let Some(saved) = saved {
        assert!(service.journal_faulted.load(Ordering::Acquire));
        assert_eq!(
            service.current().unwrap().error_code.as_deref(),
            Some("esim_operation_interrupted")
        );
        assert!(!service.current().unwrap().recoverable);
        restore_journal(&fixture, &saved);
        let reopened = Service::open(&fixture.0).unwrap();
        assert!(!reopened.active());
        assert!(matches!(
            reopened.begin_switch(ICCID),
            Err(BeginError::ReconciliationRequired)
        ));
        assert!(!reopened.operation_gate.load(Ordering::Acquire));
        // Neither reopening nor attempting unsafe readmission submitted an APDU.
        assert_eq!(
            commands
                .lock()
                .unwrap()
                .iter()
                .filter(|command| command.contains("BF31"))
                .count(),
            writes
        );
    }
    println!(
        "composed {stop_at:?} journal_fault={journal_fault}: permit_drop=1 reservation_release=1 channel_close=1 terminal=1 radio_final_receipt=1 archive_drain=1 enable_apdus={writes}; no running owner"
    );
}

#[tokio::test]
async fn composed_geteid_cancel_retains_owner_through_final_refresh() {
    composed_case(StopAt::GetEid, false).await;
}
#[tokio::test]
async fn composed_geteid_journal_fault_cancel_prevents_write_and_drains() {
    composed_case(StopAt::GetEid, true).await;
}
#[tokio::test]
async fn composed_enable_cancel_unknown_never_replays_write() {
    composed_case(StopAt::Enable, false).await;
}
#[tokio::test]
async fn composed_enable_journal_fault_cancel_unknown_never_replays_write() {
    composed_case(StopAt::Enable, true).await;
}
#[tokio::test]
async fn composed_close_cancel_keeps_final_receipt_behind_owner() {
    composed_case(StopAt::Close, false).await;
}
#[tokio::test]
async fn composed_close_journal_fault_cancel_keeps_final_receipt_behind_owner() {
    composed_case(StopAt::Close, true).await;
}
#[tokio::test]
async fn composed_refresh_cancel_holds_permit_reservation_and_finalizer() {
    composed_case(StopAt::Refresh, false).await;
}
#[tokio::test]
async fn composed_refresh_journal_fault_cancel_holds_permit_reservation_and_finalizer() {
    composed_case(StopAt::Refresh, true).await;
}
