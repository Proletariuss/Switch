use super::*;
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// Per-Store and test-only: no global hook, new production lock, or production
// timeout. The owner covers the complete original load_unlocked transaction.
pub(super) struct ReadProbe {
    started: AtomicBool,
    released: AtomicBool,
    active: AtomicUsize,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}
pub(super) struct ReadOwner<'a>(&'a ReadProbe);
impl Drop for ReadOwner<'_> {
    fn drop(&mut self) {
        assert_eq!(self.0.active.fetch_sub(1, Ordering::SeqCst), 1);
    }
}
impl ReadProbe {
    pub(super) fn enter(&self) -> ReadOwner<'_> {
        assert_eq!(self.active.fetch_add(1, Ordering::SeqCst), 0);
        let owner = ReadOwner(self);
        self.started.store(true, Ordering::SeqCst);
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .expect("independent test releaser must release the read");
        owner
    }
}

fn fixture(contents: &[u8]) -> Store {
    let root = PathBuf::from(std::env::var_os("SWITCH_BUILD_DIR").unwrap())
        .join("reports/stability-717-s12-isolation-audit/runtime")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("profile-notes.json"), contents).unwrap();
    Store::new(&root)
}
fn held_store() -> (Store, Arc<ReadProbe>, std::sync::mpsc::Sender<()>) {
    let mut store = fixture(br#"{"8901F":{"label":"kept","phone":"+12345","tags":"test"}}"#);
    let (release, wait) = std::sync::mpsc::channel();
    let probe = Arc::new(ReadProbe {
        started: AtomicBool::new(false),
        released: AtomicBool::new(false),
        active: AtomicUsize::new(0),
        release: Mutex::new(wait),
    });
    store.read_probe = Some(probe.clone());
    (store, probe, release)
}
async fn request(store: Store, phone: bool) -> Response<Full<Bytes>> {
    if phone {
        handle_internal(
            Request::builder()
                .method(Method::POST)
                .uri("/api/internal/profile-notes/phone")
                .body(Full::new(Bytes::from_static(br#"{"iccid":"8901F"}"#)))
                .unwrap(),
            store,
        )
        .await
    } else {
        handle_public(
            Request::builder()
                .method(Method::GET)
                .uri("/api/esim/notes")
                .body(Full::new(Bytes::new()))
                .unwrap(),
            store,
        )
        .await
    }
}
async fn body(response: Response<Full<Bytes>>) -> serde_json::Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn both_read_endpoints_keep_same_runtime_heartbeat_during_slow_read() {
    for phone in [false, true] {
        let (store, probe, release) = held_store();
        let release_probe = probe.clone();
        let releaser = std::thread::spawn(move || {
            let start = Instant::now();
            while !release_probe.started.load(Ordering::SeqCst) {
                assert!(start.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(1));
            }
            // This thread is independent of the runtime under test.
            std::thread::sleep(Duration::from_millis(300));
            release_probe.released.store(true, Ordering::SeqCst);
            release.send(()).unwrap();
        });
        let ticks = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let heartbeat = tokio::spawn({
            let ticks = ticks.clone();
            let probe = probe.clone();
            let done = done.clone();
            async move {
                while !done.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    if probe.started.load(Ordering::SeqCst)
                        && !probe.released.load(Ordering::SeqCst)
                    {
                        ticks.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        });
        tokio::task::yield_now().await;
        let elapsed = Instant::now();
        let result = request(store, phone).await;
        done.store(true, Ordering::SeqCst);
        heartbeat.await.unwrap();
        releaser.join().unwrap();
        assert_eq!(result.status(), StatusCode::OK);
        let result = body(result).await;
        if phone {
            assert_eq!(result, json!({"phone":"+12345"}));
        } else {
            assert_eq!(result["notes"]["8901F"]["label"], "kept");
        }
        let ticks = ticks.load(Ordering::SeqCst);
        println!(
            "heartbeat phone={phone} baseline={} held_ms=300 ticks={ticks} elapsed_ms={}",
            cfg!(baseline_expected),
            elapsed.elapsed().as_millis()
        );
        if cfg!(baseline_expected) {
            assert_eq!(
                ticks, 0,
                "the original synchronous read must block this runtime"
            );
        } else {
            assert!(
                ticks >= 5,
                "the same runtime must progress during a held read"
            );
        }
        assert_eq!(probe.active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_http_waiter_retains_real_read_owner_until_completion() {
    assert!(
        !cfg!(baseline_expected),
        "run this against the offload candidate"
    );
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    for phone in [false, true] {
        let (store, probe, release) = held_store();
        let last_store_capture = Arc::downgrade(&store.local_lock);
        let task = tokio::spawn(request(store, phone));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !probe.started.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(probe.active.load(Ordering::SeqCst), 1);
        assert!(last_store_capture.upgrade().is_some());
        let mut drain = Box::pin(crate::archive_io::finish_pending());
        assert!(poll_fn(|cx| Poll::Ready(drain.as_mut().poll(cx).is_pending())).await);
        // Cancellation has not converted observer completion into I/O completion.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(probe.active.load(Ordering::SeqCst), 1);
        assert!(last_store_capture.upgrade().is_some());
        probe.released.store(true, Ordering::SeqCst);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), drain)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(probe.active.load(Ordering::SeqCst), 0);
        assert!(last_store_capture.upgrade().is_none());
        println!(
            "cancel phone={phone} owner_held_until_release=true drain_completed_after_release=true"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_failure_status_body_and_json_headers_remain_exact() {
    for kind in ["malformed", "directory", "symlink", "oversize"] {
        let store = fixture(b"not-json");
        match kind {
            "directory" => {
                fs::remove_file(&store.path).unwrap();
                fs::create_dir(&store.path).unwrap();
            }
            "symlink" => {
                fs::remove_file(&store.path).unwrap();
                std::os::unix::fs::symlink("missing.json", &store.path).unwrap();
            }
            "oversize" => {
                File::options()
                    .write(true)
                    .open(&store.path)
                    .unwrap()
                    .set_len(MAX_FILE_BYTES + 1)
                    .unwrap();
            }
            _ => {}
        }
        for phone in [false, true] {
            let result = request(store.clone(), phone).await;
            assert_eq!(
                result.headers()[hyper::header::CONTENT_TYPE],
                "application/json; charset=utf-8"
            );
            assert_eq!(result.headers()["x-content-type-options"], "nosniff");
            assert_eq!(
                result.status(),
                if phone {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            );
            assert_eq!(
                body(result).await,
                if phone {
                    json!({"code":"profile_notes_unavailable","error":"profile notes are unavailable"})
                } else {
                    json!({"error":"本地备注读取失败"})
                }
            );
        }
        println!("error_mapping kind={kind} GET=500 internal_phone=503 exact_body=true");
    }
    let store = fixture(b"");
    fs::remove_file(&store.path).unwrap();
    assert_eq!(
        body(request(store.clone(), false).await).await,
        json!({"notes":{}})
    );
    assert_eq!(body(request(store, true).await).await, json!({"phone":""}));
}
