// Public deletion handlers over HTTP, with the real Rust archive, preview and
// proof owner. Only physical USB I/O is supplied by the storage fixture.
async fn deletion_http_inner(
    service: Service,
    directory: PathBuf,
    path: &str,
    mut body: Value,
    inject_identity: bool,
) -> (StatusCode, Value) {
    use http_body_util::BodyExt;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    let (client, server) = tokio::io::duplex(64 * 1024);
    let serving = tokio::spawn(async move {
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                TokioIo::new(server),
                service_fn(move |request: Request<Incoming>| {
                    let service = service.clone();
                    let directory = directory.clone();
                    async move {
                        let inbox = service.inner.inbox.clone();
                        let response = match request.uri().path() {
                            "/api/sms/clear" => {
                                crate::sms_clear::handle_public(request, inbox, service, &directory)
                                    .await
                            }
                            "/api/sms/delete" => {
                                crate::sms_delete::handle_public(
                                    request, inbox, service, &directory,
                                )
                                .await
                            }
                            "/api/sms/delete-sender" => {
                                crate::sms_delete_sender::handle_public(
                                    request, inbox, service, &directory,
                                )
                                .await
                            }
                            _ => panic!("unexpected public route"),
                        };
                        Ok::<_, std::convert::Infallible>(response)
                    }
                }),
            )
            .await
            .unwrap();
    });
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    if inject_identity && matches!(path, "/api/sms/clear" | "/api/sms/delete-sender") {
        body.as_object_mut()
            .unwrap()
            .entry("client_request_id")
            .or_insert_with(|| {
                json!(format!(
                    "sms-delete-http-{}",
                    crate::snapshot::new_instance_id()
                ))
            });
    }
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        sender.send_request(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
                .unwrap(),
        ),
    )
    .await
    .expect("public deletion must finish from actual storage/archive completion")
    .unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    drop(sender);
    connection.await.unwrap().unwrap();
    serving.await.unwrap();
    (status, value)
}

async fn deletion_http(
    service: Service,
    directory: PathBuf,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    deletion_http_inner(service, directory, path, body, true).await
}

async fn deletion_http_raw(
    service: Service,
    directory: PathBuf,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    deletion_http_inner(service, directory, path, body, false).await
}

fn public_archive(fixture: &Fixture) -> ArchiveStore {
    ArchiveStore::new(fixture.directory.join("sms-archive.json"))
}

fn local_record(content: &str) -> Value {
    json!({"sender":"888","content":content,"timestamp":"2026-10-05T00:00:00Z","device":"reader","direction":"in"})
}

#[tokio::test]
async fn bulk_sms_mutations_require_a_strict_caller_identity_before_selection() {
    for (path, body) in [
        ("/api/sms/clear", json!({"local":true})),
        ("/api/sms/delete-sender", json!({"sender":"888"})),
    ] {
        let mut fixture = Fixture::new();
        let store = public_archive(&fixture);
        let selected = local_record("selected");
        store.replace(vec![selected.clone()]).unwrap();
        fixture
            .service
            .inner
            .inbox
            .merge_usb(vec![selected])
            .unwrap();
        let (status, value) = deletion_http_raw(
            fixture.service.clone(),
            fixture.directory.clone(),
            path,
            body,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {value}");
        assert_eq!(value["code"], "client_request_id_required");
        assert_eq!(store.load().unwrap().len(), 1);
        assert!(fixture.physical.try_recv().is_err());
    }
}

#[tokio::test]
async fn local_clear_replay_returns_original_receipt_and_preserves_late_arrival() {
    let mut fixture = Fixture::new();
    let store = public_archive(&fixture);
    let selected = local_record("selected");
    store.replace(vec![selected.clone()]).unwrap();
    fixture
        .service
        .inner
        .inbox
        .merge_usb(vec![selected])
        .unwrap();
    let body = json!({"client_request_id":"local-clear-replay", "local":true});
    let (status, first) = deletion_http(
        fixture.service.clone(),
        fixture.directory.clone(),
        "/api/sms/clear",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let late = local_record("late-arrival");
    store.replace(vec![late.clone()]).unwrap();
    fixture
        .service
        .inner
        .inbox
        .merge_usb(vec![late.clone()])
        .unwrap();
    let (status, replay) = deletion_http(
        fixture.service.clone(),
        fixture.directory.clone(),
        "/api/sms/clear",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, first);
    assert_eq!(store.load().unwrap(), vec![late]);
    assert_eq!(fixture.service.inner.inbox.usb_snapshot().unwrap().len(), 1);
    let (status, conflict) = deletion_http(
        fixture.service.clone(),
        fixture.directory.clone(),
        "/api/sms/clear",
        json!({"client_request_id":"local-clear-replay", "sm":true}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(store.load().unwrap().len(), 1);
    assert!(fixture.physical.try_recv().is_err());
}

#[tokio::test]
async fn deletion_http_all_local_routes_use_only_rust_archive_and_preview() {
    for (path, body) in [
        ("/api/sms/clear", json!({"local":true})),
        (
            "/api/sms/delete",
            json!({"id":crate::message_archive::cache_digest(&local_record("selected"))}),
        ),
        ("/api/sms/delete-sender", json!({"sender":" 888 "})),
    ] {
        let mut fixture = Fixture::new();
        let store = public_archive(&fixture);
        let selected = local_record("selected");
        let kept =
            json!({"sender":"other","content":"retained","timestamp":"2026-10-05T00:00:01Z"});
        store.replace(vec![selected.clone(), kept.clone()]).unwrap();
        fixture
            .service
            .inner
            .inbox
            .merge_usb(vec![selected.clone(), kept])
            .unwrap();
        let (status, body) = deletion_http(
            fixture.service.clone(),
            fixture.directory.clone(),
            path,
            body,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        let expected = usize::from(path != "/api/sms/clear");
        assert_eq!(store.load().unwrap().len(), expected);
        assert_eq!(
            fixture.service.inner.inbox.usb_snapshot().unwrap().len(),
            expected
        );
        assert!(
            fixture.physical.try_recv().is_err(),
            "local operation must not submit device deletion"
        );
        assert!(fixture.snapshots.try_recv().is_err());
    }
}

#[tokio::test]
async fn deletion_http_archive_failure_preserves_preview_and_never_claims_success() {
    for (path, body) in [
        ("/api/sms/clear", json!({"local":true})),
        (
            "/api/sms/delete",
            json!({"id":crate::message_archive::cache_digest(&local_record("selected"))}),
        ),
        ("/api/sms/delete-sender", json!({"sender":"888"})),
    ] {
        let mut fixture = Fixture::new();
        let corrupt = b"{corrupt";
        std::fs::write(fixture.directory.join("sms-archive.json"), corrupt).unwrap();
        fixture
            .service
            .inner
            .inbox
            .merge_usb(vec![local_record("selected")])
            .unwrap();
        let (status, value) = deletion_http(
            fixture.service.clone(),
            fixture.directory.clone(),
            path,
            body,
        )
        .await;
        if matches!(path, "/api/sms/clear" | "/api/sms/delete-sender") {
            assert_eq!(status, StatusCode::CONFLICT, "{path}: {value}");
            assert_eq!(value["code"], "public_mutation_outcome_unknown");
            assert_eq!(value["mutation_receipt"]["outcome"], "unknown");
        } else {
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}: {value}");
            assert!(value.get("mutation_receipt").is_none());
        }
        assert!(value.get("deleted").is_none() && value.get("cleared").is_none());
        assert_eq!(
            std::fs::read(fixture.directory.join("sms-archive.json")).unwrap(),
            corrupt
        );
        assert_eq!(fixture.service.inner.inbox.usb_snapshot().unwrap().len(), 1);
        assert!(fixture.physical.try_recv().is_err());
    }
}

#[tokio::test]
async fn deletion_http_partial_sender_deletion_keeps_unknown_and_late_arrivals() {
    let mut fixture = Fixture::new();
    let rows = fixture.seed(&snapshot(
        &format!(
            "{}{}",
            listing(7, PDU),
            listing(8, "0000038188F8000862906000000000020042")
        ),
        "",
    ));
    assert_eq!(rows.len(), 2);
    let store = public_archive(&fixture);
    let local = local_record("archive-only");
    store
        .replace(vec![rows[0].clone(), rows[1].clone(), local.clone()])
        .unwrap();
    let service = fixture.service.clone();
    let directory = fixture.directory.clone();
    let task = tokio::spawn(async move {
        deletion_http(
            service,
            directory,
            "/api/sms/delete-sender",
            json!({"sender":"888"}),
        )
        .await
    });
    let (operation, reply) = fixture.pending().await;
    let StorageOperation::Delete { groups } = operation else {
        panic!("expected exact deletion groups")
    };
    assert_eq!(groups.len(), 2);
    let token = rows[0]["deletion_token"].as_str().unwrap().to_owned();
    assert!(
        groups
            .iter()
            .any(|group| group.token == token && group.card_identity == CARD)
    );
    let late = local_record("arrived-after-selection");
    store
        .replace(vec![rows[0].clone(), rows[1].clone(), local, late.clone()])
        .unwrap();
    reply
        .send(Ok(Outcome {
            complete: false,
            uncertain: true,
            storage_restored: true,
            deleted_tokens: vec![token.clone()],
            error_code: "usb_sms_delete_uncertain".into(),
            ..Outcome::default()
        }))
        .unwrap();
    let (status, value) = task.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["code"], "public_mutation_outcome_unknown");
    assert_eq!(value["mutation_receipt"]["outcome"], "unknown");
    let retained = store.load().unwrap();
    assert_eq!(retained.len(), 3);
    assert!(!retained.iter().any(|item| item["id"] == rows[0]["id"]));
    assert!(
        retained
            .iter()
            .any(|item| item["content"] == late["content"])
    );
    assert_eq!(fixture.service.inner.inbox.usb_snapshot().unwrap().len(), 1);
    let (status, _) = deletion_http(
        fixture.service.clone(),
        fixture.directory.clone(),
        "/api/sms/delete",
        json!({"index":7,"storage":"SM","deletion_token":token}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "consumed deletion proof must not replay an uncertain operation"
    );
    assert!(fixture.physical.try_recv().is_err());
}

#[tokio::test]
async fn deletion_http_physical_success_waits_for_archive_confirmation() {
    let mut fixture = Fixture::new();
    let rows = fixture.seed(&snapshot(&listing(7, PDU), ""));
    let token = rows[0]["deletion_token"].as_str().unwrap().to_owned();
    let store = public_archive(&fixture);
    store.replace(rows.clone()).unwrap();
    let service = fixture.service.clone();
    let directory = fixture.directory.clone();
    let body = json!({"id":rows[0]["id"],"index":7,"storage":"SM","deletion_token":token});
    let task =
        tokio::spawn(
            async move { deletion_http(service, directory, "/api/sms/delete", body).await },
        );
    let (_, reply) = fixture.pending().await;
    std::fs::write(fixture.directory.join("sms-archive.json"), b"{corrupt").unwrap();
    reply
        .send(Ok(Outcome {
            complete: true,
            storage_restored: true,
            deleted_tokens: vec![token],
            ..Outcome::default()
        }))
        .unwrap();
    let (status, body) = task.await.unwrap();
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], "sms_inbox_sync_failed");
    assert!(body.get("deleted").is_none());
    assert!(
        fixture
            .service
            .inner
            .inbox
            .usb_snapshot()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        std::fs::read(fixture.directory.join("sms-archive.json")).unwrap(),
        b"{corrupt"
    );
    assert!(fixture.physical.try_recv().is_err());
}

#[tokio::test]
async fn deletion_http_confirmed_physical_routes_preserve_arrivals_after_selection() {
    for path in [
        "/api/sms/delete",
        "/api/sms/delete-sender",
        "/api/sms/clear",
    ] {
        let mut fixture = Fixture::new();
        let rows = fixture.seed(&snapshot(&listing(7, PDU), ""));
        let token = rows[0]["deletion_token"].as_str().unwrap().to_owned();
        let store = public_archive(&fixture);
        store.replace(rows.clone()).unwrap();
        let body = match path {
            "/api/sms/delete" => {
                json!({"id":rows[0]["id"],"index":7,"storage":"SM","deletion_token":token})
            }
            "/api/sms/delete-sender" => json!({"sender":"888"}),
            _ => json!({"sm":true,"local":true}),
        };
        let service = fixture.service.clone();
        let directory = fixture.directory.clone();
        let task = tokio::spawn(async move { deletion_http(service, directory, path, body).await });
        let (operation, reply) = fixture.pending().await;
        let mut outcome = Outcome {
            complete: true,
            storage_restored: true,
            card_identity: CARD.into(),
            ..Outcome::default()
        };
        match operation {
            StorageOperation::Delete { groups } => {
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0].token, token);
                assert_eq!(groups[0].card_identity, CARD);
                outcome.deleted_tokens = vec![token];
            }
            StorageOperation::Clear { .. } => {
                panic!("public clear must freeze exact deletion tokens")
            }
            _ => panic!("unexpected storage mutation"),
        }
        let late = local_record("arrived-while-deleting");
        store.replace(vec![rows[0].clone(), late.clone()]).unwrap();
        reply.send(Ok(outcome)).unwrap();
        let (status, value) = task.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{path}: {value}");
        if path == "/api/sms/clear" {
            assert_eq!(value["cleared"], true);
            assert_eq!(value["details"]["sm"]["ok"], true);
            assert_eq!(value["details"]["local"]["ok"], true);
        } else {
            assert_eq!(value["deleted"], true);
            assert_eq!(value["deleted_count"], 1);
        }
        if matches!(path, "/api/sms/clear" | "/api/sms/delete-sender") {
            assert_eq!(value["mutation_receipt"]["outcome"], "completed");
        } else {
            assert!(value.get("mutation_receipt").is_none());
        }
        assert_eq!(store.load().unwrap(), vec![late]);
        assert!(
            fixture
                .service
                .inner
                .inbox
                .usb_snapshot()
                .unwrap()
                .is_empty()
        );
        assert!(fixture.physical.try_recv().is_err());
    }
}
