use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[tokio::test]
async fn receiver_finish_cancellation_and_concurrent_waiters_retain_actual_worker() {
    use std::{future::Future, task::Poll};

    let worker = ReceiveWorker::default();
    let (release, waiting) = tokio::sync::oneshot::channel::<()>();
    *worker.task.lock().unwrap() = Some(tokio::spawn(async move {
        waiting.await.unwrap();
    }));
    {
        let first = worker.finish();
        tokio::pin!(first);
        std::future::poll_fn(|context| {
            assert!(first.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(worker.task.lock().unwrap().is_none());
        // Dropping the waiter must leave the JoinHandle in shared state.
    }
    assert!(matches!(
        *worker.finish.try_lock().unwrap(),
        ReceiveFinish::Joining(_)
    ));
    let second = worker.finish();
    let concurrent = worker.finish();
    tokio::pin!(second, concurrent);
    std::future::poll_fn(|context| {
        assert!(second.as_mut().poll(context).is_pending());
        assert!(concurrent.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    release.send(()).unwrap();
    second.await.unwrap();
    concurrent.await.unwrap();
    worker.finish().await.unwrap();
}

#[tokio::test]
async fn receiver_join_failure_is_cached_for_every_shutdown_caller() {
    let worker = ReceiveWorker::default();
    *worker.task.lock().unwrap() = Some(tokio::spawn(async {
        panic!("injected stored SMS receiver failure");
    }));
    let first = worker.finish().await.unwrap_err();
    assert_eq!(first.code(), "serial_sms_receiver_join_failed");
    assert!(
        first
            .message
            .contains("injected stored SMS receiver failure")
    );
    let second = worker.finish().await.unwrap_err();
    assert_eq!(second.code(), first.code());
    assert_eq!(second.message, first.message);
}

fn event(storage: &str, index: u32) -> SmsStorageEvent {
    SmsStorageEvent {
        storage: storage.into(),
        index: index.to_string(),
    }
}

fn receive_state() -> ActorState {
    let mut state = ActorState::new();
    state.serial_session = 23;
    state.sms_generation = 5;
    state.sms_receive.enabled = true;
    state
}

#[test]
fn exhausted_receive_epoch_stays_disabled_across_repeated_invalidation() {
    let mut state = receive_state();
    state.sms_receive.epoch = u64::MAX;
    for _ in 0..3 {
        state.sms_receive.invalidate();
        assert_eq!(state.sms_receive.epoch, 0);
        assert!(!state.sms_receive.enabled);
        assert!(state.sms_receive.push(23, 5, None, None).is_err());
        assert!(state.sms_receive.pending.is_empty());
    }
}

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new(label: &str) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let switch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .expect("Switch root")
            .to_path_buf();
        let configured = std::env::temp_dir();
        let parent = if configured.starts_with(&switch_root) {
            configured
        } else {
            switch_root.join("build/tmp")
        };
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = parent.join(format!(
            "sms-receive-{label}-{}-{nonce}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }

    fn store(&self) -> proletarius_store::ArchiveStore {
        proletarius_store::ArchiveStore::new(self.0.join("sms-archive.json"))
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn duplicate_pending_cmti_coalesces_but_reused_active_slot_gets_followup() {
    let mut state = receive_state();
    let sms = event("ME", 7);
    for _ in 0..3 {
        assert!(state.sms_receive.observe(23, 5, Some(&sms)).unwrap());
    }
    assert_eq!(state.sms_receive.pending.len(), 1);
    let first = state.next_receive().unwrap();
    assert!(state.sms_receive.pending.is_empty());

    // An in-flight read may have captured an older message in this slot.
    // A later CMTI therefore requires its own subsequent storage snapshot.
    assert!(state.sms_receive.observe(23, 5, Some(&sms)).unwrap());
    assert_eq!(state.sms_receive.pending.len(), 1);
    let second = state.sms_receive.pending.front().unwrap().clone();
    assert_ne!(first.id, second.id);
    assert_eq!(second.index, Some(7));
    assert_eq!(state.next_receive(), Some(first.clone()));
    state.finish_receive(first.id);
    assert_eq!(state.next_receive(), Some(second));
}

#[test]
fn full_mailbox_coalesces_same_storage_without_erasing_other_storages() {
    let mut state = receive_state();
    for index in 1..=MAX_RECEIVE_JOBS as u32 {
        let storage = if index == 2 { "SM" } else { "ME" };
        state
            .sms_receive
            .observe(23, 5, Some(&event(storage, index)))
            .unwrap();
    }
    let original_sm = state
        .sms_receive
        .pending
        .iter()
        .find(|job| job.storage.as_deref() == Some("SM"))
        .unwrap()
        .clone();
    assert_eq!(state.sms_receive.pending.len(), MAX_RECEIVE_JOBS);
    state
        .sms_receive
        .observe(23, 5, Some(&event("ME", 100)))
        .unwrap();
    assert_eq!(state.sms_receive.pending.len(), MAX_RECEIVE_JOBS);
    assert!(
        state
            .sms_receive
            .pending
            .iter()
            .any(|job| job.storage.as_deref() == Some("ME") && job.index.is_none())
    );
    assert!(state.sms_receive.pending.contains(&original_sm));
    let ids: Vec<_> = state.sms_receive.pending.iter().map(|job| job.id).collect();
    state
        .sms_receive
        .observe(23, 5, Some(&event("ME", 101)))
        .unwrap();
    assert_eq!(
        state
            .sms_receive
            .pending
            .iter()
            .map(|job| job.id)
            .collect::<Vec<_>>(),
        ids
    );

    // No competing Go scheduler may be created when another storage cannot
    // fit: ownership remains native and the source message is retained.
    assert!(
        state
            .sms_receive
            .observe(23, 5, Some(&event("MT", 1)))
            .unwrap()
    );
    assert_eq!(state.sms_receive.pending.len(), MAX_RECEIVE_JOBS);
    assert!(state.sms_receive.pending.contains(&original_sm));
}

#[test]
fn repeated_next_returns_same_active_and_finish_is_idempotent() {
    let mut state = receive_state();
    state
        .sms_receive
        .observe(23, 5, Some(&event("ME", 1)))
        .unwrap();
    state
        .sms_receive
        .observe(23, 5, Some(&event("ME", 2)))
        .unwrap();
    let first = state.next_receive().unwrap();
    let second = state.sms_receive.pending.front().unwrap().clone();
    assert_eq!(state.next_receive(), Some(first.clone()));
    state.finish_receive(second.id);
    assert_eq!(state.next_receive(), Some(first.clone()));
    state.finish_receive(first.id);
    state.finish_receive(first.id);
    assert_eq!(state.next_receive(), Some(second.clone()));
    state.finish_receive(first.id);
    assert_eq!(state.next_receive(), Some(second.clone()));
    state.finish_receive(second.id);
    assert_eq!(state.next_receive(), None);
}

#[test]
fn generation_change_discards_pending_old_card_without_acknowledging_active_work() {
    let mut state = receive_state();
    state
        .sms_receive
        .observe(23, 5, Some(&event("ME", 1)))
        .unwrap();
    let active = state.next_receive().unwrap();
    state
        .sms_receive
        .observe(23, 5, Some(&event("ME", 2)))
        .unwrap();
    state
        .frame_input(b"+QSIMSTAT: 1,1\r\n+CMTI: \"ME\",3\r\n")
        .unwrap();
    assert_eq!(state.sms_generation, 6);
    assert_eq!(state.next_receive(), Some(active.clone()));
    assert_eq!(state.sms_receive.pending.len(), 1);
    let pending = state.sms_receive.pending.front().unwrap();
    assert_eq!(pending.generation, 6);
    assert_eq!(pending.index, Some(3));
    state.finish_receive(active.id);
    assert_eq!(state.next_receive().unwrap().generation, 6);
}

#[test]
fn fragmented_owned_cmti_preserves_marker_and_other_urc_callbacks() {
    let mut state = receive_state();
    state.frame_input(b"+CMTI: \"ME\",7\r").unwrap();
    assert!(state.sms_receive.pending.is_empty());
    state
        .frame_input(b"\nRING\r\n+CUSD: 0,\"balance\",15\r\n")
        .unwrap();
    assert_eq!(state.sms_receive.pending.len(), 1);
    let cmti = state.frames.pop_front().unwrap();
    state.command_events.push_back(cmti);
    let expected = b"+CMTI: \"ME\",7\r\n";
    let mut received = Vec::new();
    while received.len() < expected.len() {
        let fragment = state.take_frame(4).unwrap();
        assert!(fragment.sms_ingest_owned);
        assert_eq!(fragment.sms_generation, 5);
        assert_eq!(fragment.sms_event.as_ref().unwrap().index, "7");
        received.extend(fragment.data);
    }
    assert_eq!(received, expected);
    let ring = state.take_frame(MAX_READ_BYTES).unwrap();
    assert_eq!(ring.data, b"RING\r\n");
    assert!(!ring.sms_ingest_owned);
    let ussd = state.take_frame(MAX_READ_BYTES).unwrap();
    assert_eq!(ussd.ussd_event.unwrap().text.as_deref(), Some("balance"));
    assert!(!ussd.sms_ingest_owned);
    assert_eq!(state.sms_receive.pending.len(), 1);
    assert_eq!(state.queued_bytes, 0);
}

#[test]
fn malformed_cmti_does_not_break_framing_or_claim_native_ownership() {
    let mut state = receive_state();
    state
        .frame_input(b"+CMTI: \"ME\",oops\r\n+CMTI: \"ME\",0\r\n+CMTI: \"bad!\",7\r\nRING\r\n")
        .unwrap();
    assert!(state.sms_receive.pending.is_empty());
    for _ in 0..3 {
        let frame = state.take_frame(MAX_READ_BYTES).unwrap();
        assert!(!frame.sms_ingest_owned);
        assert!(frame.is_urc);
    }
    assert_eq!(state.take_frame(MAX_READ_BYTES).unwrap().data, b"RING\r\n");
}

#[cfg(unix)]
#[test]
fn disable_retires_epoch_and_pending_work_but_manual_scan_remains_available() {
    let (_master, slave) = serialport::TTYPort::pair().unwrap();
    let mut state = receive_state();
    state.port = Some(Box::new(slave));
    state
        .sms_receive
        .observe(23, 5, Some(&event("ME", 7)))
        .unwrap();
    let old_fence = state.sms_receive.pending.front().unwrap().fence;
    state.configure_receive(false).unwrap();
    assert!(!state.sms_receive.enabled);
    assert!(state.sms_receive.pending.is_empty());
    assert_ne!(state.sms_receive.epoch, old_fence.receive_epoch);
    assert_eq!(
        state.check_receive_fence(Some(old_fence)).unwrap_err().code,
        "sms_receive_session_changed"
    );
    assert!(
        !state
            .sms_receive
            .observe(23, 5, Some(&event("ME", 8)))
            .unwrap()
    );
    state.scan_receive().unwrap();
    let manual = state.next_receive().unwrap();
    assert_eq!(manual.storage, None);
    assert_eq!(manual.index, None);
    assert_eq!(manual.generation, 5);
    assert_eq!(manual.fence.receive_epoch, state.sms_receive.epoch);
    state.check_receive_fence(Some(manual.fence)).unwrap();
}

#[cfg(unix)]
fn actor_with_ports(ports: Vec<serialport::TTYPort>) -> ModemActor {
    let ports: Arc<Mutex<VecDeque<Box<dyn serialport::SerialPort>>>> = Arc::new(Mutex::new(
        ports
            .into_iter()
            .map(|port| Box::new(port) as Box<dyn serialport::SerialPort>)
            .collect(),
    ));
    ModemActor::with_port_opener(move |_, _| {
        ports
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ActorError::unavailable("no fixture port remains"))
    })
    .unwrap()
}

#[cfg(unix)]
#[test]
fn close_blocks_reopen_until_actual_finish_then_old_zero_generation_fence_is_rejected() {
    use serialport::SerialPort as _;
    let (_first_master, first_slave) = serialport::TTYPort::pair().unwrap();
    let (mut second_master, second_slave) = serialport::TTYPort::pair().unwrap();
    second_master
        .set_timeout(Duration::from_millis(100))
        .unwrap();
    let first_path = first_slave.name().unwrap();
    let second_path = second_slave.name().unwrap();
    let actor = actor_with_ports(vec![first_slave, second_slave]);
    actor.open(first_path, 115_200).unwrap();
    actor.scan_sms_receive().unwrap();
    let old = actor
        .submit(|reply| Command::NextSmsReceive { reply }, RPC_TIMEOUT)
        .unwrap()
        .unwrap();
    assert_eq!(old.generation, 0);
    actor.close().unwrap();
    assert_eq!(
        actor.open(second_path.clone(), 115_200).unwrap_err().code,
        "serial_resource_busy"
    );
    actor
        .submit(
            |reply| Command::FinishSmsReceive {
                id: old.id + 1,
                reply,
            },
            RPC_TIMEOUT,
        )
        .unwrap();
    assert_eq!(
        actor.open(second_path.clone(), 115_200).unwrap_err().code,
        "serial_resource_busy"
    );
    for _ in 0..2 {
        actor
            .submit(
                |reply| Command::FinishSmsReceive { id: old.id, reply },
                RPC_TIMEOUT,
            )
            .unwrap();
    }
    actor.open(second_path, 115_200).unwrap();
    actor.confirm_sms_generation_fenced(0, None).unwrap();
    assert_eq!(
        actor
            .read_stored_sms_fenced(Some("ME".into()), None, 0, Some(old.fence))
            .unwrap_err()
            .code,
        "sms_receive_session_changed"
    );
    assert_eq!(
        actor
            .confirm_sms_generation_fenced(0, Some(old.fence))
            .unwrap_err()
            .code,
        "sms_receive_session_changed"
    );
    assert_eq!(
        actor
            .delete_archived_sms_fenced(7, "001122".into(), 0, Some(old.fence))
            .unwrap_err()
            .code,
        "sms_receive_session_changed"
    );
    let mut unexpected = [0u8; 64];
    assert!(
        matches!(second_master.read(&mut unexpected), Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
    );
    actor.close().unwrap();
}

#[tokio::test]
async fn idle_receive_worker_keeps_only_weak_actor_and_exits_on_last_client_drop() {
    let directory = TestDirectory::new("weak-idle");
    let (sender, receiver) = mpsc::sync_channel(ACTOR_QUEUE_CAPACITY);
    let wake = Arc::new(Notify::new());
    let client = Arc::new(ActorClient {
        module_recovery: Default::default(),
        final_close: Mutex::new(None),
        actor_thread: Mutex::new(None),
        native_shared: Arc::new(NativeShared::default()),
        card_shared: Arc::new(CardShared::default()),
        module_esim_cache: Arc::new(crate::module_esim_transport::SerialCache::default()),
        live_serial_session: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        call_capture: Arc::new(std::sync::Mutex::new(None)),
        cellular_suppressed: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        sender,
        wake: wake.clone(),
    });
    let weak = Arc::downgrade(&client);
    let task = tokio::spawn(receive_loop(
        weak.clone(),
        Arc::new(ReceiveWorker::default()),
        wake,
        directory.store(),
    ));
    let (_receiver, first_request) = tokio::task::spawn_blocking(move || {
        let request = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        (receiver, request)
    })
    .await
    .unwrap();
    match first_request {
        Command::NextSmsReceive { reply } => reply.send(Ok(None)).unwrap(),
        _ => panic!("idle worker must query the existing actor queue"),
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while Arc::strong_count(&client) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert!(weak.upgrade().is_none());
    assert!(!directory.0.join("sms-archive.json").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn native_cmti_worker_reads_archives_then_deletes_without_go_ingest_request() {
    run_native_cmti_fixture(false).await;
}

#[cfg(unix)]
#[tokio::test]
async fn native_cmti_archive_failure_retains_me_source_without_any_delete_command() {
    run_native_cmti_fixture(true).await;
}

#[cfg(unix)]
async fn run_native_cmti_fixture(archive_failure: bool) {
    use serialport::SerialPort as _;
    let directory = TestDirectory::new("native-cmti");
    let archive_path = directory.0.join("sms-archive.json");
    if archive_failure {
        fs::write(&archive_path, b"invalid fixture archive JSON").unwrap();
    }
    let (mut master, slave) = serialport::TTYPort::pair().unwrap();
    master.set_timeout(Duration::from_secs(3)).unwrap();
    let path = slave.name().unwrap();
    let actor = actor_with_ports(vec![slave]);
    actor.open(path, 115_200).unwrap();
    actor.configure_sms_receive(true).unwrap();
    actor.ensure_receive_worker(directory.store());

    let archive_for_responder = archive_path.clone();
    let (finished, completed) = tokio::sync::oneshot::channel();
    let responder = thread::spawn(move || {
        master.write_all(b"+CMTI: \"ME\",7\r\nRING\r\n").unwrap();
        let mut commands = vec![
            (
                "AT+CPMS?",
                "+CPMS: \"ME\",1,10,\"ME\",1,10,\"ME\",1,10\r\nOK\r\n",
            ),
            (
                "AT+CMGL=4",
                "+CMGL: 7,0,,17\r\n0000038188f8000862906000000000020041\r\nOK\r\n",
            ),
            (
                "AT+CPMS?",
                "+CPMS: \"ME\",1,10,\"ME\",1,10,\"ME\",1,10\r\nOK\r\n",
            ),
            (
                "AT+CMGR=7",
                "+CMGR: 0,,17\r\n0000038188f8000862906000000000020041\r\nOK\r\n",
            ),
            ("AT+CMGD=7", "OK\r\n"),
        ];
        if archive_failure {
            commands.truncate(2);
        }
        let mut pending = Vec::new();
        for (expected, response) in commands {
            let command = loop {
                if let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                    let bytes = pending.drain(..=end).collect::<Vec<_>>();
                    break String::from_utf8(bytes).unwrap().trim().to_owned();
                }
                let mut buffer = [0u8; 128];
                let count = master
                    .read(&mut buffer)
                    .expect("read native worker AT command");
                pending.extend_from_slice(&buffer[..count]);
            };
            assert_eq!(command, expected);
            if expected == "AT+CMGD=7" {
                let archived: Value = serde_json::from_slice(
                    &fs::read(&archive_for_responder)
                        .expect("durable archive precedes source deletion"),
                )
                .unwrap();
                assert_eq!(archived.as_array().unwrap().len(), 1);
                assert_eq!(archived[0]["sender"], "888");
                assert_eq!(archived[0]["content"], "A");
            }
            master.write_all(response.as_bytes()).unwrap();
        }
        assert!(pending.is_empty());
        // Return the still-open master so the async test can verify that no
        // additional AT command was emitted after actual worker completion.
        assert!(finished.send(master).is_ok());
    });

    let mut master = tokio::time::timeout(Duration::from_secs(8), completed)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while Arc::strong_count(&actor.client) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut saw_cmti = false;
    let mut saw_ring = false;
    for _ in 0..10 {
        let frame = actor.read(MAX_READ_BYTES, 100).unwrap();
        if frame.sms_event.is_some() {
            assert!(frame.sms_ingest_owned);
            assert_eq!(frame.sms_event.as_ref().unwrap().index, "7");
            saw_cmti = true;
        }
        if frame.data == b"RING\r\n" {
            assert!(!frame.sms_ingest_owned);
            saw_ring = true;
        }
        if saw_cmti && saw_ring {
            break;
        }
    }
    assert!(
        saw_cmti && saw_ring,
        "native ingestion must retain compatibility URC delivery"
    );
    master.set_timeout(Duration::from_millis(100)).unwrap();
    let mut unexpected = [0u8; 64];
    assert!(
        matches!(master.read(&mut unexpected), Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
    );
    if archive_failure {
        assert_eq!(
            fs::read(&archive_path).unwrap(),
            b"invalid fixture archive JSON"
        );
    } else {
        let archived: Value = serde_json::from_slice(&fs::read(&archive_path).unwrap()).unwrap();
        assert_eq!(archived.as_array().unwrap().len(), 1);
    }
    actor.close().unwrap();
    tokio::time::timeout(Duration::from_secs(3), actor.finish_sms_receiver())
        .await
        .unwrap()
        .unwrap();
    assert!(actor.receive_worker.task.lock().unwrap().is_none());
    drop(actor);
    responder.join().unwrap();
}

#[cfg(unix)]
#[test]
fn initial_disabled_custom_window_never_schedules_or_relabels_cmti() {
    let (_master, slave) = serialport::TTYPort::pair().unwrap();
    let mut state = ActorState::new();
    state.port = Some(Box::new(slave));
    state.serial_session = 1;
    assert!(!state.sms_receive.enabled);
    assert!(!state.sms_receive.configured);
    state.frame_input(b"+CMTI: \"ME\",1\r\n").unwrap();
    assert!(state.sms_receive.pending.is_empty());
    assert!(!state.frames.front().unwrap().sms_ingest_owned);

    // A custom callback/disabled-read configuration owns the initial window.
    state.configure_receive(false).unwrap();
    state.frame_input(b"+CMTI: \"ME\",2\r\n").unwrap();
    assert!(state.sms_receive.pending.is_empty());
    assert!(state.frames.iter().all(|frame| !frame.sms_ingest_owned));
    state.configure_receive(true).unwrap();
    assert!(state.sms_receive.pending.is_empty());
    assert!(state.frames.iter().all(|frame| !frame.sms_ingest_owned));
    state.frame_input(b"+CMTI: \"ME\",3\r\n").unwrap();
    assert_eq!(state.sms_receive.pending.len(), 1);
    assert_eq!(state.sms_receive.pending.front().unwrap().index, Some(3));
    assert!(state.frames.back().unwrap().sms_ingest_owned);
}

#[cfg(unix)]
#[test]
fn first_enable_adopts_undelivered_frames_once_and_later_toggles_preserve_markers() {
    let (_master, slave) = serialport::TTYPort::pair().unwrap();
    let mut state = ActorState::new();
    state.port = Some(Box::new(slave));
    state.serial_session = 1;
    state
        .frame_input(b"+CMTI: \"ME\",1\r\n+CMTI: \"ME\",2\r\n")
        .unwrap();
    let command_event = state.frames.pop_front().unwrap();
    state.command_events.push_back(command_event);
    assert!(state.sms_receive.pending.is_empty());
    state.configure_receive(true).unwrap();
    assert!(state.sms_receive.configured);
    assert_eq!(state.sms_receive.pending.len(), 2);
    assert!(
        state
            .frames
            .iter()
            .chain(state.command_events.iter())
            .all(|frame| frame.sms_ingest_owned)
    );
    state.configure_receive(true).unwrap();
    assert_eq!(state.sms_receive.pending.len(), 2);

    state.configure_receive(false).unwrap();
    assert!(state.sms_receive.pending.is_empty());
    state.frame_input(b"+CMTI: \"ME\",3\r\n").unwrap();
    assert!(!state.frames.back().unwrap().sms_ingest_owned);
    state.configure_receive(true).unwrap();
    assert!(state.sms_receive.pending.is_empty());
    let first = state.take_frame(MAX_READ_BYTES).unwrap();
    let second = state.take_frame(MAX_READ_BYTES).unwrap();
    let third = state.take_frame(MAX_READ_BYTES).unwrap();
    assert!(first.sms_ingest_owned && second.sms_ingest_owned);
    assert!(!third.sms_ingest_owned);
    state.frame_input(b"+CMTI: \"ME\",4\r\n").unwrap();
    assert_eq!(state.sms_receive.pending.len(), 1);
    assert_eq!(state.sms_receive.pending.front().unwrap().index, Some(4));
}

#[tokio::test]
async fn explicit_finish_joins_worker_after_outstanding_actor_reply_is_released() {
    use std::future::Future as _;
    use std::task::Poll;
    let directory = TestDirectory::new("explicit-finish");
    let (sender, receiver) = mpsc::sync_channel(ACTOR_QUEUE_CAPACITY);
    let actor = ModemActor {
        compatibility_rpc: false,
        client: Arc::new(ActorClient {
            module_recovery: Default::default(),
            final_close: Mutex::new(None),
            actor_thread: Mutex::new(None),
            native_shared: Arc::new(NativeShared::default()),
            card_shared: Arc::new(CardShared::default()),
            module_esim_cache: Arc::new(crate::module_esim_transport::SerialCache::default()),
            live_serial_session: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            call_capture: Arc::new(std::sync::Mutex::new(None)),
            cellular_suppressed: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            sender,
            wake: Arc::new(Notify::new()),
        }),
        receive_worker: Arc::new(ReceiveWorker::default()),
    };
    actor.ensure_receive_worker(directory.store());
    let (_receiver, next) = tokio::task::spawn_blocking(move || {
        let command = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        (receiver, command)
    })
    .await
    .unwrap();
    let reply = match next {
        Command::NextSmsReceive { reply } => reply,
        _ => panic!("worker must first query the existing actor queue"),
    };
    // The production caller has already closed the serial session. Hold the
    // last outstanding queue reply to prove shutdown waits for the real task.
    let finish = actor.finish_sms_receiver();
    tokio::pin!(finish);
    std::future::poll_fn(|context| {
        assert!(
            finish.as_mut().poll(context).is_pending(),
            "finish returned before the worker exited"
        );
        Poll::Ready(())
    })
    .await;
    assert!(actor.receive_worker.stopping.load(Ordering::Acquire));
    reply.send(Ok(None)).unwrap();
    tokio::time::timeout(Duration::from_secs(2), finish)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(Arc::strong_count(&actor.client), 1);
    assert!(actor.receive_worker.task.lock().unwrap().is_none());
    actor.finish_sms_receiver().await.unwrap();
    actor.ensure_receive_worker(directory.store());
    assert!(actor.receive_worker.task.lock().unwrap().is_none());
    assert!(!directory.0.join("sms-archive.json").exists());
}

#[cfg(unix)]
#[test]
fn retired_receive_fence_distinguishes_unissued_delete_from_uncertain_sent_delete() {
    use serialport::SerialPort as _;
    for (step, expected_reason) in [
        (
            StoredSmsDeleteStep::VerifyReadback,
            "receive_session_changed",
        ),
        (StoredSmsDeleteStep::Delete, "delete_outcome_uncertain"),
    ] {
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_millis(100)).unwrap();
        let mut state = receive_state();
        state.port = Some(Box::new(slave));
        let retired_fence = ReceiveFence {
            session: state.serial_session,
            receive_epoch: state.sms_receive.epoch,
        };
        // Keep the same open port and SMS generation: this invalidation comes
        // only from the receive epoch, not a different SIM or disconnected I/O.
        state.sms_receive.invalidate();
        let (reply, result) = mpsc::sync_channel(1);
        state.advance_stored_sms_delete(
            StoredSmsDeleteTransaction {
                index: 7,
                pdu: "001122".into(),
                expected_sms_generation: state.sms_generation,
                receive_fence: Some(retired_fence),
                step,
                reply,
            },
            "ok",
            &["+CMGR: 0,,2".into(), "001122".into()],
        );
        let result = result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(!result.deleted);
        assert_eq!(result.reason, expected_reason);
        assert!(state.pending_command.is_none());
        assert!(state.pending_normal_commands.is_empty());
        assert!(state.pending_high_commands.is_empty());
        let mut unexpected = [0u8; 64];
        assert!(
            matches!(master.read(&mut unexpected), Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
        );
    }
}

#[test]
fn shutdown_waits_for_full_actor_queue_and_actual_close_acknowledgement() {
    let (sender, receiver) = mpsc::sync_channel(1);
    let actor = ModemActor {
        compatibility_rpc: false,
        client: Arc::new(ActorClient {
            module_recovery: Default::default(),
            final_close: Mutex::new(None),
            actor_thread: Mutex::new(None),
            native_shared: Arc::new(NativeShared::default()),
            card_shared: Arc::new(CardShared::default()),
            module_esim_cache: Arc::new(crate::module_esim_transport::SerialCache::default()),
            live_serial_session: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            call_capture: Arc::new(std::sync::Mutex::new(None)),
            cellular_suppressed: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            sender,
            wake: Arc::new(Notify::new()),
        }),
        receive_worker: Arc::new(ReceiveWorker::default()),
    };
    let (old_reply, _old_result) = mpsc::sync_channel(1);
    assert!(
        actor
            .client
            .sender
            .try_send(Command::ConfirmSmsGeneration {
                expected_sms_generation: 0,
                receive_fence: None,
                reply: old_reply,
            })
            .is_ok()
    );
    let (entered, started) = mpsc::sync_channel(0);
    let (finished, returned) = mpsc::sync_channel(1);
    let shutdown = thread::spawn(move || {
        entered.send(()).unwrap();
        actor
            .shutdown()
        .expect("actual final shutdown and thread join");
        let _ = finished.send(());
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    // The queue stays full while shutdown enters send. This bounded channel
    // wait detects an early try_send failure without using a timing sleep.
    assert!(matches!(
        returned.recv_timeout(Duration::from_millis(100)),
        Err(RecvTimeoutError::Timeout)
    ));
    match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
        Command::ConfirmSmsGeneration { reply, .. } => reply.send(Ok(())).unwrap(),
        _ => panic!("the pre-existing queued command must remain first"),
    }
    let close_reply = match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
        Command::Finalize { reply } => reply,
        _ => panic!("shutdown must enqueue the terminal Finalize command"),
    };
    // Receipt of Finalize establishes queue admission. Keep its ACK delayed:
    // teardown must wait for the actor's final cleanup before joining.
    assert!(matches!(
        returned.recv_timeout(RPC_TIMEOUT + Duration::from_millis(200)),
        Err(RecvTimeoutError::Timeout)
    ));
    close_reply.send(Ok(())).unwrap();
    returned.recv_timeout(Duration::from_secs(2)).unwrap();
    shutdown.join().unwrap();
}

#[cfg(unix)]
#[test]
fn actor_close_preserves_uncertain_inflight_delete_but_fails_unissued_delete() {
    for step in [
        StoredSmsDeleteStep::VerifyStorage,
        StoredSmsDeleteStep::VerifyReadback,
        StoredSmsDeleteStep::Delete,
    ] {
        let (_master, slave) = serialport::TTYPort::pair().unwrap();
        let mut state = receive_state();
        state.port = Some(Box::new(slave));
        state.current_path = Some("/dev/fixture-only".into());
        state.current_baud = 115_200;
        let (delete_reply, delete_result) = mpsc::sync_channel(1);
        state.pending_command = Some(PendingCommand {
            deadline: Instant::now() + SMS_DELETE_STEP_TIMEOUT,
            started: Instant::now(),
            lines: Vec::new(),
            response_bytes: 0,
            overflow: false,
            follow_up: None,
            expected_sms_generation: None,
            high_priority: false,
            completion: CommandCompletion::DeleteArchivedSms(StoredSmsDeleteTransaction {
                index: 7,
                pdu: "001122".into(),
                expected_sms_generation: state.sms_generation,
                receive_fence: Some(ReceiveFence {
                    session: state.serial_session,
                    receive_epoch: state.sms_receive.epoch,
                }),
                step,
                reply: delete_reply,
            }),
        });
        let (close_reply, closed) = mpsc::sync_channel(1);
        let opener: PortOpener = Box::new(|_, _| panic!("Close must never open a port"));
        state.process(Command::Close { reply: close_reply }, &opener);
        closed
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        let completion = delete_result.recv_timeout(Duration::from_secs(1)).unwrap();
        if matches!(step, StoredSmsDeleteStep::Delete) {
            let result = completion.unwrap();
            assert!(!result.deleted);
            assert_eq!(result.reason, "delete_outcome_uncertain");
        } else {
            assert_eq!(completion.unwrap_err().code, "serial_not_open");
        }
        assert!(state.port.is_none());
        assert!(state.current_path.is_none());
        assert_eq!(state.current_baud, 0);
        assert!(state.pending_command.is_none());
        assert!(state.pending_high_commands.is_empty());
        assert!(state.pending_normal_commands.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn native_rebind_reports_receive_restore_failure_instead_of_ready_ack() {
    let (_master, slave) = serialport::TTYPort::pair().unwrap();
    let mut state = ActorState::new();
    state.serial_session = 17;
    state.port = Some(Box::new(slave));
    state.last_radio_receive_enabled = true;
    state.sms_receive.epoch = 0;
    let (reply, result) = mpsc::sync_channel(1);
    let opener: PortOpener = Box::new(|_, _| panic!("restore cannot open"));
    state.process(
        Command::CallModeRadioInitialize {
            serial_session: 17,
            deadline: Instant::now() + Duration::from_secs(2),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reply,
        },
        &opener,
    );
    let deferred = state.initialization.deferred.as_mut().unwrap();
    deferred.transaction.step = 6;
    deferred.transaction.result.probe_succeeded = true;
    deferred.transaction.result.sms_pdu_ready = true;
    deferred.ready_at = Instant::now();
    state.poll_initialization();
    assert_eq!(result.recv().unwrap().unwrap_err().code, "serial_not_open");
    assert!(matches!(state.initialization.cached, Some(Err(_))));
    assert!(!state.sms_receive.enabled);
}

#[tokio::test]
async fn card_release_wakes_receive_for_cmti_already_observed_during_transaction() {
    let (_master, slave) = serialport::TTYPort::pair().unwrap();
    let mut state = ActorState::new();
    state.port = Some(Box::new(slave));
    state.serial_session = 1;
    state.serial_sms.owner_epoch = "fixture-epoch".into();
    let wake = Arc::new(tokio::sync::Notify::new());
    state.sms_receive = ReceiveQueue::new(wake.clone());
    state.configure_receive(true).unwrap();
    while let Some(work) = state.next_receive() {
        state.finish_receive(work.id);
    }
    wake.notified().await;
    state.card_shared.managed.store(true, Ordering::Release);
    let binding = SerialCardBinding {
        owner_epoch: state.serial_sms.owner_epoch.clone(),
        serial_session: state.serial_session,
        sim_generation: state.sms_generation,
    };
    let (reply, begun) = mpsc::sync_channel(1);
    state.process_card_request(CardRequest::Begin { binding, reply });
    let permit = begun.recv().unwrap().unwrap();
    state.frame_input(b"+CMTI: \"SM\",7\r\n").unwrap();
    // The worker has consumed the CMTI notification but must not take its
    // work until the complete logical-channel transaction releases ownership.
    wake.notified().await;
    let (reply, result) = mpsc::sync_channel(1);
    assert!(
        state
            .admit_card_boundary(Command::NextSmsReceive { reply })
            .is_none()
    );
    assert!(result.recv().unwrap().unwrap().is_none());
    let (reply, ended) = mpsc::sync_channel(1);
    state.process_card_request(CardRequest::End { permit, reply });
    ended.recv().unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(1), wake.notified())
        .await
        .unwrap();
    let work = state.next_receive().expect("earlier CMTI remains queued");
    assert_eq!(work.index, Some(7));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_early_nondefault_cmti_becomes_fresh_storage_scan_after_probe() {
    use serialport::SerialPort as _;
    let (mut master, slave) = serialport::TTYPort::pair().unwrap();
    master.set_timeout(Duration::from_secs(5)).unwrap();
    let path = slave.name().unwrap();
    let port = Mutex::new(Some(slave));
    let actor = ModemActor::with_port_opener(move |_, _| {
        Ok(Box::new(port.lock().unwrap().take().unwrap()) as Box<dyn serialport::SerialPort>)
    })
    .unwrap();
    actor.claim_native_serial_management().unwrap();
    let session = actor.open_native_serial(path, 115_200).await.unwrap();
    fn line(master: &mut serialport::TTYPort) -> String {
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0];
            assert_eq!(master.read(&mut byte).unwrap(), 1);
            bytes.push(byte[0]);
            if byte[0] == b'\n' {
                return String::from_utf8(bytes).unwrap().trim().into();
            }
        }
    }
    // Event is received before initialization/probing and has a non-default store.
    master.write_all(b"+CMTI: \"MT\",7\r\n").unwrap();
    let owner = actor.clone();
    let key = session.clone();
    let task = tokio::spawn(async move { owner.initialize_native_serial(&key).await });
    for command in [
        "AT",
        "ATE0",
        "AT+CMGF=0",
        "AT+CNMI=2,1,0,0,0",
        "AT+CLIP=1",
        "AT+QPCMV=1,2",
    ] {
        assert_eq!(line(&mut master), command);
        master.write_all(b"OK\r\n").unwrap();
    }
    assert!(task.await.unwrap().unwrap().sms_pdu_ready);
    assert!(
        actor
            .submit(|reply| Command::NextSmsReceive { reply }, RPC_TIMEOUT)
            .unwrap()
            .is_none()
    );
    let old_generation = actor.current_sms_generation().unwrap();
    let owner = actor.clone();
    let key = session.clone();
    let task = tokio::spawn(async move { owner.refresh_native_serial_metadata(&key).await });
    for (command, response) in [
        ("AT+QSIMSTAT?", "+QSIMSTAT: 1,1\r\nOK\r\n"),
        ("AT+CGSN", "123456789012345\r\nOK\r\n"),
        ("AT+CGMR", "508\r\nOK\r\n"),
        ("AT+CIMI", "234150123456789\r\nOK\r\n"),
        ("AT+CNUM", "OK\r\n"),
        ("AT+QCCID", "+QCCID: 8901234567890123456\r\nOK\r\n"),
        ("AT+CGDCONT?", "OK\r\n"),
        ("AT+QCFG=\"usbnet\"?", "OK\r\n"),
    ] {
        assert_eq!(line(&mut master), command);
        master.write_all(response.as_bytes()).unwrap();
    }
    let metadata = task.await.unwrap().unwrap();
    assert!(metadata.binding.sim_generation > old_generation);
    actor.commit_native_serial_ready(&session).await.unwrap();
    // Inspect the actual actor mailbox, without changing production visibility.
    let (reply, receive) = mpsc::sync_channel(1);
    actor
        .client
        .sender
        .send(Command::LegacyGate(Box::new(move |state, _| {
            let jobs = state
                .sms_receive
                .pending
                .iter()
                .map(|work| (work.generation, work.storage.clone(), work.index))
                .collect::<Vec<_>>();
            reply.send(jobs).unwrap();
        })))
        .unwrap();
    let jobs = receive.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(jobs.iter().all(|(generation, _, index)| *generation
        == metadata.binding.sim_generation
        && index.is_none()));
    assert!(
        jobs.iter()
            .any(|(_, storage, _)| storage.as_deref() == Some("MT"))
    );
    assert!(jobs.iter().any(|(_, storage, _)| storage.is_none()));
    // Initialization must retain native collection for later notifications,
    // not restore the compatibility owner's old disabled receive policy.
    let (observed, event_seen) = mpsc::sync_channel(1);
    actor.set_call_capture(Arc::new(move |_, line| {
        if line.contains("+CMTI") {
            let _ = observed.send(());
        }
        false
    }));
    master.write_all(b"+CMTI: \"SM\",9\r\n").unwrap();
    event_seen.recv_timeout(Duration::from_secs(3)).unwrap();
    let (reply, receive) = mpsc::sync_channel(1);
    actor
        .client
        .sender
        .send(Command::LegacyGate(Box::new(move |state, _| {
            reply
                .send((
                    state.sms_receive.enabled,
                    state
                        .sms_receive
                        .pending
                        .iter()
                        .any(|work| work.storage.as_deref() == Some("SM") && work.index == Some(9)),
                ))
                .unwrap();
        })))
        .unwrap();
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(3)).unwrap(),
        (true, true)
    );
    actor.close_native_serial(&session).await.unwrap();
}
