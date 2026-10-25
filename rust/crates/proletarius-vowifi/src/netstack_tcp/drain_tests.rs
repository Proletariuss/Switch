use super::*;
use crate::{TunnelConfiguration, netstack_runtime::TunnelNetwork};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct GatedRuntime {
    registry: TcpSocketRegistry,
    drop_incoming: Arc<AtomicBool>,
    retire: Arc<AtomicBool>,
    removed: Arc<AtomicBool>,
}
impl NetworkRuntime for GatedRuntime {
    type ApplicationError = Infallible;
    fn retire(&mut self, sockets: &mut SocketSet<'static>) {
        self.registry.retire(sockets);
        self.removed.store(true, Ordering::SeqCst);
    }
    fn service(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
    ) -> Result<(), Infallible> {
        if self.retire.load(Ordering::SeqCst) {
            self.registry.retire(sockets);
            assert_eq!(sockets.iter().count(), 0);
            self.removed.store(true, Ordering::SeqCst);
        } else {
            self.registry.service(interface, sockets);
        }
        Ok(())
    }
    fn transform_packet(
        &mut self,
        direction: PacketDirection,
        packet: Zeroizing<Vec<u8>>,
    ) -> Result<PacketTransformOutcome, io::Error> {
        if matches!(direction, PacketDirection::Inbound)
            && self.drop_incoming.load(Ordering::SeqCst)
        {
            Ok(PacketTransformOutcome::Discard)
        } else {
            Ok(PacketTransformOutcome::Forward(packet))
        }
    }
}

#[tokio::test]
async fn close_drains_real_bye_without_waiting_for_peer_read_before_cleanup() {
    real_drain_case(0).await;
}
#[tokio::test]
async fn cancelled_flush_resumes_same_frontier_until_real_tcp_ack() {
    real_drain_case(1).await;
}
#[tokio::test]
async fn cancelled_close_wait_keeps_socket_until_bye_ack_and_exact_removal() {
    real_drain_case(2).await;
}
#[tokio::test]
async fn flush_deadline_fails_delivery_but_confirms_exact_socket_removal() {
    real_drain_case(3).await;
}
#[tokio::test]
async fn explicit_runtime_retirement_does_not_ack_untransmitted_bye() {
    real_drain_case(4).await;
}

#[tokio::test]
async fn cancelled_pending_dial_aborts_even_after_handshake_ready_was_queued() {
    real_drain_case(5).await;
}

#[tokio::test]
async fn failed_partial_protocol_write_aborts_without_delivering_unknown_remainder() {
    real_drain_case(6).await;
}
#[tokio::test]
async fn drained_local_close_does_not_require_remote_fin_ack() {
    real_drain_case(7).await;
}

#[tokio::test]
async fn retired_socket_preserves_prior_real_flush_ack_but_rejects_new_writes() {
    real_drain_case(8).await;
}
#[tokio::test]
async fn final_response_immediate_local_close_preserves_peers_real_tcp_ack() {
    real_drain_case(9).await;
}
#[tokio::test]
async fn peer_reset_with_unacknowledged_data_cannot_confirm_flush() {
    real_drain_case(10).await;
}

#[tokio::test]
async fn missing_close_ack_does_not_forge_local_removal_proof() {
    let (sender, receiver) = oneshot::channel::<io::Result<()>>();
    drop(sender);
    let mut pending = PendingTcpClose {
        closed: Some(receiver),
        outcome: None,
    };
    assert_eq!(
        pending.wait_cancel_safe().await.unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert!(!pending.removal_confirmed());
    assert_eq!(
        pending.wait_cancel_safe().await.unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert!(!pending.removal_confirmed());
}

async fn real_drain_case(mode: u8) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let configuration = |ip: &str| TunnelConfiguration {
            ipv4_addresses: vec![ip.parse().unwrap()],
            ..Default::default()
        };
        let client_network = TunnelNetwork::new(&configuration("10.0.0.2")).unwrap();
        let server_network = TunnelNetwork::new(&configuration("10.20.30.9")).unwrap();
        let (client_wake, client_wake_rx) = watch::channel(0);
        let (server_wake, server_wake_rx) = watch::channel(0);
        let (client_control, client_registry) = TcpSocketRegistry::new(client_wake.clone());
        let (server_control, server_registry) = TcpSocketRegistry::new(server_wake.clone());
        let blocked = Arc::new(AtomicBool::new(false));
        let retiring = Arc::new(AtomicBool::new(false));
        let removed = Arc::new(AtomicBool::new(false));
        let client_runtime = GatedRuntime {
            registry: client_registry,
            drop_incoming: Arc::new(AtomicBool::new(false)),
            retire: retiring.clone(),
            removed: removed.clone(),
        };
        let server_runtime = GatedRuntime {
            registry: server_registry,
            drop_incoming: blocked.clone(),
            retire: Arc::new(AtomicBool::new(false)),
            removed: Arc::new(AtomicBool::new(false)),
        };
        let (client_tx, server_rx) = mpsc::channel(256);
        let (server_tx, client_rx) = mpsc::channel(256);
        let (stop_tx, stop) = watch::channel(false);
        let client_runner = client_network.run_managed(
            SocketSet::new(vec![]),
            client_rx,
            client_tx,
            stop.clone(),
            client_wake_rx,
            client_runtime,
        );
        let server_runner = server_network.run_managed(
            SocketSet::new(vec![]),
            server_rx,
            server_tx,
            stop,
            server_wake_rx,
            server_runtime,
        );
        let application = async {
            let listener = server_control.listen(5060, None).await.unwrap();
            let dial = client_control
                .dial("10.20.30.9:5060".parse().unwrap(), 32000)
                .await
                .unwrap();
            if mode == 5 {
                let server = listener.accept().await.unwrap();
                blocked.store(true, Ordering::SeqCst);
                let (_cancel, cancel) = watch::channel(true);
                let result = tokio::time::timeout(
                    Duration::from_millis(500),
                    dial.connect_until(
                        tokio::time::Instant::now() + Duration::from_secs(30),
                        cancel,
                    ),
                )
                .await
                .unwrap();
                let error = match result {
                    Ok(_) => panic!("cancelled dial was promoted"),
                    Err(error) => error,
                };
                assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                // The former Ready receipt does not turn cancelled preparation
                // into a managed close which waits for a blocked FIN ACK.
                let replacement = client_control
                    .dial("10.20.30.9:5060".parse().unwrap(), 32000)
                    .await
                    .unwrap();
                replacement.close().await.unwrap();
                blocked.store(false, Ordering::SeqCst);
                server_wake.send_modify(|n| *n += 1);
                server.close().await.unwrap();
                stop_tx.send(true).unwrap();
                return;
            }
            let (client, server) = tokio::join!(dial.connect(), listener.accept());
            let mut client = client.unwrap();
            let mut server = server.unwrap();
            if mode > 0 && !matches!(mode, 7 | 8 | 9) {
                blocked.store(true, Ordering::SeqCst);
            }
            let bye = b"BYE sip:peer SIP/2.0\r\nCSeq: 4 BYE\r\nContent-Length: 0\r\n\r\n";
            client.write_all(bye).await.unwrap();
            if mode == 9 {
                client.flush().await.unwrap();
                let mut request = vec![0; bye.len()];
                server.read_exact(&mut request).await.unwrap();
                assert_eq!(request, bye);
                let response = b"SIP/2.0 202 Accepted\r\nCSeq: 4 BYE\r\nContent-Length: 0\r\n\r\n";
                server.write_all(response).await.unwrap();
                let mut received = vec![0; response.len()];
                client.read_exact(&mut received).await.unwrap();
                assert_eq!(received, response);
                // SIP completion immediately retires local ownership. Never
                // wait for the peer flush before closing the receiving end.
                client.close().await.unwrap();
                server.flush().await.unwrap();
                assert_eq!(
                    server._lease.progress.lock().unwrap().acknowledged,
                    response.len() as u64
                );
                server.close().await.unwrap();
                stop_tx.send(true).unwrap();
                return;
            }
            if mode == 10 {
                while client._lease.progress.lock().unwrap().submitted == 0 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(client._lease.progress.lock().unwrap().acknowledged, 0);
                let mut peer_close = server.begin_abort();
                peer_close.wait_cancel_safe().await.unwrap();
                assert!(peer_close.removal_confirmed());
                blocked.store(false, Ordering::SeqCst);
                server_wake.send_modify(|n| *n += 1);
                // The real retransmission reaches a retired peer tuple and
                // produces RST, retaining the unacknowledged transmit queue.
                assert_eq!(
                    client.flush().await.unwrap_err().kind(),
                    io::ErrorKind::ConnectionReset
                );
                assert_eq!(client._lease.progress.lock().unwrap().acknowledged, 0);
                let mut close = client.begin_close();
                assert_eq!(
                    close.wait_cancel_safe().await.unwrap_err().kind(),
                    io::ErrorKind::ConnectionReset
                );
                assert!(close.removal_confirmed());
                stop_tx.send(true).unwrap();
                return;
            }
            if mode == 1 {
                let deadline;
                {
                    let mut flush = Box::pin(client.flush());
                    let mut cx = Context::from_waker(Waker::noop());
                    assert!(flush.as_mut().poll(&mut cx).is_pending());
                }
                deadline = client
                    ._lease
                    .progress
                    .lock()
                    .unwrap()
                    .flush_deadline
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
                {
                    let progress = client._lease.progress.lock().unwrap();
                    assert!(progress.acknowledged < progress.accepted);
                    assert_eq!(progress.flush_deadline, Some(deadline));
                }
                blocked.store(false, Ordering::SeqCst);
                server_wake.send_modify(|n| *n += 1);
                client.flush().await.unwrap();
                assert_eq!(
                    client._lease.progress.lock().unwrap().acknowledged,
                    bye.len() as u64
                );
            }
            if mode == 3 {
                client._lease.progress.lock().unwrap().flush_deadline =
                    Some(tokio::time::Instant::now() + Duration::from_millis(100));
                assert_eq!(
                    client.flush().await.unwrap_err().kind(),
                    io::ErrorKind::TimedOut
                );
            }
            if mode == 7 {
                client.flush().await.unwrap();
                blocked.store(true, Ordering::SeqCst);
            }
            if mode == 8 {
                client.flush().await.unwrap();
                retiring.store(true, Ordering::SeqCst);
                client_wake.send_modify(|n| *n += 1);
                while !removed.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
                client.flush().await.unwrap();
                assert_eq!(
                    client
                        .write_all(b"new-after-retire")
                        .await
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::BrokenPipe
                );
            }
            let mut pending = if mode == 6 {
                client.begin_abort()
            } else {
                client.begin_close()
            };
            if mode == 2 {
                {
                    let mut close = Box::pin(pending.wait_cancel_safe());
                    let mut cx = Context::from_waker(Waker::noop());
                    assert!(close.as_mut().poll(&mut cx).is_pending());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                assert!(!pending.removal_confirmed());
                blocked.store(false, Ordering::SeqCst);
                server_wake.send_modify(|n| *n += 1);
            }
            if mode == 4 {
                retiring.store(true, Ordering::SeqCst);
                client_wake.send_modify(|n| *n += 1);
            }
            if mode == 3 || mode == 4 || mode == 6 {
                let error =
                    tokio::time::timeout(Duration::from_millis(500), pending.wait_cancel_safe())
                        .await
                        .unwrap()
                        .unwrap_err();
                assert_eq!(
                    error.kind(),
                    if mode == 3 {
                        io::ErrorKind::TimedOut
                    } else {
                        io::ErrorKind::BrokenPipe
                    }
                );
                assert!(pending.removal_confirmed());
                assert_eq!(
                    pending.wait_cancel_safe().await.unwrap_err().kind(),
                    error.kind()
                );
                if mode == 4 {
                    assert!(removed.load(Ordering::SeqCst));
                    assert!(client_control.is_closed());
                }
                stop_tx.send(true).unwrap();
                return;
            }
            if mode == 7 {
                tokio::time::timeout(Duration::from_millis(500), pending.wait_cancel_safe())
                    .await
                    .unwrap()
                    .unwrap();
                blocked.store(false, Ordering::SeqCst);
                server_wake.send_modify(|n| *n += 1);
            } else {
                pending.wait_cancel_safe().await.unwrap();
            }
            assert!(pending.removal_confirmed());
            // No test-side receive barrier precedes flush/close: this read is
            // deliberately after the owner claims the exact cleanup outcome.
            let mut received = vec![0; bye.len()];
            server.read_exact(&mut received).await.unwrap();
            assert_eq!(received, bye);
            server.close().await.unwrap();
            if mode == 8 {
                stop_tx.send(true).unwrap();
                return;
            }
            let listener = server_control.listen(5060, None).await.unwrap();
            let dial = client_control
                .dial("10.20.30.9:5060".parse().unwrap(), 32000)
                .await
                .unwrap();
            let (client, server) = tokio::join!(dial.connect(), listener.accept());
            let (client, server) = (client.unwrap(), server.unwrap());
            let (a, b) = tokio::join!(client.close(), server.close());
            a.unwrap();
            b.unwrap();
            stop_tx.send(true).unwrap();
        };
        let (client, server, ()) = tokio::join!(client_runner, server_runner, application);
        client.unwrap();
        server.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_failed_accept_retains_real_close_token_until_exact_entry_removal() {
    let (wake, _) = watch::channel(0);
    let (_control, mut registry) = TcpSocketRegistry::new(wake);
    let mut sockets = SocketSet::new(vec![]);
    let (reply, result) = oneshot::channel();
    registry.create_listener(&mut sockets, 5060, None, None, reply);
    let mut listener = result.await.unwrap().unwrap();
    registry.entries[0]
        .ready
        .take()
        .unwrap()
        .send(Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "fake handshake rejection",
        )))
        .unwrap();
    {
        let mut accept = Box::pin(listener.accept_cancel_safe());
        let mut cx = Context::from_waker(Waker::noop());
        assert!(accept.as_mut().poll(&mut cx).is_pending());
    }
    assert_eq!(sockets.iter().count(), 1);
    assert!(listener.closing.is_some());
    {
        let close = listener.closing.as_mut().unwrap();
        let mut wait = Box::pin(close.wait_cancel_safe());
        let mut cx = Context::from_waker(Waker::noop());
        assert!(wait.as_mut().poll(&mut cx).is_pending());
    }
    assert!(!registry.entries[0].poll(&mut sockets));
    registry.remove_entry(0, &mut sockets);
    assert_eq!(sockets.iter().count(), 0);
    listener.close().await.unwrap();
}
