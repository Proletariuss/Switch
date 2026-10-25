use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Default)]
struct FakeIo {
    pending: bool,
    zero: bool,
    flush_pending: bool,
    shutdown_pending: bool,
    calls: usize,
}
impl AsyncWrite for FakeIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.calls += 1;
        if self.pending {
            Poll::Pending
        } else {
            Poll::Ready(Ok(usize::from(!self.zero && !buf.is_empty())))
        }
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.poll_write(
            cx,
            bufs.iter().find(|b| !b.is_empty()).map_or(&[], |b| &b[..]),
        )
    }
    fn is_write_vectored(&self) -> bool {
        true
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.flush_pending {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.shutdown_pending {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
}
fn armed(io: FakeIo, idle: u64, total: u64) -> TransferIo<FakeIo> {
    let ready = Instant::now();
    let plan = Arc::new(OnceLock::new());
    plan.set(TransferPlan {
        ready,
        absolute: ready + Duration::from_secs(total),
        idle: Duration::from_secs(idle),
    })
    .ok()
    .unwrap();
    TransferIo::new(io, plan)
}
fn cx() -> Context<'static> {
    Context::from_waker(std::task::Waker::noop())
}

#[tokio::test(start_paused = true)]
async fn unarmed_io_has_no_business_deadline() {
    let mut io = TransferIo::new(FakeIo::default(), Arc::new(OnceLock::new()));
    tokio::time::advance(Duration::from_secs(86400)).await;
    assert!(Pin::new(&mut io).poll_write(&mut cx(), b"x").is_ready());
    assert!(io.last_progress.is_none());
}

#[tokio::test(start_paused = true)]
async fn scalar_and_vectored_progress_never_renew_absolute_deadline() {
    for vectored in [false, true] {
        let mut io = armed(FakeIo::default(), 5, 10);
        assert!(io.is_write_vectored());
        for step in 0..3 {
            if step != 0 {
                tokio::time::advance(Duration::from_secs(4)).await;
            }
            let result = if vectored {
                Pin::new(&mut io).poll_write_vectored(&mut cx(), &[IoSlice::new(b"x")])
            } else {
                Pin::new(&mut io).poll_write(&mut cx(), b"x")
            };
            assert!(matches!(result, Poll::Ready(Ok(1))));
        }
        tokio::time::advance(Duration::from_secs(2)).await;
        let calls = io.io.calls;
        let result = Pin::new(&mut io).poll_write(&mut cx(), b"x");
        assert!(matches!(result, Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::TimedOut));
        assert_eq!(io.io.calls, calls, "expired bytes must not be written");
    }
}

#[tokio::test(start_paused = true)]
async fn empty_zero_and_flush_success_are_not_progress() {
    let mut io = armed(
        FakeIo {
            zero: true,
            ..Default::default()
        },
        5,
        20,
    );
    for _ in 0..4 {
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(Pin::new(&mut io).poll_write(&mut cx(), b"x").is_ready());
        assert!(
            Pin::new(&mut io)
                .poll_write_vectored(&mut cx(), &[IoSlice::new(b"")])
                .is_ready()
        );
        assert!(Pin::new(&mut io).poll_flush(&mut cx()).is_ready());
    }
    assert!(io.last_progress.is_none());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        matches!(Pin::new(&mut io).poll_flush(&mut cx()), Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::TimedOut)
    );
}

#[tokio::test(start_paused = true)]
async fn pending_write_flush_and_shutdown_are_woken_by_deadline() {
    for kind in 0..4 {
        let mut io = armed(
            FakeIo {
                pending: true,
                flush_pending: true,
                shutdown_pending: true,
                ..Default::default()
            },
            5,
            20,
        );
        let result = std::future::poll_fn(|cx| match kind {
            0 => Pin::new(&mut io)
                .poll_write(cx, b"x")
                .map(|r| r.map(|_| ())),
            1 => Pin::new(&mut io)
                .poll_write_vectored(cx, &[IoSlice::new(b"x")])
                .map(|r| r.map(|_| ())),
            2 => Pin::new(&mut io).poll_flush(cx),
            _ => Pin::new(&mut io).poll_shutdown(cx),
        })
        .await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }
}

fn pair() -> (tokio::net::UnixStream, tokio::net::UnixStream) {
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    socket2::SockRef::from(&server)
        .set_send_buffer_size(4096)
        .unwrap();
    socket2::SockRef::from(&client)
        .set_recv_buffer_size(4096)
        .unwrap();
    (client, server)
}
const REQUEST: &[u8] = b"GET /fixture HTTP/1.1\r\nHost: local\r\n\r\n";
fn large_body() -> Bytes {
    Bytes::from(vec![b'x'; 2 * 1024 * 1024])
}

#[tokio::test]
async fn unix_nonreader_and_one_byte_reader_retire_connection() {
    for dribble in [false, true] {
        let (mut client, server) = pair();
        let task = tokio::spawn(serve_with_policy(
            server,
            |_| async { Ok(Response::new(Full::new(large_body()))) },
            TransferPolicy::fixed(Duration::from_millis(100), Duration::from_millis(500)),
        ));
        client.write_all(REQUEST).await.unwrap();
        let reader = tokio::spawn(async move {
            loop {
                if dribble {
                    let mut byte = [0];
                    if client.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        reader.abort();
        let _ = reader.await;
    }
}

#[tokio::test]
async fn unix_slow_large_reader_receives_complete_response() {
    let (mut client, server) = pair();
    let expected = large_body();
    let body = expected.clone();
    let task = tokio::spawn(serve_with_policy(
        server,
        move |_| {
            let body = body.clone();
            async move { Ok(Response::new(Full::new(body))) }
        },
        TransferPolicy::fixed(Duration::from_millis(300), Duration::from_secs(5)),
    ));
    client.write_all(REQUEST).await.unwrap();
    let mut response = Vec::new();
    let mut chunk = [0; 16384];
    loop {
        let n = client.read(&mut chunk).await.unwrap();
        if n == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..n]);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    task.await.unwrap().unwrap();
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert_eq!(&response[split..], expected.as_ref());
}

#[tokio::test]
async fn response_deadline_starts_after_handler_and_preserves_retained_claim() {
    let (mut client, server) = pair();
    let gate = Arc::new(crate::ordinary_vowifi_admission::Gate::default());
    gate.set_go_producer("response-test").unwrap();
    let claim = Arc::new(
        gate.acquire_go("response-test", "reader", "original", 1)
            .unwrap(),
    );
    let cleanup = crate::shutdown::CleanupGate::default();
    let permit = cleanup.enter().await.unwrap();
    let (finish, finished) = tokio::sync::oneshot::channel();
    let worker_claim = claim.clone();
    let completed = Arc::new(AtomicBool::new(false));
    let worker_completed = completed.clone();
    let owner = tokio::spawn(async move {
        let _permit = permit;
        finished.await.unwrap();
        worker_claim.release_confirmed().unwrap();
        worker_completed.store(true, Ordering::SeqCst);
    });
    let dispatches = Arc::new(AtomicUsize::new(0));
    let calls = dispatches.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let ready = entered.clone();
    let task = tokio::spawn(serve_with_policy(
        server,
        move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            let ready = ready.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(250)).await;
                ready.notify_one();
                Ok(Response::new(Full::new(large_body())))
            }
        },
        TransferPolicy::fixed(Duration::from_millis(100), Duration::from_millis(150)),
    ));
    client.write_all(REQUEST).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    assert!(claim.is_current());
    assert!(!completed.load(Ordering::SeqCst));
    assert!(!owner.is_finished());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), cleanup.close())
            .await
            .is_err()
    );
    finish.send(()).unwrap();
    owner.await.unwrap();
    assert!(!claim.is_current());
    assert!(completed.load(Ordering::SeqCst));
    cleanup.close().await;
}

#[test]
fn production_policy_gives_large_responses_a_conservative_finite_budget() {
    let policy = TransferPolicy::LOCAL;
    assert_eq!(policy.total(128 * 1024 * 1024), Duration::from_secs(1084));
    assert_eq!(policy.total(u64::MAX), policy.maximum);
}

// This uses the production response driver on 64 actual Unix sockets. It does
// not claim coverage of main's listener accept/reap scheduling or other pools.
#[tokio::test]
async fn sixty_four_response_observers_close_their_unix_fds_and_next_request_succeeds() {
    let body = large_body();
    let entered = Arc::new(AtomicUsize::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    let mut clients = Vec::new();
    for _ in 0..crate::PUBLIC_CONNECTION_LIMIT {
        let (mut client, server) = pair();
        let entered = entered.clone();
        let body = body.clone();
        tasks.spawn(serve_with_policy(
            server,
            move |_| {
                entered.fetch_add(1, Ordering::SeqCst);
                let body = body.clone();
                async move { Ok(Response::new(Full::new(body))) }
            },
            TransferPolicy::fixed(Duration::from_millis(300), Duration::from_secs(1)),
        ));
        client.write_all(REQUEST).await.unwrap();
        clients.push(client);
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while entered.load(Ordering::SeqCst) != crate::PUBLIC_CONNECTION_LIMIT {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut completed = 0;
        while let Some(result) = tasks.join_next().await {
            assert!(result.unwrap().is_err());
            completed += 1;
        }
        assert_eq!(completed, crate::PUBLIC_CONNECTION_LIMIT);
        for client in &mut clients {
            let mut buffered = Vec::new();
            // EOF proves the server endpoint actually closed, even while this
            // client's FD remains live and any partial kernel bytes are read.
            client.read_to_end(&mut buffered).await.unwrap();
            assert!(buffered.len() < body.len());
        }
    })
    .await
    .unwrap();
    assert!(tasks.is_empty());
    let (mut client, server) = pair();
    let task = tokio::spawn(serve_with_policy(
        server,
        |_| async { Ok(Response::new(Full::new(Bytes::from_static(b"recovered")))) },
        TransferPolicy::fixed(Duration::from_secs(1), Duration::from_secs(2)),
    ));
    client.write_all(REQUEST).await.unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    task.await.unwrap().unwrap();
    assert!(response.ends_with(b"recovered"));
}

#[tokio::test]
async fn interim_continue_does_not_arm_final_response_deadline() {
    let (mut client, server) = pair();
    let task = tokio::spawn(serve_with_policy(
        server,
        |request| async move {
            let body = super::super::collect_body(request.into_body(), 8)
                .await
                .unwrap();
            assert_eq!(&body[..], b"x");
            tokio::time::sleep(Duration::from_millis(250)).await;
            Ok(Response::new(Full::new(Bytes::from_static(b"done"))))
        },
        TransferPolicy::fixed(Duration::from_millis(80), Duration::from_millis(150)),
    ));
    client.write_all(b"POST /fixture HTTP/1.1\r\nHost: local\r\nContent-Length: 1\r\nExpect: 100-continue\r\n\r\n").await.unwrap();
    let mut interim = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !interim.ends_with(b"\r\n\r\n") {
            interim.push(client.read_u8().await.unwrap());
        }
    })
    .await
    .unwrap();
    assert!(interim.starts_with(b"HTTP/1.1 100"));
    client.write_all(b"x").await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    task.await.unwrap().unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(response.ends_with(b"done"));
}

#[tokio::test]
async fn g3_raw_fragment_and_oversize_target_line_reject_before_business_dispatch() {
    for route in [
        "/api/reader/vowifi/stop-control",
        "/api/vowifi/stop-control",
        "/api/vowifi/%73top-control",
        "/api/reader/vowifi/stop-control/",
    ] {
        for method in ["GET", "HEAD", "POST"] {
            for oversized in [false, true] {
                let (mut client, server) = pair();
                let calls = Arc::new(AtomicUsize::new(0));
                let observed = calls.clone();
                let task = tokio::spawn(serve_with_policy(
                    server,
                    move |_| {
                        observed.fetch_add(1, Ordering::SeqCst);
                        async {
                            Ok(Response::new(Full::new(Bytes::from_static(
                                b"must-not-dispatch",
                            ))))
                        }
                    },
                    TransferPolicy::fixed(Duration::from_secs(1), Duration::from_secs(3)),
                ));
                let target = if oversized {
                    format!("{route}?scope=reader&x={}", "a".repeat(9000))
                } else {
                    format!("{route}?scope=reader&owner_generation=7#fragment")
                };
                let request = format!(
                    "{method} {target} HTTP/1.1\r\nHost: local\r\nContent-Length: 0\r\n\r\n"
                );
                // Segment the request-target, including the fragment marker.
                for chunk in request.as_bytes().chunks(7) {
                    client.write_all(chunk).await.unwrap();
                    tokio::task::yield_now().await;
                }
                let mut response = Vec::new();
                client.read_to_end(&mut response).await.unwrap();
                task.await.unwrap().unwrap();
                assert!(response.starts_with(b"HTTP/1.1 400"));
                let headers = std::str::from_utf8(&response)
                    .unwrap().split("\r\n\r\n").next().unwrap().to_ascii_lowercase();
                assert_eq!(
                    headers.lines().filter(|line| *line == "cache-control: no-store").count(),
                    1,
                );
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    0,
                    "no signal or ledger business can dispatch"
                );
            }
        }
    }
    // The probe is scoped to the new controls; no new policy is imposed on
    // an ordinary route that the retained HTTP parser previously accepted.
    let (mut client, server) = pair();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let task = tokio::spawn(serve_with_policy(
        server,
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            async { Ok(Response::new(Full::new(Bytes::from_static(b"ordinary")))) }
        },
        TransferPolicy::fixed(Duration::from_secs(1), Duration::from_secs(3)),
    ));
    client
        .write_all(b"GET /ordinary?x=1#same-parser HTTP/1.1\r\nHost: local\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(response.ends_with(b"ordinary"));
}
