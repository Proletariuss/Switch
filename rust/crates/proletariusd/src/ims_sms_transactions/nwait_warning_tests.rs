use super::*;
use crate::ims_sms_transactions::{
    NativeReportOwner, NativeSmsArchive, NativeSmsArchiveWarning, archive_native_sms_event_to,
};
use crate::native_vowifi_supervisor::{NativeVoWiFiSupervisor, SessionGeneration};
use std::sync::{Arc, Mutex};

fn delivery(generation: SessionGeneration, tpdu: &[u8]) -> crate::native_ims_actor::Event {
    use proletarius_vowifi::sip_session_runtime::{RegisteredSipEvent, RegisteredSipMessage};
    let mut event = native_event(generation);
    let RegisteredSipEvent::Message { message, .. } = &mut event.event else {
        unreachable!()
    };
    let RegisteredSipMessage::Request { body, .. } = message.as_mut() else {
        unreachable!()
    };
    let mut rpdu = vec![1, 42, 0, 0, u8::try_from(tpdu.len()).unwrap()];
    rpdu.extend_from_slice(tpdu);
    *body = zeroize::Zeroizing::new(rpdu);
    event
}
fn raw_rows(store: &crate::multistandby_store::Store) -> usize {
    let value: serde_json::Value =
        serde_json::from_reader(store.snapshot_file().unwrap().unwrap()).unwrap();
    fn count(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(map) => {
                usize::from(
                    map.get("kind").and_then(serde_json::Value::as_str) == Some("undecoded"),
                ) + map.values().map(count).sum::<usize>()
            }
            serde_json::Value::Array(values) => values.iter().map(count).sum(),
            _ => 0,
        }
    }
    count(&value)
}

#[tokio::test]
async fn nwait_committed_warning_tracks_store_isolation_not_decode_flag() {
    let f = StoreFixture::new();
    let generation = SessionGeneration::from_value(3).unwrap();
    let archive = NativeSmsArchive::Nwait {
        store: f.store.clone(),
        profile_id: "A".into(),
    };
    let event = delivery(generation, &[3]);
    for _ in 0..2 {
        let commit = archive_native_sms_event_to(
            f.root.clone(),
            "",
            generation,
            &event,
            1_800_000_000_123,
            &archive,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            commit.archive_warning(),
            Some(NativeSmsArchiveWarning::RawRecordUnmerged)
        );
    }
    let reloaded = crate::multistandby_store::Store::open(&f.root).unwrap();
    assert_eq!(raw_rows(&reloaded), 1);
    assert!(reloaded.forwarding_page(0).unwrap().0.is_empty());
    let other = NativeSmsArchive::Nwait {
        store: reloaded.clone(),
        profile_id: "B".into(),
    };
    assert!(
        archive_native_sms_event_to(
            f.root.clone(),
            "",
            generation,
            &event,
            1_800_000_000_123,
            &other
        )
        .await
        .unwrap()
        .unwrap()
        .archive_warning()
        .is_some()
    );
    assert_eq!(raw_rows(&reloaded), 2);

    // Both fragments decode as text. A conflicting same index makes the
    // durable store isolate the second raw record rather than guessing a group.
    let part = |text| {
        vec![
            0x40, 3, 0x81, 0x88, 0xf8, 0, 8, 0x62, 0x90, 0x60, 0, 0, 0, 0, 8, 5, 0, 3, 7, 2, 1, 0,
            text,
        ]
    };
    let first = delivery(generation, &part(0x41));
    let second = delivery(generation, &part(0x42));
    assert!(
        archive_native_sms_event_to(
            f.root.clone(),
            "",
            generation,
            &first,
            1_800_000_000_123,
            &archive
        )
        .await
        .unwrap()
        .unwrap()
        .archive_warning()
        .is_none()
    );
    assert_eq!(
        archive_native_sms_event_to(
            f.root.clone(),
            "",
            generation,
            &second,
            1_800_000_000_123,
            &archive
        )
        .await
        .unwrap()
        .unwrap()
        .archive_warning(),
        Some(NativeSmsArchiveWarning::RawRecordUnmerged)
    );
    assert_eq!(raw_rows(&reloaded), 3);
    assert!(reloaded.forwarding_page(0).unwrap().0.is_empty());
}

#[tokio::test]
async fn nwait_warning_is_published_after_commit_before_ack_and_survives_unknown_write() {
    let f = StoreFixture::new();
    let mut supervisor = NativeVoWiFiSupervisor::default();
    let permit = supervisor
        .reserve(
            crate::native_vowifi_supervisor::ReaderSessionKey::new(
                "warning-reader",
                "8901234567890123456",
            )
            .unwrap(),
        )
        .unwrap();
    let generation = permit.generation();
    let (commands, mut input, _, _) = crate::native_ims_actor::channels();
    let (_lease, lease) = tokio::sync::watch::channel(false);
    let observed = Arc::new(Mutex::new(None));
    let output = observed.clone();
    let observer =
        crate::native_sms_consumer::ArchiveWarningObserver::new(move |generation, warning| {
            *output.lock().unwrap() = Some((generation, warning));
        });
    let consumer = crate::native_sms_consumer::Consumer::new(
        f.root.clone(),
        String::new(),
        generation,
        commands,
        permit.stop_signal(),
        lease,
        NativeReportOwner::new(Service::default(), "native-warning-session-756".into()).unwrap(),
    )
    .with_archive(NativeSmsArchive::Nwait {
        store: f.store.clone(),
        profile_id: "A".into(),
    })
    .with_archive_warning_observer(Some(observer));
    let context = native_report_context(generation);
    let receive = consumer.receive(delivery(generation, &[3]), 1_800_000_000_123, &context);
    let actor = async {
        let crate::native_ims_actor::Command::Respond {
            generation: seen,
            status,
            reply,
            ..
        } = input.recv().await.unwrap()
        else {
            panic!("expected SIP response first")
        };
        assert_eq!(seen, generation);
        assert_eq!(status, 200);
        assert_eq!(
            raw_rows(&crate::multistandby_store::Store::open(&f.root).unwrap()),
            1
        );
        assert_eq!(
            *observed.lock().unwrap(),
            Some((generation, NativeSmsArchiveWarning::RawRecordUnmerged))
        );
        reply.send(Ok(())).unwrap();
        let crate::native_ims_actor::Command::Send {
            request,
            generation: seen,
            reply,
            ..
        } = input.recv().await.unwrap()
        else {
            panic!("expected RP ACK second")
        };
        assert_eq!(seen, generation);
        assert_eq!(request.data.body, proletarius_sms::build_ack(42));
        reply
            .send(Err(std::io::Error::other("synthetic unconfirmed write")))
            .unwrap();
    };
    let (result, ()) = tokio::join!(receive, actor);
    assert!(matches!(result, Err("native_sms_report_write_unknown")));
    assert_eq!(
        *observed.lock().unwrap(),
        Some((generation, NativeSmsArchiveWarning::RawRecordUnmerged))
    );
    assert!(input.try_recv().is_err());
    assert_eq!(raw_rows(&f.store), 1);
}

#[tokio::test]
async fn nwait_archive_failure_or_stale_event_has_no_warning_or_success_ack() {
    let f = StoreFixture::new();
    let mut supervisor = NativeVoWiFiSupervisor::default();
    let permit = supervisor
        .reserve(
            crate::native_vowifi_supervisor::ReaderSessionKey::new(
                "failed-warning-reader",
                "8901234567890123456",
            )
            .unwrap(),
        )
        .unwrap();
    let generation = permit.generation();
    let (commands, mut input, _, _) = crate::native_ims_actor::channels();
    let (_lease, lease) = tokio::sync::watch::channel(false);
    let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = observed.clone();
    let consumer = crate::native_sms_consumer::Consumer::new(
        f.root.clone(),
        String::new(),
        generation,
        commands,
        permit.stop_signal(),
        lease,
        NativeReportOwner::new(Service::default(), "native-warning-failure-756".into()).unwrap(),
    )
    .with_archive(NativeSmsArchive::Nwait {
        store: f.store.clone(),
        profile_id: "A".into(),
    })
    .with_archive_warning_observer(Some(
        crate::native_sms_consumer::ArchiveWarningObserver::new(move |_, _| {
            output.fetch_add(1, Ordering::SeqCst);
        }),
    ));
    let context = native_report_context(generation);
    assert!(matches!(
        consumer
            .receive(
                delivery(
                    SessionGeneration::from_value(generation.value() + 1).unwrap(),
                    &[3]
                ),
                1_800_000_000_123,
                &context
            )
            .await,
        Err("native_sms_session_mismatch")
    ));
    std::fs::write(
        f.root.join("multistandby/store-v1.json"),
        b"invalid synthetic store",
    )
    .unwrap();
    assert!(matches!(
        consumer
            .receive(delivery(generation, &[3]), 1_800_000_000_123, &context)
            .await,
        Err("sms_store_unavailable")
    ));
    assert_eq!(observed.load(Ordering::SeqCst), 0);
    assert!(input.try_recv().is_err());
}
