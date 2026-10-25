//! In-memory control transport only: no USB/device, sockets, proxy or carrier.
use super::*;
use std::collections::VecDeque;

#[derive(Clone)]
struct Record {
    request: Vec<u8>,
    timeout: Duration,
}
enum Write {
    Ack,
    Silent,
    Delay(Duration, bool),
    Error(io::ErrorKind),
    Hold(mpsc::SyncSender<()>, Receiver<()>),
}
struct FixtureTransport {
    incoming: Mutex<Receiver<Vec<u8>>>,
    replies: mpsc::Sender<Vec<u8>>,
    writes: Arc<Mutex<Vec<Record>>>,
    script: Mutex<VecDeque<Write>>,
    closed: Arc<AtomicBool>,
}
impl Transport for FixtureTransport {
    fn read(&self, output: &mut [u8]) -> io::Result<usize> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        match lock(&self.incoming).recv_timeout(Duration::from_millis(5)) {
            Ok(bytes) => {
                output[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            Err(RecvTimeoutError::Timeout) => Err(io::ErrorKind::WouldBlock.into()),
            Err(RecvTimeoutError::Disconnected) => Err(io::ErrorKind::UnexpectedEof.into()),
        }
    }
    fn write_all(&self, _: &[u8]) -> io::Result<()> {
        panic!("core must pass remaining budget to write_all_timeout")
    }
    fn write_all_timeout(&self, request: &[u8], timeout: Duration) -> io::Result<()> {
        lock(&self.writes).push(Record {
            request: request.to_vec(),
            timeout,
        });
        let action = lock(&self.script).pop_front().unwrap_or(Write::Ack);
        let ack = match action {
            Write::Ack => true,
            Write::Silent => false,
            Write::Delay(delay, ack) => {
                thread::sleep(delay);
                ack
            }
            Write::Hold(entered, waiting) => {
                entered.send(()).unwrap();
                waiting.recv_timeout(Duration::from_secs(3)).unwrap();
                true
            }
            Write::Error(kind) => {
                return Err(io::Error::new(kind, "submitted write outcome unknown"));
            }
        };
        if ack {
            self.replies.send(response(request)).unwrap();
        }
        Ok(())
    }
    fn shutdown(&self) -> io::Result<()> {
        self.closed.store(true, Ordering::Release);
        let _ = self.replies.send(Vec::new());
        Ok(())
    }
}
struct Fixture {
    client: Client,
    writes: Arc<Mutex<Vec<Record>>>,
    replies: mpsc::Sender<Vec<u8>>,
}
impl Fixture {
    fn new(script: Vec<Write>) -> Self {
        Self::with_capacity(script, 1)
    }
    fn with_capacity(script: Vec<Write>, max_in_flight: usize) -> Self {
        let (replies, incoming) = mpsc::channel();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let client = Client::from_transport(
            Box::new(FixtureTransport {
                incoming: Mutex::new(incoming),
                replies: replies.clone(),
                writes: writes.clone(),
                script: Mutex::new(script.into()),
                closed: Arc::new(AtomicBool::new(false)),
            }),
            ClientOptions {
                max_in_flight,
                ..ClientOptions::default()
            },
        )
        .unwrap();
        Self {
            client,
            writes,
            replies,
        }
    }
    fn hook(&self, stage: RequestStage, hook: impl FnOnce() + Send + 'static) {
        lock(&self.client.inner.request_hooks).push((stage, Box::new(hook)));
    }
    fn clean(&self, writes: usize) {
        assert_eq!(lock(&self.writes).len(), writes);
        assert!(lock(&self.client.inner.transactions).is_empty());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.client.close().unwrap();
    }
}
fn response(request: &[u8]) -> Vec<u8> {
    let packet = decode_packet(request).unwrap();
    let mut meta = packet.meta;
    meta.message_control_flags = if meta.service == CONTROL_SERVICE {
        1
    } else {
        2
    };
    let mut output = vec![0; 64];
    let len = encode_packet(
        meta,
        &[Tlv::new(RESULT_TLV_KIND, &[0, 0, 0, 0])],
        &mut output,
    )
    .unwrap();
    output.truncate(len);
    output
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline, "fixture condition timed out");
        thread::sleep(Duration::from_millis(1));
    }
}
fn request(client: &Client, timeout: Duration) -> Result<OwnedPacket, ClientError> {
    client.send_request_with_timeout(0x12, 4, 0x90, &[], timeout)
}

#[test]
fn zero_or_unrepresentable_deadline_never_reserves_or_writes() {
    let fixture = Fixture::new(vec![]);
    for timeout in [Duration::ZERO, Duration::MAX] {
        assert!(matches!(
            request(&fixture.client, timeout),
            Err(ClientError::Timeout)
        ));
        fixture.clean(0);
    }
    request(&fixture.client, Duration::from_secs(1))
        .unwrap()
        .check_result()
        .unwrap();
    fixture.clean(1);
}

#[test]
fn encoding_and_post_gate_work_share_entry_deadline_and_cleanup_reservation() {
    for stage in [
        RequestStage::Reserved,
        RequestStage::Encoded,
        RequestStage::WriteAcquired,
    ] {
        let fixture = Fixture::new(vec![]);
        fixture.hook(stage, || thread::sleep(Duration::from_millis(80)));
        assert!(matches!(
            request(&fixture.client, Duration::from_millis(30)),
            Err(ClientError::Timeout)
        ));
        fixture.clean(0);
        request(&fixture.client, Duration::from_secs(1)).unwrap();
        fixture.clean(1);
    }
}

#[test]
fn encoding_failure_releases_the_single_reservation_without_transport_write() {
    let fixture = Fixture::new(vec![]);
    let huge = vec![0; usize::from(u16::MAX) + 1];
    assert!(matches!(
        fixture.client.send_request_with_timeout(
            0x12,
            4,
            0x90,
            &[Tlv::new(1, &huge)],
            Duration::from_secs(1)
        ),
        Err(ClientError::Encode(_))
    ));
    fixture.clean(0);
    request(&fixture.client, Duration::from_secs(1)).unwrap();
    fixture.clean(1);
}

#[test]
fn queued_deadline_expires_while_real_write_gate_remains_held_and_write_count_is_zero() {
    let fixture = Fixture::new(vec![]);
    let held = fixture.client.inner.writes.shutdown();
    let client = fixture.client.clone();
    let (sent, received) = mpsc::channel();
    let started = Instant::now();
    let worker = thread::spawn(move || {
        sent.send(request(&client, Duration::from_millis(60)))
            .unwrap();
    });
    let result = received.recv_timeout(Duration::from_millis(600)).unwrap();
    assert!(matches!(result, Err(ClientError::Timeout)));
    assert!(started.elapsed() < Duration::from_millis(600));
    fixture.clean(0);
    // The blocked writer has not released. Queue timeout cannot wait for it
    // and cannot submit the expired request when it eventually becomes free.
    drop(held);
    worker.join().unwrap();
    request(&fixture.client, Duration::from_secs(1)).unwrap();
    fixture.clean(1);
}

#[test]
fn transport_receives_only_budget_remaining_after_write_queue() {
    let fixture = Fixture::new(vec![]);
    let held = fixture.client.inner.writes.shutdown();
    let (sent, received) = mpsc::channel();
    fixture.hook(RequestStage::Encoded, move || sent.send(()).unwrap());
    let client = fixture.client.clone();
    let worker = thread::spawn(move || request(&client, Duration::from_secs(2)));
    received.recv_timeout(Duration::from_secs(1)).unwrap();
    thread::sleep(Duration::from_millis(200));
    drop(held);
    worker.join().unwrap().unwrap();
    fixture.clean(1);
    let budget = lock(&fixture.writes)[0].timeout;
    assert!(budget > Duration::ZERO);
    assert!(
        budget < Duration::from_millis(1900),
        "queue wait must reduce transport budget: {budget:?}"
    );
}

#[test]
fn transport_write_and_response_wait_do_not_each_receive_fresh_timeout() {
    let fixture = Fixture::new(vec![Write::Delay(Duration::from_millis(70), false)]);
    let started = Instant::now();
    assert!(matches!(
        request(&fixture.client, Duration::from_millis(120)),
        Err(ClientError::Timeout)
    ));
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(100));
    assert!(elapsed < Duration::from_millis(300));
    fixture.clean(1);
    assert!(lock(&fixture.writes)[0].timeout <= Duration::from_millis(120));
}

#[test]
fn overlong_submitted_transport_is_observed_until_return_and_never_becomes_success() {
    // A synchronous transport that ignores its contract cannot be interrupted
    // safely. Its ACK may already be buffered, but the expired call is not OK.
    let fixture = Fixture::new(vec![Write::Delay(Duration::from_millis(80), true)]);
    let started = Instant::now();
    assert!(matches!(
        request(&fixture.client, Duration::from_millis(30)),
        Err(ClientError::Timeout)
    ));
    assert!(started.elapsed() >= Duration::from_millis(80));
    fixture.clean(1); // submitted; this Timeout is not a zero-write guarantee
    assert!(lock(&fixture.writes)[0].timeout <= Duration::from_millis(30));
}

#[test]
fn submitted_transport_error_and_late_reply_do_not_replay_or_poison_follow_up() {
    let fixture = Fixture::new(vec![Write::Error(io::ErrorKind::TimedOut), Write::Ack]);
    let error = request(&fixture.client, Duration::from_secs(1)).unwrap_err();
    assert!(matches!(error, ClientError::Io(ref error) if error.kind() == io::ErrorKind::TimedOut));
    fixture.clean(1);
    let old = lock(&fixture.writes)[0].request.clone();
    fixture.replies.send(response(&old)).unwrap();
    wait_for(|| fixture.client.stats().unmatched_responses == 1);
    request(&fixture.client, Duration::from_secs(1)).unwrap();
    fixture.clean(2);
    let records = lock(&fixture.writes);
    assert_ne!(
        decode_packet(&records[0].request)
            .unwrap()
            .meta
            .transaction_id,
        decode_packet(&records[1].request)
            .unwrap()
            .meta
            .transaction_id
    );
}

#[test]
fn close_wakes_unsubmitted_queue_waiter_without_overtaking_live_writer() {
    let fixture = Fixture::new(vec![]);
    let held = fixture.client.inner.writes.shutdown();
    let (encoded, ready) = mpsc::channel();
    fixture.hook(RequestStage::Encoded, move || encoded.send(()).unwrap());
    let request_client = fixture.client.clone();
    let (sent, received) = mpsc::channel();
    let waiter = thread::spawn(move || {
        sent.send(request(&request_client, Duration::from_secs(5)))
            .unwrap();
    });
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let closer = fixture.client.clone();
    let (closed, closing) = mpsc::channel();
    let close = thread::spawn(move || {
        closed.send(closer.close()).unwrap();
    });
    wait_for(|| fixture.client.is_closed());
    assert!(matches!(
        received.recv_timeout(Duration::from_millis(600)).unwrap(),
        Err(ClientError::Closed)
    ));
    fixture.clean(0);
    assert!(matches!(
        closing.recv_timeout(Duration::from_millis(30)),
        Err(RecvTimeoutError::Timeout)
    ));
    drop(held);
    closing
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    waiter.join().unwrap();
    close.join().unwrap();
}

#[test]
fn close_after_gate_acquisition_is_rechecked_before_transport_invocation() {
    let fixture = Fixture::new(vec![]);
    let (entered, ready) = mpsc::channel();
    let (release, waiting) = mpsc::channel();
    fixture.hook(RequestStage::WriteAcquired, move || {
        entered.send(()).unwrap();
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
    });
    let request_client = fixture.client.clone();
    let waiter = thread::spawn(move || request(&request_client, Duration::from_secs(5)));
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let closer = fixture.client.clone();
    let close = thread::spawn(move || closer.close());
    wait_for(|| fixture.client.is_closed());
    release.send(()).unwrap();
    assert!(matches!(waiter.join().unwrap(), Err(ClientError::Closed)));
    close.join().unwrap().unwrap();
    fixture.clean(0);
}

#[test]
fn close_after_submission_fails_waiter_without_claiming_request_was_unsent() {
    let fixture = Fixture::new(vec![Write::Silent]);
    let (written, ready) = mpsc::channel();
    fixture.hook(RequestStage::Written, move || written.send(()).unwrap());
    let request_client = fixture.client.clone();
    let waiter = thread::spawn(move || request(&request_client, Duration::from_secs(5)));
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    fixture.client.close().unwrap();
    assert!(matches!(waiter.join().unwrap(), Err(ClientError::Closed)));
    fixture.clean(1); // close is not a non-submission receipt
}

#[test]
fn active_transport_write_does_not_make_an_expired_queued_request_submit_late() {
    let (entered, ready) = mpsc::sync_channel(1);
    let (release, waiting) = mpsc::channel();
    let fixture = Fixture::with_capacity(vec![Write::Hold(entered, waiting)], 2);
    let first_client = fixture.client.clone();
    let first = thread::spawn(move || request(&first_client, Duration::from_secs(2)));
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let second_client = fixture.client.clone();
    let (sent, received) = mpsc::channel();
    let second = thread::spawn(move || {
        sent.send(request(&second_client, Duration::from_millis(60)))
            .unwrap();
    });
    assert!(matches!(
        received.recv_timeout(Duration::from_millis(600)).unwrap(),
        Err(ClientError::Timeout)
    ));
    assert_eq!(lock(&fixture.writes).len(), 1);
    assert_eq!(lock(&fixture.client.inner.transactions).len(), 1);
    release.send(()).unwrap();
    first.join().unwrap().unwrap();
    second.join().unwrap();
    fixture.clean(1);
    request(&fixture.client, Duration::from_secs(1)).unwrap();
    fixture.clean(2);
}

#[test]
fn close_wakes_queue_but_still_joins_actual_active_transport_write() {
    let (entered, ready) = mpsc::sync_channel(1);
    let (release, waiting) = mpsc::channel();
    let fixture = Fixture::with_capacity(vec![Write::Hold(entered, waiting)], 2);
    let first_client = fixture.client.clone();
    let first = thread::spawn(move || request(&first_client, Duration::from_secs(2)));
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let second_client = fixture.client.clone();
    let (sent, received) = mpsc::channel();
    let second = thread::spawn(move || {
        sent.send(request(&second_client, Duration::from_secs(5)))
            .unwrap();
    });
    wait_for(|| lock(&fixture.client.inner.transactions).len() == 2);
    let closer = fixture.client.clone();
    let (closed, closing) = mpsc::channel();
    let close = thread::spawn(move || {
        closed.send(closer.close()).unwrap();
    });
    assert!(matches!(
        received.recv_timeout(Duration::from_millis(600)).unwrap(),
        Err(ClientError::Closed)
    ));
    assert_eq!(lock(&fixture.writes).len(), 1);
    assert!(matches!(
        closing.recv_timeout(Duration::from_millis(30)),
        Err(RecvTimeoutError::Timeout)
    ));
    release.send(()).unwrap();
    closing
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    // The admitted first write really happened. Its racing response/close may
    // be observed, but no result here asserts that it was never submitted.
    let observed = first.join().unwrap();
    assert!(observed.is_ok() || matches!(observed, Err(ClientError::Closed)));
    second.join().unwrap();
    close.join().unwrap();
    fixture.clean(1);
}
