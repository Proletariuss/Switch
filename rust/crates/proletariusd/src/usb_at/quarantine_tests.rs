use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn trapped_owner(calls: Arc<AtomicUsize>) -> Owner {
    Owner::with_factory_adopt_recovery(
        "quarantined-producer".into(),
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(TransportError {
                code: "synthetic_usb_trap",
                message: "no physical USB".into(),
                response: String::new(),
                uncertain: false,
            })
        }),
        false,
        crate::module_recovery::Gate::quarantined_for_tests(),
    )
}

#[test]
fn quarantine_rejects_even_direct_ensure_open_before_mock_usb_factory() {
    let calls = Arc::new(AtomicUsize::new(0));
    let owner = trapped_owner(calls.clone());
    let error = ensure_open(&owner.inner, 0).unwrap_err();
    assert_eq!(error.code, crate::module_recovery::CODE);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn quarantine_rejects_module_hardware_observation_and_still_closes_idle_owner() {
    let calls = Arc::new(AtomicUsize::new(0));
    let owner = trapped_owner(calls.clone());
    let observed = owner.native_hardware_observation().await;
    owner.shutdown().await.unwrap();
    assert_eq!(observed.unwrap_err().code, crate::module_recovery::CODE);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn quarantine_does_not_authorize_shutdown_when_local_physical_work_is_unknown() {
    let owner = trapped_owner(Arc::new(AtomicUsize::new(0)));
    owner.inner.state.lock().unwrap().busy = true;
    let error = owner.shutdown().await.unwrap_err();
    assert_eq!(error.code, "module_quarantine_release_unconfirmed");
    assert!(!owner.inner.state.lock().unwrap().stopping);
}
