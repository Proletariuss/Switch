use super::*;
#[test]
fn shared_owner_is_exact_and_never_drop_released() {
    let g = Gate::default();
    g.set_go_producer("p").unwrap();
    let c = g.acquire_go("p", "module", "t", 1).unwrap();
    assert!(Arc::ptr_eq(
        &c,
        &g.acquire_go("p", "module", "t", 1).unwrap()
    ));
    assert!(g.acquire_native("n", 1).is_err());
    drop(c);
    assert!(g.acquire_native("n", 1).is_err());
    let c = g.acquire_go("p", "module", "t", 1).unwrap();
    c.release_confirmed().unwrap();
    assert!(!c.is_current());
    assert!(g.acquire_go("p", "module", "t", 1).is_err());
    let n = g.acquire_native("n", 1).unwrap();
    c.release_confirmed().unwrap();
    assert!(n.is_current());
}
#[test]
fn confirmed_child_exit_does_not_retire_native_owner() {
    let g = Gate::default();
    g.set_go_producer("p").unwrap();
    let n = g.acquire_native("n", 1).unwrap();
    g.retire_go_producer("p", "q").unwrap();
    assert!(n.is_current());
    n.release_confirmed().unwrap();
    assert!(g.acquire_go("p", "reader", "t", 2).is_err());
    assert!(g.acquire_go("q", "reader", "t", 2).is_ok());
}
#[test]
fn shutdown_keeps_claim_until_exact_join_proof() {
    let g = Gate::default();
    g.set_go_producer("p").unwrap();
    let c = g.acquire_native("n", 1).unwrap();
    g.begin_shutdown();
    assert!(c.is_current());
    assert!(g.acquire_native("n", 1).is_err());
    c.release_confirmed().unwrap();
    assert!(!c.is_current());
}

#[test]
fn release_before_late_acquire_keeps_a_tombstone() {
    let g = Gate::default();
    g.set_go_producer("p").unwrap();
    let key = Key::Go("p".into(), "reader".into(), "cancelled".into(), 7);
    assert!(!g.go_status(&key, true).unwrap());
    assert!(g.acquire_go("p", "reader", "cancelled", 7).is_err());
    let n = g.acquire_native("native", 1).unwrap();
    assert!(!g.go_status(&key, true).unwrap());
    assert!(n.is_current());
}
#[test]
fn retirement_cannot_reuse_a_producer_or_resurrect_old_claim() {
    let g = Gate::default();
    g.set_go_producer("p").unwrap();
    let old = g.acquire_go("p", "module", "t", 1).unwrap();
    assert!(g.retire_go_producer("p", "p").is_err());
    assert!(old.is_current());
    g.retire_go_producer("p", "q").unwrap();
    let new = g.acquire_go("q", "module", "t", 1).unwrap();
    old.release_confirmed().unwrap();
    assert!(new.is_current());
}
#[test]
fn simultaneous_native_and_legacy_start_have_one_winner() {
    let g = Arc::new(Gate::default());
    g.set_go_producer("p").unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let a = g.clone();
    let b = barrier.clone();
    let native = std::thread::spawn(move || {
        b.wait();
        a.acquire_native("native", 1).is_ok()
    });
    let a = g.clone();
    let b = barrier.clone();
    let legacy = std::thread::spawn(move || {
        b.wait();
        a.acquire_go("p", "module", "t", 1).is_ok()
    });
    barrier.wait();
    assert_ne!(native.join().unwrap(), legacy.join().unwrap());
}
#[tokio::test]
async fn http_is_strict_bounded_and_cleanup_survives_shutdown() {
    async fn call(g: Arc<Gate>, body: &str, stopping: bool) -> (StatusCode, serde_json::Value) {
        let r = handle(
            Request::builder()
                .method("POST")
                .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
                .unwrap(),
            g,
            stopping,
        )
        .await;
        let status = r.status();
        let data = r.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&data).unwrap())
    }
    let g = Arc::new(Gate::default());
    g.set_go_producer("p").unwrap();
    let body =
        r#"{"action":"acquire","producer_id":"p","source":"module","token":"t","sequence":1}"#;
    assert_eq!(call(g.clone(), body, false).await.0, StatusCode::OK);
    for body in [
        r#"{"action":"release","producer_id":"p","source":"module","token":"t","sequence":1}"#,
        r#"{"action":"acquire","producer_id":"p","source":"module","token":"t","sequence":1,"extra":true}"#,
        r#"{"action":"acquire","producer_id":"p","source":"module","token":"t","sequence":1,"sequence":2}"#,
    ] {
        assert_eq!(
            call(g.clone(), body, false).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(g.clone(), &"x".repeat(4097), false).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(g.clone(), body, true).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let release = r#"{"action":"release","producer_id":"p","source":"module","token":"t","sequence":1,"finished":true}"#;
    let (status, reply) = call(g.clone(), release, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["released"], true);
    assert!(g.acquire_go("p", "module", "t", 1).is_err());
}

#[test]
fn initial_selection_fence_blocks_reader_start_without_stopping_an_independent_module() {
    let gate = Gate::default();
    gate.set_go_producer("go").unwrap();
    let module = gate.acquire_go("go", "module", "module", 1).unwrap();
    let maintenance = gate
        .begin_reader_maintenance("new-reader", 0, MaintenancePolicy::Reject)
        .unwrap();
    assert!(module.is_current());
    assert!(
        gate.acquire_native_reader("native", 1, "new-reader", 1)
            .is_err()
    );
    assert!(gate.acquire_go("go", "reader", "reader", 1).is_err());
    assert!(Arc::ptr_eq(
        &module,
        &gate.acquire_go("go", "module", "module", 1).unwrap()
    ));
    assert!(maintenance.ordinary_drained());
    maintenance.release_confirmed().unwrap();
    assert!(module.is_current());
    module.release_confirmed().unwrap();
}

#[test]
fn stop_fence_requires_exact_native_reader_generation_before_publication() {
    let gate = Gate::default();
    let native = gate
        .acquire_native_reader("native", 1, "reader", 4)
        .unwrap();
    for (reader, generation) in [("other", 4), ("reader", 5), ("reader", 0)] {
        assert!(
            gate.begin_reader_maintenance(reader, generation, MaintenancePolicy::StopAndDrain)
                .is_err()
        );
        gate.validate_reader_claim(&native, "reader", 4).unwrap();
        assert!(native.is_current());
    }
    assert!(
        gate.begin_reader_maintenance("reader", 4, MaintenancePolicy::Reject)
            .is_err()
    );
    let maintenance = gate
        .begin_reader_maintenance("reader", 4, MaintenancePolicy::StopAndDrain)
        .unwrap();
    assert!(Arc::ptr_eq(&maintenance.native_claim().unwrap(), &native));
    assert!(maintenance.release_confirmed().is_err());
    assert!(gate.validate_reader_claim(&native, "reader", 4).is_err());
    assert!(native.is_current());
    native.release_confirmed().unwrap();
    assert!(maintenance.ordinary_drained());
    assert!(
        gate.acquire_native_reader("native", 2, "reader", 4)
            .is_err()
    );
    maintenance.release_confirmed().unwrap();
    assert!(gate.acquire_native_reader("native", 2, "reader", 4).is_ok());
}

#[test]
fn dropped_maintenance_and_old_release_cannot_open_a_newer_slot() {
    let gate = Gate::default();
    let old = gate
        .begin_reader_maintenance("reader", 4, MaintenancePolicy::Reject)
        .unwrap();
    let retained = old.clone();
    drop(old);
    assert!(
        gate.acquire_native_reader("native", 1, "reader", 4)
            .is_err()
    );
    assert!(
        gate.begin_reader_maintenance("reader", 4, MaintenancePolicy::Reject)
            .is_err()
    );
    retained.release_confirmed().unwrap();
    let next = gate
        .begin_reader_maintenance("reader", 4, MaintenancePolicy::Reject)
        .unwrap();
    retained.release_confirmed().unwrap();
    retained.abort_undispatched().unwrap();
    assert!(next.is_current());
    assert!(
        gate.acquire_native_reader("native", 1, "reader", 4)
            .is_err()
    );
    next.release_confirmed().unwrap();
    assert!(gate.acquire_native_reader("native", 1, "reader", 4).is_ok());
}

#[test]
fn foreign_and_unbound_claims_cannot_authorize_ordinary_identity_preparation() {
    let gate = Gate::default();
    let other = Gate::default();
    let foreign = other
        .acquire_native_reader("native", 1, "reader", 4)
        .unwrap();
    assert!(gate.validate_reader_claim(&foreign, "reader", 4).is_err());
    assert!(foreign.validate_reader_start("reader", 4).is_ok());
    let unbound = gate.acquire_native("native", 1).unwrap();
    assert!(gate.validate_reader_claim(&unbound, "reader", 4).is_err());
    assert!(
        gate.begin_reader_maintenance("reader", 4, MaintenancePolicy::StopAndDrain)
            .is_err()
    );
    unbound.release_confirmed().unwrap();
    let bound = gate
        .acquire_native_reader("native", 2, "reader", 4)
        .unwrap();
    gate.validate_reader_claim(&bound, "reader", 4).unwrap();
    for (reader, generation) in [("other", 4), ("reader", 5)] {
        assert!(
            gate.validate_reader_claim(&bound, reader, generation)
                .is_err()
        );
    }
    assert!(
        gate.acquire_native_reader("native", 3, "reader", 0)
            .is_err()
    );
}

#[test]
fn captured_go_reader_status_is_queryable_but_lost_acquire_ack_cannot_restart_during_maintenance() {
    let gate = Gate::default();
    gate.set_go_producer("go").unwrap();
    let reader = gate.acquire_go("go", "reader", "reader", 1).unwrap();
    let maintenance = gate
        .begin_reader_maintenance("reader", 4, MaintenancePolicy::StopAndDrain)
        .unwrap();
    assert!(
        gate.go_status(
            &Key::Go("go".into(), "reader".into(), "reader".into(), 1),
            false
        )
        .unwrap()
    );
    assert!(gate.acquire_go("go", "reader", "reader", 1).is_err());
    assert!(maintenance.native_claim().is_none());
    assert!(maintenance.release_confirmed().is_err());
    reader.release_confirmed().unwrap();
    assert!(maintenance.ordinary_drained());
    maintenance.release_confirmed().unwrap();
    assert!(gate.acquire_go("go", "reader", "reader", 1).is_err());
    assert!(gate.acquire_go("go", "reader", "reader", 2).is_ok());
}

#[test]
fn actual_parallel_maintenance_and_native_start_have_one_atomic_winner() {
    let gate = Arc::new(Gate::default());
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let start_gate = gate.clone();
    let start_barrier = barrier.clone();
    let start = std::thread::spawn(move || {
        start_barrier.wait();
        start_gate
            .acquire_native_reader("native", 1, "reader", 4)
            .is_ok()
    });
    let maintenance_gate = gate.clone();
    let maintenance_barrier = barrier.clone();
    let maintenance = std::thread::spawn(move || {
        maintenance_barrier.wait();
        maintenance_gate
            .begin_reader_maintenance("reader", 4, MaintenancePolicy::Reject)
            .is_ok()
    });
    barrier.wait();
    assert_ne!(start.join().unwrap(), maintenance.join().unwrap());
}

#[test]
fn shutdown_and_child_exit_never_drop_a_maintenance_capability() {
    let gate = Gate::default();
    gate.set_go_producer("old").unwrap();
    let owner = gate.acquire_go("old", "reader", "reader", 1).unwrap();
    let maintenance = gate
        .begin_reader_maintenance("reader", 4, MaintenancePolicy::StopAndDrain)
        .unwrap();
    gate.begin_shutdown();
    gate.retire_go_producer("old", "next").unwrap();
    assert!(!owner.is_current());
    assert!(maintenance.is_current());
    assert!(maintenance.ordinary_drained());
    maintenance.release_confirmed().unwrap();
    assert!(!maintenance.is_current());
    assert!(
        gate.acquire_native_reader("native", 1, "reader", 4)
            .is_err()
    );
}

#[test]
fn legacy_business_drop_and_go_exit_observation_do_not_release_a_physical_guard() {
    let gate = Gate::default();
    gate.set_go_producer("go").unwrap();
    let guard = gate.begin_legacy_reader_business(false).unwrap();
    let retained = guard.clone();
    drop(guard);
    assert!(
        gate.acquire_native_reader("native", 1, "reader", 4)
            .is_err()
    );
    assert!(
        gate.begin_reader_maintenance("reader", 4, MaintenancePolicy::StopAndDrain)
            .is_err()
    );
    let go = gate.acquire_go("go", "reader", "reader", 1).unwrap();
    assert!(
        go.is_current(),
        "legacy native fence must not alter Go ordinary admission"
    );
    gate.retire_go_producer("go", "next").unwrap();
    assert!(
        retained.is_current(),
        "physical completion, not child exit alone, retires legacy guard"
    );
    assert!(
        gate.acquire_native_reader("native", 1, "reader", 4)
            .is_err()
    );
    retained.release_confirmed().unwrap();
    let native = gate
        .acquire_native_reader("native", 1, "reader", 4)
        .unwrap();
    retained.release_confirmed().unwrap();
    assert!(native.is_current());
}

#[test]
fn ordinary_go_session_promotion_authorizes_stop_but_lpa_operation_with_same_go_claim_does_not() {
    let gate = Gate::default();
    gate.set_go_producer("go").unwrap();
    let go = gate.acquire_go("go", "reader", "reader", 1).unwrap();
    let operation = gate.begin_legacy_reader_business(false).unwrap();
    assert!(
        gate.begin_reader_maintenance("reader", 4, MaintenancePolicy::StopAndDrain)
            .is_err()
    );
    assert!(go.is_current());
    gate.promote_legacy_reader_business(&operation).unwrap();
    let maintenance = gate
        .begin_reader_maintenance("reader", 4, MaintenancePolicy::StopAndDrain)
        .unwrap();
    go.release_confirmed().unwrap();
    assert!(!maintenance.ordinary_drained());
    assert!(maintenance.release_confirmed().is_err());
    operation.release_confirmed().unwrap();
    assert!(maintenance.ordinary_drained());
    maintenance.release_confirmed().unwrap();
}

#[test]
fn legacy_to_multistandby_conversion_has_no_native_start_gap_or_stale_release() {
    let gate = Gate::default();
    let operation = gate.begin_legacy_reader_business(false).unwrap();
    let multistandby = gate
        .transfer_to_maintenance(&operation, "reader", 4)
        .unwrap();
    assert!(!operation.is_current());
    operation.release_confirmed().unwrap();
    assert!(multistandby.is_current());
    assert!(
        gate.acquire_native_reader("native", 1, "reader", 4)
            .is_err()
    );
    assert!(gate.begin_legacy_reader_business(false).is_err());
    multistandby.release_confirmed().unwrap();
    let next = gate.begin_legacy_reader_business(false).unwrap();
    operation.release_confirmed().unwrap();
    assert!(next.is_current());
    assert!(
        gate.transfer_to_maintenance(&operation, "reader", 4)
            .is_err()
    );
    next.release_confirmed().unwrap();
}

#[test]
fn all_ordinary_maintenance_rejects_module_and_blocks_late_start_without_drop_release() {
    let gate = Gate::default();
    gate.set_go_producer("go").unwrap();
    let module = gate.acquire_go("go", "module", "module", 1).unwrap();
    assert!(
        gate.begin_reader_maintenance("Reader", 4, MaintenancePolicy::RejectAllOrdinary)
            .is_err()
    );
    // CardOnly/N-wait retain the old module coexistence policy.
    let card_only = gate
        .begin_reader_maintenance("Reader", 4, MaintenancePolicy::Reject)
        .unwrap();
    card_only.release_confirmed().unwrap();
    module.release_confirmed().unwrap();
    let guard = gate
        .begin_reader_maintenance("Reader", 4, MaintenancePolicy::RejectAllOrdinary)
        .unwrap();
    gate.validate_reader_maintenance(&guard, "Reader", 4, MaintenancePolicy::RejectAllOrdinary)
        .unwrap();
    assert!(gate.acquire_go("go", "module", "module", 2).is_err());
    assert!(gate.acquire_go("go", "reader", "reader", 1).is_err());
    assert!(
        gate.acquire_native_reader("native", 1, "Reader", 4)
            .is_err()
    );
    let retained = guard.clone();
    drop(guard);
    assert!(gate.acquire_go("go", "module", "module", 2).is_err());
    retained.release_confirmed().unwrap();
    let next = gate.acquire_go("go", "module", "module", 2).unwrap();
    retained.release_confirmed().unwrap();
    assert!(next.is_current());
    next.release_confirmed().unwrap();
}

#[test]
fn all_ordinary_maintenance_and_module_start_have_one_atomic_winner() {
    for _ in 0..32 {
        let gate = Gate::default();
        gate.set_go_producer("go").unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let b = barrier.clone();
        let g = gate.clone();
        let module = std::thread::spawn(move || {
            b.wait();
            g.acquire_go("go", "module", "module", 1)
        });
        barrier.wait();
        let maintenance =
            gate.begin_reader_maintenance("Reader", 0, MaintenancePolicy::RejectAllOrdinary);
        let module = module.join().unwrap();
        assert_ne!(maintenance.is_ok(), module.is_ok());
        if let Ok(guard) = maintenance {
            guard.release_confirmed().unwrap();
        }
        if let Ok(claim) = module {
            claim.release_confirmed().unwrap();
        }
    }
}

#[test]
fn go_claim_observation_is_exact_and_sensitive_binding_is_not_an_authority() {
    let gate = Gate::default();
    gate.set_go_producer("go").unwrap();
    assert!(gate.current_go_claim().unwrap().is_none());
    assert!(!gate.has_go_ordinary_claim());
    let first = gate.acquire_go("go", "module", "secret-token", 1).unwrap();
    let observed = gate.current_go_claim().unwrap().unwrap();
    assert!(Arc::ptr_eq(&first, &observed));
    let binding = observed.go_binding().unwrap();
    assert_eq!(binding.producer, "go");
    assert_eq!(binding.source, "module");
    assert_eq!(binding.token, "secret-token");
    assert_eq!(binding.sequence, 1);
    first.release_confirmed().unwrap();
    let second = gate.acquire_go("go", "reader", "second-token", 1).unwrap();
    observed.release_confirmed().unwrap();
    assert!(second.is_current());
    assert!(!observed.is_current());
    assert!(gate.has_go_ordinary_claim());
    second.release_confirmed().unwrap();
    let native = gate
        .acquire_native_reader("native", 1, "Reader", 4)
        .unwrap();
    assert!(native.go_binding().is_none());
    assert!(gate.current_go_claim().unwrap().is_none());
    native.release_confirmed().unwrap();
}

#[test]
fn maintenance_validation_rejects_foreign_wrong_generation_and_stale_capability() {
    let gate = Gate::default();
    let other = Gate::default();
    let guard = gate
        .begin_reader_maintenance("Reader", 4, MaintenancePolicy::RejectAllOrdinary)
        .unwrap();
    assert!(
        other
            .validate_reader_maintenance(&guard, "Reader", 4, MaintenancePolicy::RejectAllOrdinary)
            .is_err()
    );
    assert!(
        gate.validate_reader_maintenance(&guard, "Other", 4, MaintenancePolicy::RejectAllOrdinary)
            .is_err()
    );
    assert!(
        gate.validate_reader_maintenance(&guard, "Reader", 5, MaintenancePolicy::RejectAllOrdinary)
            .is_err()
    );
    assert!(
        gate.validate_reader_maintenance(&guard, "Reader", 4, MaintenancePolicy::Reject)
            .is_err()
    );
    guard.release_confirmed().unwrap();
    let current = gate
        .begin_reader_maintenance("Reader", 4, MaintenancePolicy::RejectAllOrdinary)
        .unwrap();
    assert!(
        gate.validate_reader_maintenance(&guard, "Reader", 4, MaintenancePolicy::RejectAllOrdinary)
            .is_err()
    );
    assert!(current.is_current());
    current.release_confirmed().unwrap();
}

#[test]
fn poisoned_go_owner_observation_never_confirms_release_or_absence() {
    let gate = Gate::default();
    gate.set_go_producer("go").unwrap();
    let claim = gate.acquire_go("go", "module", "token", 1).unwrap();
    let inner = gate.0.clone();
    let _ = std::thread::spawn(move || {
        let _state = inner.state.lock().unwrap();
        panic!("controlled poisoned owner lock");
    })
    .join();
    assert!(gate.current_go_claim().is_err());
    assert!(gate.has_go_ordinary_claim());
    assert!(claim.try_is_current().is_err());
    assert!(claim.is_current());
}

#[tokio::test]
async fn cleanup_handler_ready_empty_body_does_not_release_claim_and_returns_owned_slot() {
    struct ReadyEmpty {
        first: std::sync::Mutex<Option<Bytes>>,
        _pinned: std::marker::PhantomPinned,
    }
    impl hyper::body::Body for ReadyEmpty {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            let bytes = self
                .as_ref()
                .get_ref()
                .first
                .lock()
                .unwrap()
                .take()
                .unwrap_or_default();
            std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes))))
        }
    }
    let gate = Arc::new(Gate::default());
    gate.set_go_producer("ingress-cleanup-producer").unwrap();
    let claim = gate
        .acquire_go("ingress-cleanup-producer", "reader", "retained-token", 1)
        .unwrap();
    let request = Request::builder().method("POST").uri(PATH).body(ReadyEmpty {
        first: std::sync::Mutex::new(Some(Bytes::from_static(br#"{"producer_id":"ingress-cleanup-producer","source":"reader","token":"retained-token","sequence":1,"action":"release","finished":true}"#))),
        _pinned: std::marker::PhantomPinned,
    }).unwrap();
    // A private one-slot owner models ingress reservation lifetime without
    // exhausting the process-global budget used by concurrent tests.
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = slots.clone().acquire_owned().await.unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(7), async {
        let _permit = permit;
        handle(request, gate.clone(), true).await
    })
    .await
    .expect("cleanup ingress retains the original five-second deadline");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let value: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        value,
        json!({"error":"invalid admission request","code":"invalid_request"})
    );
    assert_eq!(slots.available_permits(), 1);
    assert!(
        claim.is_current(),
        "incomplete release body must not retire the original owner"
    );
    claim.release_confirmed().unwrap();
}
