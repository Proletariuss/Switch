// Shared USB/serial AT-channel software boundary: real Service/ArchiveStore,
// protocol adapters and retained owners, with no device or network access.
use super::*;
use crate::module_esim_channel::{AtCommandTransport, ModuleEuiccChannel};

const CARD_A: [u8; 16] = [0x12; 16];
const CARD_B: [u8; 16] = [0x34; 16];
const ICCID: &str = "8985200014631959072";

mod composed_acceptance {
    include!("composed_acceptance_tests.rs");
}

struct AtStep {
    prefix: &'static str,
    expected_fragment: Option<&'static str>,
    reply: Result<String, crate::usb_at::Failure>,
    barrier: Option<(tokio::sync::oneshot::Sender<()>, Receiver<()>)>,
}

struct BarrierAt {
    steps: VecDeque<AtStep>,
    commands: Arc<Mutex<Vec<String>>>,
}

impl AtCommandTransport for BarrierAt {
    type Error = crate::usb_at::Failure;

    fn command(&mut self, command: &str, _: Duration, _: usize) -> Result<String, Self::Error> {
        let step = self.steps.pop_front().expect("no unplanned AT command");
        assert!(
            command.starts_with(step.prefix),
            "unexpected command {command}"
        );
        if let Some(fragment) = step.expected_fragment {
            assert!(
                command.contains(fragment),
                "unexpected protocol step {command}"
            );
        }
        self.commands.lock().unwrap().push(command.to_owned());
        if let Some((entered, release)) = step.barrier {
            entered.send(()).expect("controller observes barrier");
            release
                .recv_timeout(Duration::from_secs(10))
                .expect("controller releases physical owner");
        }
        step.reply
    }
}

fn cgla(bytes: &[u8]) -> String {
    let hex = encode_hex_upper(bytes);
    format!("+CGLA: {},\"{hex}\"\r\nOK\r\n", hex.len())
}

fn step(prefix: &'static str, reply: String) -> AtStep {
    AtStep {
        prefix,
        expected_fragment: None,
        reply: Ok(reply),
        barrier: None,
    }
}

fn apdu(fragment: &'static str, bytes: &[u8]) -> AtStep {
    AtStep {
        expected_fragment: Some(fragment),
        ..step("AT+CGLA=", cgla(bytes))
    }
}

fn barrier(step: &mut AtStep) -> (tokio::sync::oneshot::Receiver<()>, SyncSender<()>) {
    let (entered, observed) = tokio::sync::oneshot::channel();
    let (release, released) = mpsc::sync_channel(1);
    step.barrier = Some((entered, released));
    (observed, release)
}

async fn reached(receiver: tokio::sync::oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .expect("owner reached protocol barrier")
        .unwrap();
}

fn obstruct_journal(fixture: &JournalFixture) -> PathBuf {
    let journal = fixture.0.join("reader-esim-operation.json");
    let saved = fixture.0.join("original-operation.json");
    fs::rename(&journal, &saved).unwrap();
    fs::create_dir(&journal).unwrap();
    saved
}

fn restore_journal(fixture: &JournalFixture, saved: &Path) {
    let journal = fixture.0.join("reader-esim-operation.json");
    fs::remove_dir(&journal).unwrap();
    fs::rename(saved, journal).unwrap();
}

async fn cancel_waiter_and_check_exclusion(
    waiter: tokio::task::JoinHandle<()>,
    service: &Arc<Service>,
) {
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let admitted_calls = Arc::new(AtomicU64::new(0));
    let calls = admitted_calls.clone();
    let response = with_profile_mutation_gate(service, || async move {
        calls.fetch_add(1, Ordering::SeqCst);
        json_response(StatusCode::OK, &json!({}))
    })
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(admitted_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        service.begin_module_operation(),
        Err(ModuleOperationAdmissionError::Busy)
    ));
}

async fn pending_shutdown(service: &Arc<Service>) -> tokio::task::JoinHandle<Result<(), String>> {
    service.begin_shutdown();
    let drain_service = service.clone();
    let mut drain = tokio::spawn(async move { drain_service.wait_for_shutdown().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut drain)
            .await
            .is_err()
    );
    drain
}

#[tokio::test]
async fn shared_module_at_proof_write_fault_survives_waiter_cancel_and_shutdown() {
    let fixture = JournalFixture::new();
    let service = Arc::new(Service::open(&fixture.0).unwrap());
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut eid = apdu("BF3E", &scripted_eid_response(CARD_A));
    let (at_eid, release_eid) = barrier(&mut eid);
    let mut close = step("AT+CCHC=1", "OK\r\n".into());
    let (at_close, release_close) = barrier(&mut close);
    let mut at = BarrierAt {
        steps: VecDeque::from([
            step("AT+CCHO=", "+CCHO: 1\r\nOK\r\n".into()),
            apdu("0070000001", &[1, 0x90, 0]),
            apdu("A40400", &[0x90, 0]),
            eid,
            apdu("0070800100", &[0x90, 0]),
            close,
        ]),
        commands: commands.clone(),
    };
    let owner_service = service.clone();
    let (completed, completion) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        spawn_retained_operation(async move {
            tokio::task::spawn_blocking(move || {
                let permit = owner_service.begin().unwrap();
                assert!(owner_service.bind_reader_owner(&permit.id, "synthetic-module", 7));
                let mut channel =
                    ModuleEuiccChannel::open(&mut at, KNOWN_ISDR_APPLICATION_AIDS[0]).unwrap();
                let activation = ActivationCode::parse("LPA:1$smdp.example$fixture-match").unwrap();
                let mut observer = ReaderDownloadObserver {
                    service: &owner_service,
                    id: &permit.id,
                    confirmation: permit.confirmation,
                    cancelled: permit.cancelled,
                    matching_id: "fixture-match",
                    confirmation_code: "",
                    stage: "init_auth",
                };
                let result = install_profile(
                    &mut channel,
                    KNOWN_ISDR_APPLICATION_AIDS[0],
                    &format_eid(&CARD_A).unwrap(),
                    &activation,
                    "",
                    None,
                    &mut observer,
                );
                assert!(matches!(&result, Err(DownloadFailure::Failed { code, .. })
                    if code == "esim_operation_journal_failed"));
                channel.close().unwrap();
                assert!(at.steps.is_empty());
                owner_service.finish(&permit.id, result);
                drop(permit._ownership);
                drop(permit._operation_gate);
                completed.send(()).unwrap();
            })
            .await
            .unwrap();
        })
        .await
        .unwrap();
    });
    reached(at_eid).await;
    let saved = obstruct_journal(&fixture);
    cancel_waiter_and_check_exclusion(waiter, &service).await;
    let drain = pending_shutdown(&service).await;
    release_eid.send(()).unwrap();
    reached(at_close).await;
    assert!(service.journal_faulted.load(Ordering::Acquire));
    assert!(service.operation_gate.load(Ordering::Acquire));
    assert!(
        !drain.is_finished(),
        "channel cleanup still belongs to retained owner"
    );
    let observed = commands.lock().unwrap().clone();
    assert_eq!(observed.len(), 6);
    // Exactly channel open, MANAGE CHANNEL, SELECT, GetEID and both closes:
    // no authentication, package, rename/delete or EnableProfile APDU.
    assert!(observed[3].contains("BF3E"));
    assert!(observed.iter().all(|command| {
        !["BF38", "BF21", "BF31", "BF29", "BF33"]
            .iter()
            .any(|tag| command.contains(tag))
    }));
    restore_journal(&fixture, &saved);
    release_close.send(()).unwrap();
    reached(completion).await;
    tokio::time::timeout(Duration::from_secs(2), drain)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!service.operation_gate.load(Ordering::Acquire));
    assert!(!service.current().unwrap().recoverable);
    let reopened = Service::open(&fixture.0).unwrap();
    assert_eq!(
        reopened.current().unwrap().error_code.as_deref(),
        Some("esim_operation_interrupted")
    );
}

#[tokio::test]
async fn shared_module_at_unknown_switch_fault_keeps_card_proof_after_cancel_and_restart() {
    let fixture = JournalFixture::new();
    let service = Arc::new(Service::open(&fixture.0).unwrap());
    fn tlv(tag: &[u8], bytes: &[u8]) -> Vec<u8> {
        [tag, &[bytes.len() as u8], bytes].concat()
    }
    let fields = [
        tlv(&[0x5a], &encode_iccid(ICCID).unwrap()),
        tlv(&[0x9f, 0x70], &[0]),
    ]
    .concat();
    let mut profiles = tlv(&[0xbf, 0x2d], &tlv(&[0xa0], &tlv(&[0xe3], &fields)));
    profiles.extend_from_slice(&[0x90, 0]);
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut enable = AtStep {
        prefix: "AT+CGLA=",
        expected_fragment: Some("BF31"),
        reply: Err(crate::usb_at::Failure::new(
            StatusCode::BAD_GATEWAY,
            "synthetic_response_lost",
            "synthetic response lost after submission",
        )),
        barrier: None,
    };
    let (at_enable, release_enable) = barrier(&mut enable);
    let mut close = step("AT+CCHC=1", "OK\r\n".into());
    let (at_close, release_close) = barrier(&mut close);
    let mut at = BarrierAt {
        steps: VecDeque::from([
            step("AT+CCHO=", "+CCHO: 1\r\nOK\r\n".into()),
            apdu("0070000001", &[1, 0x90, 0]),
            apdu("A40400", &[0x90, 0]),
            apdu("BF2D", &profiles),
            apdu("BF3E", &scripted_eid_response(CARD_A)),
            enable,
            apdu("0070800100", &[0x90, 0]),
            close,
        ]),
        commands: commands.clone(),
    };
    let owner_service = service.clone();
    let (completed, completion) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        spawn_retained_operation(async move {
            tokio::task::spawn_blocking(move || {
                let permit = owner_service.begin_switch(ICCID).unwrap();
                assert!(owner_service.bind_reader_owner(&permit.id, "synthetic-module", 7));
                let mut channel =
                    ModuleEuiccChannel::open(&mut at, KNOWN_ISDR_APPLICATION_AIDS[0]).unwrap();
                let result = switch_profile_on_transport_with_proof(
                    &mut channel,
                    ICCID,
                    Some(KNOWN_ISDR_APPLICATION_AIDS[0]),
                    Arc::new(AtomicBool::new(false)),
                    Some((&owner_service, &permit.id)),
                );
                assert!(matches!(result, Err(SwitchFailure::Operation(_))));
                channel.close().unwrap();
                assert!(at.steps.is_empty());
                owner_service.finish_switch(
                    &permit.id,
                    Err(failed(
                        "synthetic_switch_unknown",
                        "synthetic switch outcome unknown",
                        "post-submit response lost",
                    )),
                );
                drop(permit);
                completed.send(()).unwrap();
            })
            .await
            .unwrap();
        })
        .await
        .unwrap();
    });
    reached(at_enable).await;
    let journal = fixture.0.join("reader-esim-operation.json");
    let armed_bytes = fs::read(&journal).unwrap();
    let persisted: Vec<PersistedOperation> = serde_json::from_slice(&armed_bytes).unwrap();
    assert!(matches!(persisted[0].proof, CardProof::Armed { .. }));
    assert!(commands.lock().unwrap().last().unwrap().contains("BF31"));
    let saved = obstruct_journal(&fixture);
    cancel_waiter_and_check_exclusion(waiter, &service).await;
    let drain = pending_shutdown(&service).await;
    release_enable.send(()).unwrap();
    reached(at_close).await;
    assert!(service.operation_gate.load(Ordering::Acquire));
    assert!(!drain.is_finished());
    release_close.send(()).unwrap();
    reached(completion).await;
    tokio::time::timeout(Duration::from_secs(2), drain)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(service.journal_faulted.load(Ordering::Acquire));
    assert!(!service.operation_gate.load(Ordering::Acquire));
    assert!(!service.current().unwrap().recoverable);
    assert!(matches!(
        service.begin_module_operation(),
        Err(ModuleOperationAdmissionError::UnsafeState)
    ));
    let identities_a = [(CARD_A.to_vec(), KNOWN_ISDR_APPLICATION_AIDS[0].to_vec())];
    let identities_b = [(CARD_B.to_vec(), KNOWN_ISDR_APPLICATION_AIDS[0].to_vec())];
    assert!(service.reconcile_after_overview_refresh("synthetic-module", 7, &identities_b));
    assert!(!service.current().unwrap().recoverable);
    assert!(!service.reconcile_after_overview_refresh("synthetic-module", 7, &identities_a));
    assert!(
        !service.current().unwrap().recoverable,
        "failed reconciliation persistence cannot unlock writes"
    );
    assert_eq!(fs::read(&saved).unwrap(), armed_bytes);
    let observed = commands.lock().unwrap();
    assert_eq!(observed.len(), 8);
    assert_eq!(
        observed
            .iter()
            .filter(|command| command.contains("BF31"))
            .count(),
        1,
        "post-submit unknown must never replay EnableProfile"
    );
    drop(observed);
    restore_journal(&fixture, &saved);
    let reopened = Service::open(&fixture.0).unwrap();
    let interrupted = fs::read(&journal).unwrap();
    assert_eq!(
        reopened.current().unwrap().error_code.as_deref(),
        Some("esim_operation_interrupted")
    );
    assert!(reopened.reconcile_after_overview_refresh("synthetic-module", 7, &identities_b));
    assert!(!reopened.current().unwrap().recoverable);
    assert_eq!(fs::read(&journal).unwrap(), interrupted);
    assert!(matches!(
        reopened.begin_switch(ICCID),
        Err(BeginError::ReconciliationRequired)
    ));
    assert!(reopened.reconcile_after_overview_refresh("another-module", 99, &identities_a));
    assert!(reopened.current().unwrap().recoverable);
    drop(
        reopened
            .begin_module_operation()
            .expect("only matching durable card proof clears fence"),
    );
}
