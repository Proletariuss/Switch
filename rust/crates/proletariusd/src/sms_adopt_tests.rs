use super::*;
use http_body_util::Full;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn root() -> PathBuf {
    let switch_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("Switch root");
    let parent = switch_root.join("build/tmp/sms-adoption-tests");
    fs::create_dir_all(&parent).unwrap();
    let root = parent.join(format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    root
}

fn request(method: Method, body: &[u8]) -> Request<Full<Bytes>> {
    Request::builder()
        .method(method)
        .uri("/api/sms/adopt")
        .body(Full::new(Bytes::copy_from_slice(body)))
        .unwrap()
}

async fn response_json(response: Response<Full<Bytes>>) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn legacy_adoption_json_loads_without_changing_the_file() {
    let root = root();
    let path = root.join("sms-adopt.json");
    let original = br#"{"enabled":true}"#;
    fs::write(&path, original).unwrap();
    let store = Store::new(&root);
    assert!(store.load().unwrap());
    assert_eq!(fs::read(path).unwrap(), original);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn corrupt_or_symlinked_state_is_not_followed_or_overwritten() {
    let root = root();
    let store = Store::new(&root);
    let path = root.join("sms-adopt.json");
    fs::write(&path, b"not json").unwrap();
    assert!(store.load().is_err());
    assert!(store.save(false).is_ok());

    let target = root.join("target.json");
    fs::write(&target, br#"{"enabled":true}"#).unwrap();
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(store.load().is_err());
    assert!(store.save(false).is_err());
    assert_eq!(fs::read(target).unwrap(), br#"{"enabled":true}"#);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn public_get_and_post_keep_the_existing_response_shapes() {
    let root = root();
    let owner = crate::usb_at::Owner::new("sms-adopt-test-producer".into());
    let store = Store::new(&root);
    let (status, value) =
        response_json(handle_public(request(Method::GET, b""), owner.clone(), store.clone()).await)
            .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, json!({"enabled":false}));

    let (status, value) = response_json(
        handle_public(
            request(Method::POST, br#"{"enabled":true}"#),
            owner.clone(),
            store.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, json!({"adopt":true}));
    assert!(store.load().unwrap());
    assert!(owner.adoption_enabled().await.unwrap());

    let (status, value) = response_json(
        handle_public(
            request(Method::POST, br#"{"enabled":false,"extra":1}"#),
            owner,
            store,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(value["error"].is_string());
    assert!(value.get("code").is_none());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn missing_null_trailing_and_oversized_bodies_match_legacy_request_semantics() {
    let root = root();
    let owner = crate::usb_at::Owner::new("sms-adopt-test-producer".into());
    let store = Store::new(&root);
    for body in [b"{}".as_slice(), b"null".as_slice()] {
        let (status, value) = response_json(
            handle_public(request(Method::POST, body), owner.clone(), store.clone()).await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value, json!({"adopt":false}));
    }

    let (status, value) = response_json(
        handle_public(
            request(Method::POST, b"{} {}"),
            owner.clone(),
            store.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"], "请求只能包含一个 JSON 对象");

    let oversized = vec![b' '; (1 << 20) + 1];
    let (status, value) =
        response_json(handle_public(request(Method::POST, &oversized), owner, store).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"], "http: request body too large");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn concurrent_updates_keep_runtime_and_durable_value_in_agreement() {
    let root = root();
    let owner = crate::usb_at::Owner::new("sms-adopt-test-producer".into());
    let store = Store::new(&root);
    let left_owner = owner.clone();
    let left_store = store.clone();
    let left = tokio::spawn(async move { left_owner.set_adoption(left_store, true).await });
    let right_owner = owner.clone();
    let right_store = store.clone();
    let right = tokio::spawn(async move { right_owner.set_adoption(right_store, false).await });
    left.await.unwrap().unwrap();
    right.await.unwrap().unwrap();
    assert_eq!(
        store.load().unwrap(),
        owner.adoption_enabled().await.unwrap()
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn persistence_failure_keeps_the_legacy_success_and_live_update_semantics() {
    let root = root();
    let target = root.join("target.json");
    fs::write(&target, br#"{"enabled":false}"#).unwrap();
    std::os::unix::fs::symlink(&target, root.join("sms-adopt.json")).unwrap();
    let owner = crate::usb_at::Owner::new("sms-adopt-test-producer".into());
    let store = Store::new(&root);
    let (status, value) = response_json(
        handle_public(
            request(Method::POST, br#"{"enabled":true}"#),
            owner.clone(),
            store.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, json!({"adopt":true}));
    assert!(owner.adoption_enabled().await.unwrap());
    assert_eq!(fs::read(target).unwrap(), br#"{"enabled":false}"#);
    assert!(store.load().is_err());
    fs::remove_dir_all(root).unwrap();
}

// A syntactically complete prefix is not accepted until the ingress reaches EOF.
struct UnfinishedAdoptionBody(Option<Bytes>);
impl hyper::body::Body for UnfinishedAdoptionBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        match self.0.take() {
            Some(bytes) => std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes)))),
            None => std::task::Poll::Pending,
        }
    }
}

#[tokio::test]
async fn s07_pending_adoption_never_dispatches_or_persists_and_limit_failure_recovers() {
    let root = root();
    let owner = crate::usb_at::Owner::new("sms-adopt-ingress-test".into());
    let store = Store::new(&root);
    let pending = Request::builder()
        .method(Method::POST)
        .uri("/api/sms/adopt")
        .body(UnfinishedAdoptionBody(Some(Bytes::from_static(
            br#"{"enabled":true}"#,
        ))))
        .unwrap();
    let (status, value) =
        response_json(handle_public(pending, owner.clone(), store.clone()).await).await;
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(value["code"], "request_body_timeout");
    assert!(!owner.adoption_enabled().await.unwrap());
    assert!(!root.join("sms-adopt.json").exists());
    let (status, value) = response_json(
        handle_public(
            request(Method::POST, &vec![b' '; MAX_REQUEST_BYTES + 1]),
            owner.clone(),
            store.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value["error"], "http: request body too large");
    assert!(!root.join("sms-adopt.json").exists());
    let (status, _) = response_json(
        handle_public(
            request(Method::POST, br#"{"enabled":true}"#),
            owner.clone(),
            store.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(owner.adoption_enabled().await.unwrap());
    assert!(store.load().unwrap());
    fs::remove_dir_all(root).unwrap();
}
