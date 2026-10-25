use proletarius_qmi_core::{Client, ClientError, ClientOptions, Transport};
use proletarius_qmi_wire::{PacketMeta, Tlv, decode_packet, encode_packet};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

struct MemoryTransport {
    incoming: Mutex<Receiver<Vec<u8>>>,
    incoming_wake: Sender<Vec<u8>>,
    outgoing: SyncSender<Vec<u8>>,
    closed: Arc<AtomicBool>,
}

impl Transport for MemoryTransport {
    fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "closed"));
        }
        match self
            .incoming
            .lock()
            .expect("incoming lock")
            .recv_timeout(Duration::from_millis(20))
        {
            Ok(frame) => {
                if frame.len() > buffer.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "frame too large",
                    ));
                }
                buffer[..frame.len()].copy_from_slice(&frame);
                Ok(frame.len())
            }
            Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "no fixture frame",
            )),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "fixture closed",
            )),
        }
    }

    fn write_all(&self, data: &[u8]) -> io::Result<()> {
        self.outgoing
            .send(data.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "fixture closed"))
    }

    fn shutdown(&self) -> io::Result<()> {
        self.closed.store(true, Ordering::Release);
        let _ = self.incoming_wake.send(Vec::new());
        Ok(())
    }
}

#[test]
fn ctl_client_ids_allocate_and_release_over_one_transport_owner() {
    let (client, requests, responses) = client_with_memory_transport();
    let allocator = client.clone();
    let allocate = thread::spawn(move || allocator.allocate_client_id(0x12));

    let request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    let packet = decode_packet(&request).unwrap();
    assert_eq!(packet.meta.service, 0);
    assert_eq!(packet.meta.message_id, 0x0022);
    assert_eq!(packet.find_tlv(0x01).unwrap().value, [0x12]);
    responses
        .send(response(
            &request,
            &[Tlv::new(0x02, &[0, 0, 0, 0]), Tlv::new(0x01, &[0x12, 0x07])],
        ))
        .unwrap();
    assert_eq!(allocate.join().unwrap().unwrap(), 0x07);

    let releaser = client.clone();
    let release = thread::spawn(move || releaser.release_client_id(0x12, 0x07));
    let request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    let packet = decode_packet(&request).unwrap();
    assert_eq!(packet.meta.message_id, 0x0023);
    assert_eq!(packet.find_tlv(0x01).unwrap().value, [0x12, 0x07]);
    responses
        .send(response(&request, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
        .unwrap();
    release.join().unwrap().unwrap();
    client.close().unwrap();
}

#[test]
fn concurrent_service_responses_are_matched_by_transaction_not_arrival_order() {
    let (client, requests, responses) = client_with_memory_transport();
    let one_client = client.clone();
    let two_client = client.clone();
    let first = thread::spawn(move || one_client.send_request(0x12, 3, 0x0090, &[]));
    let second = thread::spawn(move || two_client.send_request(0x12, 3, 0x0091, &[]));

    let request_a = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    let request_b = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    let decoded_a = decode_packet(&request_a).unwrap();
    let decoded_b = decode_packet(&request_b).unwrap();
    assert_ne!(decoded_a.meta.transaction_id, decoded_b.meta.transaction_id);

    responses
        .send(response(&request_b, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
        .unwrap();
    responses
        .send(response(&request_a, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
        .unwrap();

    let first = first.join().unwrap().unwrap();
    let second = second.join().unwrap().unwrap();
    assert_eq!(first.meta.message_id, 0x0090);
    assert_eq!(second.meta.message_id, 0x0091);
    client.close().unwrap();
}

#[test]
fn late_reply_after_deadline_is_discarded_without_poisoning_follow_up() {
    let (client, requests, responses) = client_with_memory_transport();
    let timed_client = client.clone();
    let timed = thread::spawn(move || {
        timed_client.send_request_with_timeout(0x12, 4, 0x0090, &[], Duration::from_millis(30))
    });
    let expired_request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(timed.join().unwrap(), Err(ClientError::Timeout)));
    responses
        .send(response(&expired_request, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
        .unwrap();

    let next_client = client.clone();
    let next = thread::spawn(move || next_client.send_request(0x12, 4, 0x0091, &[]));
    let next_request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ne!(
        decode_packet(&expired_request).unwrap().meta.transaction_id,
        decode_packet(&next_request).unwrap().meta.transaction_id
    );
    responses
        .send(response(&next_request, &[Tlv::new(0x02, &[0, 0, 0, 0])]))
        .unwrap();
    assert_eq!(next.join().unwrap().unwrap().meta.message_id, 0x0091);

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while client.stats().unmatched_responses == 0 && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(client.stats().unmatched_responses, 1);
    client.close().unwrap();
}

enum ReaderFinish {
    Normal,
    Panic,
    FatalIo,
}

struct GatedCloseTransport {
    entered: SyncSender<()>,
    release: Mutex<Receiver<ReaderFinish>>,
    shutdown_seen: Sender<()>,
    shutdown_calls: Arc<AtomicUsize>,
    fail_shutdown: bool,
    panic_shutdown: bool,
}

impl Transport for GatedCloseTransport {
    fn read(&self, _: &mut [u8]) -> io::Result<usize> {
        let _ = self.entered.try_send(());
        // Bound the fixture's wait so an assertion failure cannot leave an
        // unkillable test reader. No production timeout is introduced.
        match self
            .release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
        {
            Ok(ReaderFinish::Panic) => panic!("injected QMI reader panic"),
            Ok(ReaderFinish::FatalIo) => {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "reader lost"))
            }
            Ok(ReaderFinish::Normal) | Err(_) => Err(io::Error::from(io::ErrorKind::Interrupted)),
        }
    }

    fn write_all(&self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn shutdown(&self) -> io::Result<()> {
        self.shutdown_calls.fetch_add(1, Ordering::SeqCst);
        let _ = self.shutdown_seen.send(());
        assert!(!self.panic_shutdown, "injected transport shutdown panic");
        if self.fail_shutdown {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected transport shutdown failure",
            ))
        } else {
            Ok(())
        }
    }
}

fn gated_close_client(
    fail_shutdown: bool,
    panic_shutdown: bool,
) -> (Client, Sender<ReaderFinish>, Receiver<()>, Arc<AtomicUsize>) {
    let (entered, started) = mpsc::sync_channel(1);
    let (release, waiting) = mpsc::channel();
    let (shutdown_seen, shutdown) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let client = Client::from_transport(
        Box::new(GatedCloseTransport {
            entered,
            release: Mutex::new(waiting),
            shutdown_seen,
            shutdown_calls: calls.clone(),
            fail_shutdown,
            panic_shutdown,
        }),
        ClientOptions::default(),
    )
    .unwrap();
    started.recv_timeout(Duration::from_secs(1)).unwrap();
    (client, release, shutdown, calls)
}

fn close_observation(result: Result<(), ClientError>) -> Result<(), (io::ErrorKind, String)> {
    result.map_err(|error| match error {
        ClientError::Io(error) => (error.kind(), error.to_string()),
        other => panic!("unexpected close error: {other}"),
    })
}

#[test]
fn final_close_waits_for_reader_and_caches_all_failures_for_concurrent_callers() {
    for fail_shutdown in [false, true] {
        for panic_reader in [false, true] {
            let (client, release, shutdown, calls) = gated_close_client(fail_shutdown, false);
            let (reported, results) = mpsc::channel();
            let first_client = client.clone();
            let first_report = reported.clone();
            let first = thread::spawn(move || {
                first_report
                    .send(close_observation(first_client.close()))
                    .unwrap();
            });
            shutdown.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(client.is_closed());
            let second_client = client.clone();
            let second = thread::spawn(move || {
                reported
                    .send(close_observation(second_client.close()))
                    .unwrap();
            });
            assert!(
                matches!(
                    results.recv_timeout(Duration::from_millis(30)),
                    Err(RecvTimeoutError::Timeout)
                ),
                "closed admission or shutdown error must not bypass the live reader"
            );
            release
                .send(if panic_reader {
                    ReaderFinish::Panic
                } else {
                    ReaderFinish::Normal
                })
                .unwrap();
            let result = results.recv_timeout(Duration::from_secs(1)).unwrap();
            let concurrent = results.recv_timeout(Duration::from_secs(1)).unwrap();
            first.join().unwrap();
            second.join().unwrap();
            assert_eq!(result, concurrent);
            assert_eq!(result, close_observation(client.close()));
            assert_eq!(result.is_err(), fail_shutdown || panic_reader);
            if let Err((kind, message)) = result {
                assert_eq!(
                    message.contains("injected transport shutdown failure"),
                    fail_shutdown
                );
                assert_eq!(message.contains("QMI reader thread panicked"), panic_reader);
                assert_eq!(
                    kind,
                    if fail_shutdown {
                        io::ErrorKind::PermissionDenied
                    } else {
                        io::ErrorKind::Other
                    }
                );
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "shutdown must never replay"
            );
            assert!(client.take_events().is_none());
        }
    }
}

#[test]
fn reader_initiated_shutdown_failure_is_not_erased_by_closed_admission() {
    let (client, release, shutdown, calls) = gated_close_client(true, false);
    release.send(ReaderFinish::FatalIo).unwrap();
    shutdown.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(client.is_closed());
    let first = close_observation(client.close()).unwrap_err();
    assert_eq!(first.0, io::ErrorKind::PermissionDenied);
    assert!(first.1.contains("injected transport shutdown failure"));
    assert_eq!(close_observation(client.close()).unwrap_err(), first);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn transport_shutdown_panic_still_joins_reader_and_caches_unknown_result() {
    let (client, release, shutdown, calls) = gated_close_client(false, true);
    let closer = client.clone();
    let (finished, observed) = mpsc::channel();
    let worker = thread::spawn(move || {
        finished.send(close_observation(closer.close())).unwrap();
    });
    shutdown.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        observed.recv_timeout(Duration::from_millis(30)),
        Err(RecvTimeoutError::Timeout)
    ));
    release.send(ReaderFinish::Normal).unwrap();
    let first = observed
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap_err();
    worker.join().unwrap();
    assert_eq!(first.0, io::ErrorKind::Other);
    assert!(
        first
            .1
            .contains("shutdown panicked; physical release unconfirmed")
    );
    assert_eq!(close_observation(client.close()).unwrap_err(), first);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn reader_initiated_shutdown_panic_remains_unconfirmed_without_replay() {
    let (client, release, shutdown, calls) = gated_close_client(false, true);
    release.send(ReaderFinish::FatalIo).unwrap();
    shutdown.recv_timeout(Duration::from_secs(1)).unwrap();
    let first = close_observation(client.close()).unwrap_err();
    assert!(
        first
            .1
            .contains("shutdown panicked; physical release unconfirmed")
    );
    assert_eq!(close_observation(client.close()).unwrap_err(), first);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

fn client_with_memory_transport() -> (Client, Receiver<Vec<u8>>, Sender<Vec<u8>>) {
    let (outgoing, requests) = mpsc::sync_channel(8);
    let (responses, incoming) = mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let transport = MemoryTransport {
        incoming: Mutex::new(incoming),
        incoming_wake: responses.clone(),
        outgoing,
        closed,
    };
    let client = Client::from_transport(
        Box::new(transport),
        ClientOptions {
            default_request_timeout: Duration::from_secs(1),
            ..ClientOptions::default()
        },
    )
    .unwrap();
    (client, requests, responses)
}

fn response(request: &[u8], tlvs: &[Tlv<'_>]) -> Vec<u8> {
    let request = decode_packet(request).unwrap();
    let mut meta = PacketMeta::request(
        request.meta.service,
        request.meta.client_id,
        request.meta.transaction_id,
        request.meta.message_id,
    );
    meta.message_control_flags = if meta.service == 0 { 0x01 } else { 0x02 };
    let mut frame = vec![0; 128];
    let length = encode_packet(meta, tlvs, &mut frame).unwrap();
    frame.truncate(length);
    frame
}
