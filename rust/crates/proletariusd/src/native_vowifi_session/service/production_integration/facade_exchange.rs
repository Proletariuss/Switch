//! Public runtime boundary composed entirely in Rust, with real local IKE/SIP.
use super::*;
use crate::multistandby_native_runtime::{Action, ConnectionOptions, Resources};
use crate::multistandby_native_service::Service as Facade;
pub(super) fn resources(
    physical: &crate::multistandby_card::production::NativeFixture,
    root: &std::path::Path,
    connection: ConnectionOptions,
) -> Resources {
    let (owner, gate, card_service) = physical.inventory_resources();
    Resources {
        owner,
        gate,
        card_service,
        data_root: root.into(),
        store: crate::multistandby_store::Store::open(root).unwrap(),
        journal: Arc::new(std::sync::Mutex::new(
            crate::multistandby_runtime_report::Journal::open(&root.join("runtime-report.json"), 1),
        )),
        transactions: Default::default(),
        connection,
    }
}
// Exercise the real public handlers over HTTP, while injecting only the typed
// fixture resources. There is no Go UDS listener or alternate executor.
pub(super) async fn public_http(
    facade: Arc<Facade>,
    store: crate::multistandby_store::Store,
    operations: crate::multistandby_operations::Store,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (hyper::StatusCode, serde_json::Value) {
    use http_body_util::{BodyExt, Full};
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let server = tokio::spawn(async move {
        let result = hyper::server::conn::http1::Builder::new()
            .serve_connection(
                hyper_util::rt::TokioIo::new(server),
                hyper::service::service_fn(
                    move |request: hyper::Request<hyper::body::Incoming>| {
                        let facade = facade.clone();
                        let store = store.clone();
                        let operations = operations.clone();
                        async move {
                            let response = match request.uri().path() {
                                "/api/multistandby/cards" => {
                                    crate::multistandby_store::handle_cards(store, facade).await
                                }
                                "/api/multistandby/profiles" => {
                                    crate::multistandby_store::handle_profiles(
                                        request, store, facade,
                                    )
                                    .await
                                }
                                "/api/multistandby/messages/send" => {
                                    crate::multistandby_send::handle_public(request, store, facade)
                                        .await
                                }
                                _ => {
                                    crate::multistandby_operations::handle_public(
                                        request, operations, facade,
                                    )
                                    .await
                                }
                            };
                            Ok::<_, std::convert::Infallible>(response)
                        }
                    },
                ),
            )
            .await;
        if let Err(error) = result {
            assert!(
                crate::benign_connection_error_message(&error.to_string()),
                "{error}"
            );
        }
    });
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(client))
            .await
            .unwrap();
    let connection = tokio::spawn(connection);
    let request = hyper::Request::builder()
        .method(method)
        .uri(path)
        .body(Full::new(bytes::Bytes::from(
            serde_json::to_vec(&body).unwrap(),
        )))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    drop(sender);
    if let Err(error) = connection.await.unwrap() {
        assert!(
            crate::benign_connection_error_message(&error.to_string()),
            "{error}"
        );
    }
    server.await.unwrap();
    (status, value)
}
async fn ready(facade: &Facade, profile: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !facade.ready(profile).channel_ready {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn facade_discovers_native_profiles_and_runs_existing_nwait_sms_without_go_rpc() {
    const CHILD: &str = "SWITCH_NATIVE_FACADE_497_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = PathBuf::from(std::env::var("SWITCH_BUILD_DIR").unwrap()).join(format!(
            "runtime/native-facade-wire-497-{}",
            crate::multistandby_store::random_token().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("carrier.yaml");
        std::fs::write(
            &config,
            "carrier_overrides:\n  '234/33': {ip_stack: ipv4}\n",
        )
        .unwrap();
        let output=tokio::task::spawn_blocking(move ||std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","native_vowifi_session::service::production_integration::facade_exchange::facade_discovers_native_profiles_and_runs_existing_nwait_sms_without_go_rpc","--nocapture"])
            .env(CHILD,&root).env("SWITCH_DATA_DIR",&root).env("PROLETARIUS_VOWIFI_CONFIG",config).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    tokio::time::timeout(Duration::from_secs(45),async {
        let root=PathBuf::from(std::env::var_os(CHILD).unwrap());
        let physical=crate::multistandby_card::production::NativeFixture::new();physical.pool.shutdown().await.unwrap();
        let discover=Facade::new(resources(&physical,&root,ConnectionOptions::default())).unwrap();
        let inventory=discover.snapshot("",true,false).await.unwrap();assert_eq!(inventory["available"],true,"{inventory}");
        let profiles=discover.snapshot("",false,false).await.unwrap();let rows=profiles["profiles"].as_array().unwrap();
        let first=rows.iter().find(|r|r["iccid"]==physical.profile(true).iccid).unwrap()["profile_id"].as_str().unwrap().to_owned();
        let second=rows.iter().find(|r|r["iccid"]==physical.profile(false).iccid).unwrap()["profile_id"].as_str().unwrap().to_owned();
        let card=rows[0]["card_id"].as_str().unwrap().to_owned();
        assert_eq!(physical.auth_count(),0);assert_eq!(physical.switch_count(),0);
        discover.shutdown().await.unwrap();
        let store=crate::multistandby_store::Store::open(&root).unwrap();
        let(peer,mut sms)=ike_peer::Peer::with_multi_sms(root.clone(),store.clone(),first.clone()).await;
        let facade=Facade::new(resources(&physical,&root,ConnectionOptions{proxy:peer.proxy.to_string(),epdg:"epdg.invalid".into(),epdg_port:Some(500)})).unwrap();
        assert_eq!(facade.snapshot("",true,true).await.unwrap()["card_profiles"],inventory["card_profiles"]);
        let operations=crate::multistandby_operations::Store::open(&root).unwrap();operations.recover().unwrap();
        let(status,cards)=public_http(facade.clone(),store.clone(),operations.clone(),"GET","/api/multistandby/cards",serde_json::json!({})).await;
        assert_eq!(status,hyper::StatusCode::OK);assert_eq!(cards["cards"].as_array().unwrap().len(),1);
        let(status,profiles)=public_http(facade.clone(),store.clone(),operations.clone(),"GET","/api/multistandby/profiles",serde_json::json!({})).await;
        assert_eq!(status,hyper::StatusCode::OK);assert_eq!(profiles["profiles"].as_array().unwrap().len(),2);
        let(release,entered)=physical.hold_next_apdu();
        let start=serde_json::json!({"card_id":card,"profile_ids":[first],"client_request_id":"facade-http-start"});
        let(status,admitted)=public_http(facade.clone(),store.clone(),operations.clone(),"POST","/api/multistandby/start",start.clone()).await;
        assert_eq!(status,hyper::StatusCode::ACCEPTED);
        // The HTTP connection has closed while the retained operation is still
        // waiting for an actual card reply. Replaying its ID must not dispatch.
        tokio::time::timeout(Duration::from_secs(5),async {while !entered.load(Ordering::Acquire){tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();
        let(_,replayed)=public_http(facade.clone(),store.clone(),operations.clone(),"POST","/api/multistandby/start",start).await;
        assert_eq!(replayed,admitted);assert_eq!(physical.auth_count(),0);
        release.send(()).unwrap();ready(&facade,&first).await;
        let operation_path=format!("/api/multistandby/operations/{}",admitted["operation_id"].as_str().unwrap());
        let(status,receipt)=public_http(facade.clone(),store.clone(),operations.clone(),"GET",&operation_path,serde_json::json!({})).await;
        assert_eq!(status,hyper::StatusCode::OK);assert_eq!(receipt["state"],"accepted");
        assert_eq!(sms.next().await,sms_peer::Event::IncomingAccepted);assert_eq!(sms.next().await,sms_peer::Event::IncomingRpAck);
        facade.execute(Action::Start,card.clone(),vec![second.clone()]).await.unwrap();ready(&facade,&second).await;
        assert_eq!(facade.snapshot("",true,false).await.unwrap()["available"],true,"active Pool refresh uses inventory cache");
        let(status,queued)=public_http(facade.clone(),store.clone(),operations.clone(),"POST","/api/multistandby/messages/send",serde_json::json!({"profile_id":first,"client_request_id":"facade-mo-request","phone":sms_peer::RECIPIENT,"body":sms_peer::OUTGOING})).await;
        assert_eq!(status,hyper::StatusCode::ACCEPTED);
        assert!(facade.maintenance());assert!(matches!(sms.next().await,sms_peer::Event::Submitted(_)));assert_eq!(sms.next().await,sms_peer::Event::SubmitRpAckAccepted);
        tokio::time::timeout(Duration::from_secs(5),async {loop{
            let archive:serde_json::Value=serde_json::from_reader(store.snapshot_file().unwrap().unwrap()).unwrap();
            if archive.to_string().contains("\"state\":\"accepted\""){assert!(archive.to_string().contains(queued["message_id"].as_str().unwrap()));break;}
            tokio::time::sleep(Duration::from_millis(5)).await;
        }}).await.unwrap();
        peer.trace.stop_requested.store(true,Ordering::SeqCst);
        let stale=facade.retire_generation("Workflow Reader",99).await.unwrap();assert_eq!(stale["stale"],true);
        assert!(facade.ready(&first).channel_ready);assert!(facade.ready(&second).channel_ready);
        facade.execute(Action::Stop,card.clone(),vec![first.clone()]).await.unwrap();assert!(!facade.ready(&first).channel_ready);assert!(facade.ready(&second).channel_ready);
        let receipt=facade.stop_and_drain("Workflow Reader",1).await.unwrap();assert_eq!(receipt["card_released"],true);assert_eq!(receipt["original_profile_restored"],true);
        assert!(!facade.ready(&second).channel_ready);
        facade.shutdown().await.unwrap();assert_eq!(peer.trace.deletes.load(Ordering::SeqCst),2);
        // Process-owner replacement recovers only the persisted desired second
        // line. Repeated refresh must not dispatch another physical/network start.
        let recovered=Facade::new(resources(&physical,&root,ConnectionOptions{proxy:peer.proxy.to_string(),epdg:"epdg.invalid".into(),epdg_port:Some(500)})).unwrap();
        assert_ne!(recovered.boot_id(),facade.boot_id());
        assert_eq!(recovered.snapshot("",true,true).await.unwrap()["available"],true);
        ready(&recovered,&second).await;assert!(!recovered.ready(&first).channel_ready);
        assert_eq!(physical.auth_count(),6);assert_eq!(peer.trace.registers.load(Ordering::SeqCst),3);
        assert_eq!(recovered.snapshot("",true,true).await.unwrap()["available"],true);
        assert_eq!(physical.auth_count(),6);
        recovered.execute(Action::Stop,card,vec![second]).await.unwrap();recovered.shutdown().await.unwrap();
        assert_eq!(peer.trace.deletes.load(Ordering::SeqCst),3);
        assert_eq!(physical.transaction_count(),0);peer.finish().await;physical.finish().await;
        assert!(!root.join("go.sock").exists());
    }).await.unwrap();
}
