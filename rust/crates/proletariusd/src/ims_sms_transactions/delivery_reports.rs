//! Bounded durable consumption receipts for ordinary/module TP status reports.
//! These records never enter the message archive or forwarding producers.
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use proletarius_store::{ArchiveStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub(super) const FILE: &str = "sms-delivery-reports.json";
const MAX_REPORTS: usize = 1_024;
const MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Report {
    schema_version: u8,
    id: String,
    device: String,
    subscription_id: String,
    rpdu_base64: String,
    tpdu_base64: String,
    recipient: String,
    message_reference: u8,
    status_code: u8,
    received_at_unix_ms: i64,
}

fn invalid() -> StoreError {
    StoreError::Configuration("invalid_sms_delivery_report".into())
}

impl Report {
    fn identity(&self) -> Result<String, StoreError> {
        // The TPDU includes TP-MR, recipient, service-centre/discharge times and
        // status. SIP transaction and RP references may change on retransmit.
        let identity = serde_json::to_vec(&(
            self.device.as_str(),
            self.subscription_id.as_str(),
            self.tpdu_base64.as_str(),
        ))
        .map_err(StoreError::Json)?;
        Ok(proletarius_sms::stable_digest_hex(&identity))
    }

    fn validate(&self) -> Result<(), StoreError> {
        if self.schema_version != 1
            || !matches!(self.device.as_str(), "reader" | "dji")
            || (!self.subscription_id.is_empty()
                && !super::valid_subscription_id(&self.subscription_id))
            || self.received_at_unix_ms <= 0
            || self.rpdu_base64.len() > 340
            || self.tpdu_base64.len() > 340
            || self.id != self.identity()?
        {
            return Err(invalid());
        }
        let rpdu = BASE64.decode(&self.rpdu_base64).map_err(|_| invalid())?;
        let tpdu = BASE64.decode(&self.tpdu_base64).map_err(|_| invalid())?;
        let envelope = proletarius_sms::MobileTerminatedData::parse(&rpdu)
            .map_err(|_| invalid())?;
        let report = proletarius_sms::status_report::parse(&tpdu).map_err(|_| invalid())?;
        if envelope.tpdu != tpdu
            || report.recipient != self.recipient
            || report.reference != self.message_reference
            || report.status_code != self.status_code
        {
            return Err(invalid());
        }
        Ok(())
    }
}

pub(super) async fn persist(
    root: PathBuf,
    device: &str,
    subscription_id: &str,
    event: &Value,
) -> Result<(), StoreError> {
    let string = |name: &str| {
        event[name].as_str().map(str::to_owned).ok_or_else(invalid)
    };
    let byte = |name: &str| {
        event[name].as_u64().and_then(|value| u8::try_from(value).ok())
            .ok_or_else(invalid)
    };
    let mut report = Report {
        schema_version: 1,
        id: String::new(),
        device: device.into(),
        subscription_id: subscription_id.into(),
        rpdu_base64: string("rpdu_base64")?,
        tpdu_base64: string("tpdu_base64")?,
        recipient: string("recipient")?,
        message_reference: byte("message_reference")?,
        status_code: byte("status_code")?,
        received_at_unix_ms: event["timestamp_unix_ms"].as_i64().ok_or_else(invalid)?,
    };
    report.id = report.identity()?;
    report.validate()?;
    crate::archive_io::run(move || {
        // Existing empty/corrupt/disappeared receipts fail closed. Retention is
        // explicit below; it never grows an unbounded session-owned map.
        let store = ArchiveStore::with_limits(root.join(FILE), MAX_BYTES, MAX_REPORTS)?
            .require_nonempty_ledger()?;
        let inserted = store.mutate_if_changed(|items| {
            let mut seen = std::collections::HashSet::new();
            for item in items.iter() {
                let saved: Report = serde_json::from_value(item.clone()).map_err(StoreError::Json)?;
                saved.validate()?;
                if !seen.insert(saved.id) {
                    return Err(invalid());
                }
            }
            if seen.contains(&report.id) {
                return Ok((false, false));
            }
            items.insert(0, serde_json::to_value(report).map_err(StoreError::Json)?);
            items.truncate(MAX_REPORTS);
            Ok((true, true))
        })?;
        if !inserted {
            // A prior rename may have succeeded before directory fsync failed.
            // A replay may acknowledge only after confirming that durability.
            store.confirm_directory_sync()?;
        }
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn status_reports_are_bounded_idempotent_and_subscription_scoped() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../build/tmp/status-report-ledger")
            .join(format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&root).unwrap();
        let subscription = format!("sub1_{}", "a".repeat(64));
        let event = |reference: u8, year: u8, rp_reference: u8| {
            let tpdu = vec![
                2, reference, 4, 0x91, 0x44, 0x77, year, 0x80, 0x62, 0x61, 0x21, 0, 0,
                year, 0x80, 0x62, 0x61, 0x21, 0, 0, 0,
            ];
            let mut rpdu = vec![1, rp_reference, 0, 0, tpdu.len() as u8];
            rpdu.extend_from_slice(&tpdu);
            super::super::decode_incoming_delivery(
                "status-fixture".into(), 1, 1_800_000_000_123, BASE64.encode(rpdu),
            ).unwrap()
        };
        let first = event(1, 0x62, 42);
        persist(root.clone(), "reader", &subscription, &first).await.unwrap();
        let store = ArchiveStore::with_limits(root.join(FILE), MAX_BYTES, MAX_REPORTS).unwrap();
        let bytes = std::fs::read(root.join(FILE)).unwrap();
        persist(root.clone(), "reader", &subscription, &event(1, 0x62, 43)).await.unwrap();
        assert_eq!(std::fs::read(root.join(FILE)).unwrap(), bytes);
        persist(root.clone(), "dji", &subscription, &first).await.unwrap();
        persist(root.clone(), "reader", &format!("sub1_{}", "b".repeat(64)), &first).await.unwrap();
        assert_eq!(store.load().unwrap().len(), 3);

        // Fill the real bounded archive in one atomic fixture write, then use
        // production admission to prove the oldest receipt is evicted.
        let template = store.load().unwrap().remove(0);
        let mut records = Vec::new();
        for index in 0..MAX_REPORTS {
            let value = event(index as u8, 0x62 + ((index / 256) as u8) * 0x10, 42);
            let mut report: Report = serde_json::from_value(template.clone()).unwrap();
            report.rpdu_base64 = value["rpdu_base64"].as_str().unwrap().into();
            report.tpdu_base64 = value["tpdu_base64"].as_str().unwrap().into();
            report.message_reference = index as u8;
            report.id = report.identity().unwrap();
            report.validate().unwrap();
            records.push(serde_json::to_value(report).unwrap());
        }
        store.replace(records).unwrap();
        let oldest = store.load().unwrap().last().unwrap()["id"].clone();
        persist(root.clone(), "dji", &subscription, &event(9, 0x13, 44)).await.unwrap();
        let records = store.load().unwrap();
        assert_eq!(records.len(), MAX_REPORTS);
        assert!(!records.iter().any(|record| record["id"] == oldest));
        assert!(!root.join("sms-archive.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
