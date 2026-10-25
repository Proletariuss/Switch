use super::*;
use hyper::body::SizeHint;
use std::sync::atomic::{AtomicUsize, Ordering};

fn target() -> Target {
    Target {
        scope: "reader".into(),
        daemon_epoch: "epoch-1".into(),
        owner_id: "owner-1".into(),
        owner_generation: 7,
    }
}
fn request(query: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .method("POST")
        .uri(format!("{READER_PATH}?{query}"))
        .body(Full::new(Bytes::new()))
        .unwrap()
}

#[test]
fn strict_query_rejects_aliases_partial_unknown_and_noncanonical_generation() {
    let good = target().query_string();
    assert!(Target::query(Some(&format!("{good}#x")), "reader").is_err());
    assert_eq!(
        Target::request(&request(&good), "reader").unwrap(),
        target()
    );
    let mut cases = vec![
        format!("{good}&"),
        format!("&{good}"),
        good.replace('&', "&&"),
        format!("{good}&scope=reader"),
        format!("{good}&extra=x"),
        good.replace("scope=reader&", ""),
        good.replace("reader", "module"),
    ];
    for generation in [
        "0",
        "00",
        "07",
        "+7",
        "%2B7",
        "18446744073709551616",
        "-7",
        "7.0",
    ] {
        cases.push(good.replace(
            "owner_generation=7",
            &format!("owner_generation={generation}"),
        ));
    }
    for value in [
        "%",
        "%0",
        "%GG",
        "%00",
        "%0A",
        "%7f",
        "%6fwner-1",
        "%6Fwner-1",
        "owner%2d1",
        "owner+1",
        "owner%20x",
        "owner:1",
    ] {
        cases.push(good.replace("owner-1", value));
    }
    cases.push(good.replace("scope=", "%73cope="));
    cases.push(good.replace("owner-1", &"x".repeat(129)));
    for case in cases {
        assert!(
            Target::request(&request(&case), "reader").is_err(),
            "accepted {case}"
        );
    }
}
#[test]
fn method_and_every_nonempty_or_ambiguous_body_are_rejected_before_signal() {
    let query = target().query_string();
    for method in ["PUT", "HEAD", "DELETE"] {
        let mut req = request(&query);
        *req.method_mut() = method.parse().unwrap();
        assert!(Target::request(&req, "reader").is_err());
    }
    let mut req = request(&query);
    req.headers_mut()
        .append(hyper::header::CONTENT_LENGTH, "0".parse().unwrap());
    req.headers_mut()
        .append(hyper::header::CONTENT_LENGTH, "0".parse().unwrap());
    assert!(Target::request(&req, "reader").is_err());
    for (header, value) in [
        ("content-length", "0, 0"),
        ("content-length", "1"),
        ("content-length", "00"),
        ("transfer-encoding", "chunked"),
        ("transfer-encoding", "identity"),
    ] {
        let mut req = request(&query);
        req.headers_mut().insert(
            hyper::header::HeaderName::from_bytes(header.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        assert!(Target::request(&req, "reader").is_err());
    }
    let req = request(&query).map(|_| Full::new(Bytes::from_static(b"x")));
    assert!(Target::request(&req, "reader").is_err());
    struct Unknown;
    impl Body for Unknown {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            std::task::Poll::Pending
        }
        fn size_hint(&self) -> SizeHint {
            SizeHint::default()
        }
    }
    assert!(Target::request(&request(&query).map(|_| Unknown), "reader").is_err());
}
#[tokio::test]
async fn direct_get_is_not_a_signal_or_join_receipt() {
    let owner = Arc::new(Owner::default());
    let p = owner.project(owner.target("reader", "epoch-1", 7)).unwrap();
    assert_eq!(p.signal_state, "not_sent");
    assert_eq!(p.owner_join, "unconfirmed");
    assert_eq!(p.cleanup, "unconfirmed");
    assert_eq!(p.poll_after_ms, 0);
    assert!(!p.message.contains("已向"));
    assert_eq!(response(Ok(p)).status(), StatusCode::OK);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signalling_window_is_not_published_and_duplicate_post_never_resignals() {
    let owner = Arc::new(Owner::default());
    let entered = Arc::new(std::sync::Barrier::new(2));
    let resume = Arc::new(std::sync::Barrier::new(2));
    let signals = Arc::new(AtomicUsize::new(0));
    let join = Arc::new(AtomicUsize::new(0));
    let worker = owner.clone();
    let e = entered.clone();
    let r = resume.clone();
    let s = signals.clone();
    let j = join.clone();
    let runtime = tokio::runtime::Handle::current();
    let thread = std::thread::spawn(move || {
        let _entered = runtime.enter();
        let completed = worker.clone();
        worker.begin(
            || {
                s.fetch_add(1, Ordering::SeqCst);
                e.wait();
                r.wait();
            },
            async move {
                j.fetch_add(1, Ordering::SeqCst);
                completed.finish(true, true, None);
            },
        )
    });
    entered.wait();
    let p = owner.project(owner.target("reader", "epoch-1", 7)).unwrap();
    assert_eq!(p.signal_state, "not_sent");
    assert!(!p.message.contains("已向"));
    owner
        .begin(|| panic!("duplicate signal"), async {
            panic!("duplicate join")
        })
        .unwrap();
    resume.wait();
    thread
        .join()
        .expect("signal thread panic")
        .expect("begin failed");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if owner
                .project(owner.target("reader", "epoch-1", 7))
                .unwrap()
                .owner_join
                == "joined"
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(signals.load(Ordering::SeqCst), 1);
    assert_eq!(join.load(Ordering::SeqCst), 1);
    assert_eq!(
        owner
            .project(owner.target("reader", "epoch-1", 7))
            .unwrap()
            .signal_state,
        "published"
    );
}
#[tokio::test]
async fn join_panic_and_signal_panic_are_explicit_and_never_release_the_fence() {
    let owner = Arc::new(Owner::default());
    let copy = owner.clone();
    owner
        .begin(
            || {
                assert_eq!(
                    copy.project(copy.target("reader", "epoch-1", 7))
                        .unwrap()
                        .signal_state,
                    "not_sent"
                );
            },
            async { panic!("join fault") },
        )
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let p = owner.project(owner.target("reader", "epoch-1", 7)).unwrap();
            if p.owner_join == "failed" {
                assert_eq!(p.cleanup, "failed");
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(owner.busy());
    let owner = Arc::new(Owner::default());
    assert!(
        owner
            .begin(|| panic!("signal fault"), async { panic!("must not run") })
            .is_err()
    );
    let p = owner.project(owner.target("reader", "epoch-1", 7)).unwrap();
    assert_eq!(p.signal_state, "unconfirmed");
    assert_eq!(p.owner_join, "failed");
    assert!(owner.state.lock().unwrap().task.is_none());
    assert!(owner.busy());
}
#[tokio::test]
async fn unique_harvester_survives_two_cancelled_observers_and_records_panic() {
    let join = TaskJoin::default();
    assert!(join.wait().await.is_err());
    let (finish, finished) = tokio::sync::oneshot::channel();
    join.spawn(async move {
        finished.await.unwrap();
    })
    .unwrap();
    let a = join.clone();
    let a = tokio::spawn(async move { a.wait().await });
    let b = join.clone();
    let b = tokio::spawn(async move { b.wait().await });
    tokio::task::yield_now().await;
    a.abort();
    b.abort();
    let _ = a.await;
    let _ = b.await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), join.wait())
            .await
            .is_err()
    );
    finish.send(()).unwrap();
    assert!(join.wait().await.unwrap());
    assert!(join.wait().await.unwrap());
    let failed = TaskJoin::default();
    failed
        .spawn(async { panic!("outer worker panic") })
        .unwrap();
    assert!(!failed.wait().await.unwrap());
    let never = TaskJoin::default();
    never.confirm_unspawned().unwrap();
    assert!(never.wait().await.unwrap());
    assert!(never.spawn(async {}).is_err());
}

#[tokio::test]
async fn metadata_poison_after_signal_never_discards_the_actual_join_owner() {
    let owner = Arc::new(Owner::default());
    let signal = owner.clone();
    let joined = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let actual = joined.clone();
    assert!(
        owner
            .begin(|| signal.test_poison(), async move {
                actual.store(true, Ordering::SeqCst);
            })
            .is_err()
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !joined.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(owner.busy());
    assert!(owner.project(owner.target("reader", "epoch-1", 7)).is_err());
    let poisoned = match owner.state.lock() {
        Err(poisoned) => poisoned,
        Ok(_) => panic!("control poison was cleared"),
    };
    assert!(
        poisoned.into_inner().task.is_some(),
        "handle custody survives without clearing poison or reporting cleanup success"
    );
}
