use super::*;
use crate::module_radio::tests::tests::{Fixture, request_public_route};

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() { tokio::task::yield_now().await; }
    }).await.unwrap();
}
fn allow_radio(fixture: &Fixture) {
    fixture.service.0.store.save(Preferences {
        enabled: false, restore_cfun: 1, user_configured: true,
    }).unwrap();
}
async fn waiting(service: &Service) {
    until(|| service.0.usbnet_waiters.load(Ordering::SeqCst) == 1
        && service.0.state.lock().unwrap().jobs == 0).await;
}

#[tokio::test]
async fn readonly_esim_wait_releases_slots_then_usbnet_http_writes_once_and_replays() {
    let mut fixture = Fixture::new();
    fixture.configure().await;
    allow_radio(&fixture);
    let lease = fixture.service.reserve_esim_read().await.unwrap();
    let service = fixture.service.clone();
    let body = r#"{"client_request_id":"native-read-usbnet-once","mode":1}"#;
    let request = tokio::spawn(async move {
        request_public_route(service, "POST", "/api/network/usbnet", body).await
    });
    waiting(&fixture.service).await;
    assert!(!request.is_finished());
    assert!(fixture.service.sim_channel_busy());
    fixture.empty();
    assert_eq!(fixture.service.0.slots.available_permits(), 8);
    fixture.service.release_esim(lease).await.unwrap();
    let (action, reply) = fixture.next().await;
    assert!(matches!(action, Action::Diagnostic { ref command, timeout_ms: 8_000 }
        if command == r#"AT+QCFG="usbnet",1"#));
    reply.send(Ok(Outcome {
        complete: true, submitted: true, verified: true, response: "OK".into(),
        ..Outcome::default()
    })).unwrap();
    let (status, receipt) = request.await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(receipt["mode"], 1);
    assert_eq!(receipt["needs_reboot"], true);
    assert_eq!(receipt["mutation_receipt"]["outcome"], "completed");
    let (status, replay) = request_public_route(
        fixture.service.clone(), "POST", "/api/network/usbnet", body,
    ).await;
    assert_eq!(status, 200);
    assert_eq!(replay, receipt);
    fixture.empty();
    fixture.service.finish_shutdown_receipt().await.unwrap();
}

#[tokio::test]
async fn ordinary_esim_write_call_and_unknown_read_keep_immediate_busy() {
    let mut fixture = Fixture::new();
    fixture.configure().await;
    allow_radio(&fixture);
    let write = fixture.service.reserve_esim().await.unwrap();
    let (status, body) = request_public_route(
        fixture.service.clone(), "POST", "/api/network/usbnet",
        r#"{"client_request_id":"esim-write-remains-busy","mode":1}"#,
    ).await;
    assert_eq!(status, 409);
    assert_eq!(body["code"], "module_radio_busy");
    assert_eq!(fixture.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
    fixture.empty();
    fixture.service.release_esim(write).await.unwrap();

    fixture.service.submit(Job::Reservation(ReservationRequest {
        producer_id: "producer-1".into(), action: "acquire".into(),
        purpose: Purpose::Call, token: "active-call".into(), sequence: 1,
    })).await.unwrap();
    let (status, _) = request_public_route(
        fixture.service.clone(), "POST", "/api/network/usbnet",
        r#"{"client_request_id":"active-call-remains-busy","mode":1}"#,
    ).await;
    assert_eq!(status, 409);
    assert_eq!(fixture.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
    fixture.empty();
    fixture.service.submit(Job::Reservation(ReservationRequest {
        producer_id: "producer-1".into(), action: "release".into(),
        purpose: Purpose::Call, token: "active-call".into(), sequence: 1,
    })).await.unwrap();

    let read = fixture.service.reserve_esim_read().await.unwrap();
    fixture.service.0.state.lock().unwrap().reservation.as_mut().unwrap().unknown = true;
    let binding = fixture.service.public_mutation_binding();
    let failure = fixture.service.submit_usbnet_mode_bound(1, &binding).await.unwrap_err();
    assert_eq!(failure.code, "module_radio_busy");
    assert!(!failure.submitted && !failure.uncertain);
    assert_eq!(fixture.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
    fixture.empty();
    fixture.service.release_esim(read).await.unwrap();
    fixture.service.finish_shutdown_receipt().await.unwrap();
}

#[test]
fn readonly_esim_marker_is_not_a_reservation_request_field() {
    let request = r#"{"producer_id":"producer-1","action":"acquire","purpose":"esim","token":"rust-esim-forged","sequence":1,"waitable_read":true}"#;
    assert!(serde_json::from_str::<ReservationRequest>(request).is_err());
}

#[tokio::test]
async fn readonly_esim_deadline_keeps_original_owner_and_never_writes() {
    let mut fixture = Fixture::new();
    fixture.configure().await;
    let lease = fixture.service.reserve_esim_read().await.unwrap();
    let token = lease.token.clone();
    let binding = fixture.service.public_mutation_binding();
    let failure = fixture.service.submit_usbnet_mode_until(
        1, &binding, Instant::now() + Duration::from_millis(40),
    ).await.unwrap_err();
    assert_eq!(failure.code, "module_radio_busy");
    assert!(!failure.submitted && !failure.uncertain);
    assert_eq!(fixture.service.0.state.lock().unwrap().reservation.as_ref().unwrap().identity.token, token);
    assert_eq!(fixture.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
    fixture.empty();
    fixture.service.release_esim(lease).await.unwrap();
    fixture.service.finish_shutdown_receipt().await.unwrap();
}

#[tokio::test]
async fn readonly_esim_wait_does_not_adopt_retired_owner_or_successor() {
    let mut fixture = Fixture::new();
    fixture.configure().await;
    let lease = fixture.service.reserve_esim_read().await.unwrap();
    let binding = fixture.service.public_mutation_binding();
    let service = fixture.service.clone();
    let request = tokio::spawn(async move {
        service.submit_usbnet_mode_bound(1, &binding).await
    });
    waiting(&fixture.service).await;
    fixture.service.retire_producer("producer-1".into()).await.unwrap();
    let failure = tokio::time::timeout(Duration::from_secs(5), request)
        .await.unwrap().unwrap().unwrap_err();
    assert_eq!(failure.code, "radio_stale_generation");
    assert!(!failure.submitted && !failure.uncertain);
    fixture.configure().await;
    let successor = fixture.service.reserve_esim_read().await.unwrap();
    assert_eq!(fixture.service.release_esim(lease).await.unwrap_err().code, "radio_stale_generation");
    assert!(fixture.service.sim_channel_busy());
    fixture.empty();
    fixture.service.release_esim(successor).await.unwrap();
    fixture.service.finish_shutdown_receipt().await.unwrap();
}

#[tokio::test]
async fn cancelled_http_read_wait_preserves_unknown_receipt_and_never_redispatches() {
    use tokio::io::AsyncWriteExt;
    let mut fixture = Fixture::new();
    fixture.configure().await;
    let lease = fixture.service.reserve_esim_read().await.unwrap();
    let token = lease.token.clone();
    let body = r#"{"client_request_id":"cancelled-native-read-http","mode":1}"#;
    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let service = fixture.service.clone();
    let serving = tokio::spawn(async move {
        crate::http_server::builder().serve_connection(
            hyper_util::rt::TokioIo::new(server),
            hyper::service::service_fn(move |request: Request<Incoming>| {
                let service = service.clone();
                async move {
                    Ok::<_, std::convert::Infallible>(handle_public(request, service, false).await)
                }
            }),
        ).await
    });
    client.write_all(format!(
        "POST /api/network/usbnet HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len(),
    ).as_bytes()).await.unwrap();
    waiting(&fixture.service).await;
    serving.abort();
    assert!(serving.await.unwrap_err().is_cancelled());
    drop(client);
    assert_eq!(fixture.service.0.usbnet_waiters.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.service.0.state.lock().unwrap().reservation.as_ref().unwrap().identity.token, token);
    fixture.service.release_esim(lease).await.unwrap();
    let (status, receipt) = request_public_route(
        fixture.service.clone(), "POST", "/api/network/usbnet", body,
    ).await;
    assert_eq!(status, 409);
    assert_eq!(receipt["code"], "public_mutation_outcome_unknown");
    assert_eq!(receipt["mutation_receipt"]["outcome"], "unknown");
    fixture.empty();
    fixture.service.finish_shutdown_receipt().await.unwrap();
}
