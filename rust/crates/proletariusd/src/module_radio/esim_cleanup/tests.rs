use super::*;
use crate::module_radio::tests::tests::Fixture;

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn ordinary_status_cannot_borrow_esim_owner_but_cleanup_can() {
    let fixture = Fixture::new();
    fixture.configure().await;
    let reservation = fixture.service.reserve_esim().await.unwrap();
    assert_eq!(
        fixture
            .service
            .borrow_shared_voice_operation("producer-1", false)
            .err()
            .unwrap()
            .code,
        "module_radio_busy"
    );
    let cleanup = fixture
        .service
        .borrow_shared_voice_operation("producer-1", true)
        .unwrap();
    drop(cleanup);
    fixture.service.release_esim(reservation).await.unwrap();
    let ordinary = fixture
        .service
        .borrow_shared_voice_operation("producer-1", false)
        .unwrap();
    drop(ordinary);
    fixture.service.finish_shutdown_receipt().await.unwrap();
}

#[tokio::test]
async fn cleanup_waits_for_shared_pin_then_restores_network_admission() {
    let fixture = Fixture::new();
    fixture.configure().await;
    let reservation = fixture.service.reserve_esim().await.unwrap();
    let token = reservation.token.clone();
    let sequence = reservation.sequence;
    let pin = fixture
        .service
        .borrow_shared_voice_operation("producer-1", true)
        .unwrap();
    let service = fixture.service.clone();
    let release = tokio::spawn(async move { service.release_esim(reservation).await });
    until(|| fixture.service.esim_release_pending_for_test()).await;
    assert!(!release.is_finished());
    assert!(fixture.service.sim_channel_busy());
    {
        let state = fixture.service.0.state.lock().unwrap();
        assert_eq!(state.reservation.as_ref().unwrap().identity.token, token);
        assert!(!state.waters.get(&Purpose::Esim).unwrap().released);
    }
    drop(pin);
    tokio::time::timeout(Duration::from_secs(5), release)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    {
        let state = fixture.service.0.state.lock().unwrap();
        let water = state.waters.get(&Purpose::Esim).unwrap();
        assert_eq!((&water.token, water.sequence, water.released), (&token, sequence, true));
        assert!(state.reservation.is_none());
    }
    assert!(!fixture.service.sim_channel_busy());
    // USB-net changes acquire this same Diagnostic reservation in production.
    fixture.service.submit(Job::Reservation(ReservationRequest {
        producer_id: "producer-1".into(),
        action: "acquire".into(),
        purpose: Purpose::Diagnostic,
        token: "network-after-cleanup".into(),
        sequence: 1,
    })).await.unwrap();
    fixture.service.submit(Job::Reservation(ReservationRequest {
        producer_id: "producer-1".into(),
        action: "release".into(),
        purpose: Purpose::Diagnostic,
        token: "network-after-cleanup".into(),
        sequence: 1,
    })).await.unwrap();
    fixture.service.finish_shutdown_receipt().await.unwrap();
}

#[tokio::test]
async fn cancelled_cleanup_waiter_drains_through_full_queue_and_shutdown() {
    let fixture = Fixture::new();
    fixture.configure().await;
    let reservation = fixture.service.reserve_esim().await.unwrap();
    let pin = fixture
        .service
        .borrow_shared_voice_operation("producer-1", true)
        .unwrap();
    // One permit belongs to the real shared physical owner; fill the rest.
    let slots = fixture.service.0.slots.clone().acquire_many_owned(7).await.unwrap();
    let gate = fixture.service.0.gate.lock().await;
    let service = fixture.service.clone();
    let waiter = tokio::spawn(async move { service.release_esim(reservation).await });
    until(|| fixture.service.esim_release_pending_for_test()).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let service = fixture.service.clone();
    let shutdown = tokio::spawn(async move { service.finish_shutdown_receipt().await });
    until(|| fixture.service.0.state.lock().unwrap().stopping).await;
    assert!(!shutdown.is_finished());
    assert!(fixture.service.sim_channel_busy());
    drop(gate);
    drop(pin);
    // The retained release does not need a fresh queue slot.
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!fixture.service.esim_release_pending_for_test());
    let state = fixture.service.0.state.lock().unwrap();
    assert!(state.finalized);
    assert!(state.reservation.is_none());
    assert_eq!((state.jobs, state.active_shared_voice_pins), (0, 0));
    assert!(state.waters.get(&Purpose::Esim).unwrap().released);
    drop(state);
    drop(slots);
}

#[tokio::test]
async fn retired_cleanup_cannot_release_successor_reservation() {
    let fixture = Fixture::new();
    fixture.configure().await;
    let reservation = fixture.service.reserve_esim().await.unwrap();
    // A delayed old completion is test input only; native handles are non-Clone.
    let stale = EsimReservation {
        producer_id: reservation.producer_id.clone(),
        epoch: reservation.epoch,
        token: reservation.token.clone(),
        sequence: reservation.sequence,
    };
    let pin = fixture
        .service
        .borrow_shared_voice_operation("producer-1", true)
        .unwrap();
    let service = fixture.service.clone();
    let release = tokio::spawn(async move { service.release_esim(reservation).await });
    until(|| fixture.service.esim_release_pending_for_test()).await;
    let service = fixture.service.clone();
    let retirement = tokio::spawn(async move {
        service.retire_producer("producer-1".into()).await
    });
    until(|| fixture.service.0.state.lock().unwrap().retiring).await;
    assert!(!retirement.is_finished());
    drop(pin);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), release)
            .await.unwrap().unwrap().unwrap_err().code,
        "radio_stale_generation"
    );
    tokio::time::timeout(Duration::from_secs(5), retirement)
        .await.unwrap().unwrap().unwrap();
    fixture.configure().await;
    let successor = fixture.service.reserve_esim().await.unwrap();
    let successor_token = successor.token.clone();
    assert_eq!(
        fixture.service.release_esim(stale).await.unwrap_err().code,
        "radio_stale_generation"
    );
    assert_eq!(
        fixture.service.0.state.lock().unwrap().reservation.as_ref().unwrap().identity.token,
        successor_token
    );
    assert!(fixture.service.sim_channel_busy());
    fixture.service.release_esim(successor).await.unwrap();
    fixture.service.finish_shutdown_receipt().await.unwrap();
}
