//! Actual PTY bytes and native lifecycle entry; no Go frame reader or hardware.
use super::*;
use crate::availability::BackendAvailability;
use serialport::SerialPort as _;
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub(crate) const PRODUCER: &str = "native-serial-lifecycle-fixture";
pub(crate) const ICCID: &str = "8986012345678901234";
#[derive(Default)]
struct Script {
    commands: Mutex<Vec<String>>,
    gate_command: Mutex<Option<String>>,
    gated: AtomicBool,
    release: AtomicBool,
    stop: AtomicBool,
    opens: AtomicUsize,
    fail_first_open: AtomicBool,
    omit_phone: AtomicBool,
    reject_pdu: AtomicBool,
    cfun: AtomicUsize,
}
impl Script {
    fn response(&self, command: &str) -> String {
        match command {
            "AT+CMGF=0" if self.reject_pdu.load(Ordering::Acquire) => "ERROR\r\n".into(),
            "AT+CGSN" => "123456789012345\r\nOK\r\n".into(),
            "AT+CGMR" => "FIXTURE-REVISION\r\nOK\r\n".into(),
            "AT+QSIMSTAT?" => "+QSIMSTAT: 1,1\r\nOK\r\n".into(),
            "AT+CPIN?" => "+CPIN: READY\r\nOK\r\n".into(),
            "AT+QCCID" => format!("+QCCID: {ICCID}\r\nOK\r\n"),
            "AT+CIMI" => "460001234567890\r\nOK\r\n".into(),
            "AT+CNUM" if self.omit_phone.load(Ordering::Acquire) => "OK\r\n".into(),
            "AT+CNUM" => "+CNUM: \"\",\"+447700900123\",145\r\nOK\r\n".into(),
            "AT+CFUN?" => format!("+CFUN: {}\r\nOK\r\n", self.cfun.load(Ordering::SeqCst)),
            "AT+CREG?" => "+CREG: 0,1,\"0001\",\"0002\"\r\nOK\r\n".into(),
            "AT+CSQ" => "+CSQ: 20,0\r\nOK\r\n".into(),
            "AT+COPS?" => "+COPS: 0,2,\"46000\",7\r\nOK\r\n".into(),
            "AT+QNWINFO" => "+QNWINFO: \"FDD LTE\",\"46000\",\"LTE BAND 3\",1300\r\nOK\r\n".into(),
            "AT+CGDCONT?" => {
                "+CGDCONT: 1,\"IPV4V6\",\"fixture.apn\",\"0.0.0.0\",0,0\r\nOK\r\n".into()
            }
            "AT+QIMS?" => "+QIMS: 1\r\nOK\r\n".into(),
            "AT+QCFG=\"ims\"" => "+QCFG: \"ims\",1\r\nOK\r\n".into(),
            "AT+QCFG=\"usbnet\"" | "AT+QCFG=\"usbnet\"?" => "+QCFG: \"usbnet\",2\r\nOK\r\n".into(),
            "AT+CPMS?" => "+CPMS: \"SM\",0,50,\"SM\",0,50,\"SM\",0,50\r\nOK\r\n".into(),
            c if c.starts_with("AT+CFUN=") => {
                self.cfun.store(
                    c.trim_start_matches("AT+CFUN=").parse().unwrap(),
                    Ordering::SeqCst,
                );
                "OK\r\n".into()
            }
            c if c.starts_with("AT+CCHO=") => "ERROR\r\n".into(),
            _ => "OK\r\n".into(),
        }
    }
}
pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
    pub(crate) port: String,
    script: Arc<Script>,
    peer: Option<std::thread::JoinHandle<()>>,
    pub(crate) modem: ModemActor,
    pub(crate) usb: usb_at::Owner,
    pub(crate) radio: module_radio::Service,
    pub(crate) inbox: InboxState,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("s508-{}", crate::snapshot::new_instance_id()));
        std::fs::create_dir_all(&root).unwrap();
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        master.set_timeout(Duration::from_millis(20)).unwrap();
        let port = slave.name().unwrap();
        drop(slave);
        let script = Arc::new(Script::default());
        script.cfun.store(1, Ordering::SeqCst);
        let peer_script = script.clone();
        let peer = std::thread::spawn(move || {
            let mut pending = Vec::new();
            while !peer_script.stop.load(Ordering::Acquire) {
                let mut byte = [0u8];
                if master.read(&mut byte).ok() != Some(1) {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                if byte[0] != b'\n' {
                    pending.push(byte[0]);
                    continue;
                }
                let command = String::from_utf8(std::mem::take(&mut pending))
                    .unwrap()
                    .trim()
                    .to_owned();
                if command.is_empty() {
                    continue;
                }
                peer_script.commands.lock().unwrap().push(command.clone());
                let gated = {
                    let mut gate = peer_script.gate_command.lock().unwrap();
                    if gate.as_deref() == Some(&command) {
                        gate.take();
                        true
                    } else {
                        false
                    }
                };
                if gated {
                    peer_script.gated.store(true, Ordering::Release);
                    while !peer_script.release.load(Ordering::Acquire)
                        && !peer_script.stop.load(Ordering::Acquire)
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                let _ = master.write_all(peer_script.response(&command).as_bytes());
            }
        });
        let availability = BackendAvailability::new();
        let module_gate = availability.module_recovery().clone();
        let expected = port.clone();
        let opening = script.clone();
        let inbox = InboxState::for_go_producer("compatibility-fixture".into());
        let opened_inbox = inbox.clone();
        let modem = ModemActor::with_test_port_opener_and_module_recovery(
            move |path, baud| {
                assert_eq!(path, expected, "never open physical hardware");
                assert_eq!(baud, BAUD);
                assert_eq!(
                    opened_inbox.current_module_producer_id().as_deref(),
                    Some(PRODUCER),
                    "native source must be installed before the physical open"
                );
                opening.opens.fetch_add(1, Ordering::SeqCst);
                if opening.fail_first_open.swap(false, Ordering::SeqCst) {
                    return Err(ModemActor::discovery_error_for_test(
                        "serial_fixture_open_failed",
                    ));
                }
                Ok(serialport::new(path, 0)
                    .timeout(Duration::from_millis(20))
                    .open()
                    .unwrap())
            },
            module_gate.clone(),
        )
        .unwrap();
        let usb = usb_at::Owner::unavailable_for_tests(PRODUCER.into())
            .with_recovery_gate_for_tests(module_gate);
        let radio = module_radio::Service::new(
            root.clone(),
            usb.clone(),
            modem.clone(),
            availability,
            PRODUCER.into(),
        );
        Self {
            root,
            port,
            script,
            peer: Some(peer),
            modem,
            usb,
            radio,
            inbox,
        }
    }
    pub(crate) fn start(&self) -> Service {
        Service::start_serial_for_test(
            self.modem.clone(),
            self.usb.clone(),
            self.radio.clone(),
            self.inbox.clone(),
            PRODUCER.into(),
            proletarius_store::ArchiveStore::new(self.root.join("sms.json")),
            self.port.clone(),
        )
    }
    pub(crate) fn omit_phone(&self) {
        self.script.omit_phone.store(true, Ordering::Release);
    }
    pub(crate) fn command_count(&self) -> usize {
        self.script.commands.lock().unwrap().len()
    }
    pub(crate) fn open_count(&self) -> usize {
        self.script.opens.load(Ordering::Acquire)
    }
    fn gate(&self, command: &str) {
        *self.script.gate_command.lock().unwrap() = Some(command.into());
    }
    pub(crate) async fn finish(&self, service: &Service) {
        service.begin_shutdown();
        self.radio.finish_shutdown().await.unwrap();
        service.shutdown().await.unwrap();
        self.usb.shutdown().await.unwrap();
        self.modem
            .shutdown()
            .expect("final modem close acknowledgement");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.script.release.store(true, Ordering::Release);
        self.script.stop.store(true, Ordering::Release);
        if let Some(peer) = self.peer.take() {
            peer.join().unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(12), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_serial_startup_waits_for_real_init_then_publishes_metadata_without_go_reader() {
    let fixture = Fixture::new();
    fixture.gate("AT+CMGF=0");
    let service = fixture.start();
    until(|| fixture.script.gated.load(Ordering::Acquire)).await;
    assert_eq!(
        *service.selection().borrow(),
        Selection::Serial {
            port: fixture.port.clone()
        }
    );
    assert_eq!(
        fixture.inbox.current_module_producer_id().as_deref(),
        Some(PRODUCER)
    );
    assert!(fixture.modem.native_serial_managed());
    assert!(!fixture.usb.native_managed());
    assert!(!fixture.modem.native_serial_observation().ready);
    assert!(
        !fixture
            .script
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "AT+QCCID" || c.starts_with("AT+CMGL"))
    );
    fixture.script.release.store(true, Ordering::Release);
    until(|| fixture.modem.native_serial_observation().ready).await;
    let observation = fixture.modem.native_serial_observation();
    assert!(observation.initialized);
    assert_eq!(observation.metadata.unwrap().status["iccid"], ICCID);
    let trace = fixture.script.commands.lock().unwrap().clone();
    for command in [
        "AT",
        "ATE0",
        "AT+CMGF=0",
        "AT+CNMI=2,1,0,0,0",
        "AT+CLIP=1",
        "AT+QPCMV=1,2",
        "AT+CFUN?",
        "AT+QSIMSTAT?",
        "AT+QCCID",
    ] {
        assert!(
            trace.iter().any(|item| item == command),
            "missing physical stage {command}: {trace:?}"
        );
    }
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        fixture
            .script
            .commands
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.as_str() == "AT+QSIMSTAT?")
            .count()
            <= 2,
        "metadata probe must converge without self-triggering"
    );
    assert_eq!(service.last_error(), None);
    fixture.finish(&service).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serial_initial_open_failure_and_later_close_reconnect_only_the_selected_port() {
    let fixture = Fixture::new();
    fixture
        .script
        .fail_first_open
        .store(true, Ordering::Release);
    let service = fixture.start();
    until(|| {
        fixture.script.opens.load(Ordering::Acquire) == 1
            && fixture.modem.native_serial_observation().discovery_error
                == "serial_fixture_open_failed"
    })
    .await;
    let failed = fixture.modem.native_serial_observation();
    assert_eq!(failed.port, fixture.port);
    assert!(
        failed.terminal.is_none(),
        "an open failure cannot fabricate a physical close receipt"
    );
    assert!(matches!(
        *service.selection().borrow(),
        Selection::Serial { .. }
    ));
    assert!(!fixture.usb.native_managed());
    until(|| fixture.modem.native_serial_observation().ready).await;
    assert!(
        fixture
            .modem
            .native_serial_observation()
            .discovery_error
            .is_empty()
    );
    let old = fixture.modem.native_serial_session().unwrap();
    fixture.modem.close_native_serial(&old).await.unwrap();
    until(|| {
        fixture.modem.native_serial_observation().ready
            && fixture.modem.native_serial_observation().serial_session != Some(old.serial_session)
    })
    .await;
    let new = fixture.modem.native_serial_session().unwrap();
    assert_ne!(new.serial_session, old.serial_session);
    assert!(old.observation().terminal.unwrap().physical_closed);
    assert_eq!(fixture.script.opens.load(Ordering::Acquire), 3);
    assert_eq!(
        *service.selection().borrow(),
        Selection::Serial {
            port: fixture.port.clone()
        }
    );
    fixture.finish(&service).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_freezes_new_stages_but_retains_metadata_until_finalize_and_actual_close() {
    let fixture = Fixture::new();
    fixture.gate("AT+QCCID");
    let service = fixture.start();
    until(|| fixture.script.gated.load(Ordering::Acquire)).await;
    let original = fixture.modem.native_serial_session().unwrap();
    service.begin_shutdown();
    assert_eq!(
        fixture.modem.native_serial_observation().serial_session,
        Some(original.serial_session)
    );
    assert!(
        original.observation().terminal.is_none(),
        "begin must not close the cleanup transport"
    );
    fixture.radio.finish_shutdown().await.unwrap();
    let waiter = service.clone();
    let task = tokio::spawn(async move { waiter.shutdown().await });
    tokio::task::yield_now().await;
    assert!(
        !task.is_finished(),
        "a blocked physical metadata command is not completion"
    );
    task.abort();
    let _ = task.await;
    assert!(original.observation().terminal.is_none());
    fixture.script.release.store(true, Ordering::Release);
    service.shutdown().await.unwrap();
    assert!(original.observation().terminal.unwrap().physical_closed);
    assert!(fixture.modem.native_serial_session().is_none());
    assert_eq!(fixture.script.opens.load(Ordering::Acquire), 1);
    fixture.finish(&service).await;
}

#[tokio::test]
async fn corrupt_archive_blocks_serial_open_and_never_falls_back_to_usb() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("sms.json"), b"invalid-json").unwrap();
    let service = fixture.start();
    until(|| service.last_error() == Some("module_startup_archive_unavailable")).await;
    assert_eq!(fixture.script.opens.load(Ordering::Acquire), 0);
    assert!(!fixture.usb.native_managed());
    assert!(fixture.modem.native_serial_managed());
    fixture.finish(&service).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_pdu_configuration_preserves_native_metadata_and_esim_without_sms_reads_or_reopen() {
    let fixture = Fixture::new();
    fixture.script.reject_pdu.store(true, Ordering::Release);
    let mut state = crate::reader_pcsc_gateway_tests::state(&fixture.root);
    state.modem_actor = fixture.modem.clone();
    state.usb_at = fixture.usb.clone();
    state.module_radio = fixture.radio.clone();
    state.sms_inbox = fixture.inbox.clone();
    let service = fixture.start();
    until(|| fixture.modem.native_serial_observation().ready).await;
    let observation = fixture.modem.native_serial_observation();
    assert!(observation.initialized);
    assert_eq!(observation.metadata.unwrap().status["iccid"], ICCID);
    let session = observation.serial_session.unwrap();
    assert!(fixture.radio.native_serial_bootstrapped(session));
    assert!(crate::module_esim_transport::serial_route_available(&state));
    let actor = fixture.modem.clone();
    let sms = tokio::task::spawn_blocking(move || actor.serial_sms_observation())
        .await
        .unwrap()
        .unwrap();
    assert!(!sms.ready, "failed PDU setup must not become SMS ready");
    tokio::time::sleep(Duration::from_millis(80)).await;
    let trace = fixture.script.commands.lock().unwrap().clone();
    assert_eq!(
        trace.iter().filter(|c| c.as_str() == "AT+CMGF=0").count(),
        1
    );
    assert!(
        !trace
            .iter()
            .any(|c| c.starts_with("AT+CMGL") || c.starts_with("AT+CMGR")),
        "SMS reads after failed PDU setup: {trace:?}"
    );
    assert_eq!(fixture.open_count(), 1);
    assert_eq!(
        fixture.modem.native_serial_observation().serial_session,
        Some(session)
    );
    assert!(
        fixture
            .modem
            .native_serial_session()
            .unwrap()
            .observation()
            .terminal
            .is_none()
    );
    assert_eq!(service.last_error(), None);
    fixture.finish(&service).await;
}
