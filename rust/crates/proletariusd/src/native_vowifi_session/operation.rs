//! Read-only UI operation projection over retained owners. An observation
//! deadline is never a lease release, cleanup receipt, or protocol stop.
use proletarius_store::ArchiveStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use tokio::time::Instant;

#[derive(Clone, Default, Deserialize)]
pub(super) struct Options {
    #[serde(default)]
    pub observe_operation: Option<bool>,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub operation_generation: Option<u64>,
    #[serde(default)]
    pub client_request_id: Option<String>,
}
impl Options {
    pub(super) fn body(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut decoder = serde_json::Deserializer::from_slice(bytes);
        let value = Option::<serde_json::Value>::deserialize(&mut decoder)
            .map_err(|_| "invalid_operation_observation")?
            .unwrap_or_default();
        // Public mutations always carry a durable caller identity. Legacy
        // observation opt-out must never select an unrecorded spawn/stop path.
        if value.get("observe_operation") != Some(&serde_json::Value::Bool(true)) {
            return Err("invalid_operation_observation");
        }
        let options: Self =
            serde_json::from_value(value).map_err(|_| "invalid_operation_observation")?;
        options.paired()?;
        if options.request_id().is_none() {
            return Err("invalid_operation_observation");
        }
        Ok(options)
    }
    pub(super) fn query(query: Option<&str>) -> Result<Self, &'static str> {
        let values: Vec<_> =
            url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()).collect();
        let flags: Vec<_> = values
            .iter()
            .filter(|(key, _)| key == "observe_operation")
            .collect();
        if flags.is_empty() || (flags.len() == 1 && flags[0].1 == "false") {
            return Ok(Self::default());
        }
        if flags.len() != 1 || flags[0].1 != "true" {
            return Err("invalid_operation_observation");
        }
        let mut options = Self::default();
        for (key, value) in values {
            match key.as_ref() {
                "observe_operation" if options.observe_operation.is_none() => {
                    options.observe_operation = Some(match value.as_ref() {
                        "true" => true,
                        "false" => false,
                        _ => return Err("invalid_operation_observation"),
                    })
                }
                "operation_id" if options.operation_id.is_none() => {
                    options.operation_id = Some(value.into_owned())
                }
                "operation_generation" if options.operation_generation.is_none() => {
                    options.operation_generation =
                        Some(value.parse().map_err(|_| "invalid_operation_observation")?)
                }
                "client_request_id" if options.client_request_id.is_none() => {
                    options.client_request_id = Some(value.into_owned())
                }
                "observe_operation"
                | "operation_id"
                | "operation_generation"
                | "client_request_id" => {
                    return Err("invalid_operation_observation");
                }
                _ => {}
            }
        }
        options.paired()?;
        Ok(options)
    }
    fn paired(&self) -> Result<(), &'static str> {
        if self.enabled() && (self.operation_id.is_some() != self.operation_generation.is_some()) {
            return Err("invalid_operation_observation");
        }
        if self
            .client_request_id
            .as_deref()
            .is_some_and(|value| !valid_request_id(value))
        {
            return Err("invalid_operation_observation");
        }
        Ok(())
    }
    pub(super) fn enabled(&self) -> bool {
        self.observe_operation == Some(true)
    }
    pub(super) fn validate(&self, operation: &Projection) -> Result<(), &'static str> {
        self.validate_identity(&operation.operation_id, operation.generation)
    }
    pub(super) fn validate_identity(&self, id: &str, generation: u64) -> Result<(), &'static str> {
        if self.enabled()
            && self.operation_id.as_ref().is_some_and(|expected| {
                expected != id || self.operation_generation != Some(generation)
            })
        {
            return Err("vowifi_operation_stale");
        }
        Ok(())
    }
    pub(super) fn request_id(&self) -> Option<&str> {
        self.client_request_id.as_deref()
    }
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

#[derive(Clone)]
pub(super) struct Clock {
    pub(super) sequence: u64,
    enable: bool,
    cancelled: bool,
    deadline: Instant,
}
impl Clock {
    pub(super) fn validate(
        &self,
        options: &Options,
        epoch: &str,
        record: u64,
        generation: u64,
    ) -> Result<(), &'static str> {
        options.validate_identity(&format!("{epoch}-{record}-{}", self.sequence), generation)
    }
    pub(super) fn start(now: Instant, budget: Duration) -> Self {
        Self {
            sequence: 0,
            enable: true,
            cancelled: false,
            deadline: now + budget,
        }
    }
    pub(super) fn stop(
        &mut self,
        now: Instant,
        budget: Duration,
        cancelled: bool,
    ) -> Result<(), &'static str> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("vowifi_operation_sequence_exhausted")?;
        self.enable = false;
        self.cancelled = cancelled;
        self.deadline = now + budget;
        Ok(())
    }
    pub(super) fn project(
        &self,
        now: Instant,
        epoch: &str,
        record: u64,
        facts: Facts,
    ) -> Projection {
        let (outcome, message) = if facts.superseded {
            (
                Some("superseded"),
                "号卡或操作代次已变化，原结果不再适用".into(),
            )
        } else if !self.enable && facts.cleanup_confirmed {
            (
                Some(if self.cancelled {
                    "cancelled"
                } else {
                    "stopped"
                }),
                "Wi-Fi Calling 已关闭，资源释放已确认".into(),
            )
        } else if let Some(error) = facts.error {
            (Some("failed"), error)
        } else if self.enable && facts.ready {
            (
                Some("ready"),
                if facts.sms_ready && facts.call_ready {
                    "Wi-Fi Calling 已就绪，SMS 与通话可用"
                } else if facts.call_ready {
                    "IMS 与通话已就绪，SMS 通道尚未就绪"
                } else {
                    "Wi-Fi Calling 已完成 IMS 注册"
                }
                .into(),
            )
        } else {
            (
                None,
                if self.enable {
                    "正在建立 Wi-Fi Calling…"
                } else {
                    "正在释放 VoWiFi 资源，尚未确认关闭"
                }
                .into(),
            )
        };
        let terminal = outcome.is_some();
        let expired = !terminal && now >= self.deadline;
        Projection {
            operation_id: format!("{epoch}-{record}-{}", self.sequence),
            generation: facts.generation,
            requested_enable: self.enable,
            phase: facts.phase,
            outcome: outcome.map(str::to_owned),
            terminal,
            succeeded: matches!(outcome, Some("ready" | "cancelled" | "stopped")),
            observation_expired: expired,
            cleanup_confirmed: facts.cleanup_confirmed,
            poll_after_ms: if terminal || expired { 0 } else { 1_500 },
            message: if expired {
                "等待操作结果超时；后台 owner 仍在工作，请刷新核对，资源未因此释放".into()
            } else {
                message
            },
            client_request_id: None,
        }
    }

    pub(super) fn receipt(
        &self,
        request_id: &str,
        fingerprint: &str,
        epoch: &str,
        record: u64,
        generation: u64,
    ) -> Receipt {
        Receipt::capture(
            request_id,
            fingerprint,
            &self.project(
                Instant::now(),
                epoch,
                record,
                Facts {
                    generation,
                    phase: "admitted".into(),
                    ready: false,
                    sms_ready: false,
                    call_ready: false,
                    cleanup_confirmed: false,
                    superseded: false,
                    error: None,
                },
            ),
        )
    }
}
pub(super) struct Facts {
    pub(super) generation: u64,
    pub(super) phase: String,
    pub(super) ready: bool,
    pub(super) sms_ready: bool,
    pub(super) call_ready: bool,
    pub(super) cleanup_confirmed: bool,
    pub(super) superseded: bool,
    pub(super) error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct Projection {
    pub(super) operation_id: String,
    pub(super) generation: u64,
    pub(super) requested_enable: bool,
    pub(super) phase: String,
    pub(super) outcome: Option<String>,
    pub(super) terminal: bool,
    pub(super) succeeded: bool,
    pub(super) observation_expired: bool,
    pub(super) cleanup_confirmed: bool,
    pub(super) poll_after_ms: u64,
    pub(super) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) client_request_id: Option<String>,
}

impl Projection {
    fn with_request_id(mut self, request_id: &str) -> Self {
        self.client_request_id = Some(request_id.to_owned());
        self
    }
}

/// Retains the exact mutation identity independently from whichever VoWiFi
/// record is currently published. HTTP waiter loss can therefore recover the
/// admitted operation by the caller's request ID without adopting a successor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Receipt {
    request_id: String,
    fingerprint: String,
    operation_id: String,
    generation: u64,
    requested_enable: bool,
    snapshot: Option<Projection>,
    #[serde(skip)]
    recovered: bool,
}

impl Receipt {
    pub(super) fn capture(request_id: &str, fingerprint: &str, projection: &Projection) -> Self {
        Self {
            request_id: request_id.to_owned(),
            fingerprint: fingerprint.to_owned(),
            operation_id: projection.operation_id.clone(),
            generation: projection.generation,
            requested_enable: projection.requested_enable,
            snapshot: Some(projection.clone().with_request_id(request_id)),
            recovered: false,
        }
    }

    /// A request rejected before owner dispatch has a durable terminal identity
    /// of its own. It must not borrow the current reader's operation or status.
    pub(super) fn rejected(
        request_id: &str,
        fingerprint: &str,
        requested_enable: bool,
        message: &str,
    ) -> Self {
        Self::capture(
            request_id,
            fingerprint,
            &Projection {
                operation_id: format!("rejected-{request_id}"),
                generation: 0,
                requested_enable,
                phase: "rejected".into(),
                outcome: Some("failed".into()),
                terminal: true,
                succeeded: false,
                observation_expired: false,
                cleanup_confirmed: false,
                poll_after_ms: 0,
                message: message.into(),
                client_request_id: Some(request_id.into()),
            },
        )
    }

    pub(super) fn resolve(&self, current: Option<Projection>) -> Projection {
        if let Some(snapshot) = &self.snapshot
            && (snapshot.terminal || self.recovered)
        {
            return snapshot.clone().with_request_id(&self.request_id);
        }
        if let Some(current) = current
            && current.operation_id == self.operation_id
            && current.generation == self.generation
        {
            return current.with_request_id(&self.request_id);
        }
        Projection {
            operation_id: self.operation_id.clone(),
            generation: self.generation,
            requested_enable: self.requested_enable,
            phase: "superseded".into(),
            outcome: Some("superseded".into()),
            terminal: true,
            succeeded: false,
            observation_expired: false,
            cleanup_confirmed: false,
            poll_after_ms: 0,
            message: "号卡或操作代次已变化，原结果不再适用".into(),
            client_request_id: Some(self.request_id.clone()),
        }
    }
}

/// One archive per owner domain; never evict identities. A full/unreadable
/// archive fails closed rather than making an old ID dispatchable again.
#[derive(Default)]
pub(super) struct RequestLedger {
    entries: BTreeMap<String, Receipt>,
    store: Option<ArchiveStore>,
    failed: bool,
    #[cfg(test)]
    fail_after_persist: std::cell::Cell<bool>,
    #[cfg(test)]
    fail_write: std::cell::Cell<Option<std::io::ErrorKind>>,
}

impl RequestLedger {
    pub(super) fn durable(&self) -> bool {
        self.store.is_some() && !self.failed
    }

    pub(super) fn open(root: &Path, owner: &str) -> Result<Self, String> {
        let store = ArchiveStore::with_limits(
            root.join(format!("vowifi-{owner}-request-ledger.json")),
            32 * 1024 * 1024,
            100_000,
        )
        .and_then(ArchiveStore::require_nonempty_ledger)
        .map_err(|e| e.to_string())?;
        let mut entries = BTreeMap::new();
        for mut receipt in
            Self::decode_entries(store.load().map_err(|e| e.to_string())?)?.into_values()
        {
            if !receipt.snapshot.as_ref().is_some_and(|p| p.terminal) {
                let mut unknown = receipt.resolve(None);
                unknown.phase = "unknown".into();
                unknown.outcome = Some("unknown".into());
                unknown.terminal = false;
                unknown.succeeded = false;
                unknown.observation_expired = true;
                unknown.poll_after_ms = 1_500;
                unknown.message =
                    "后台重启；原请求提交结果未知，禁止自动重发或接管其他 owner".into();
                receipt.snapshot = Some(unknown);
                receipt.recovered = true;
            }
            entries.insert(receipt.request_id.clone(), receipt);
        }
        let ledger = Self {
            entries,
            store: Some(store),
            failed: false,
            #[cfg(test)]
            fail_after_persist: std::cell::Cell::new(false),
            #[cfg(test)]
            fail_write: Default::default(),
        };
        if !ledger.entries.is_empty() {
            ledger.persist().map_err(str::to_owned)?;
        }
        Ok(ledger)
    }

    fn decode_entries(values: Vec<serde_json::Value>) -> Result<BTreeMap<String, Receipt>, String> {
        let mut entries = BTreeMap::new();
        for value in values {
            let receipt: Receipt = serde_json::from_value(value).map_err(|e| e.to_string())?;
            let snapshot_matches_identity = receipt.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.operation_id == receipt.operation_id
                    && snapshot.generation == receipt.generation
                    && snapshot.requested_enable == receipt.requested_enable
                    && snapshot
                        .client_request_id
                        .as_deref()
                        .is_none_or(|request_id| request_id == receipt.request_id)
            });
            if !valid_request_id(&receipt.request_id)
                || receipt.operation_id.is_empty()
                || receipt.fingerprint.len() != 64
                || !receipt
                    .fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || !snapshot_matches_identity
                || entries.contains_key(&receipt.request_id)
            {
                return Err("invalid_vowifi_request_ledger".into());
            }
            entries.insert(receipt.request_id.clone(), receipt);
        }
        Ok(entries)
    }

    fn persist(&self) -> Result<(), &'static str> {
        // Deterministic storage-admission fault injection, not a claim that a
        // mounted read-only filesystem or real ENOSPC has been exercised.
        #[cfg(test)]
        if self.fail_write.get().is_some() {
            return Err("vowifi_request_ledger_unavailable");
        }
        if let Some(store) = &self.store {
            let values = self
                .entries
                .values()
                .map(serde_json::to_value)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "vowifi_request_ledger_unavailable")?;
            store
                .mutate(|items| {
                    Self::decode_entries(items.clone())
                        .map_err(proletarius_store::StoreError::Configuration)?;
                    *items = values;
                    Ok(())
                })
                .map_err(|_| "vowifi_request_ledger_unavailable")?;
            // Test-only lost commit acknowledgement: the real store mutation
            // above has completed. This is not a filesystem fsync fault.
            #[cfg(test)]
            if self.fail_after_persist.replace(false) {
                return Err("vowifi_request_ledger_unavailable");
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn test_fail_after_next_persist(&self) {
        self.fail_after_persist.set(true);
    }

    #[cfg(test)]
    pub(super) fn test_write_fault(&self, kind: std::io::ErrorKind) {
        self.fail_write.set(Some(kind));
    }
    #[cfg(test)]
    pub(super) fn test_fill_capacity(&mut self) -> usize {
        let template = self.entries.values().next().expect("seed request").clone();
        let mut snapshot = template.snapshot.clone().expect("seed projection");
        snapshot.terminal = true;
        snapshot.outcome = Some("failed".into());
        snapshot.succeeded = false;
        for index in self.entries.len()..100_000 {
            let id = format!("g3-capacity-{index}");
            self.entries.insert(
                id.clone(),
                Receipt::capture(&id, &template.fingerprint, &snapshot),
            );
        }
        self.entries.len()
    }
    #[cfg(test)]
    pub(super) fn test_count(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn observe(&mut self, current: Option<Projection>) -> Result<(), &'static str> {
        if self.failed {
            return Err("vowifi_request_ledger_unavailable");
        }
        let mut changed = Vec::new();
        for receipt in self.entries.values_mut() {
            if receipt.recovered || receipt.snapshot.as_ref().is_some_and(|p| p.terminal) {
                continue;
            }
            let projection = receipt.resolve(current.clone());
            if receipt.snapshot.as_ref() != Some(&projection) {
                changed.push((receipt.request_id.clone(), receipt.snapshot.clone()));
                receipt.snapshot = Some(projection);
            }
        }
        if !changed.is_empty() {
            if let Err(error) = self.persist() {
                for (request_id, snapshot) in changed {
                    if let Some(receipt) = self.entries.get_mut(&request_id) {
                        receipt.snapshot = snapshot;
                    }
                }
                self.failed = true;
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) fn mark_submitted(&mut self, request_id: &str) -> Result<(), &'static str> {
        if self.failed {
            return Err("vowifi_request_ledger_unavailable");
        }
        let receipt = self
            .entries
            .get_mut(request_id)
            .ok_or("vowifi_request_id_unknown")?;
        let previous = receipt.snapshot.clone();
        if let Some(snapshot) = &mut receipt.snapshot {
            snapshot.phase = "submitted".into();
        }
        if let Err(error) = self.persist() {
            if let Some(receipt) = self.entries.get_mut(request_id) {
                receipt.snapshot = previous;
            }
            self.failed = true;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn lookup(
        &self,
        request_id: &str,
        fingerprint: Option<&str>,
    ) -> Result<Option<Receipt>, &'static str> {
        if self.failed {
            return Err("vowifi_request_ledger_unavailable");
        }
        if let Some(store) = &self.store {
            Self::decode_entries(
                store
                    .load()
                    .map_err(|_| "vowifi_request_ledger_unavailable")?,
            )
            .map_err(|_| "vowifi_request_ledger_unavailable")?;
        }
        let Some(receipt) = self.entries.get(request_id) else {
            return Ok(None);
        };
        if fingerprint.is_some_and(|value| value != receipt.fingerprint) {
            return Err("vowifi_request_id_conflict");
        }
        // Reconfirm an uncertain previous rename without rewriting unchanged bytes.
        if let Some(store) = &self.store {
            store
                .confirm_directory_sync()
                .map_err(|_| "vowifi_request_ledger_unavailable")?;
        }
        Ok(Some(receipt.clone()))
    }

    pub(super) fn insert(&mut self, receipt: Receipt) -> Result<(), &'static str> {
        if self.failed {
            return Err("vowifi_request_ledger_unavailable");
        }
        if self
            .lookup(&receipt.request_id, Some(&receipt.fingerprint))?
            .is_some()
        {
            return Ok(());
        }
        if self.entries.len() >= 100_000 {
            return Err("vowifi_request_ledger_full");
        }
        let request_id = receipt.request_id.clone();
        self.entries.insert(request_id.clone(), receipt);
        if let Err(error) = self.persist() {
            self.entries.remove(&request_id);
            self.failed = true;
            return Err(error);
        }
        Ok(())
    }
}

/// Stable semantic JSON encoding: property ordering/whitespace do not create a
/// new request. Preserve array order, nulls, and all business/owner fields.
///
/// The public compatibility decoders intentionally preserve duplicate top-level
/// fields and their order (including Go's case folding and null behavior).
/// `serde_json::Value` alone keeps only the final duplicate, which could make
/// two different frozen proxy/ePDG intents share one durable request receipt.
pub(super) fn request_fingerprint(bytes: &[u8]) -> String {
    fn canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(values) => {
                let sorted: BTreeMap<_, _> =
                    values.into_iter().map(|(k, v)| (k, canonical(v))).collect();
                serde_json::to_value(sorted).expect("JSON values serialize")
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonical).collect())
            }
            other => other,
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let encoded = Option::<super::api::Fields>::deserialize(&mut decoder)
        .ok()
        .flatten()
        .and_then(|fields| {
            let mut grouped: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
            let mut epdg_aliases = Vec::new();
            for (key, value) in fields.0 {
                let key = super::api::field_name(&key);
                let value = canonical(value);
                if matches!(key.as_str(), "epdg" | "epdg_address") {
                    epdg_aliases.push((key.clone(), value.clone()));
                }
                grouped.entry(key).or_default().push(value);
            }
            if grouped.contains_key("epdg") && grouped.contains_key("epdg_address") {
                // The module decoder assigns both aliases to the same field
                // in wire order. Preserve their interleaving while leaving
                // independent property ordering and old single-alias hashes
                // unchanged. Ambiguous legacy receipts conflict by request ID;
                // never fall back to their order-insensitive fingerprint.
                let mut encoded = b"switch-vowifi-epdg-alias-order-v1\0".to_vec();
                encoded.extend(serde_json::to_vec(&(grouped, epdg_aliases)).ok()?);
                return Some(encoded);
            }
            let duplicate = grouped.values().any(|values| values.len() > 1);
            if duplicate {
                // Domain-separate the duplicate-preserving representation from
                // every valid JSON body, so a crafted ordinary object cannot
                // collide with this sequence encoding.
                let mut encoded = b"switch-vowifi-duplicate-fields-v1\0".to_vec();
                encoded.extend(serde_json::to_vec(&grouped).ok()?);
                Some(encoded)
            } else {
                let object = grouped
                    .into_iter()
                    .map(|(key, mut values)| (key, values.pop().expect("one field value")))
                    .collect();
                serde_json::to_vec(&serde_json::Value::Object(object)).ok()
            }
        });
    proletarius_sms::stable_digest_hex(encoded.as_deref().unwrap_or(bytes))
}

#[cfg(test)]
mod tests;
