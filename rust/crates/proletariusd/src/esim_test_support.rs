//! Tests invoking the production singleton serialize journal workflows, just
//! as the daemon admits one operation at a time. The journal's directory lives
//! for the test process, independently of a device fixture's cleanup.
pub(crate) async fn journal_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    static INITIALIZE: std::sync::Once = std::sync::Once::new();
    let guard = GATE.lock().await;
    INITIALIZE.call_once(|| {
        let root = crate::tests::test_directory("native-esim-process-journal");
        crate::reader_esim_download::initialize(&root)
            .expect("initialize the process journal before any fixture uses it");
        crate::esim_mutation_intent::initialize(&root)
            .expect("initialize mutation intents before any fixture uses them");
    });
    guard
}

/// Full authenticated public HTTP dispatch over a local socket pair, with no
/// listener, device, network peer or compatibility backend.
pub(crate) async fn refresh_request(
    state: crate::ServerState,
    path: &str,
) -> (hyper::StatusCode, serde_json::Value) {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper_util::rt::TokioIo;
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let server = tokio::spawn(async move {
        if let Err(error) = crate::serve_connection(server, state).await {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let response = tokio::time::timeout(
        crate::esim_refresh::WAITER_TIMEOUT + std::time::Duration::from_secs(5),
        sender.send_request(
            hyper::Request::builder()
                .uri(path)
                .header(crate::APP_TOKEN_HEADER, "app-fixture")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    server.await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn receipt_events() -> &'static tokio::sync::broadcast::Sender<serde_json::Value> {
    static EVENTS: std::sync::OnceLock<tokio::sync::broadcast::Sender<serde_json::Value>> =
        std::sync::OnceLock::new();
    EVENTS.get_or_init(|| tokio::sync::broadcast::channel(128).0)
}
pub(crate) fn observe_refresh_receipts() -> tokio::sync::broadcast::Receiver<serde_json::Value> {
    receipt_events().subscribe()
}
pub(crate) fn record_refresh_receipt(receipt: serde_json::Value) {
    let _ = receipt_events().send(receipt);
}
pub(crate) async fn next_refresh_receipt(
    events: &mut tokio::sync::broadcast::Receiver<serde_json::Value>,
    id: &str,
    phase: &str,
) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match events.recv().await {
                Ok(receipt) if receipt["operation_id"] == id && receipt["phase"] == phase => {
                    return receipt;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => panic!("receipt observer ended: {error}"),
            }
        }
    })
    .await
    .expect("retained worker must publish its actual terminal receipt")
}
