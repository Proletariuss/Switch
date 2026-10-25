//! Download admission, preflight rejection and cleanup use the real UDS/PTY
//! path. The fixture rejects eUICC information before any SM-DP+ request; this
//! does not stand in for successful network authentication or BPP installation.
use super::*;

#[tokio::test]
async fn serial_download_preflight_failure_closes_channel_releases_radio_and_expires_confirmation()
{
    let _journal = crate::esim_test_support::journal_guard().await;
    let f = Fixture::new().await;
    let listener = tokio::net::UnixListener::bind(&f.state.go_socket_path).unwrap();
    f.state.availability.mark_ready();
    let actor = f.state.modem_actor.clone();
    assert!(
        !tokio::task::spawn_blocking(move || actor.serial_sms_observation())
            .await
            .unwrap()
            .unwrap()
            .ready
    );

    let (status, admitted) = request(
        &f.state,
        "POST",
        "/api/esim/download",
        json!({
            "client_request_id":"serial-download-preflight",
            "smdp":"example.invalid",
            "matching_id":"preflight-fixture",
            "aid":AID,
            "imei":"350225641234561"
        }),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{admitted}");
    let id = admitted["id"]
        .as_str()
        .expect("native durable operation ID")
        .to_owned();
    tokio::time::timeout(Duration::from_secs(15), async {
        while crate::reader_esim_download::has_active_operation() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("preflight rejection must finish its retained worker");
    let (status, terminal) =
        request(&f.state, "GET", "/api/esim/operation", Value::Null, true).await;
    assert_eq!(status, StatusCode::OK, "{terminal}");
    assert_eq!(terminal["operation"]["id"], id);
    assert_eq!(terminal["operation"]["state"], "failed", "{terminal}");
    assert_eq!(
        terminal["operation"]["error_code"], "download_failed",
        "{terminal}"
    );
    assert!(terminal["operation"]["finished_at"].is_string());
    assert!(
        !f.state.module_radio.sim_channel_busy(),
        "RF reservation survived the completed failure"
    );

    let commands = f.port.card.commands.lock().unwrap().clone();
    let opens = commands
        .iter()
        .filter(|command| command.as_str() == format!("AT+CCHO=\"{AID}\""))
        .count();
    assert!(opens > 0, "test must reach the real card channel");
    assert!(
        commands
            .iter()
            .any(|command| command.starts_with("AT+CGLA="))
    );
    assert_eq!(
        opens,
        commands
            .iter()
            .filter(|command| command.starts_with("AT+CCHC="))
            .count(),
        "failed preflight left an open logical channel"
    );
    assert!(!f.port.card.enabled.load(Ordering::Acquire));
    assert!(!f.port.card.deleted.load(Ordering::Acquire));
    let before_confirmation = commands.len();
    let (status, confirmation) = request(
        &f.state,
        "POST",
        "/api/esim/download/confirm",
        json!({"operation_id":id, "confirm":true}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{confirmation}");
    assert_eq!(confirmation["code"], "esim_download_confirmation_expired");
    assert_eq!(
        f.port.card.commands.lock().unwrap().len(),
        before_confirmation,
        "expired confirmation started hardware work"
    );
    let (status, overview) =
        request(&f.state, "GET", "/api/esim?refresh=true", Value::Null, true).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "cleanup must leave the original card readable: {overview}"
    );
    assert_eq!(overview["profiles"][0]["profiles"][0]["iccid"], ICCID);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err(),
        "native serial download or confirmation contacted Go"
    );
    drop(listener);
    f.finish().await;
}
