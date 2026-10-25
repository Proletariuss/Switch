//! Private deletion authority. UI IDs identify history; only an opaque proof
//! from a current native snapshot can authorize fixed USB storage positions.
use super::*;
use crate::usb_at_storage::{DeleteGroup, Outcome, Storage, StorageOperation, StoredSource};
use std::collections::{BTreeMap, BTreeSet};

const MAX_PROOFS: usize = 1500;
const MAX_SOURCES: usize = 4096;
const MAX_PROOF_BYTES: usize = 1024 * 1024;
const MAX_BODY: usize = 128 * 1024;

#[derive(Clone)]
struct Proof {
    ids: Vec<String>,
    sender: String,
    card_identity: String,
    sources: Vec<StoredSource>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrozenDeletionSelection {
    pub(crate) tokens: Vec<String>,
    pub(crate) binding: String,
}
#[derive(Default)]
pub(super) struct ProofRegistry {
    pub(super) observation: Option<SmsObservationToken>,
    producer: String,
    session: String,
    complete: bool,
    entries: BTreeMap<String, Proof>,
}
pub(super) struct Projection {
    pub(super) messages: Vec<Value>,
    proofs: Vec<Proof>,
    complete: bool,
}
impl ProofRegistry {
    fn frozen_selection<F>(
        &self,
        producer: &str,
        mut include: F,
    ) -> Result<FrozenDeletionSelection, Failure>
    where
        F: FnMut(&Proof) -> bool,
    {
        if self.producer != producer
            || self.session.is_empty()
            || !self.complete
            || self.entries.len() > MAX_PROOFS
        {
            return Err(refresh_required());
        }
        let mut selected: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, proof)| include(proof))
            .map(|(token, proof)| {
                if token.len() != 64 || proof.card_identity.is_empty() || proof.sources.is_empty() {
                    return Err(refresh_required());
                }
                Ok((token.clone(), proof.card_identity.clone()))
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        selected.sort();
        let binding_material =
            serde_json::to_vec(&(self.producer.as_str(), self.session.as_str(), &selected))
                .map_err(|_| refresh_required())?;
        Ok(FrozenDeletionSelection {
            tokens: selected.into_iter().map(|(token, _)| token).collect(),
            binding: format!(
                "usb-sms-proof-v1:{}",
                proletarius_sms::stable_digest_hex(&binding_material)
            ),
        })
    }

    pub(super) fn frozen_sender_selection(
        &self,
        producer: &str,
        sender: &str,
    ) -> Result<FrozenDeletionSelection, Failure> {
        self.frozen_selection(producer, |proof| proof.sender.trim() == sender)
    }

    pub(super) fn frozen_storage_selection(
        &self,
        producer: &str,
        storages: &BTreeSet<Storage>,
    ) -> Result<FrozenDeletionSelection, Failure> {
        self.frozen_selection(producer, |proof| {
            proof
                .sources
                .iter()
                .any(|source| storages.contains(&source.storage))
        })
    }

    /// Private physical-source presence metadata, including partial snapshots.
    /// Never turn incomplete physical evidence into a successful local-only
    /// deletion. Execution independently requires complete coverage and checks
    /// the fixed lease, session, card and every source PDU.
    pub(super) fn deletion_senders(&self, producer: &str) -> Vec<String> {
        if self.producer != producer
            || self.session.is_empty()
            || self.entries.is_empty()
            || self.entries.len() > MAX_PROOFS
            || self
                .entries
                .values()
                .any(|proof| proof.card_identity.is_empty())
        {
            return Vec::new();
        }
        self.entries
            .values()
            .map(|proof| proof.sender.trim())
            .filter(|sender| !sender.is_empty())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(super) fn prune_ids(&mut self, ids: &[String]) {
        self.entries
            .retain(|_, proof| !proof.ids.iter().any(|id| ids.contains(id)));
        if !ids.is_empty() {
            self.complete = false;
        }
    }
    pub(super) fn publish(
        &mut self,
        snapshot: &RawSnapshot,
        projection: Projection,
        messages: &mut [Value],
    ) {
        let same = self.producer == snapshot.producer_id && self.session == snapshot.session_id;
        let previous = std::mem::take(&mut self.entries);
        self.producer.clone_from(&snapshot.producer_id);
        self.session.clone_from(&snapshot.session_id);
        self.complete = projection.complete;
        let count: usize = projection
            .proofs
            .iter()
            .map(|proof| proof.sources.len())
            .sum();
        let ids: usize = projection.proofs.iter().map(|proof| proof.ids.len()).sum();
        let bytes: usize = projection
            .proofs
            .iter()
            .flat_map(|proof| &proof.sources)
            .map(|source| source.pdu.len())
            .sum();
        if projection.proofs.len() > MAX_PROOFS
            || ids > MAX_PROOFS
            || count > MAX_SOURCES
            || bytes > MAX_PROOF_BYTES
        {
            self.complete = false;
            return;
        }
        for proof in projection.proofs {
            let old = same
                .then(|| {
                    previous.iter().find(|(_, old)| {
                        old.ids == proof.ids
                            && old.sender == proof.sender
                            && old.card_identity == proof.card_identity
                            && old.sources == proof.sources
                    })
                })
                .flatten();
            let token = match old {
                Some((token, _)) => token.clone(),
                None => {
                    // The public token discloses no PDU-derived identity. A
                    // system entropy failure grants no deletion authority.
                    let mut random = [0_u8; 32];
                    if getrandom::fill(&mut random).is_err() {
                        self.complete = false;
                        continue;
                    }
                    proletarius_sms::stable_digest_hex(&random)
                }
            };
            for message in messages.iter_mut() {
                if proof
                    .ids
                    .contains(&crate::message_archive::cache_digest(message))
                {
                    message["deletion_token"] = json!(token);
                }
            }
            self.entries.insert(token, proof);
        }
    }
}

pub(super) fn project_with_proofs(snapshot: &RawSnapshot) -> Result<Projection, Failure> {
    let fallback = normalized_time(&snapshot.captured_at).ok_or_else(Failure::invalid)?;
    let mut result = Projection {
        messages: Vec::new(),
        proofs: Vec::new(),
        complete: snapshot.mode == "usb",
    };
    let mut succeeded = 0;
    let mut identities = BTreeMap::<String, usize>::new();
    for storage in &snapshot.storages {
        if !storage.error_code.is_empty() {
            result.complete = false;
            continue;
        }
        let Ok(projected) =
            project_storage_with_proofs(&storage.response, &storage.storage, &fallback)
        else {
            result.complete = false;
            continue;
        };
        succeeded += 1;
        result.complete &= projected.complete;
        for message in projected.messages {
            let key = format!(
                "{}\0{}\0{}",
                message["sender"].as_str().unwrap_or_default(),
                message["content"].as_str().unwrap_or_default(),
                message["timestamp"].as_str().unwrap_or_default()
            );
            let id = crate::message_archive::cache_digest(&message);
            let proof = projected
                .proofs
                .iter()
                .find(|proof| proof.ids.contains(&id));
            if let Some(position) = identities.get(&key).copied() {
                let original_id = crate::message_archive::cache_digest(&result.messages[position]);
                if let Some(proof) = proof {
                    if let Some(existing) = result
                        .proofs
                        .iter_mut()
                        .find(|proof| proof.ids.contains(&original_id))
                    {
                        existing.sources.extend(proof.sources.clone());
                        existing.sources.sort();
                        existing.sources.dedup();
                        existing.ids.extend(proof.ids.clone());
                        existing.ids.sort();
                        existing.ids.dedup();
                    } else {
                        result.complete = false;
                    }
                } else {
                    // A logical duplicate with unprovable sources must not
                    // leave a token that authorizes deleting just one copy.
                    result
                        .proofs
                        .retain(|proof| !proof.ids.contains(&original_id));
                    result.complete = false;
                }
            } else {
                identities.insert(key, result.messages.len());
                if let Some(proof) = proof {
                    result.proofs.push(proof.clone());
                }
                result.messages.push(message);
            }
        }
        result.proofs.extend(
            projected
                .proofs
                .into_iter()
                .filter(|proof| proof.ids.is_empty()),
        );
    }
    if snapshot.mode == "usb" && succeeded == 0 {
        return Err(Failure::new(
            StatusCode::BAD_GATEWAY,
            "usb_sms_storage_failed",
            "SM 和 ME 短信存储均读取失败",
            true,
        ));
    }
    result.messages.sort_by_cached_key(|item| {
        std::cmp::Reverse(
            OffsetDateTime::parse(item["timestamp"].as_str().unwrap_or_default(), &Rfc3339)
                .unwrap_or(OffsetDateTime::UNIX_EPOCH),
        )
    });
    if snapshot.card_identity.is_empty() {
        result.proofs.clear();
        result.complete = false;
    } else {
        for proof in &mut result.proofs {
            proof.card_identity.clone_from(&snapshot.card_identity);
        }
    }
    Ok(result)
}

pub(super) fn project_storage_with_proofs(
    response: &str,
    storage: &str,
    fallback: &str,
) -> Result<Projection, Failure> {
    let storage = match storage {
        "SM" => Storage::SM,
        "ME" => Storage::ME,
        _ => return Err(Failure::invalid()),
    };
    let decoded = crate::sms_codec::decode_stored_snapshot_with_sources(response)
        .map_err(|_| Failure::invalid())?;
    let records = decoded["records"].as_array().ok_or_else(Failure::invalid)?;
    let mut result = Projection {
        messages: Vec::new(),
        proofs: Vec::new(),
        complete: true,
    };
    // The presentation decoder deliberately tolerates bad framing. Destructive
    // sender selection cannot mistake skipped records for a complete listing.
    result.complete &= response
        .replace('\r', "\n")
        .lines()
        .filter(|line| line.trim().starts_with("+CMGL:"))
        .count()
        == records.len();
    let mut sources = BTreeMap::<u32, Option<StoredSource>>::new();
    let mut covered = BTreeSet::new();
    for record in records {
        let Some(index) = record["index"]
            .as_u64()
            .and_then(|index| u32::try_from(index).ok())
            .filter(|index| *index > 0)
        else {
            result.complete = false;
            continue;
        };
        let source = record["canonical_pdu"].as_str().map(|pdu| StoredSource {
            storage,
            index,
            pdu: pdu.to_owned(),
        });
        if sources.contains_key(&index) {
            sources.insert(index, None);
            result.complete = false;
        } else {
            sources.insert(index, source);
        }
        if record["decoded"]["complete"] == true {
            if let Some(indices) = record["source_indices"].as_array() {
                covered.extend(indices.iter().filter_map(Value::as_u64));
            }
        }
    }
    for record in records {
        if let Some(error) = record.get("error").and_then(Value::as_str) {
            result.complete = false;
            let cause = match error {
                "pdu_too_short" => "PDU too short",
                "invalid_smsc_length" => "PDU has invalid SMSC length",
                "tpdu_decode_failed" => "Rust 本机服务拒绝请求（HTTP 422）",
                _ => return Err(Failure::invalid()),
            };
            let pdu = record["pdu"].as_str().ok_or_else(Failure::invalid)?;
            result.messages.push(json!({"sender":"PDU","content":format!("[短信解析失败] {cause}\n{pdu}"),"timestamp":fallback}));
            continue;
        }
        let value = &record["decoded"];
        let sender = value["sender"]
            .as_str()
            .filter(|sender| !sender.trim().is_empty())
            .ok_or_else(Failure::invalid)?;
        if value["kind"] != "deliver" {
            return Err(Failure::invalid());
        }
        let complete = value["complete"].as_bool().ok_or_else(Failure::invalid)?;
        if !complete && covered.contains(&record["index"].as_u64().unwrap_or(0)) {
            continue;
        }
        let refs = record["source_indices"].as_array().and_then(|indices| {
            indices
                .iter()
                .map(|index| {
                    let index = u32::try_from(index.as_u64()?).ok()?;
                    sources.get(&index)?.clone()
                })
                .collect::<Option<Vec<_>>>()
        });
        let mut proof = refs.filter(|refs| !refs.is_empty()).map(|mut sources| {
            sources.sort();
            sources.dedup();
            Proof {
                ids: Vec::new(),
                sender: sender.to_owned(),
                card_identity: String::new(),
                sources,
            }
        });
        if proof.is_none() {
            result.complete = false;
        }
        if complete {
            let content = if value["is_binary"] == true
                || value.get("binary").is_some_and(|value| !value.is_null())
            {
                value["binary_summary"].as_str()
            } else {
                value["text"].as_str()
            }
            .ok_or_else(Failure::invalid)?;
            let timestamp = sms_timestamp(value, fallback).ok_or_else(Failure::invalid)?;
            let index = record["index"].as_u64().ok_or_else(Failure::invalid)?;
            let message = json!({"sender":sender,"content":content,"timestamp":timestamp,"direction":"in","device":"dji","module_storage":storage.as_str(),"module_index":index});
            if let Some(proof) = &mut proof {
                proof
                    .ids
                    .push(crate::message_archive::cache_digest(&message));
            }
            result.messages.push(message);
        }
        if let Some(proof) = proof {
            result.proofs.push(proof);
        }
    }
    Ok(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageRequest {
    producer_id: String,
    session_id: String,
    lease_id: String,
    action: Action,
}
pub(crate) enum PublicAction {
    Clear {
        storages: Vec<Storage>,
    },
    Delete {
        tokens: Vec<String>,
    },
    DeleteScoped {
        tokens: Vec<String>,
        storages: Vec<Storage>,
    },
    DeleteSender {
        sender: String,
    },
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Usage {},
    Clear {
        storages: Vec<Storage>,
    },
    Delete {
        tokens: Vec<String>,
    },
    DeleteScoped {
        tokens: Vec<String>,
        storages: Vec<Storage>,
    },
    DeleteSender {
        sender: String,
    },
}
fn invalid() -> Failure {
    Failure::new(
        StatusCode::BAD_REQUEST,
        "usb_sms_storage_invalid",
        "短信存储请求无效",
        false,
    )
}
fn refresh_required() -> Failure {
    Failure::new(
        StatusCode::CONFLICT,
        "usb_sms_refresh_required",
        "短信来源已变化，请刷新后重试",
        false,
    )
}
fn busy() -> Failure {
    Failure::new(
        StatusCode::CONFLICT,
        "usb_sms_storage_busy",
        "短信存储事务正在进行，请稍后重试",
        true,
    )
}
impl StorageRequest {
    fn validate(&self) -> Result<(), Failure> {
        for id in [&self.producer_id, &self.session_id] {
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
            {
                return Err(invalid());
            }
        }
        if !self.lease_id.is_empty()
            && (self.lease_id.len() > 128
                || !self
                    .lease_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)))
        {
            return Err(invalid());
        }
        match &self.action {
            Action::Usage {} => Ok(()),
            Action::Clear { storages }
                if !storages.is_empty()
                    && storages.len() <= 2
                    && storages.iter().collect::<BTreeSet<_>>().len() == storages.len() =>
            {
                Ok(())
            }
            Action::Delete { tokens }
                if !tokens.is_empty()
                    && tokens.len() <= MAX_PROOFS
                    && tokens.iter().collect::<BTreeSet<_>>().len() == tokens.len()
                    && tokens.iter().all(|token| {
                        token.len() == 64
                            && token
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    }) =>
            {
                Ok(())
            }
            Action::DeleteScoped { tokens, storages }
                if !tokens.is_empty()
                    && tokens.len() <= MAX_PROOFS
                    && tokens.iter().collect::<BTreeSet<_>>().len() == tokens.len()
                    && tokens.iter().all(|token| {
                        token.len() == 64
                            && token
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
                    && !storages.is_empty()
                    && storages.len() <= 2
                    && storages.iter().collect::<BTreeSet<_>>().len() == storages.len() =>
            {
                Ok(())
            }
            Action::DeleteSender { sender } if !sender.trim().is_empty() && sender.len() <= 512 => {
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
}

pub(super) async fn handle(
    request: Request<Incoming>,
    service: Service,
    stopping: bool,
) -> Response<Full<Bytes>> {
    if stopping {
        return Failure::unavailable().response();
    }
    let bytes = match crate::http_server::collect_body_with_timeout(
        request.into_body(),
        MAX_BODY,
        Duration::from_secs(5),
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(crate::http_server::BodyReadError::Timeout) => {
            return Failure::new(
                StatusCode::REQUEST_TIMEOUT,
                "request_timeout",
                "读取请求超时",
                true,
            )
            .response();
        }
        Err(crate::http_server::BodyReadError::TooLarge) => {
            return Failure::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "请求过大",
                false,
            )
            .response();
        }
        Err(crate::http_server::BodyReadError::Invalid) => return invalid().response(),
    };
    let input: StorageRequest = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return invalid().response(),
    };
    match service.storage_operation(input).await {
        Ok(value) => crate::json_response(StatusCode::OK, &value),
        Err(error) => error.response(),
    }
}

struct MutationFinished(Arc<Inner>);
impl Drop for MutationFinished {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.mutations -= 1;
        self.0.finished.notify_waiters();
    }
}
impl Service {
    pub(super) async fn execute_public_storage_unleased(
        &self,
        action: PublicAction,
    ) -> Result<Value, Failure> {
        let (producer_id, session_id) = {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.stopping {
                return Err(Failure::unavailable());
            }
            if state.proofs.session.is_empty()
                || !self.inner.source.current(state.proofs.observation.as_ref())
            {
                return Err(refresh_required());
            }
            (state.producer_id.clone(), state.proofs.session.clone())
        };
        let action = match action {
            PublicAction::Clear { storages } => Action::Clear { storages },
            PublicAction::Delete { tokens } => Action::Delete { tokens },
            PublicAction::DeleteScoped { tokens, storages } => {
                Action::DeleteScoped { tokens, storages }
            }
            PublicAction::DeleteSender { sender } => Action::DeleteSender { sender },
        };
        self.storage_operation(StorageRequest {
            producer_id,
            session_id,
            lease_id: String::new(),
            action,
        })
        .await
    }

    async fn storage_operation(&self, input: StorageRequest) -> Result<Value, Failure> {
        input.validate()?;
        let slot = self
            .inner
            .mutation_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy())?;
        let epoch = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.stopping {
                return Err(Failure::unavailable());
            }
            if state.producer_id != input.producer_id {
                return Err(Failure::stale());
            }
            state.mutations += 1;
            state.epoch
        };
        let inner = self.inner.clone();
        let (reply, wait) = oneshot::channel();
        // Accepted work owns both its admission slot and lifecycle until real
        // I/O, cleanup and preview publication end, even if HTTP disconnects.
        tokio::spawn(async move {
            let _finished = MutationFinished(inner.clone());
            let _slot = slot;
            let result = execute(&inner, input, epoch).await;
            let _ = reply.send(result);
        });
        wait.await.map_err(|_| Failure::unavailable())?
    }
}

async fn execute(inner: &Inner, input: StorageRequest, epoch: u64) -> Result<Value, Failure> {
    let _lifecycle = tokio::time::timeout(Duration::from_secs(30), inner.lifecycle.lock())
        .await
        .map_err(|_| busy())?;
    let (operation, selected, observation) = {
        let mut state = inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.stopping {
            return Err(Failure::unavailable());
        }
        if state.epoch != epoch || state.producer_id != input.producer_id {
            return Err(Failure::stale());
        }
        let observation = state.proofs.observation.clone();
        if !inner.source.current(observation.as_ref()) {
            return Err(refresh_required());
        }
        let registry = &mut state.proofs;
        let current =
            registry.producer == input.producer_id && registry.session == input.session_id;
        let selected = match &input.action {
            Action::Usage {} => BTreeMap::new(),
            Action::Clear { storages } => registry
                .entries
                .iter()
                .filter(|(_, proof)| {
                    current
                        && proof
                            .sources
                            .iter()
                            .any(|source| storages.contains(&source.storage))
                })
                .map(|(token, proof)| (token.clone(), proof.clone()))
                .collect(),
            Action::Delete { tokens } | Action::DeleteScoped { tokens, .. } => {
                if !current {
                    return Err(refresh_required());
                }
                tokens
                    .iter()
                    .map(|token| {
                        registry
                            .entries
                            .get(token)
                            .cloned()
                            .map(|proof| (token.clone(), proof))
                            .ok_or_else(refresh_required)
                    })
                    .collect::<Result<BTreeMap<_, _>, _>>()?
            }
            Action::DeleteSender { sender } => {
                if !current || !registry.complete {
                    return Err(refresh_required());
                }
                registry
                    .entries
                    .iter()
                    .filter(|(_, proof)| proof.sender.trim() == sender.trim())
                    .map(|(token, proof)| (token.clone(), proof.clone()))
                    .collect()
            }
        };
        let operation = match &input.action {
            Action::Usage {} => StorageOperation::Usage,
            Action::Clear { storages } => StorageOperation::Clear {
                storages: storages.clone(),
            },
            Action::Delete { .. } | Action::DeleteSender { .. } => StorageOperation::Delete {
                groups: selected
                    .iter()
                    .map(|(token, proof)| DeleteGroup {
                        token: token.clone(),
                        card_identity: proof.card_identity.clone(),
                        sources: proof.sources.clone(),
                    })
                    .collect(),
            },
            Action::DeleteScoped { storages, .. } => StorageOperation::Delete {
                groups: selected
                    .iter()
                    .map(|(token, proof)| DeleteGroup {
                        token: token.clone(),
                        card_identity: proof.card_identity.clone(),
                        sources: proof
                            .sources
                            .iter()
                            .filter(|source| storages.contains(&source.storage))
                            .cloned()
                            .collect(),
                    })
                    .collect(),
            },
        };
        if !matches!(input.action, Action::Usage {}) {
            // Consume before any submission. Unknown physical outcome is not
            // permission to replay old slot indices under a reused UI ID.
            for token in selected.keys() {
                registry.entries.remove(token);
            }
            registry.complete = false;
            state.preview_revision = state.preview_revision.wrapping_add(1);
            inner
                .inbox
                .clear_usb_deletion_tokens()
                .map_err(|_| Failure::unavailable())?;
        }
        (operation, selected, observation)
    };
    let outcome = if matches!(&operation, StorageOperation::Delete { groups } if groups.is_empty())
    {
        Outcome {
            complete: true,
            storage_restored: true,
            ..Outcome::default()
        }
    } else {
        physical(inner, &input, operation, observation.clone()).await?
    };
    finish(
        inner,
        &input,
        epoch,
        &selected,
        outcome,
        observation.as_ref(),
    )
}

#[cfg(test)]
pub(super) type StorageFixture = tokio::sync::mpsc::UnboundedSender<(
    StorageOperation,
    oneshot::Sender<Result<Outcome, Failure>>,
)>;
async fn physical(
    inner: &Inner,
    input: &StorageRequest,
    operation: StorageOperation,
    observation: Option<SmsObservationToken>,
) -> Result<Outcome, Failure> {
    #[cfg(test)]
    {
        let fixture = inner.storage_fixture.lock().unwrap().clone();
        if let Some(fixture) = fixture {
            let (send, receive) = oneshot::channel();
            fixture
                .send((operation, send))
                .map_err(|_| Failure::unavailable())?;
            return receive.await.map_err(|_| Failure::unavailable())?;
        }
    }
    match &inner.source {
        SnapshotSource::Native(owner) => {
            let result = if input.lease_id.is_empty() {
                owner
                    .storage_exclusive_observed(
                        &input.producer_id,
                        &input.session_id,
                        operation,
                        observation,
                    )
                    .await
            } else {
                owner
                    .storage_observed(
                        input.producer_id.clone(),
                        input.session_id.clone(),
                        input.lease_id.clone(),
                        operation,
                        observation,
                    )
                    .await
            };
            result.map_err(|error| Failure::new(error.status, error.code, error.message, false))
        }
        #[cfg(test)]
        SnapshotSource::Fixture(_) => Err(Failure::unavailable()),
    }
}
fn finish(
    inner: &Inner,
    input: &StorageRequest,
    epoch: u64,
    selected: &BTreeMap<String, Proof>,
    outcome: Outcome,
    observation: Option<&SmsObservationToken>,
) -> Result<Value, Failure> {
    let deleted_ids: BTreeSet<String> = selected
        .iter()
        .filter(|(token, proof)| match &input.action {
            Action::Clear { .. } => {
                !proof.card_identity.is_empty()
                    && proof.card_identity == outcome.card_identity
                    && proof.sources.iter().all(|source| {
                        outcome
                            .details
                            .get(&source.storage)
                            .is_some_and(|detail| detail.delete_confirmed)
                    })
            }
            Action::Delete { .. } | Action::DeleteSender { .. } => {
                outcome.deleted_tokens.contains(token)
            }
            Action::DeleteScoped { storages, .. } => {
                outcome.deleted_tokens.contains(token)
                    && proof
                        .sources
                        .iter()
                        .all(|source| storages.contains(&source.storage))
            }
            Action::Usage {} => false,
        })
        .flat_map(|(_, proof)| proof.ids.clone())
        .collect();
    let state = inner
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if state.epoch == epoch && state.producer_id == input.producer_id {
        // The completion belongs to its submitted observation, never a later
        // registry that happened to reuse the same producer and UI IDs.
        if let Ok(result) = inner.source.with_current(observation, || {
            inner
                .inbox
                .prune_usb(&deleted_ids.iter().cloned().collect::<Vec<_>>())
        }) {
            result.map_err(|_| Failure::unavailable())?;
        }
    }
    // Confirmed old-session history remains factual even after revocation; it
    // must never prune the new session's volatile rows with colliding IDs.
    Ok(
        json!({"session_id":input.session_id,"complete":outcome.complete,"uncertain":outcome.uncertain,"storage_restored":outcome.storage_restored,"usage":outcome.usage,"deleted_ids":deleted_ids,"details":outcome.details,"error_code":outcome.error_code}),
    )
}

#[cfg(test)]
mod tests {
    include!("usb_sms_storage_service_tests.rs");
}
