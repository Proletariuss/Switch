//! Fixed, non-replaying USB SMS storage transactions. The USB owner supplies
//! one transport and distinct active/cleanup fences; this module never opens it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hyper::StatusCode;
use serde::{Deserialize, Serialize};

use crate::usb_at::Failure;

const MAX_GROUPS: usize = 1500;
const MAX_SOURCES: usize = 4096;
const MAX_PDU_HEX: usize = 8192;
const MAX_TOTAL_PDU_HEX: usize = 1024 * 1024;
const RESPONSE_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum Storage {
    SM,
    ME,
}
impl Storage {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SM => "SM",
            Self::ME => "ME",
        }
    }
}

#[derive(Clone)]
pub(crate) enum StorageOperation {
    Usage,
    Clear { storages: Vec<Storage> },
    Delete { groups: Vec<DeleteGroup> },
}
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct DeleteGroup {
    pub(crate) token: String,
    pub(crate) card_identity: String,
    pub(crate) sources: Vec<StoredSource>,
}
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct StoredSource {
    pub(crate) storage: Storage,
    pub(crate) index: u32,
    pub(crate) pdu: String,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Eq, PartialEq)]
pub(crate) struct StorageUsage {
    pub(crate) used: u32,
    pub(crate) total: u32,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ClearDetail {
    pub(crate) before: u32,
    pub(crate) after: Option<u32>,
    pub(crate) delete_confirmed: bool,
}
#[derive(Clone, Default, Serialize)]
pub(crate) struct Outcome {
    pub(crate) complete: bool,
    pub(crate) uncertain: bool,
    pub(crate) storage_restored: bool,
    pub(crate) usage: BTreeMap<Storage, StorageUsage>,
    pub(crate) deleted_tokens: Vec<String>,
    pub(crate) details: BTreeMap<Storage, ClearDetail>,
    pub(crate) error_code: String,
    #[serde(skip)]
    pub(crate) card_identity: String,
}
impl std::fmt::Debug for Outcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Outcome")
            .field("complete", &self.complete)
            .field("uncertain", &self.uncertain)
            .field("storage_restored", &self.storage_restored)
            .field("deleted_count", &self.deleted_tokens.len())
            .field("error_code", &self.error_code)
            .finish()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Phase {
    Active,
    Cleanup,
}
pub(crate) struct Execution {
    pub(crate) outcome: Outcome,
    pub(crate) retire_transport: bool,
}

impl StorageOperation {
    pub(crate) fn validate(&self) -> Result<(), Failure> {
        let invalid = || {
            Failure::new(
                StatusCode::BAD_REQUEST,
                "usb_sms_storage_invalid",
                "USB SMS storage request is invalid",
            )
        };
        match self {
            Self::Usage => Ok(()),
            Self::Clear { storages } => {
                if storages.is_empty()
                    || storages.len() > 2
                    || storages.iter().copied().collect::<BTreeSet<_>>().len() != storages.len()
                {
                    Err(invalid())
                } else {
                    Ok(())
                }
            }
            Self::Delete { groups } => {
                if groups.is_empty() || groups.len() > MAX_GROUPS {
                    return Err(invalid());
                }
                let mut tokens = BTreeSet::new();
                let mut sources = BTreeMap::<(Storage, u32), String>::new();
                let mut count = 0usize;
                let mut bytes = 0usize;
                for group in groups {
                    if group.token.is_empty()
                        || group.token.len() > 128
                        || !group
                            .token
                            .bytes()
                            .all(|v| v.is_ascii_alphanumeric() || b"-_".contains(&v))
                        || !tokens.insert(&group.token)
                        || group.sources.is_empty()
                        || !valid_card_identity(&group.card_identity)
                    {
                        return Err(invalid());
                    }
                    for source in &group.sources {
                        count += 1;
                        bytes = bytes.saturating_add(source.pdu.len());
                        if count > MAX_SOURCES
                            || bytes > MAX_TOTAL_PDU_HEX
                            || source.index == 0
                            || canonical_pdu(&source.pdu).is_none()
                        {
                            return Err(invalid());
                        }
                        let pdu = source.pdu.to_ascii_uppercase();
                        if let Some(previous) =
                            sources.insert((source.storage, source.index), pdu.clone())
                            && previous != pdu
                        {
                            return Err(invalid());
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

fn canonical_pdu(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > MAX_PDU_HEX
        || !value.len().is_multiple_of(2)
        || !value.bytes().all(|v| v.is_ascii_hexdigit())
    {
        return None;
    }
    crate::sms_codec::canonical_stored_pdu("+CMGR:", value)
}

fn readback_pdu(response: &str) -> Option<String> {
    let lines = response
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let mut headers = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("+CMGR:"));
    let (position, header) = headers.next()?;
    if headers.next().is_some() {
        return None;
    }
    crate::sms_codec::canonical_stored_pdu(header, lines.get(position + 1)?)
}

#[derive(Clone)]
struct Selection {
    stores: [String; 3],
    usage: [StorageUsage; 3],
}
fn parse_selection(response: &str) -> Option<Selection> {
    let mut found = response
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("+CPMS:"));
    let fields = found.next()?.split(',').map(str::trim).collect::<Vec<_>>();
    if fields.len() != 9 || found.next().is_some() {
        return None;
    }
    let mut stores = [String::new(), String::new(), String::new()];
    let mut usage = [StorageUsage::default(); 3];
    for position in 0..3 {
        let storage = fields[position * 3].strip_prefix('"')?.strip_suffix('"')?;
        // Restoring the modem's original selection may involve MT/BM/SR/TA.
        // This is parsed device state, never caller-supplied command text.
        if !matches!(storage, "SM" | "ME" | "MT" | "BM" | "SR" | "TA") {
            return None;
        }
        let used = fields[position * 3 + 1].parse().ok()?;
        let total = fields[position * 3 + 2].parse().ok()?;
        if used > total {
            return None;
        }
        stores[position] = storage.into();
        usage[position] = StorageUsage { used, total };
    }
    Some(Selection { stores, usage })
}
fn select_command(stores: &[String; 3]) -> String {
    format!(
        "AT+CPMS=\"{}\",\"{}\",\"{}\"",
        stores[0], stores[1], stores[2]
    )
}

enum Terminal {
    Ok,
    Rejected,
    Incomplete,
}
fn terminal(response: &str) -> Terminal {
    let lines = response
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.iter().any(|line| {
        *line == "ERROR" || line.starts_with("+CMS ERROR:") || line.starts_with("+CME ERROR:")
    }) {
        Terminal::Rejected
    } else if lines.last() == Some(&"OK") {
        Terminal::Ok
    } else {
        Terminal::Incomplete
    }
}
fn valid_card_identity(value: &str) -> bool {
    (10..=22).contains(&value.len()) && value.bytes().all(|v| v.is_ascii_digit())
}
pub(crate) fn card_identity(response: &str) -> Option<String> {
    if !matches!(terminal(response), Terminal::Ok) {
        return None;
    }
    let mut fields = response
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("+QCCID:"));
    let identity = fields.next()?.trim();
    let identity = if identity.starts_with('"') {
        identity.strip_prefix('"')?.strip_suffix('"')?
    } else {
        identity
    };
    if fields.next().is_some() || !valid_card_identity(identity) {
        return None;
    }
    Some(identity.into())
}
struct StepError {
    code: String,
    uncertain: bool,
}
impl StepError {
    fn known(code: &str) -> Self {
        Self {
            code: code.into(),
            uncertain: false,
        }
    }
}

struct Transaction<F> {
    command: F,
    outcome: Outcome,
    original: Option<[String; 3]>,
    selected: Option<Selection>,
    selection_touched: bool,
    retire_transport: bool,
}
impl<F> Transaction<F>
where
    F: FnMut(Phase, &str, Duration, usize) -> Result<String, Failure>,
{
    fn run(
        &mut self,
        phase: Phase,
        text: &str,
        timeout: Duration,
        code: &str,
        deleting: bool,
    ) -> Result<String, StepError> {
        let response =
            (self.command)(phase, text, timeout, RESPONSE_LIMIT).map_err(|error| StepError {
                code: error.code.into(),
                uncertain: deleting && error.uncertain,
            })?;
        match terminal(&response) {
            Terminal::Ok => Ok(response),
            Terminal::Rejected => Err(StepError::known(code)),
            Terminal::Incomplete => {
                // A partial response can leave unread bytes in the stream. Do
                // not run a restore or another command against that stream.
                self.retire_transport = true;
                Err(StepError {
                    code: "usb_sms_storage_incomplete_response".into(),
                    uncertain: deleting,
                })
            }
        }
    }
    fn query(&mut self, phase: Phase) -> Result<Selection, StepError> {
        let response = self.run(
            phase,
            "AT+CPMS?",
            Duration::from_secs(3),
            "usb_sms_storage_query_failed",
            false,
        )?;
        parse_selection(&response).ok_or_else(|| StepError::known("usb_sms_storage_query_invalid"))
    }
    fn read_card(&mut self, phase: Phase) -> Result<String, StepError> {
        let response = self.run(
            phase,
            "AT+QCCID",
            Duration::from_secs(3),
            "usb_sms_storage_card_unconfirmed",
            false,
        )?;
        card_identity(&response).ok_or_else(|| StepError::known("usb_sms_storage_card_unconfirmed"))
    }
    fn confirm_card(&mut self, phase: Phase, after_delete: bool) -> Result<(), StepError> {
        let result = self.read_card(phase).and_then(|identity| {
            if identity != self.outcome.card_identity {
                self.retire_transport = true;
                Err(StepError::known("usb_sms_storage_card_changed"))
            } else {
                Ok(())
            }
        });
        result.map_err(|mut error| {
            // The immediately preceding CMGD had a complete OK, but a failed
            // identity confirmation cannot attribute that result to this card.
            error.uncertain |= after_delete;
            error
        })
    }
    fn select(&mut self, storage: Storage) -> Result<StorageUsage, StepError> {
        let stores = [
            storage.as_str().into(),
            storage.as_str().into(),
            storage.as_str().into(),
        ];
        if self
            .selected
            .as_ref()
            .is_none_or(|selection| selection.stores != stores)
        {
            self.selection_touched = true;
            self.run(
                Phase::Active,
                &select_command(&stores),
                Duration::from_secs(5),
                "usb_sms_storage_select_failed",
                false,
            )?;
            let selected = self.query(Phase::Active)?;
            if selected.stores != stores {
                return Err(StepError::known("usb_sms_storage_selection_mismatch"));
            }
            self.selected = Some(selected);
        }
        Ok(self.selected.as_ref().unwrap().usage[0])
    }
    fn perform(&mut self, operation: &StorageOperation) -> Result<(), StepError> {
        if !matches!(operation, StorageOperation::Usage) {
            self.outcome.card_identity = self.read_card(Phase::Active)?;
            if let StorageOperation::Delete { groups } = operation
                && groups
                    .iter()
                    .any(|group| group.card_identity != self.outcome.card_identity)
            {
                return Err(StepError::known("usb_sms_storage_card_changed"));
            }
        }
        let initial = self.query(Phase::Active)?;
        self.original = Some(initial.stores.clone());
        self.selected = Some(initial);
        match operation {
            StorageOperation::Usage => {
                for storage in [Storage::SM, Storage::ME] {
                    let usage = self.select(storage)?;
                    self.outcome.usage.insert(storage, usage);
                }
            }
            StorageOperation::Clear { storages } => {
                for &storage in storages {
                    let before = self.select(storage)?;
                    self.outcome.usage.insert(storage, before);
                    self.outcome.details.insert(
                        storage,
                        ClearDetail {
                            before: before.used,
                            after: None,
                            delete_confirmed: false,
                        },
                    );
                    self.confirm_card(Phase::Active, false)?;
                    self.run(
                        Phase::Active,
                        "AT+CMGD=1,4",
                        Duration::from_secs(20),
                        "usb_sms_storage_delete_failed",
                        true,
                    )?;
                    self.confirm_card(Phase::Active, true)?;
                    self.outcome
                        .details
                        .get_mut(&storage)
                        .unwrap()
                        .delete_confirmed = true;
                    let after = self.query(Phase::Active)?;
                    if after.stores != self.selected.as_ref().unwrap().stores {
                        return Err(StepError::known("usb_sms_storage_selection_mismatch"));
                    }
                    self.outcome.details.get_mut(&storage).unwrap().after =
                        Some(after.usage[0].used);
                    self.outcome.usage.insert(storage, after.usage[0]);
                    self.selected = Some(after);
                    if self.outcome.usage[&storage].used != 0 && self.outcome.error_code.is_empty()
                    {
                        self.outcome.error_code = "usb_sms_storage_not_empty".into();
                    }
                }
            }
            StorageOperation::Delete { groups } => {
                self.run(
                    Phase::Active,
                    "AT+CMGF=0",
                    Duration::from_secs(3),
                    "usb_sms_storage_pdu_mode_failed",
                    false,
                )?;
                let mut sources = BTreeMap::new();
                for group in groups {
                    for source in &group.sources {
                        sources.insert(
                            (source.storage, source.index),
                            source.pdu.to_ascii_uppercase(),
                        );
                    }
                }
                // Verify every source before the first delete, including all
                // multipart pieces and duplicate SM/ME representations.
                for (&(storage, index), expected) in &sources {
                    self.select(storage)?;
                    let response = self.run(
                        Phase::Active,
                        &format!("AT+CMGR={index}"),
                        Duration::from_secs(5),
                        "usb_sms_storage_read_failed",
                        false,
                    )?;
                    let matches = readback_pdu(&response).is_some_and(|pdu| &pdu == expected);
                    if !matches {
                        return Err(StepError::known("usb_sms_storage_message_changed"));
                    }
                }
                let mut ordered = sources.keys().copied().collect::<Vec<_>>();
                ordered.sort_by_key(|&(storage, index)| (storage, std::cmp::Reverse(index)));
                let mut confirmed = BTreeSet::new();
                for (storage, index) in ordered {
                    self.select(storage)?;
                    self.confirm_card(Phase::Active, false)?;
                    // The initial whole-batch preflight avoids known partial
                    // groups; this immediate check protects a slot reused by
                    // the device while a large preflight was still running.
                    let response = self.run(
                        Phase::Active,
                        &format!("AT+CMGR={index}"),
                        Duration::from_secs(5),
                        "usb_sms_storage_read_failed",
                        false,
                    )?;
                    if readback_pdu(&response).as_deref()
                        != sources.get(&(storage, index)).map(String::as_str)
                    {
                        return Err(StepError::known("usb_sms_storage_message_changed"));
                    }
                    self.run(
                        Phase::Active,
                        &format!("AT+CMGD={index}"),
                        Duration::from_secs(10),
                        "usb_sms_storage_delete_failed",
                        true,
                    )?;
                    self.confirm_card(Phase::Active, true)?;
                    confirmed.insert((storage, index));
                    for group in groups {
                        if !self.outcome.deleted_tokens.contains(&group.token)
                            && group
                                .sources
                                .iter()
                                .all(|source| confirmed.contains(&(source.storage, source.index)))
                        {
                            self.outcome.deleted_tokens.push(group.token.clone());
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn restore(&mut self) -> Result<(), StepError> {
        if self.retire_transport {
            return Err(StepError::known("usb_sms_storage_restore_unconfirmed"));
        }
        let Some(original) = self.original.clone() else {
            return Ok(()); // No selection was changed by this transaction.
        };
        if self.selection_touched {
            if !self.outcome.card_identity.is_empty() {
                self.confirm_card(Phase::Cleanup, false)?;
            }
            self.run(
                Phase::Cleanup,
                &select_command(&original),
                Duration::from_secs(5),
                "usb_sms_storage_restore_unconfirmed",
                false,
            )?;
            let restored = self.query(Phase::Cleanup)?;
            if restored.stores != original {
                return Err(StepError::known("usb_sms_storage_restore_unconfirmed"));
            }
        }
        Ok(())
    }
}

/// Input is validated before admission by Owner. Every error after admission is
/// represented in the outcome so confirmed mutations are never discarded.
pub(crate) fn execute<F>(operation: &StorageOperation, command: F) -> Execution
where
    F: FnMut(Phase, &str, Duration, usize) -> Result<String, Failure>,
{
    let mut transaction = Transaction {
        command,
        outcome: Outcome::default(),
        original: None,
        selected: None,
        selection_touched: false,
        retire_transport: false,
    };
    if let Err(error) = transaction.perform(operation) {
        transaction.outcome.error_code = error.code;
        transaction.outcome.uncertain |= error.uncertain;
    }
    match transaction.restore() {
        Ok(()) => transaction.outcome.storage_restored = true,
        Err(error) => {
            transaction.retire_transport = true;
            if transaction.outcome.error_code.is_empty() {
                transaction.outcome.error_code = error.code;
            }
        }
    }
    transaction.outcome.complete = transaction.outcome.error_code.is_empty()
        && transaction.outcome.storage_restored
        && !transaction.outcome.uncertain;
    Execution {
        outcome: transaction.outcome,
        retire_transport: transaction.retire_transport,
    }
}

#[cfg(test)]
#[path = "usb_at_storage/tests.rs"]
mod tests;
