use super::*;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, StopBits};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

struct ProbePort {
    read_entered: SyncSender<()>,
    read_release: Receiver<()>,
    close_entered: SyncSender<()>,
    close_release: Receiver<()>,
    closed: Arc<AtomicBool>,
    found: bool,
}
impl Read for ProbePort {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.read_entered.send(()).unwrap();
        self.read_release.recv().unwrap();
        if self.found {
            let value = b"353936932685355\r\nOK\r\n";
            bytes[..value.len()].copy_from_slice(value);
            Ok(value.len())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "probe disconnected",
            ))
        }
    }
}
impl Write for ProbePort {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for ProbePort {
    fn drop(&mut self) {
        self.close_entered.send(()).unwrap();
        self.close_release.recv().unwrap();
        self.closed.store(true, Ordering::Release);
    }
}
impl serialport::SerialPort for ProbePort {
    fn name(&self) -> Option<String> {
        Some("/dev/cu.usbmodem-fixture".into())
    }
    fn baud_rate(&self) -> serialport::Result<u32> {
        Ok(115200)
    }
    fn data_bits(&self) -> serialport::Result<DataBits> {
        Ok(DataBits::Eight)
    }
    fn flow_control(&self) -> serialport::Result<FlowControl> {
        Ok(FlowControl::None)
    }
    fn parity(&self) -> serialport::Result<Parity> {
        Ok(Parity::None)
    }
    fn stop_bits(&self) -> serialport::Result<StopBits> {
        Ok(StopBits::One)
    }
    fn timeout(&self) -> Duration {
        AT_DISCOVERY_READ_TIMEOUT
    }
    fn set_baud_rate(&mut self, _: u32) -> serialport::Result<()> {
        Ok(())
    }
    fn set_data_bits(&mut self, _: DataBits) -> serialport::Result<()> {
        Ok(())
    }
    fn set_flow_control(&mut self, _: FlowControl) -> serialport::Result<()> {
        Ok(())
    }
    fn set_parity(&mut self, _: Parity) -> serialport::Result<()> {
        Ok(())
    }
    fn set_stop_bits(&mut self, _: StopBits) -> serialport::Result<()> {
        Ok(())
    }
    fn set_timeout(&mut self, _: Duration) -> serialport::Result<()> {
        Ok(())
    }
    fn write_request_to_send(&mut self, _: bool) -> serialport::Result<()> {
        Ok(())
    }
    fn write_data_terminal_ready(&mut self, _: bool) -> serialport::Result<()> {
        Ok(())
    }
    fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
        Ok(true)
    }
    fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
        Ok(true)
    }
    fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
        Ok(false)
    }
    fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
        Ok(true)
    }
    fn bytes_to_read(&self) -> serialport::Result<u32> {
        Ok(0)
    }
    fn bytes_to_write(&self) -> serialport::Result<u32> {
        Ok(0)
    }
    fn clear(&self, _: ClearBuffer) -> serialport::Result<()> {
        Ok(())
    }
    fn try_clone(&self) -> serialport::Result<Box<dyn serialport::SerialPort>> {
        Err(serialport::Error::new(
            serialport::ErrorKind::Unknown,
            "probe cannot clone",
        ))
    }
    fn set_break(&self) -> serialport::Result<()> {
        Ok(())
    }
    fn clear_break(&self) -> serialport::Result<()> {
        Ok(())
    }
}
struct Fixture {
    actor: ModemActor,
    read_entered: Receiver<()>,
    read_release: SyncSender<()>,
    close_entered: Receiver<()>,
    close_release: SyncSender<()>,
    closed: Arc<AtomicBool>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.read_release.try_send(());
        let _ = self.close_release.try_send(());
    }
}
impl Fixture {
    fn new(found: bool) -> Self {
        let (read_entered, read_wait) = mpsc::sync_channel(1);
        let (read_release, read_go) = mpsc::sync_channel(1);
        let (close_entered, close_wait) = mpsc::sync_channel(1);
        let (close_release, close_go) = mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let port = Mutex::new(Some(ProbePort {
            read_entered,
            read_release: read_go,
            close_entered,
            close_release: close_go,
            closed: closed.clone(),
            found,
        }));
        let actor = ModemActor::with_port_opener(move |path, baud| {
            assert_eq!(path, "/dev/cu.usbmodem-fixture");
            assert_eq!(baud, AT_DISCOVERY_BAUD_RATE);
            Ok(Box::new(port.lock().unwrap().take().unwrap()))
        })
        .unwrap();
        Self {
            actor,
            read_entered: read_wait,
            read_release,
            close_entered: close_wait,
            close_release,
            closed,
        }
    }
    fn discover(&self) -> tokio::task::JoinHandle<Result<DiscoveredATPort, ActorError>> {
        let actor = self.actor.clone();
        tokio::spawn(async move {
            actor
                .discover_at_port_using(|| Ok(vec!["/dev/cu.usbmodem-fixture".into()]))
                .await
        })
    }
    fn entered(receiver: &Receiver<()>) {
        // A timeout fails the test; only the actual worker signal proves entry.
        receiver.recv_timeout(Duration::from_secs(10)).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_success_waits_for_actual_probe_and_close() {
    let fixture = Fixture::new(true);
    let job = fixture.discover();
    Fixture::entered(&fixture.read_entered);
    assert!(!job.is_finished());
    fixture.read_release.send(()).unwrap();
    Fixture::entered(&fixture.close_entered);
    assert!(!fixture.closed.load(Ordering::Acquire));
    assert!(
        !job.is_finished(),
        "finding a port is not close confirmation"
    );
    fixture.close_release.send(()).unwrap();
    assert_eq!(job.await.unwrap().unwrap().port, "/dev/cu.usbmodem-fixture");
    assert!(fixture.closed.load(Ordering::Acquire));
    fixture.actor.shutdown().unwrap();
}

#[test]
fn actor_success_ack_is_not_sent_before_probe_handle_drop_completes() {
    let fixture = Fixture::new(true);
    let (reply, result) = mpsc::sync_channel(1);
    assert!(
        fixture
            .actor
            .client
            .sender
            .try_send(Command::DiscoverATPort {
                candidates: Some(vec!["/dev/cu.usbmodem-fixture".into()]),
                reply,
            })
            .is_ok()
    );
    Fixture::entered(&fixture.read_entered);
    fixture.read_release.send(()).unwrap();
    Fixture::entered(&fixture.close_entered);
    // Keep the real reply receiver here, with no competing task consuming it.
    // A send-before-drop implementation deterministically exposes its ACK.
    assert!(matches!(result.try_recv(), Err(TryRecvError::Empty)));
    fixture.close_release.send(()).unwrap();
    assert_eq!(
        result
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap()
            .port,
        "/dev/cu.usbmodem-fixture"
    );
    assert!(fixture.closed.load(Ordering::Acquire));
    fixture.actor.shutdown().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn not_found_authorizes_fallback_only_after_actual_close() {
    let fixture = Fixture::new(false);
    let job = fixture.discover();
    Fixture::entered(&fixture.read_entered);
    assert!(!job.is_finished());
    fixture.read_release.send(()).unwrap();
    Fixture::entered(&fixture.close_entered);
    assert!(
        !job.is_finished(),
        "failed probe still owns its closing port"
    );
    fixture.close_release.send(()).unwrap();
    assert_eq!(job.await.unwrap().unwrap_err().code(), "at_port_not_found");
    assert!(fixture.closed.load(Ordering::Acquire));
    fixture.actor.shutdown().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_observer_retains_probe_and_shutdown_cannot_overtake_close() {
    let fixture = Fixture::new(false);
    let job = fixture.discover();
    Fixture::entered(&fixture.read_entered);
    job.abort();
    assert!(job.await.unwrap_err().is_cancelled());
    // Queue a real close while the submitted probe is still inside read. The
    // cancelled observation is not an actor-close or USB-fallback receipt.
    let (close_reply, close_result) = mpsc::sync_channel(1);
    assert!(
        fixture
            .actor
            .client
            .sender
            .try_send(Command::Close { reply: close_reply })
            .is_ok()
    );
    assert!(matches!(close_result.try_recv(), Err(TryRecvError::Empty)));
    fixture.read_release.send(()).unwrap();
    Fixture::entered(&fixture.close_entered);
    assert!(!fixture.closed.load(Ordering::Acquire));
    assert!(matches!(close_result.try_recv(), Err(TryRecvError::Empty)));
    fixture.close_release.send(()).unwrap();
    close_result
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert!(fixture.closed.load(Ordering::Acquire));
}

#[tokio::test]
async fn confirmed_discovery_preserves_enumeration_failure_and_empty_scan() {
    let actor = ModemActor::with_port_opener(|_, _| panic!("no physical device allowed")).unwrap();
    assert_eq!(
        actor
            .discover_at_port_using(|| Err(ActorError::unavailable("enumeration failed")))
            .await
            .unwrap_err()
            .code(),
        "serial_not_open"
    );
    assert_eq!(
        actor
            .discover_at_port_using(|| Ok(Vec::new()))
            .await
            .unwrap_err()
            .code(),
        "at_port_not_found"
    );
    actor.shutdown().unwrap();
}

impl ModemActor {
    /// Startup fixtures exercise failure classification without device enumeration.
    pub(crate) fn discovery_error_for_test(code: &'static str) -> ActorError {
        ActorError::new(code, "isolated startup discovery fixture")
    }
}

#[test]
fn actor_not_found_ack_is_not_sent_before_failed_probe_handle_drop_completes() {
    let fixture = Fixture::new(false);
    let (reply, result) = mpsc::sync_channel(1);
    assert!(
        fixture
            .actor
            .client
            .sender
            .try_send(Command::DiscoverATPort {
                candidates: Some(vec!["/dev/cu.usbmodem-fixture".into()]),
                reply,
            })
            .is_ok()
    );
    Fixture::entered(&fixture.read_entered);
    fixture.read_release.send(()).unwrap();
    Fixture::entered(&fixture.close_entered);
    assert!(
        matches!(result.try_recv(), Err(TryRecvError::Empty)),
        "failed serial probe still physically owns its port until Drop returns"
    );
    fixture.close_release.send(()).unwrap();
    assert_eq!(
        result
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap_err()
            .code(),
        "at_port_not_found"
    );
    assert!(fixture.closed.load(Ordering::Acquire));
    fixture.actor.shutdown().unwrap();
}
