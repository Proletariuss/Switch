use super::*;
use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;
use std::convert::Infallible;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const REQUEST: &[u8] = b"GET /fixture HTTP/1.1\r\nHost: local\r\n\r\n";
static ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn test_root() -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("Switch source root");
    let parent = source
        .parent()
        .expect("Switch root")
        .join("build/runtime/g4-connection-pools");
    fs::create_dir_all(&parent).unwrap();
    let sequence = ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    // Keep the complete UDS names comfortably below Darwin's SUN_LEN.
    let root = parent.join(format!("{}-{sequence}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn fd_count() -> usize {
    fs::read_dir("/dev/fd").unwrap().count()
}

async fn wait_for(label: &str, condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !condition() {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
}

async fn small_request(path: &Path, expected: &'static [u8]) {
    let mut stream = tokio::net::UnixStream::connect(path).await.unwrap();
    stream.write_all(REQUEST).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(response.ends_with(expected));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_three_pool_driver_reaps_64_admits_queued_65_and_preserves_other_pools() {
    // Warm Tokio/UDS before taking the process-wide descriptor baseline.
    let (warm_a, warm_b) = tokio::net::UnixStream::pair().unwrap();
    drop((warm_a, warm_b));
    tokio::time::sleep(Duration::from_millis(20)).await;
    let fd_before = fd_count();

    let root = test_root();
    let public_path = root.join("public.sock");
    let callback_path = root.join("callback.sock");
    let internal_path = root.join("internal.sock");
    let (public, public_guard) = crate::SocketGuard::bind(&public_path).await.unwrap();
    let (callback, callback_guard) = crate::SocketGuard::bind(&callback_path).await.unwrap();
    let (internal, internal_guard) = crate::SocketGuard::bind(&internal_path).await.unwrap();

    let public_accepts = Arc::new(AtomicUsize::new(0));
    let public_dispatches = Arc::new(AtomicUsize::new(0));
    let public_observer_timeouts = Arc::new(AtomicUsize::new(0));
    let public_joins = Arc::new(AtomicUsize::new(0));
    let callback_joins = Arc::new(AtomicUsize::new(0));
    let internal_joins = Arc::new(AtomicUsize::new(0));
    let callback_dispatches = Arc::new(AtomicUsize::new(0));
    let internal_dispatches = Arc::new(AtomicUsize::new(0));
    let owner_dispatches = Arc::new(AtomicUsize::new(0));
    let owner_completed = Arc::new(AtomicBool::new(false));
    let response_barrier = Arc::new(tokio::sync::Barrier::new(
        crate::PUBLIC_CONNECTION_LIMIT + 1,
    ));

    let admission = Arc::new(crate::ordinary_vowifi_admission::Gate::default());
    admission.set_go_producer("g4-production-driver").unwrap();
    let claim = Arc::new(
        admission
            .acquire_go("g4-production-driver", "reader", "retained-owner", 1)
            .unwrap(),
    );
    let cleanup = crate::shutdown::CleanupGate::default();
    let cleanup_permit = Arc::new(Mutex::new(Some(cleanup.enter().await.unwrap())));
    let (release_owner, owner_release) = tokio::sync::oneshot::channel();
    let owner_release = Arc::new(Mutex::new(Some(owner_release)));
    let (owner_started, owner_started_rx) = tokio::sync::oneshot::channel();
    let owner_started = Arc::new(Mutex::new(Some(owner_started)));
    let owner_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(None));

    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let driver_accepts = public_accepts.clone();
    let driver_dispatches = public_dispatches.clone();
    let driver_timeouts = public_observer_timeouts.clone();
    let driver_joins = public_joins.clone();
    let driver_callback_joins = callback_joins.clone();
    let driver_internal_joins = internal_joins.clone();
    let driver_callback = callback_dispatches.clone();
    let driver_internal = internal_dispatches.clone();
    let driver_owner_dispatches = owner_dispatches.clone();
    let driver_owner_completed = owner_completed.clone();
    let driver_claim = claim.clone();
    let driver_release = owner_release.clone();
    let driver_started = owner_started.clone();
    let driver_owner_task = owner_task.clone();
    let driver_cleanup_permit = cleanup_permit.clone();
    let driver_response_barrier = response_barrier.clone();
    let driver = tokio::spawn(async move {
        let _guards = (public_guard, callback_guard, internal_guard);
        let mut pools = Driver::default();
        loop {
            tokio::select! {
                changed = stopped.changed() => {
                    assert!(changed.is_ok() && *stopped.borrow());
                    break;
                }
                event = pools.next(&public, &callback, &internal) => {
                    match event {
                        Event::Accepted(Pool::Public, accepted) => {
                            let stream = accepted.unwrap();
                            socket2::SockRef::from(&stream).set_send_buffer_size(4096).unwrap();
                            let ordinal = driver_accepts.fetch_add(1, Ordering::SeqCst) + 1;
                            let dispatches = driver_dispatches.clone();
                            let timeouts = driver_timeouts.clone();
                            let owner_dispatches = driver_owner_dispatches.clone();
                            let owner_completed = driver_owner_completed.clone();
                            let claim = driver_claim.clone();
                            let release = driver_release.clone();
                            let started = driver_started.clone();
                            let owner_task = driver_owner_task.clone();
                            let cleanup_permit = driver_cleanup_permit.clone();
                            let response_barrier = driver_response_barrier.clone();
                            pools.spawn(Pool::Public, async move {
                                let result = crate::http_server::serve_full_response_with_deadlines(
                                    stream,
                                    move |_| {
                                        dispatches.fetch_add(1, Ordering::SeqCst);
                                        if ordinal == 1 {
                                            assert_eq!(owner_dispatches.fetch_add(1, Ordering::SeqCst), 0);
                                            let receiver = release.lock().unwrap().take().unwrap();
                                            let started = started.lock().unwrap().take().unwrap();
                                            let completed = owner_completed.clone();
                                            let claim = claim.clone();
                                            let permit = cleanup_permit.lock().unwrap().take().unwrap();
                                            let handle = tokio::spawn(async move {
                                                let _permit = permit;
                                                started.send(()).unwrap();
                                                receiver.await.unwrap();
                                                claim.release_confirmed().unwrap();
                                                completed.store(true, Ordering::SeqCst);
                                            });
                                            *owner_task.lock().unwrap() = Some(handle);
                                        }
                                        let response_barrier = response_barrier.clone();
                                        async move {
                                            let body = if ordinal <= crate::PUBLIC_CONNECTION_LIMIT {
                                                response_barrier.wait().await;
                                                Bytes::from(vec![b'x'; 2 * 1024 * 1024])
                                            } else {
                                                Bytes::from_static(b"queued-65-recovered")
                                            };
                                            Ok::<_, Infallible>(Response::new(Full::new(body)))
                                        }
                                    },
                                    Duration::from_millis(500),
                                    Duration::from_millis(1500),
                                ).await;
                                if ordinal <= crate::PUBLIC_CONNECTION_LIMIT {
                                    assert!(result.is_err(), "saturated observer unexpectedly completed");
                                    timeouts.fetch_add(1, Ordering::SeqCst);
                                } else {
                                    result.unwrap();
                                }
                            });
                        }
                        Event::Accepted(Pool::Callback, accepted) => {
                            let stream = accepted.unwrap();
                            let calls = driver_callback.clone();
                            pools.spawn(Pool::Callback, async move {
                                crate::http_server::serve_full_response_with_deadlines(
                                    stream,
                                    move |_| {
                                        calls.fetch_add(1, Ordering::SeqCst);
                                        async { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"callback-live")))) }
                                    },
                                    Duration::from_millis(500),
                                    Duration::from_millis(1500),
                                ).await.unwrap();
                            });
                        }
                        Event::Accepted(Pool::Internal, accepted) => {
                            let stream = accepted.unwrap();
                            let calls = driver_internal.clone();
                            pools.spawn(Pool::Internal, async move {
                                crate::http_server::serve_full_response_with_deadlines(
                                    stream,
                                    move |_| {
                                        calls.fetch_add(1, Ordering::SeqCst);
                                        async { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"internal-live")))) }
                                    },
                                    Duration::from_millis(500),
                                    Duration::from_millis(1500),
                                ).await.unwrap();
                            });
                        }
                        Event::Completed(pool, completed) => {
                            completed.unwrap();
                            if pool == Pool::Public {
                                driver_joins.fetch_add(1, Ordering::SeqCst);
                            } else if pool == Pool::Callback {
                                driver_callback_joins.fetch_add(1, Ordering::SeqCst);
                            } else {
                                driver_internal_joins.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(pools.len(Pool::Public), 0);
        assert_eq!(pools.len(Pool::Callback), 0);
        assert_eq!(pools.len(Pool::Internal), 0);
        let mut report = crate::shutdown::Report::default();
        pools.abort_all();
        pools.drain(&mut report).await;
        report.finish().unwrap();
    });

    let mut no_readers = Vec::new();
    let mut dribblers = Vec::new();
    for index in 0..crate::PUBLIC_CONNECTION_LIMIT {
        let mut stream = tokio::net::UnixStream::connect(&public_path).await.unwrap();
        socket2::SockRef::from(&stream)
            .set_recv_buffer_size(4096)
            .unwrap();
        stream.write_all(REQUEST).await.unwrap();
        if index % 2 == 0 {
            no_readers.push(stream);
        } else {
            dribblers.push(tokio::spawn(async move {
                let mut byte = [0_u8; 1];
                let until = tokio::time::Instant::now() + Duration::from_millis(1_750);
                while tokio::time::Instant::now() < until {
                    match stream.read(&mut byte).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => tokio::time::sleep(Duration::from_millis(25)).await,
                    }
                }
            }));
        }
    }
    wait_for("64 public dispatches", || {
        public_dispatches.load(Ordering::SeqCst) == crate::PUBLIC_CONNECTION_LIMIT
    })
    .await;
    owner_started_rx.await.unwrap();

    // This connection is established and its request queued while the driver
    // has exactly 64 occupied public slots.
    let mut queued_65 = tokio::net::UnixStream::connect(&public_path).await.unwrap();
    queued_65.write_all(REQUEST).await.unwrap();
    // Other pools remain serviced while the first 64 public handlers are held
    // behind an explicit barrier, independently of any deadline duration.
    tokio::join!(
        small_request(&callback_path, b"callback-live"),
        small_request(&internal_path, b"internal-live")
    );
    assert_eq!(callback_dispatches.load(Ordering::SeqCst), 1);
    assert_eq!(internal_dispatches.load(Ordering::SeqCst), 1);
    assert_eq!(
        public_dispatches.load(Ordering::SeqCst),
        crate::PUBLIC_CONNECTION_LIMIT
    );
    assert_eq!(
        public_accepts.load(Ordering::SeqCst),
        crate::PUBLIC_CONNECTION_LIMIT
    );
    assert_eq!(public_joins.load(Ordering::SeqCst), 0);
    assert_eq!(public_observer_timeouts.load(Ordering::SeqCst), 0);

    // Releasing the 64 handlers starts their real response observers; only
    // their deadline-driven retirement can free a slot for queued request 65.
    response_barrier.wait().await;

    let mut recovered = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(3),
        queued_65.read_to_end(&mut recovered),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(recovered.ends_with(b"queued-65-recovered"));
    drop(queued_65);
    wait_for("64 observer timeouts and 65 public joins", || {
        public_observer_timeouts.load(Ordering::SeqCst) == crate::PUBLIC_CONNECTION_LIMIT
            && public_joins.load(Ordering::SeqCst) == crate::PUBLIC_CONNECTION_LIMIT + 1
            && callback_joins.load(Ordering::SeqCst) == 1
            && internal_joins.load(Ordering::SeqCst) == 1
    })
    .await;

    // The connection observer ended, but the accepted business owner remains
    // singular and holds the actual claim/cleanup permit until real completion.
    assert_eq!(owner_dispatches.load(Ordering::SeqCst), 1);
    assert!(claim.is_current());
    assert!(!owner_completed.load(Ordering::SeqCst));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), cleanup.close())
            .await
            .is_err()
    );
    release_owner.send(()).unwrap();
    let owner = owner_task.lock().unwrap().take().unwrap();
    owner.await.unwrap();
    assert!(!claim.is_current());
    assert!(owner_completed.load(Ordering::SeqCst));
    cleanup.close().await;

    drop(no_readers);
    for dribbler in dribblers {
        dribbler.await.unwrap();
    }
    stop.send(true).unwrap();
    driver.await.unwrap();
    for path in [&public_path, &callback_path, &internal_path] {
        assert!(!path.exists(), "owned socket path survived guard drop");
        let (listener, guard) = crate::SocketGuard::bind(path).await.unwrap();
        drop(listener);
        drop(guard);
        assert!(
            !path.exists(),
            "rebound socket path survived exact guard drop"
        );
    }

    // A stale guard must not remove a replacement listener at the same path.
    let replacement_path = root.join("replacement.sock");
    let (owned, guard) = crate::SocketGuard::bind(&replacement_path).await.unwrap();
    drop(owned);
    fs::remove_file(&replacement_path).unwrap();
    let replacement = tokio::net::UnixListener::bind(&replacement_path).unwrap();
    drop(guard);
    assert!(replacement_path.exists());
    drop(replacement);
    fs::remove_file(&replacement_path).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    let fd_after = fd_count();
    eprintln!(
        "G4_CONNECTION_POOL_METRIC fd_before={fd_before} fd_after={fd_after} public_accepts={} public_joins={}",
        public_accepts.load(Ordering::SeqCst),
        public_joins.load(Ordering::SeqCst),
    );
    assert_eq!(fd_after, fd_before, "test process leaked file descriptors");
    let mut residuals = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    residuals.sort();
    let mut expected_locks = [
        &public_path,
        &callback_path,
        &internal_path,
        &replacement_path,
    ]
    .map(|path| PathBuf::from(format!("{}.switch-listener.lock", path.display())))
    .to_vec();
    expected_locks.sort();
    assert_eq!(
        residuals, expected_locks,
        "unexpected socket test residuals"
    );
    for lock in expected_locks {
        fs::remove_file(lock).unwrap();
    }
    fs::remove_dir(&root).unwrap();
}
