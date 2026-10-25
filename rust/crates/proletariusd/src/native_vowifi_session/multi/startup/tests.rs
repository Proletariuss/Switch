use super::*;

async fn finished(starts: &Starts) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while starts.tasks.iter().any(|task| !task.join.is_finished()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retained fixture worker exits");
}

#[tokio::test]
async fn public_failure_is_not_a_join_and_confirmed_completion_is_not_cached() {
    let mut starts = Starts::default();
    let (stop, _) = watch::channel(false);
    let (result, received) = oneshot::channel();
    let (release, cleanup) = oneshot::channel();
    starts.tasks.push(Task {
        stop: stop.clone(),
        join: tokio::spawn(async move {
            let _ = result.send(Err(StartFailure {
                code: "fixture transient registration failure".into(),
                cleanup_confirmed: true,
                retry: RetryClass::RetryAfterRelease,
            }));
            // Public failure publication and actual worker retirement can race.
            cleanup.await.unwrap();
            Ok(())
        }),
    });
    let startup = Startup {
        stop,
        result: Some(received),
        completed: false,
    };
    let error = startup.wait().await.err().expect("typed startup failure");
    assert!(error.cleanup_confirmed);
    assert_eq!(error.retry, RetryClass::RetryAfterRelease);
    starts.reap_finished();
    assert_eq!(starts.tasks.len(), 1, "public Err is not worker completion");
    assert!(starts.failure.is_none());

    release.send(()).unwrap();
    finished(&starts).await;
    starts.reap_finished();
    assert!(starts.tasks.is_empty());
    assert!(
        starts.failure.is_none(),
        "business Err must not fence recovery"
    );

    let (stop, _) = watch::channel(false);
    starts.tasks.push(Task {
        stop,
        join: tokio::spawn(async { Ok(()) }),
    });
    finished(&starts).await;
    starts.reap_finished();
    assert!(
        starts.tasks.is_empty(),
        "the next independent worker also joins"
    );
    assert!(starts.failure.is_none());
}

#[tokio::test]
async fn cleanup_unknown_and_panicked_workers_keep_the_owner_fenced() {
    for panic in [false, true] {
        let mut starts = Starts::default();
        let (stop, _) = watch::channel(false);
        starts.tasks.push(Task {
            stop,
            join: tokio::spawn(async move {
                assert!(!panic, "fixture retained startup worker panic");
                Err("fixture cleanup unconfirmed".into())
            }),
        });
        finished(&starts).await;
        starts.reap_finished();
        assert!(starts.tasks.is_empty(), "the failed task really joined");
        let original = starts.failure.clone().expect("cleanup is fenced");
        let (stop, _) = watch::channel(false);
        starts.tasks.push(Task {
            stop,
            join: tokio::spawn(async { Ok(()) }),
        });
        finished(&starts).await;
        starts.reap_finished();
        assert_eq!(starts.failure.as_deref(), Some(original.as_str()));
    }
}

#[tokio::test]
async fn cancelling_startup_waiter_requests_stop_but_retains_cleanup_worker() {
    let mut starts = Starts::default();
    let (stop, mut cancelled) = watch::channel(false);
    let (result, received) = oneshot::channel();
    let (observed, observed_stop) = oneshot::channel();
    let (release, cleanup) = oneshot::channel();
    starts.tasks.push(Task {
        stop: stop.clone(),
        join: tokio::spawn(async move {
            while !*cancelled.borrow() {
                cancelled.changed().await.unwrap();
            }
            observed.send(()).unwrap();
            cleanup.await.unwrap();
            drop(result);
            Ok(())
        }),
    });
    let startup = Startup {
        stop,
        result: Some(received),
        completed: false,
    };
    let waiter = tokio::spawn(startup.wait());
    // Aborting even before the waiter first polls must drop its owned Startup.
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    tokio::time::timeout(Duration::from_secs(1), observed_stop)
        .await
        .unwrap()
        .unwrap();
    assert!(*starts.tasks[0].stop.borrow());
    starts.reap_finished();
    assert_eq!(starts.tasks.len(), 1, "cancellation cannot discard cleanup");
    assert!(starts.failure.is_none());
    release.send(()).unwrap();
    finished(&starts).await;
    starts.reap_finished();
    assert!(starts.tasks.is_empty());
    assert!(starts.failure.is_none());
}
