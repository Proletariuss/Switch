//! Public audio-route controls. Rust owns readiness admission, radio/ADB
//! observation, the USB voice-transition lease, and route-helper lifecycle.

use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde_json::json;

use crate::{call_mode_adb, message_archive, module_radio, usb_at};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioRequest {
    client_request_id: String,
    #[serde(default)]
    audio_generation: Option<String>,
}
static PUBLIC_AUDIO_ADMISSION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const READINESS_TIMEOUT: Duration = Duration::from_secs(35);
const ROUTE_TIMEOUT: Duration = Duration::from_secs(180);

pub(crate) async fn handle_public(
    request: Request<Incoming>,
    data_root: &Path,
    inbox: message_archive::InboxState,
    module_radio: module_radio::Service,
    usb_at: usb_at::Owner,
) -> Response<Full<Bytes>> {
    let binding = audio_binding(&module_radio);
    let expected = binding.clone();
    handle_public_with(request, data_root, binding, |start| {
        execute_public(start, data_root, inbox, module_radio, usb_at, &expected)
    })
    .await
}
async fn handle_public_with<B, F, Fut>(
    request: Request<B>,
    data_root: &Path,
    binding: String,
    dispatch: F,
) -> Response<Full<Bytes>>
where
    B: hyper::body::Body<Data = Bytes> + Send,
    F: FnOnce(bool) -> Fut,
    Fut: std::future::Future<Output = Response<Full<Bytes>>>,
{
    let path = request.uri().path();
    if !matches!(path, "/api/call/audio/start" | "/api/call/audio/stop") {
        return error_response(StatusCode::NOT_FOUND, "route not found");
    }
    if request.method() != Method::POST {
        return error_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }

    let start = path == "/api/call/audio/start";
    if request.uri().query().is_some() {
        return error_response(StatusCode::BAD_REQUEST, "unexpected query");
    }
    // A v2 Candidate is not an executable request on this route. In
    // particular, never let the permissive v1 ID alphabet claim a new m2 ID
    // while the GUI has no trusted monotonic checkpoint and the physical
    // audio owner still uses the v1 receipt protocol.
    let has_version_header = request
        .headers()
        .contains_key("x-switch-mutation-identity-version");
    let data = match crate::http_server::collect_body_with_timeout(
        std::pin::pin!(request.into_body()),
        4096,
        Duration::from_secs(3),
    )
    .await
    {
        Ok(v) => v,
        _ => return error_response(StatusCode::BAD_REQUEST, "invalid audio mutation body"),
    };
    if has_version_header {
        return crate::public_mutation_receipt::error(
            StatusCode::UPGRADE_REQUIRED,
            "audio_identity_v2_unavailable",
        );
    }
    let input: AudioRequest = match serde_json::from_slice(&data) {
        Ok(v) => v,
        Err(_) => return error_response(StatusCode::BAD_REQUEST, "invalid audio mutation body"),
    };
    if !crate::public_mutation_receipt::valid_id(&input.client_request_id)
        || (start && input.audio_generation.is_some())
        || (!start
            && input
                .audio_generation
                .as_deref()
                .is_none_or(|v| !crate::public_mutation_receipt::valid_id(v)))
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "audio mutation identity/generation required",
        );
    }
    if input.client_request_id.starts_with("m2:") {
        // A historical v1 client was allowed to choose this spelling. Keep
        // its exact receipt replay, including an unknown outcome, while
        // refusing fresh v1 admission under a future v2 wire identity.
        let route = if start {
            "/api/call/audio/start"
        } else {
            "/api/call/audio/stop"
        };
        return match crate::public_mutation_receipt::replay_async(
            data_root,
            route,
            &input.client_request_id,
            &data,
        )
        .await
        {
            Ok(Some(response)) => response,
            Ok(None) => crate::public_mutation_receipt::error(
                StatusCode::UPGRADE_REQUIRED,
                "audio_identity_v2_unavailable",
            ),
            Err(response) => response,
        };
    }
    let _admission = PUBLIC_AUDIO_ADMISSION.lock().await;
    let module_binding = binding.clone();
    let binding = format!(
        "{}:audio:{}",
        binding,
        input
            .audio_generation
            .as_deref()
            .unwrap_or(&input.client_request_id)
    );
    crate::public_mutation_receipt::run(
        data_root,
        if start {
            "/api/call/audio/start"
        } else {
            "/api/call/audio/stop"
        },
        &input.client_request_id,
        &data,
        binding,
        || async {
            if !start {
                match crate::public_mutation_receipt::latest_async(
                    data_root,
                    "/api/call/audio/start",
                )
                .await
                {
                    Ok(Some((generation, original_binding)))
                        if Some(generation.as_str()) == input.audio_generation.as_deref()
                            && original_binding
                                == format!("{module_binding}:audio:{generation}") => {}
                    Ok(_) => {
                        return crate::public_mutation_receipt::error(
                            StatusCode::CONFLICT,
                            "stale_audio_generation",
                        );
                    }
                    Err(_) => {
                        return crate::public_mutation_receipt::error(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "public_mutation_store_unavailable",
                        );
                    }
                }
            }
            dispatch(start).await
        },
    )
    .await
}
fn audio_binding(radio: &module_radio::Service) -> String {
    format!(
        "{}:voice-runtime:{}",
        radio.public_mutation_binding(),
        call_mode_adb::public_runtime_binding()
    )
}
async fn execute_public(
    start: bool,
    data_root: &Path,
    inbox: message_archive::InboxState,
    module_radio: module_radio::Service,
    usb_at: usb_at::Owner,
    expected_binding: &str,
) -> Response<Full<Bytes>> {
    if audio_binding(&module_radio) != expected_binding {
        return crate::public_mutation_receipt::error(
            StatusCode::CONFLICT,
            "stale_audio_module_binding",
        );
    }
    let (runtime_version, operation_lock, admitted_producer) = if start {
        let operation_lock =
            match crate::call_mode_backups::try_call_mode_operation_shared_lock(data_root) {
                Ok(lock) => lock,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return error_response(
                        StatusCode::CONFLICT,
                        "模块配置或通话模式正在变更，请等待操作完成",
                    );
                }
                Err(error) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        &format!("无法确认模块通话模式操作状态：{error}"),
                    );
                }
            };
        let root = data_root.to_path_buf();
        let task_inbox = inbox.clone();
        let task_radio = module_radio.clone();
        let readiness = tokio::spawn(async move {
            let (version, producer) = generation_bound_readiness(&task_inbox, |producer| {
                call_readiness(&root, &task_inbox, task_radio, producer)
            })
            .await?;
            Ok::<_, String>((version, operation_lock, producer))
        });
        match tokio::time::timeout(READINESS_TIMEOUT, readiness).await {
            Ok(Ok(Ok((version, operation_lock, producer)))) => {
                (Some(version), Some(operation_lock), Some(producer))
            }
            Ok(Ok(Err(message))) => return error_response(StatusCode::CONFLICT, &message),
            Ok(Err(_)) => {
                return error_response(StatusCode::SERVICE_UNAVAILABLE, "通话模式就绪检查未完成");
            }
            // Dropping a JoinHandle detaches the task, so it retains the shared
            // lock until accepted module and ADB reads have actually finished.
            Err(_) => {
                return error_response(StatusCode::GATEWAY_TIMEOUT, "模块通话模式就绪检查超过时限");
            }
        }
    } else {
        (None, None, None)
    };

    let Some(producer_id) = admitted_producer.or_else(|| inbox.current_module_producer_id()) else {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "当前后端代次不可用");
    };
    if !inbox.owns_module_producer(&producer_id) {
        return error_response(
            StatusCode::CONFLICT,
            "后端代次已变化，请重新确认通话模式就绪状态",
        );
    }
    if audio_binding(&module_radio) != expected_binding {
        return crate::public_mutation_receipt::error(
            StatusCode::CONFLICT,
            "stale_audio_module_binding",
        );
    }
    let action = if start {
        call_mode_adb::VoiceRouteAction::Start
    } else {
        call_mode_adb::VoiceRouteAction::Stop
    };
    match tokio::time::timeout(
        ROUTE_TIMEOUT,
        call_mode_adb::execute_voice_route_action(
            data_root,
            inbox,
            usb_at,
            module_radio,
            producer_id,
            action,
            operation_lock.map(call_mode_adb::VoiceRouteGuard::shared),
        ),
    )
    .await
    {
        Ok(Ok(true)) if start => crate::json_response(
            StatusCode::OK,
            &json!({"started": true, "runtime_version": runtime_version.unwrap_or_default()}),
        ),
        Ok(Ok(false)) if !start => crate::json_response(StatusCode::OK, &json!({"stopped": true})),
        Ok(Ok(_)) => error_response(StatusCode::BAD_GATEWAY, "模块没有确认语音路由状态"),
        Ok(Err(error)) => error_response(StatusCode::BAD_GATEWAY, &error.message),
        Err(_) => error_response(StatusCode::GATEWAY_TIMEOUT, "模块语音路由操作超过时限"),
    }
}

async fn generation_bound_readiness<F, Fut>(
    inbox: &message_archive::InboxState,
    read: F,
) -> Result<(String, String), String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let producer = inbox
        .current_module_producer_id()
        .ok_or_else(|| "当前后端代次不可用".to_owned())?;
    let version = read(producer.clone()).await?;
    if !inbox.owns_module_producer(&producer) {
        return Err("后端代次已变化，请重新确认通话模式就绪状态".into());
    }
    Ok((version, producer))
}

pub(crate) async fn call_readiness(
    data_root: &Path,
    inbox: &message_archive::InboxState,
    module_radio: module_radio::Service,
    producer_id: String,
) -> Result<String, String> {
    let facts = module_radio
        .call_mode_readiness_facts()
        .await
        .map_err(|error| error.message)?;

    validate_call_interfaces(facts.usb, facts.voice)?;
    call_mode_adb::probe_module_adb(
        module_radio.module_recovery(),
        data_root,
        &producer_id,
        inbox.clone(),
    )
    .await
    .map_err(|error| error.message)?;
    if !crate::call_mode_runtime::installed_runtime_verified(data_root).await {
        return Err("请先在通话页面下载并准备语音运行时".into());
    }
    if !call_mode_adb::is_module_voice_runtime_prepared(
        module_radio.module_recovery(),
        data_root,
        &producer_id,
        inbox.clone(),
    )
    .await
    {
        return Err("模块侧语音运行时尚未部署完成，请等待通话页面显示已就绪".into());
    }
    Ok(crate::call_mode_runtime_manifest::manifest()
        .version
        .clone())
}

fn validate_call_interfaces(
    usb: crate::module_radio_transport::UsbComposition,
    voice: Option<crate::module_radio_transport::VoiceConfiguration>,
) -> Result<(), String> {
    if usb.flags[5] != 1 || usb.flags[6] != 1 {
        return Err("请先在通话页面开启通话模式".into());
    }
    let voice = voice.ok_or_else(|| "Rust IMS/VoLTE 查询未返回已验证状态".to_owned())?;
    if voice.ims != 1 || voice.volte_capability != 1 || voice.volte_disabled != 0 {
        return Err(format!(
            "IMS/VoLTE 尚未就绪：IMS={}、VoLTE capability={}、volte_disabled={}",
            voice.ims, voice.volte_capability, voice.volte_disabled
        ));
    }
    Ok(())
}

fn error_response(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    crate::json_response(status, &json!({"error": message}))
}

#[cfg(test)]
mod tests {
    struct IngressBody {
        ready_empty: bool,
        polled: std::sync::Arc<tokio::sync::Notify>,
        _pinned: std::marker::PhantomPinned,
    }
    impl hyper::body::Body for IngressBody {
        type Data = bytes::Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
            self.polled.notify_one();
            if self.ready_empty {
                std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes::Bytes::new()))))
            } else {
                std::task::Poll::Pending
            }
        }
    }

    async fn ingress_expires_without_dispatch_and_releases_gate(ready_empty: bool) {
        use http_body_util::BodyExt;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../build/runtime/stability-695")
            .join(format!(
                "audio-ingress-{}",
                crate::snapshot::new_instance_id()
            ));
        assert!(!root.exists());
        let gate = crate::shutdown::CleanupGate::default();
        let permit = gate.enter().await.unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let dispatched = Arc::new(AtomicBool::new(false));
        let observed = dispatched.clone();
        let body = IngressBody {
            ready_empty,
            polled: started.clone(),
            _pinned: std::marker::PhantomPinned,
        };
        let request = hyper::Request::builder()
            .method(hyper::Method::POST)
            .uri("/api/call/audio/start")
            .body(body)
            .unwrap();
        let task_root = root.clone();
        let task = tokio::spawn(async move {
            // Same guard ownership as main's retained cleanup handler: body
            // expiry must drop the guard without submitting a physical owner.
            let _permit = permit;
            super::handle_public_with(request, &task_root, "ingress".into(), move |_| async move {
                observed.store(true, Ordering::SeqCst);
                crate::json_response(hyper::StatusCode::OK, &serde_json::json!({}))
            })
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
            .await
            .unwrap();
        let closing_gate = gate.clone();
        let closing = tokio::spawn(async move { closing_gate.close().await });
        tokio::task::yield_now().await;
        assert!(!closing.is_finished());
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        // This existing endpoint deliberately reports invalid audio body400
        // for all ingress failures, including its original three-second timeout.
        assert_eq!(response.status(), hyper::StatusCode::BAD_REQUEST);
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["error"], "invalid audio mutation body");
        assert!(!dispatched.load(Ordering::SeqCst));
        assert!(!root.exists());
        tokio::time::timeout(std::time::Duration::from_secs(1), closing)
            .await
            .unwrap()
            .unwrap();
        assert!(gate.enter().await.is_none());
    }

    #[tokio::test]
    async fn pending_and_ready_empty_network_bodies_expire_before_audio_dispatch_and_release_cleanup_gate()
     {
        tokio::join!(
            ingress_expires_without_dispatch_and_releases_gate(false),
            ingress_expires_without_dispatch_and_releases_gate(true),
        );
    }

    async fn request(
        root: std::path::PathBuf,
        path: &str,
        body: serde_json::Value,
    ) -> (hyper::StatusCode, serde_json::Value, usize) {
        request_with_binding(root, path, body, "mock-module-1".into()).await
    }
    async fn request_with_binding(
        root: std::path::PathBuf,
        path: &str,
        body: serde_json::Value,
        binding: String,
    ) -> (hyper::StatusCode, serde_json::Value, usize) {
        request_with_binding_and_version(root, path, body, binding, None).await
    }
    async fn request_with_binding_and_version(
        root: std::path::PathBuf,
        path: &str,
        body: serde_json::Value,
        binding: String,
        version: Option<&str>,
    ) -> (hyper::StatusCode, serde_json::Value, usize) {
        use bytes::Bytes;
        use http_body_util::{BodyExt, Full};
        use hyper::{Request, StatusCode};
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let (client, server) = tokio::io::duplex(128 * 1024);
        let server = tokio::spawn(async move {
            hyper::server::conn::http1::Builder::new().serve_connection(hyper_util::rt::TokioIo::new(server),hyper::service::service_fn(move |request| {
                let root=root.clone();let counted=counted.clone();let binding=binding.clone();
                async move {Ok::<_,std::convert::Infallible>(super::handle_public_with(request,&root,binding,move |start|async move {
                    counted.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                    crate::json_response(StatusCode::OK,&serde_json::json!({"started":start,"stopped":!start,"runtime_version":"fixture"}))
                }).await)}
            })).await.unwrap();
        });
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(client))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        let mut builder = Request::builder().method("POST").uri(path);
        if let Some(version) = version {
            builder = builder.header("x-switch-mutation-identity-version", version);
        }
        let r = sender
            .send_request(
                builder
                    .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = r.status();
        let value =
            serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
        drop(sender);
        connection.await.unwrap().unwrap();
        server.await.unwrap();
        (
            status,
            value,
            calls.load(std::sync::atomic::Ordering::SeqCst),
        )
    }
    #[tokio::test]
    async fn v2_audio_identity_never_enters_v1_owner_and_legacy_m2_replays_exactly() {
        use hyper::StatusCode;
        use serde_json::json;
        let root = std::path::PathBuf::from(std::env::var_os("SWITCH_BUILD_DIR").unwrap())
            .join("runtime/audio-v2-gate")
            .join(crate::snapshot::new_instance_id());
        let id = "m2:pa:0123456789abcdef0123456789abcdef:0000000000000001:0000000000000001";
        let body = json!({"client_request_id":id});
        for version in [None, Some("2"), Some("1"), Some("3")] {
            let (status, response, calls) = request_with_binding_and_version(
                root.clone(),
                "/api/call/audio/start",
                body.clone(),
                "mock-module-1".into(),
                version,
            )
            .await;
            assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
            assert_eq!(response["code"], "audio_identity_v2_unavailable");
            assert_eq!(calls, 0);
        }
        assert!(!root.join(crate::public_mutation_receipt::FILE).exists());

        // The old v1 alphabet admitted arbitrary m2-shaped strings. A frozen
        // historical receipt keeps its original result and never re-dispatches.
        let data = serde_json::to_vec(&body).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let original = crate::public_mutation_receipt::run(
            &root,
            "/api/call/audio/start",
            id,
            &data,
            format!("mock-module-1:audio:{id}"),
            || async { crate::json_response(StatusCode::OK, &json!({"started":true})) },
        )
        .await;
        assert_eq!(original.status(), StatusCode::OK);
        let (status, replay, calls) = request(root.clone(), "/api/call/audio/start", body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replay["mutation_receipt"]["client_request_id"], id);
        assert_eq!(calls, 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn cancelled_audio_waiter_late_owner_completion_cannot_regrant_start_or_block_exact_stop()
    {
        use bytes::Bytes;
        use http_body_util::Full;
        use hyper::{Request, StatusCode};
        use serde_json::json;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let root = std::path::PathBuf::from(std::env::var_os("SWITCH_BUILD_DIR").unwrap())
            .join("runtime/audio-cancel-owner")
            .join(crate::snapshot::new_instance_id());
        std::fs::create_dir_all(&root).unwrap();
        let id = "audio-start-late-owner";
        let payload = json!({"client_request_id":id});
        let start_request = Request::builder()
            .method("POST")
            .uri("/api/call/audio/start")
            .body(Full::new(Bytes::from(
                serde_json::to_vec(&payload).unwrap(),
            )))
            .unwrap();
        let (entered, admitted) = tokio::sync::oneshot::channel();
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let completed = std::sync::Arc::new(AtomicUsize::new(0));
        let task_root = root.clone();
        let worker_release = release.clone();
        let worker_completed = completed.clone();
        let waiter = tokio::spawn(async move {
            super::handle_public_with(
                start_request,
                &task_root,
                "mock-module-1".into(),
                move |_| async move {
                    tokio::spawn(async move {
                        worker_release.notified().await;
                        worker_completed.fetch_add(1, Ordering::SeqCst);
                    });
                    entered.send(()).unwrap();
                    std::future::pending::<hyper::Response<Full<Bytes>>>().await
                },
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), admitted)
            .await
            .unwrap()
            .unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while completed.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let (status, receipt, calls) =
            request(root.clone(), "/api/call/audio/start", payload).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(receipt["mutation_receipt"]["outcome"], "unknown");
        assert_eq!(calls, 0);
        let (status, _, calls) = request(
            root.clone(),
            "/api/call/audio/stop",
            json!({"client_request_id":"audio-stop-late-owner","audio_generation":id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(calls, 1);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn public_audio_http_identity_replay_restart_and_old_stop_cannot_retire_successor() {
        use hyper::StatusCode;
        use serde_json::json;
        let root = std::path::PathBuf::from(std::env::var_os("SWITCH_BUILD_DIR").unwrap())
            .join("runtime/audio-http-receipts")
            .join(crate::snapshot::new_instance_id());
        for path in ["/api/call/audio/start", "/api/call/audio/stop"] {
            for body in [
                json!({}),
                json!({"client_request_id":null}),
                json!({"client_request_id":"bad id"}),
                json!({"client_request_id":"valid-id-0001","extra":1}),
            ] {
                let (status, _, calls) = request(root.clone(), path, body).await;
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert_eq!(calls, 0);
            }
        }
        let start = json!({"client_request_id":"audio-start-original"});
        assert_eq!(
            request(root.clone(), "/api/call/audio/start", start.clone())
                .await
                .2,
            1
        );
        let stop = json!({"client_request_id":"audio-stop-original","audio_generation":"audio-start-original"});
        let (status, receipt, calls) =
            request(root.clone(), "/api/call/audio/stop", stop.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(calls, 1);
        assert_eq!(
            request(
                root.clone(),
                "/api/call/audio/start",
                json!({"client_request_id":"audio-start-successor"})
            )
            .await
            .2,
            1
        );
        let (status, replay, calls) = request(root.clone(), "/api/call/audio/stop", stop).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(receipt, replay);
        assert_eq!(calls, 0);
        let (status,_,calls)=request(root.clone(),"/api/call/audio/stop",json!({"client_request_id":"audio-stop-stale-new","audio_generation":"audio-start-original"})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(calls, 0);
        let (status, _, calls) = request(root.clone(), "/api/call/audio/start", start).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(calls, 0);
        let (status,_,calls)=request_with_binding(root.clone(),"/api/call/audio/stop",json!({"client_request_id":"audio-stop-old-module","audio_generation":"audio-start-successor"}),"replacement-module-2".into()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(calls, 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn completed_readiness_cannot_rebind_to_a_replacement_producer() {
        let inbox = crate::message_archive::InboxState::for_go_producer("old".into());
        let worker_inbox = inbox.clone();
        let (entered, observed) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            super::generation_bound_readiness(&worker_inbox, |producer| async move {
                entered.send(producer).unwrap();
                resumed.await.unwrap();
                Ok("verified-runtime".into())
            })
            .await
        });
        assert_eq!(observed.await.unwrap(), "old");
        inbox.replace_go_producer("new".into()).unwrap();
        resume.send(()).unwrap();
        assert_eq!(
            worker.await.unwrap().unwrap_err(),
            "后端代次已变化，请重新确认通话模式就绪状态"
        );
    }

    #[tokio::test]
    async fn native_readiness_remains_bound_while_go_rotates_during_physical_observation() {
        let inbox = crate::message_archive::InboxState::for_go_producer("go-old".into());
        inbox
            .bind_native_module_producer("rust-module".into())
            .unwrap();
        let worker_inbox = inbox.clone();
        let (entered, observed) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            super::generation_bound_readiness(&worker_inbox, |producer| async move {
                entered.send(producer).unwrap();
                resumed.await.unwrap();
                Ok("verified-runtime".into())
            })
            .await
        });
        assert_eq!(observed.await.unwrap(), "rust-module");
        inbox.replace_go_producer("go-new".into()).unwrap();
        assert!(!worker.is_finished());
        resume.send(()).unwrap();
        assert_eq!(
            worker.await.unwrap().unwrap(),
            ("verified-runtime".into(), "rust-module".into())
        );
        assert!(!inbox.owns_current_producer("rust-module"));
    }

    #[tokio::test]
    async fn admitted_readiness_returns_the_exact_observed_producer() {
        let inbox = crate::message_archive::InboxState::for_go_producer("current".into());
        let result = super::generation_bound_readiness(&inbox, |producer| async move {
            assert_eq!(producer, "current");
            Ok("verified-runtime".into())
        })
        .await
        .unwrap();
        assert_eq!(result, ("verified-runtime".into(), "current".into()));
    }

    #[test]
    fn readiness_preserves_usb_before_voice_error_order() {
        let usb = crate::module_radio_transport::UsbComposition {
            vendor_id: 1,
            product_id: 2,
            flags: [0, 0, 0, 0, 0, 0, 0],
        };
        assert_eq!(
            super::validate_call_interfaces(usb, None).unwrap_err(),
            "请先在通话页面开启通话模式"
        );
    }

    #[test]
    fn readiness_reports_legacy_ims_volte_facts_when_usb_is_ready() {
        let usb = crate::module_radio_transport::UsbComposition {
            vendor_id: 1,
            product_id: 2,
            flags: [0, 0, 0, 0, 0, 1, 1],
        };
        let voice = crate::module_radio_transport::VoiceConfiguration {
            ims: 0,
            volte_capability: 1,
            volte_disabled: 0,
        };
        assert_eq!(
            super::validate_call_interfaces(usb, Some(voice)).unwrap_err(),
            "IMS/VoLTE 尚未就绪：IMS=0、VoLTE capability=1、volte_disabled=0"
        );
    }
}
