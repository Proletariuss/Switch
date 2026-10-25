use super::*;

fn wait_ending(owner: &Owner, binding: &Binding) {
    let worker = owner.worker(&binding.producer_id, &binding.reader).unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        if worker
            .state
            .lock()
            .unwrap()
            .lease
            .as_ref()
            .is_some_and(|lease| lease.ending.is_some())
        {
            return;
        }
        assert!(
            Instant::now() < until,
            "retirement never reached the exact lease"
        );
        thread::yield_now();
    }
}

#[test]
fn workflow_release_waits_for_blocked_enable_and_multisegment_recovery() {
    let fixture = Fixture::new("ESTKme fixture");
    fixture.open();
    let binding = fixture.acquire(1);
    let workflow = fixture.owner.retain_card_workflow(&binding).unwrap();
    workflow.recovery_started(Duration::from_secs(45)).unwrap();
    let (entered, unblock) = fixture.block_io();
    let (recovered_send, recovered) = mpsc::sync_channel(1);
    let (drop_guard, drop_wait) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        assert_eq!(
            workflow
                .with_transaction(|transport| transport.transmit(&[0x80, 0xe2, 0, 0]))
                .unwrap()
                .unwrap(),
            vec![0x90, 0]
        );
        assert!(workflow.checkpoint().is_err());
        workflow.reset(ResetAction::Reopen).unwrap();
        workflow.reset(ResetAction::Reset).unwrap();
        assert_eq!(
            workflow
                .with_transaction(|transport| transport.transmit(&[0, 0xca, 0, 0]))
                .unwrap()
                .unwrap(),
            vec![0x90, 0]
        );
        recovered_send.send(()).unwrap();
        drop_wait.recv().unwrap();
        drop(workflow);
    });
    entered.recv_timeout(Duration::from_secs(3)).unwrap();
    let owner = fixture.owner.clone();
    let released_binding = binding.clone();
    let (done_send, done) = mpsc::sync_channel(1);
    let release = thread::spawn(move || {
        done_send.send(owner.release(&released_binding)).unwrap();
    });
    wait_ending(&fixture.owner, &binding);
    assert!(done.try_recv().is_err());
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
    assert!(
        fixture
            .owner
            .with_transaction(Some(&binding), &binding.reader, |_| ())
            .is_err()
    );
    assert!(
        fixture
            .owner
            .transmit(&binding, None, vec![0, 0xa4, 0, 0], None)
            .is_err()
    );
    unblock.send(()).unwrap();
    recovered.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(done.try_recv().is_err());
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
    let (close_send, close_started) = mpsc::sync_channel(1);
    let (finish_close, close_wait) = mpsc::sync_channel(1);
    *fixture.trace.close_gate.lock().unwrap() = Some((close_send, close_wait));
    drop_guard.send(()).unwrap();
    worker.join().unwrap();
    close_started.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(
        done.try_recv().is_err(),
        "guard drop is not physical close confirmation"
    );
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
    finish_close.send(()).unwrap();
    done.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
    release.join().unwrap();
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], true);
    assert_eq!(*fixture.trace.resets.lock().unwrap(), vec![true]);
}

#[test]
fn workflow_retirement_before_recovery_cannot_arm_or_submit_work() {
    let fixture = Fixture::new("Generic reader");
    fixture.open();
    let binding = fixture.acquire(1);
    let workflow = fixture.owner.retain_card_workflow(&binding).unwrap();
    assert!(!fixture.owner.retire_operation_binding(&binding).unwrap());
    assert!(workflow.recovery_started(Duration::from_secs(45)).is_err());
    assert!(workflow.checkpoint().is_err());
    assert!(
        workflow
            .with_transaction(|_| panic!("retired work ran"))
            .is_err()
    );
    assert!(workflow.reset(ResetAction::Reopen).is_err());
    assert!(!fixture.owner.retire_operation_binding(&binding).unwrap());
    drop(workflow);
    fixture.owner.release(&binding).unwrap();
    assert!(fixture.owner.retire_operation_binding(&binding).unwrap());
    assert!(fixture.trace.calls.lock().unwrap().is_empty());
}

#[test]
fn workflow_expired_lease_recovers_without_renewing_fresh_authority() {
    let fixture = Fixture::new("ESTKme fixture");
    fixture.open();
    let binding = fixture.acquire(1);
    let workflow = fixture.owner.retain_card_workflow(&binding).unwrap();
    workflow.recovery_started(Duration::from_secs(45)).unwrap();
    assert!(workflow.recovery_started(Duration::from_secs(45)).is_err());
    let worker = fixture
        .owner
        .worker(&binding.producer_id, &binding.reader)
        .unwrap();
    worker
        .state
        .lock()
        .unwrap()
        .lease
        .as_mut()
        .unwrap()
        .deadline = Instant::now();
    assert!(workflow.checkpoint().is_err());
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
    workflow.reset(ResetAction::Reopen).unwrap();
    workflow
        .with_transaction(|transport| transport.transmit(&[0, 0xca, 0, 0]))
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .owner
            .renew(&binding, Duration::from_secs(120))
            .is_err()
    );
    drop(workflow);
    fixture.owner.release(&binding).unwrap();
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], true);
}

#[test]
fn workflow_shutdown_and_close_wait_until_actual_guard_retirement() {
    for shutdown in [false, true] {
        let fixture = Fixture::new("ESTKme fixture");
        fixture.open();
        let binding = fixture.acquire(1);
        let workflow = fixture.owner.retain_card_workflow(&binding).unwrap();
        workflow.recovery_started(Duration::from_secs(45)).unwrap();
        let owner = fixture.owner.clone();
        let requested = binding.clone();
        let (done_send, done) = mpsc::sync_channel(1);
        let retire = thread::spawn(move || {
            let result = if shutdown {
                owner.retire_blocking(None, None, true)
            } else {
                owner
                    .close(
                        &requested.producer_id,
                        &requested.reader,
                        &requested.session_id,
                        1,
                        Some(&requested),
                    )
                    .map(|_| ())
            };
            done_send.send(result).unwrap();
        });
        wait_ending(&fixture.owner, &binding);
        assert!(workflow.checkpoint().is_err());
        assert!(done.try_recv().is_err());
        workflow.reset(ResetAction::Reopen).unwrap();
        workflow
            .with_transaction(|transport| transport.transmit(&[0, 0xca, 0, 0]))
            .unwrap()
            .unwrap();
        assert!(done.try_recv().is_err());
        drop(workflow);
        done.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        retire.join().unwrap();
        let state = fixture.owner.inner.state.lock().unwrap();
        if shutdown {
            assert!(!state.active && state.readers.is_empty());
        } else {
            assert!(
                !state.readers[&fixture.reader]
                    .state
                    .lock()
                    .unwrap()
                    .port_open
            );
        }
    }
}

#[test]
fn workflow_recovery_deadline_rejects_new_io_but_joins_submitted_io() {
    let fixture = Fixture::new("ESTKme fixture");
    fixture.open();
    let binding = fixture.acquire(1);
    let workflow = fixture.owner.retain_card_workflow(&binding).unwrap();
    workflow
        .recovery_started(Duration::from_millis(30))
        .unwrap();
    let (entered, unblock) = fixture.block_io();
    let (completed_send, completed) = mpsc::sync_channel(1);
    let (drop_guard, drop_wait) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        workflow
            .with_transaction(|transport| transport.transmit(&[0, 0xca, 0, 0]))
            .unwrap()
            .unwrap();
        assert!(
            workflow
                .with_transaction(|_| panic!("expired recovery ran"))
                .is_err()
        );
        assert!(workflow.reset(ResetAction::Reopen).is_err());
        completed_send.send(()).unwrap();
        drop_wait.recv().unwrap();
        drop(workflow);
    });
    entered.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(!fixture.owner.retire_operation_binding(&binding).unwrap());
    thread::sleep(Duration::from_millis(40));
    assert_eq!(fixture.trace.closes.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
    unblock.send(()).unwrap();
    completed.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(!fixture.owner.retire_operation_binding(&binding).unwrap());
    drop_guard.send(()).unwrap();
    worker.join().unwrap();
    fixture.owner.release(&binding).unwrap();
    assert!(fixture.owner.retire_operation_binding(&binding).unwrap());
}

#[test]
fn workflow_failed_reopen_retains_exact_lease_for_power_reset_fallback() {
    let mut fixture = Fixture::new("ESTKme fixture");
    let trace = fixture.trace.clone();
    Arc::get_mut(&mut fixture.owner.inner).unwrap().factory = Arc::new(move |_| {
        let attempt = trace.opens.fetch_add(1, Ordering::SeqCst);
        if attempt == 1 {
            Err(Failure::unknown())
        } else {
            Ok(Box::new(Fake(trace.clone())))
        }
    });
    fixture.open();
    let binding = fixture.acquire(1);
    let workflow = fixture.owner.retain_card_workflow(&binding).unwrap();
    workflow.recovery_started(Duration::from_secs(45)).unwrap();
    assert!(!fixture.owner.retire_operation_binding(&binding).unwrap());
    assert!(workflow.reset(ResetAction::Reopen).is_err());
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], false);
    workflow.reset(ResetAction::Reset).unwrap();
    workflow
        .with_transaction(|transport| transport.transmit(&[0, 0xca, 0, 0]))
        .unwrap()
        .unwrap();
    assert!(workflow.checkpoint().is_err());
    drop(workflow);
    fixture.owner.release(&binding).unwrap();
    assert_eq!(fixture.owner.status(&binding).unwrap()["terminal"], true);
    assert_eq!(*fixture.trace.resets.lock().unwrap(), vec![true]);
}
