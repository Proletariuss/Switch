#[cfg(test)]
mod tests {
    use super::super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::mpsc;
    const PDU: &str = "0000038188F8000862906000000000020041";
    const CARD: &str = "8901234567890123456";
    fn listing(index: u32, pdu: &str) -> String {
        format!("+CMGL: {index},0,,{}\r\n{pdu}\r\n", pdu.len() / 2 - 1)
    }
    fn snapshot(sm: &str, me: &str) -> RawSnapshot {
        RawSnapshot {
            producer_id: "producer-1".into(),
            mode: "usb".into(),
            adopt: false,
            poll_interval_ms: 8000,
            captured_at: "2026-10-05T00:00:00Z".into(),
            session_id: "session-1".into(),
            card_identity: CARD.into(),
            storages: vec![
                RawStorage {
                    storage: "SM".into(),
                    response: format!("{sm}OK\r\n"),
                    error_code: String::new(),
                },
                RawStorage {
                    storage: "ME".into(),
                    response: format!("{me}OK\r\n"),
                    error_code: String::new(),
                },
            ],
        }
    }
    struct Fixture {
        directory: PathBuf,
        service: Service,
        snapshots: mpsc::UnboundedReceiver<FixtureSnapshotReply>,
        physical:
            mpsc::UnboundedReceiver<(StorageOperation, oneshot::Sender<Result<Outcome, Failure>>)>,
    }
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    impl Fixture {
        fn new() -> Self {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(4)
                .unwrap();
            let directory = root
                .join("build/runtime/usb-sms-storage-service-tests")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    SEQUENCE.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(&directory).unwrap();
            let availability = BackendAvailability::new();
            availability.mark_ready();
            let mut service = Service::new(
                ArchiveStore::new(directory.join("sms.json")),
                InboxState::default(),
                crate::usb_at::Owner::new("producer-1".into()),
                availability,
                "producer-1".into(),
            );
            let (snapshot_send, snapshots) = mpsc::unbounded_channel();
            let (storage_send, physical) = mpsc::unbounded_channel();
            let inner = Arc::get_mut(&mut service.inner).unwrap();
            inner.source = SnapshotSource::Fixture(snapshot_send);
            *inner.storage_fixture.get_mut().unwrap() = Some(storage_send);
            Self {
                directory,
                service,
                snapshots,
                physical,
            }
        }
        fn seed(&self, snapshot: &RawSnapshot) -> Vec<Value> {
            let projection = project_with_proofs(snapshot).unwrap();
            let mut messages = projection.messages.clone();
            self.service.inner.state.lock().unwrap().proofs.publish(
                snapshot,
                projection,
                &mut messages,
            );
            self.service.inner.inbox.merge_usb(messages).unwrap();
            self.service.inner.inbox.usb_snapshot().unwrap()
        }
        fn request(&self, action: Action) -> StorageRequest {
            StorageRequest {
                producer_id: "producer-1".into(),
                session_id: "session-1".into(),
                lease_id: "lease-1".into(),
                action,
            }
        }
        async fn pending(
            &mut self,
        ) -> (StorageOperation, oneshot::Sender<Result<Outcome, Failure>>) {
            tokio::time::timeout(Duration::from_secs(3), self.physical.recv())
                .await
                .unwrap()
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
    async fn until(mut predicate: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn public_delete_uses_current_rust_proof_and_unleased_storage_owner_path() {
        let mut fixture = Fixture::new();
        let rows = fixture.seed(&snapshot(&listing(7, PDU), ""));
        let item_id = rows[0]["id"].as_str().unwrap().to_owned();
        let token = rows[0]["deletion_token"].as_str().unwrap().to_owned();
        let request_token = token.clone();
        let service = fixture.service.clone();
        let task = tokio::spawn(async move {
            service
                .execute_public_storage_unleased(PublicAction::Delete {
                    tokens: vec![request_token.clone()],
                })
                .await
                .map(|value| (value, request_token))
        });
        let (operation, reply) = fixture.pending().await;
        let StorageOperation::Delete { groups } = operation else {
            panic!("physical operation must be an exact proof-backed delete");
        };
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].token, token);
        assert_eq!(groups[0].card_identity, CARD);
        assert_eq!(groups[0].sources.len(), 1);
        reply
            .send(Ok(Outcome {
                complete: true,
                storage_restored: true,
                deleted_tokens: vec![token.clone()],
                ..Outcome::default()
            }))
            .unwrap();
        let (value, _) = task.await.unwrap().unwrap();
        assert_eq!(value["complete"], true);
        assert_eq!(value["deleted_ids"], json!([item_id]));
        assert!(
            fixture.service.inbox_snapshot().unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn frozen_storage_delete_is_slot_scoped_and_preserves_unselected_duplicate() {
        let mut fixture = Fixture::new();
        let rows = fixture.seed(&snapshot(&listing(7, PDU), &listing(9, PDU)));
        assert_eq!(rows.len(), 1);
        let frozen = fixture
            .service
            .frozen_storage_deletion(&[Storage::SM])
            .unwrap();
        assert_eq!(frozen.tokens.len(), 1);
        let token = frozen.tokens[0].clone();
        let service = fixture.service.clone();
        let task = tokio::spawn(async move {
            service
                .execute_public_storage_unleased(PublicAction::DeleteScoped {
                    tokens: vec![token],
                    storages: vec![Storage::SM],
                })
                .await
        });
        let (operation, reply) = fixture.pending().await;
        let StorageOperation::Delete { groups } = operation else {
            panic!("delete expected")
        };
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].sources.len(), 1);
        assert_eq!(groups[0].sources[0].storage, Storage::SM);
        reply
            .send(Ok(Outcome {
                complete: true,
                storage_restored: true,
                deleted_tokens: vec![groups[0].token.clone()],
                ..Outcome::default()
            }))
            .unwrap();
        let value = task.await.unwrap().unwrap();
        assert_eq!(value["deleted_ids"], json!([]));
        assert_eq!(fixture.service.inner.inbox.usb_snapshot().unwrap().len(), 1);
    }

    #[test]
    fn frozen_deletion_binding_changes_with_session_card_and_tokens() {
        let fixture = Fixture::new();
        fixture.seed(&snapshot(&listing(7, PDU), ""));
        let first = fixture.service.frozen_sender_deletion("888").unwrap();
        let mut successor = snapshot(&listing(7, PDU), "");
        successor.session_id = "session-2".into();
        successor.card_identity = "8909999999999999999".into();
        fixture.seed(&successor);
        let second = fixture.service.frozen_sender_deletion("888").unwrap();
        assert_ne!(first.binding, second.binding);
        assert_ne!(first.tokens, second.tokens);
    }

    #[tokio::test]
    async fn public_sender_delete_uses_only_current_sender_proofs_without_go_lease() {
        let mut fixture = Fixture::new();
        let rows = fixture.seed(&snapshot(&listing(7, PDU), ""));
        let item_id = rows[0]["id"].as_str().unwrap().to_owned();
        let sender = rows[0]["sender"].as_str().unwrap().to_owned();
        let token = rows[0]["deletion_token"].as_str().unwrap().to_owned();
        let service = fixture.service.clone();
        let task = tokio::spawn(async move {
            service
                .execute_public_storage_unleased(PublicAction::DeleteSender { sender })
                .await
        });
        let (operation, reply) = fixture.pending().await;
        let StorageOperation::Delete { groups } = operation else {
            panic!("sender deletion must become a proof-backed physical delete");
        };
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].token, token);
        assert_eq!(groups[0].card_identity, CARD);
        assert_eq!(groups[0].sources.len(), 1);
        reply
            .send(Ok(Outcome {
                complete: true,
                storage_restored: true,
                deleted_tokens: vec![token],
                ..Outcome::default()
            }))
            .unwrap();
        let value = task.await.unwrap().unwrap();
        assert_eq!(value["complete"], true);
        assert_eq!(value["deleted_ids"], json!([item_id]));
        assert!(
            fixture.service.inbox_snapshot().unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn public_clear_uses_rust_producer_session_and_exact_requested_storages() {
        let mut fixture = Fixture::new();
        let rows = fixture.seed(&snapshot(&listing(7, PDU), ""));
        let item_id = rows[0]["id"].as_str().unwrap().to_owned();
        let service = fixture.service.clone();
        let task = tokio::spawn(async move {
            service
                .execute_public_storage_unleased(PublicAction::Clear {
                    storages: vec![Storage::SM],
                })
                .await
        });
        let (operation, reply) = fixture.pending().await;
        let StorageOperation::Clear { storages } = operation else {
            panic!("public clear must remain a fixed storage operation");
        };
        assert_eq!(storages, vec![Storage::SM]);
        let details = BTreeMap::from([(
            Storage::SM,
            crate::usb_at_storage::ClearDetail {
                before: 1,
                after: Some(0),
                delete_confirmed: true,
            },
        )]);
        reply
            .send(Ok(Outcome {
                complete: true,
                storage_restored: true,
                card_identity: CARD.into(),
                details,
                ..Outcome::default()
            }))
            .unwrap();
        let value = task.await.unwrap().unwrap();
        assert_eq!(value["complete"], true);
        assert_eq!(value["deleted_ids"], json!([item_id]));
        assert_eq!(value["details"]["SM"]["delete_confirmed"], true);
        assert!(
            fixture.service.inbox_snapshot().unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn proof_covers_every_multipart_duplicate_and_cross_storage_copy() {
        let a = "0040038188F800006290600000000008050003C9020182";
        let b = "0040038188F800006290700000000008050003C9020284";
        let snap = snapshot(
            &format!("{}{}{}", listing(1, a), listing(2, b), listing(3, a)),
            &format!("{}{}", listing(8, a), listing(9, b)),
        );
        let projected = project_with_proofs(&snap).unwrap();
        assert!(projected.complete);
        assert_eq!(projected.messages.len(), 1);
        assert_eq!(projected.messages[0]["content"], "AB");
        assert_eq!(projected.proofs.len(), 1);
        assert_eq!(projected.proofs[0].sources.len(), 5);
        let wire =
            crate::sms_codec::decode_stored_snapshot_response(&snap.storages[0].response).unwrap();
        let private =
            crate::sms_codec::decode_stored_snapshot_with_sources(&snap.storages[0].response)
                .unwrap();
        assert!(
            wire["records"]
                .as_array()
                .unwrap()
                .iter()
                .all(|record| record.get("canonical_pdu").is_none()
                    && record.get("source_indices").is_none())
        );
        let completed = private["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["decoded"]["complete"] == true)
            .unwrap();
        assert_eq!(completed["source_indices"], json!([1, 2, 3]));
        assert!(completed["canonical_pdu"].is_string());
        assert_eq!(
            projected.proofs[0]
                .sources
                .iter()
                .map(|s| (s.storage, s.index))
                .collect::<Vec<_>>(),
            vec![
                (Storage::SM, 1),
                (Storage::SM, 2),
                (Storage::SM, 3),
                (Storage::ME, 8),
                (Storage::ME, 9)
            ]
        );
        let incomplete = project_with_proofs(&snapshot(&listing(1, a), "")).unwrap();
        assert!(incomplete.messages.is_empty());
        assert_eq!(incomplete.proofs.len(), 1);
        assert!(incomplete.proofs[0].ids.is_empty());
    }
    #[test]
    fn proof_requires_card_and_unique_complete_framing_but_keeps_display() {
        let mut snap = snapshot(&listing(7, PDU), "");
        snap.card_identity.clear();
        let projected = project_with_proofs(&snap).unwrap();
        assert_eq!(projected.messages.len(), 1);
        assert!(projected.proofs.is_empty());
        assert!(!projected.complete);
        let projected = project_with_proofs(&snapshot(
            &format!("{}{}", listing(7, PDU), listing(7, PDU)),
            "",
        ))
        .unwrap();
        assert!(!projected.complete);
        assert!(projected.proofs.is_empty());
        assert_eq!(projected.messages.len(), 1);
        let projected = project_with_proofs(&snapshot(
            &format!("{}+CMGL: 99\r\nnot-hex\r\n", listing(7, PDU)),
            "",
        ))
        .unwrap();
        assert!(!projected.complete);
        assert!(crate::sms_codec::canonical_stored_pdu("+CMGL: 1,0,,200", PDU).is_none());
        assert_eq!(
            crate::sms_codec::canonical_stored_pdu(
                "+CMGL: 1,0,,17",
                &format!("{}FFFF", PDU.to_ascii_lowercase())
            )
            .unwrap(),
            PDU
        );
    }
    #[test]
    fn same_proof_keeps_token_card_and_session_changes_replace_authority() {
        let fixture = Fixture::new();
        let mut snap = snapshot(&listing(7, PDU), "");
        let first = fixture.seed(&snap);
        let token = first[0]["deletion_token"].clone();
        let id = first[0]["id"].clone();
        assert_eq!(fixture.seed(&snap)[0]["deletion_token"], token);
        snap.card_identity = "8901234567890123457".into();
        let changed = fixture.seed(&snap);
        assert_eq!(changed[0]["id"], id);
        assert_ne!(changed[0]["deletion_token"], token);
        let token = changed[0]["deletion_token"].clone();
        snap.session_id = "session-2".into();
        assert_ne!(fixture.seed(&snap)[0]["deletion_token"], token);
        snap.card_identity.clear();
        assert!(fixture.seed(&snap)[0].get("deletion_token").is_none());
    }
    #[test]
    fn bounded_registry_refuses_destructive_completeness_instead_of_truncating() {
        let mut registry = ProofRegistry::default();
        let snap = snapshot("", "");
        let proof = Proof {
            ids: vec![],
            sender: "888".into(),
            card_identity: CARD.into(),
            sources: vec![StoredSource {
                storage: Storage::SM,
                index: 1,
                pdu: PDU.into(),
            }],
        };
        registry.publish(
            &snap,
            Projection {
                messages: vec![],
                proofs: vec![proof; MAX_PROOFS + 1],
                complete: true,
            },
            &mut [],
        );
        assert!(!registry.complete);
        assert!(registry.entries.is_empty());
        assert!(registry.deletion_senders("producer-1").is_empty());
    }

    #[tokio::test]
    async fn private_sender_metadata_covers_hidden_rows_and_remains_session_fenced() {
        let mut fixture = Fixture::new();
        let mut sm = (1..=500)
            .map(|index| {
                listing(
                    index,
                    &format!("0000038188F800086290600000000002{:04X}", 0x400 + index),
                )
            })
            .collect::<String>();
        let hidden = PDU.replace("8188F8", "8177F7").replace("629060", "529060");
        sm.push_str(&listing(501, &hidden));
        sm.push_str(&listing(502, &hidden.replace("0041", "0042")));
        let observed = snapshot(&sm, "");
        let preview = fixture.seed(&observed);
        assert_eq!(preview.len(), 500);
        assert!(preview.iter().all(|message| message["sender"] == "888"));
        let wire = fixture.service.inbox_snapshot().unwrap();
        assert_eq!(wire["deletion_senders"], json!(["777", "888"]));
        assert!(wire.get("card_identity").is_none());
        assert!(wire.get("session_id").is_none());

        let mut partial = snapshot(&sm, "");
        partial.storages[1].response.clear();
        partial.storages[1].error_code = "read_failed".into();
        fixture.seed(&partial);
        assert_eq!(
            fixture.service.inbox_snapshot().unwrap()["deletion_senders"],
            json!(["777", "888"]),
            "partial evidence must not hide a physical sender from the Go adapter"
        );
        let input = fixture.request(Action::DeleteSender {
            sender: "777".into(),
        });
        let error = fixture.service.storage_operation(input).await.unwrap_err();
        assert_eq!(error.code, "usb_sms_refresh_required");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            fixture.physical.try_recv().is_err(),
            "incomplete snapshot authorizes no physical operation"
        );
        fixture.seed(&observed);

        let mut stale = fixture.request(Action::DeleteSender {
            sender: "777".into(),
        });
        stale.session_id = "new-session-without-proof".into();
        assert_eq!(
            fixture
                .service
                .storage_operation(stale)
                .await
                .unwrap_err()
                .code,
            "usb_sms_refresh_required"
        );
        assert!(fixture.physical.try_recv().is_err());
        let input = fixture.request(Action::DeleteSender {
            sender: "777".into(),
        });
        let service = fixture.service.clone();
        let task = tokio::spawn(async move { service.storage_operation(input).await });
        let (operation, reply) = fixture.pending().await;
        let StorageOperation::Delete { groups } = operation else {
            panic!("delete expected")
        };
        assert_eq!(groups.len(), 2);
        assert_eq!(
            fixture.service.inbox_snapshot().unwrap()["deletion_senders"],
            json!(["888"]),
            "consumed hidden proofs vanish while unconsumed physical sources remain visible"
        );
        reply
            .send(Ok(Outcome {
                complete: true,
                storage_restored: true,
                deleted_tokens: groups.iter().map(|group| group.token.clone()).collect(),
                ..Outcome::default()
            }))
            .unwrap();
        assert_eq!(
            task.await.unwrap().unwrap()["deleted_ids"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            fixture.service.inner.inbox.usb_snapshot().unwrap().len(),
            500
        );
    }

    #[test]
    fn private_sender_metadata_keeps_partial_sources_but_revokes_missing_card_and_generation() {
        let fixture = Fixture::new();
        let mut observed = snapshot(&listing(7, PDU), "");
        fixture.seed(&observed);
        assert_eq!(
            fixture.service.inbox_snapshot().unwrap()["deletion_senders"],
            json!(["888"])
        );
        assert!(
            fixture
                .service
                .inner
                .state
                .lock()
                .unwrap()
                .proofs
                .deletion_senders("other-producer")
                .is_empty()
        );
        observed.card_identity.clear();
        fixture.seed(&observed);
        assert_eq!(
            fixture.service.inbox_snapshot().unwrap()["deletion_senders"],
            json!([])
        );
        assert_eq!(fixture.service.inner.inbox.usb_snapshot().unwrap().len(), 1);
        observed.card_identity = CARD.into();
        observed.storages[1].response.clear();
        observed.storages[1].error_code = "read_failed".into();
        fixture.seed(&observed);
        assert_eq!(
            fixture.service.inbox_snapshot().unwrap()["deletion_senders"],
            json!(["888"])
        );
        observed.storages[1].response = "OK\r\n".into();
        observed.storages[1].error_code.clear();
        fixture.seed(&observed);
        assert_eq!(
            fixture.service.inbox_snapshot().unwrap()["deletion_senders"],
            json!(["888"])
        );
        fixture.service.invalidate_backend("producer-2".into());
        assert_eq!(
            fixture.service.inbox_snapshot().unwrap()["deletion_senders"],
            json!([])
        );
    }
    #[test]
    fn strict_wire_rejects_positions_unknown_fields_duplicates_and_bad_ids() {
        let base = json!({"producer_id":"producer-1","session_id":"session-1","lease_id":"lease-1","action":{"kind":"usage"}});
        assert!(
            serde_json::from_value::<StorageRequest>(base.clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        for action in [
            json!({"kind":"delete","tokens":["f".repeat(64)],"index":7}),
            json!({"kind":"usage","storages":["SM"]}),
            json!({"kind":"clear","storages":["MT"]}),
        ] {
            let mut request = base.clone();
            request["action"] = action;
            assert!(serde_json::from_value::<StorageRequest>(request).is_err());
        }
        for action in [
            Action::Delete {
                tokens: vec!["f".repeat(64); 2],
            },
            Action::Delete {
                tokens: vec!["not-a-proof".into()],
            },
            Action::Clear { storages: vec![] },
            Action::DeleteSender { sender: " ".into() },
        ] {
            let fixture = Fixture::new();
            assert!(fixture.request(action).validate().is_err());
        }
    }
    #[tokio::test]
    async fn partial_physical_delete_prunes_only_confirmed_groups_and_consumes_retry_tokens() {
        let mut fixture = Fixture::new();
        let other = PDU.replace("0041", "0042");
        let messages = fixture.seed(&snapshot(
            &format!("{}{}", listing(7, PDU), listing(8, &other)),
            "",
        ));
        let tokens = messages
            .iter()
            .map(|m| m["deletion_token"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let input = fixture.request(Action::Delete {
            tokens: tokens.clone(),
        });
        let service = fixture.service.clone();
        let task = tokio::spawn(async move { service.storage_operation(input).await });
        let (op, reply) = fixture.pending().await;
        let StorageOperation::Delete { groups } = op else {
            panic!("delete expected")
        };
        assert_eq!(groups.len(), 2);
        assert_eq!(
            fixture.service.inbox_snapshot().unwrap()["deletion_senders"],
            json!([]),
            "all selected proofs are consumed before physical I/O completes"
        );
        reply
            .send(Ok(Outcome {
                deleted_tokens: vec![tokens[0].clone()],
                uncertain: true,
                error_code: "usb_sms_delete_uncertain".into(),
                ..Outcome::default()
            }))
            .unwrap();
        let result = task.await.unwrap().unwrap();
        assert_eq!(result["deleted_ids"], json!([messages[0]["id"]]));
        assert_eq!(fixture.service.inner.inbox.usb_snapshot().unwrap().len(), 1);
        let error = fixture
            .service
            .storage_operation(fixture.request(Action::Delete { tokens }))
            .await
            .unwrap_err();
        assert_eq!(error.code, "usb_sms_refresh_required");
        assert!(fixture.physical.try_recv().is_err());
    }
    #[tokio::test]
    async fn clear_one_copy_or_different_card_never_reports_entire_logical_message_deleted() {
        for card in [CARD, "8901234567890123457"] {
            let fixture = Fixture::new();
            fixture.seed(&snapshot(&listing(7, PDU), &listing(9, PDU)));
            let state = fixture.service.inner.state.lock().unwrap();
            let selected = state.proofs.entries.clone();
            drop(state);
            let mut outcome = Outcome {
                complete: true,
                storage_restored: true,
                card_identity: card.into(),
                ..Outcome::default()
            };
            outcome.details.insert(
                Storage::SM,
                crate::usb_at_storage::ClearDetail {
                    before: 1,
                    after: Some(0),
                    delete_confirmed: true,
                },
            );
            let request = fixture.request(Action::Clear {
                storages: vec![Storage::SM],
            });
            assert_eq!(
                finish(
                    &fixture.service.inner,
                    &request,
                    0,
                    &selected,
                    outcome.clone(),
                    None,
                )
                .unwrap()["deleted_ids"],
                json!([])
            );
            outcome.details.insert(
                Storage::ME,
                crate::usb_at_storage::ClearDetail {
                    before: 1,
                    after: None,
                    delete_confirmed: true,
                },
            );
            let result = finish(
                &fixture.service.inner,
                &request,
                0,
                &selected,
                outcome,
                None,
            )
            .unwrap();
            assert_eq!(
                result["deleted_ids"],
                if card == CARD {
                    json!(
                        selected
                            .values()
                            .flat_map(|proof| proof.ids.clone())
                            .collect::<BTreeSet<_>>()
                    )
                } else {
                    json!([])
                }
            );
        }
    }
    #[tokio::test]
    async fn sender_targets_include_rows_beyond_preview_capacity() {
        let mut fixture = Fixture::new();
        let sm = (1..=600)
            .map(|index| {
                listing(
                    index,
                    &format!("0000038188F800086290600000000002{:04X}", 0x400 + index),
                )
            })
            .collect::<String>();
        assert_eq!(fixture.seed(&snapshot(&sm, "")).len(), 500);
        let input = fixture.request(Action::DeleteSender {
            sender: "888".into(),
        });
        let service = fixture.service.clone();
        let task = tokio::spawn(async move { service.storage_operation(input).await });
        let (operation, reply) = fixture.pending().await;
        let StorageOperation::Delete { groups } = operation else {
            panic!("delete expected")
        };
        assert_eq!(groups.len(), 600);
        reply
            .send(Ok(Outcome {
                complete: true,
                storage_restored: true,
                deleted_tokens: groups.iter().map(|g| g.token.clone()).collect(),
                ..Outcome::default()
            }))
            .unwrap();
        assert_eq!(
            task.await.unwrap().unwrap()["deleted_ids"]
                .as_array()
                .unwrap()
                .len(),
            600
        );
        assert!(
            fixture
                .service
                .inner
                .inbox
                .usb_snapshot()
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn cancelled_http_keeps_mutation_and_shutdown_waiting_for_real_work() {
        let mut fixture = Fixture::new();
        let service = fixture.service.clone();
        let input = fixture.request(Action::Usage {});
        let caller = tokio::spawn(async move { service.storage_operation(input).await });
        let (_, reply) = fixture.pending().await;
        caller.abort();
        let service = fixture.service.clone();
        let shutdown = tokio::spawn(async move { service.shutdown().await });
        until(|| fixture.service.inner.state.lock().unwrap().stopping).await;
        assert!(!shutdown.is_finished());
        assert_eq!(fixture.service.inner.state.lock().unwrap().mutations, 1);
        reply
            .send(Ok(Outcome {
                complete: true,
                storage_restored: true,
                ..Outcome::default()
            }))
            .unwrap();
        shutdown.await.unwrap().unwrap();
        assert_eq!(fixture.service.inner.state.lock().unwrap().mutations, 0);
    }
    #[tokio::test]
    async fn queued_mutation_is_revoked_before_io_and_admission_is_bounded() {
        let fixture = Fixture::new();
        let inner = fixture.service.inner.clone();
        let gate = inner.lifecycle.lock().await;
        let mut tasks = vec![];
        for _ in 0..8 {
            let input = fixture.request(Action::Usage {});
            let service = fixture.service.clone();
            tasks.push(tokio::spawn(async move {
                service.storage_operation(input).await
            }));
        }
        until(|| inner.state.lock().unwrap().mutations == 8).await;
        assert_eq!(
            fixture
                .service
                .storage_operation(fixture.request(Action::Usage {}))
                .await
                .unwrap_err()
                .code,
            "usb_sms_storage_busy"
        );
        fixture.service.invalidate_backend("producer-2".into());
        drop(gate);
        for task in tasks {
            assert_eq!(
                task.await.unwrap().unwrap_err().code,
                "usb_sms_stale_generation"
            );
        }
        assert_eq!(inner.state.lock().unwrap().mutations, 0);
    }
    #[tokio::test]
    async fn collector_owns_gate_until_archive_and_preview_are_published() {
        let mut fixture = Fixture::new();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(fixture.directory.join("sms.json.lock"))
            .unwrap();
        lock.lock().unwrap();
        let inner = fixture.service.inner.clone();
        let identity = 0;
        let collect =
            tokio::spawn(async move { collect_once(&inner, identity, "producer-1").await });
        let reply = fixture.snapshots.recv().await.unwrap();
        let service = fixture.service.clone();
        let input = fixture.request(Action::Usage {});
        let mutation = tokio::spawn(async move { service.storage_operation(input).await });
        until(|| fixture.service.inner.state.lock().unwrap().mutations == 1).await;
        assert!(fixture.physical.try_recv().is_err());
        reply.send((StatusCode::OK,json!({"producer_id":"producer-1","mode":"usb","adopt":true,"poll_interval_ms":8000,"captured_at":"2026-10-05T00:00:00Z","session_id":"session-1","card_identity":CARD,"storages":[{"storage":"SM","response":format!("{}OK\r\n",listing(7,PDU)),"error_code":""},{"storage":"ME","response":"OK\r\n","error_code":""}]}))).unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            fixture.service.inner.archive_started.notified(),
        )
        .await
        .unwrap();
        assert!(!collect.is_finished());
        assert!(
            fixture.physical.try_recv().is_err(),
            "mutation must wait for the actual archive fsync transaction"
        );
        drop(lock);
        let (_, reply) = fixture.pending().await;
        assert_eq!(collect.await.unwrap().unwrap(), 1);
        let archive = fixture.service.inner.archive.load().unwrap();
        assert_eq!(archive.len(), 1);
        assert!(archive[0].get("deletion_token").is_none());
        assert!(
            fixture.service.inner.inbox.usb_snapshot().unwrap()[0]
                .get("deletion_token")
                .is_some()
        );
        reply
            .send(Ok(Outcome {
                complete: true,
                storage_restored: true,
                ..Outcome::default()
            }))
            .unwrap();
        mutation.await.unwrap().unwrap();
    }
    include!("sms_delete_http_tests.rs");
}
