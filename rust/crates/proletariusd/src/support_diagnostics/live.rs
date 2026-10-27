//! A bounded current-runtime view, independent of disk export and its permit.
use super::*;
use std::collections::VecDeque;

const MAX_EVENTS: usize = 256;
const MAX_BYTES: usize = 256 * 1024;
const RESPONSE_BYTES: usize = MAX_BYTES + 32 * 1024;

#[derive(Default)]
pub(super) struct Buffer {
    records: VecDeque<(Record, usize)>,
    bytes: usize,
    evicted: u64,
}
impl Buffer {
    fn push(&mut self, record: Record, bytes: usize) {
        while self.records.len() >= MAX_EVENTS || self.bytes + bytes > MAX_BYTES {
            let Some((_, removed)) = self.records.pop_front() else { break };
            self.bytes -= removed;
            self.evicted = self.evicted.saturating_add(1);
        }
        self.bytes += bytes;
        self.records.push_back((record, bytes));
    }
}

pub(super) fn record(shared: &Shared, record: &Record) {
    // Producers never wait behind a UI snapshot, a disk export, or each other.
    let Ok(bytes) = serde_json::to_vec(record) else {
        shared.live_missed.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if bytes.len() > RECORD_BYTES || !valid(record) {
        shared.live_missed.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let Ok(mut buffer) = shared.live.try_lock() else {
        shared.live_missed.fetch_add(1, Ordering::Relaxed);
        return;
    };
    buffer.push(record.clone(), bytes.len());
}

fn snapshot(shared: &Shared) -> Option<serde_json::Value> {
    // Clone only a bounded window under the lock, then release it before JSON
    // encoding. No root paths, stderr text, headers or raw errors are observed.
    let (mut records, retained_bytes, evicted) = {
        let buffer = shared.live.try_lock().ok()?;
        (buffer.records.iter().map(|(record, _)| record.clone()).collect::<Vec<_>>(),
            buffer.bytes, buffer.evicted)
    };
    records.sort_unstable_by_key(|record| record.sequence);
    let recovery = shared.recovery.try_lock().ok()?.snapshot();
    let counters = &shared.counters;
    let missed = shared.live_missed.load(Ordering::Relaxed);
    let dropped = counters.dropped.load(Ordering::Relaxed);
    let failures = counters.write_failures.load(Ordering::Relaxed);
    let mut warnings = vec!["live_current_runtime_only", "best_effort_not_lossless", "snapshot_not_atomic"];
    if evicted != 0 { warnings.push("live_window_truncated"); }
    if missed != 0 { warnings.push("live_records_missed"); }
    if dropped != 0 { warnings.push("records_dropped"); }
    if failures != 0 { warnings.push("write_failed"); }
    if !shared.available.load(Ordering::Relaxed) { warnings.push("writer_unavailable"); }
    Some(serde_json::json!({
        "schema_version":1, "view":"live", "generated_at_ms":now_ms(),
        "version":crate::APP_RELEASE_VERSION, "build":crate::APP_BUILD_NUMBER,
        "source_digest":crate::SOURCE_DIGEST, "runtime_id":shared.runtime_id,
        "limits":{"queue_capacity":QUEUE,"max_record_bytes":RECORD_BYTES,
            "segment_bytes":SEGMENT_BYTES,"segments":NAMES.len(),
            "export_bytes":EXPORT_BYTES,"max_events":MAX_EVENTS,
            "memory_bytes":MAX_BYTES,"response_bytes":RESPONSE_BYTES},
        "counters":{"accepted":counters.accepted.load(Ordering::Relaxed),
            "written":counters.written.load(Ordering::Relaxed),"dropped":dropped,
            "write_failures":failures,"invalid_records":0,"read_failures":0,
            "live_evicted":evicted,"live_missed":missed,
            "retained_bytes":retained_bytes},
        "module_recovery":recovery, "events":records, "warnings":warnings
    }))
}

pub(super) fn response(shared: &Shared) -> Response<Full<Bytes>> {
    let bytes = snapshot(shared).and_then(|value| serde_json::to_vec(&value).ok())
        .filter(|bytes| bytes.len() <= RESPONSE_BYTES);
    match bytes {
        Some(bytes) => Response::builder().status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .header(hyper::header::CACHE_CONTROL, "no-store")
            .body(Full::new(Bytes::from(bytes))).expect("static live support response"),
        None => unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            // No directory exists or is created: the live path must not use it.
            root: PathBuf::from("/switch-live-fixture-must-not-be-read"),
            runtime_id: "a".repeat(32),
            sequence: AtomicU64::new(0),
            counters: Counters::default(),
            available: AtomicBool::new(false),
            exporting: AtomicBool::new(true),
            recovery: Mutex::new(recovery::Observation::default()),
            live: Mutex::new(Buffer::default()),
            live_missed: AtomicU64::new(0),
        })
    }
    fn event(sequence: u64) -> Record {
        Record { schema_version:1, runtime_id:"a".repeat(32), sequence, time_ms:sequence,
            event:Event::Lifecycle { phase:"startup".into() } }
    }

    #[test]
    fn live_survives_full_disk_queue_and_never_uses_export_permit_or_root() {
        let shared = shared();
        let (sender, _receiver) = mpsc::sync_channel(1);
        let recorder = Recorder { sender, shared:shared.clone() };
        for _ in 0..3 { recorder.emit(Event::Lifecycle { phase:"startup".into() }); }
        let value = snapshot(&shared).unwrap();
        assert_eq!(value["events"].as_array().unwrap().len(), 3);
        assert_eq!(value["counters"]["accepted"], 1);
        assert_eq!(value["counters"]["dropped"], 2);
        assert!(shared.exporting.load(Ordering::Relaxed));
        assert_eq!(response(&shared).status(), StatusCode::OK);
        assert_eq!(response(&shared).headers()[hyper::header::CACHE_CONTROL], "no-store");
    }

    #[test]
    fn oldest_records_are_evicted_and_new_runtime_does_not_adopt_old_rows() {
        let state = shared();
        for sequence in 1..=600 { record(&state, &event(sequence)); }
        let value = snapshot(&state).unwrap();
        let rows = value["events"].as_array().unwrap();
        assert_eq!(rows.len(), MAX_EVENTS);
        assert_eq!(rows.first().unwrap()["sequence"], 345);
        assert_eq!(rows.last().unwrap()["sequence"], 600);
        assert_eq!(value["counters"]["live_evicted"], 344);
        assert!(value["counters"]["retained_bytes"].as_u64().unwrap() <= MAX_BYTES as u64);
        assert!(serde_json::to_vec(&value).unwrap().len() <= RESPONSE_BYTES);
        assert!(snapshot(&shared()).unwrap()["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn byte_bound_is_independent_of_record_count() {
        let mut buffer = Buffer::default();
        for sequence in 1..=100 { buffer.push(event(sequence), RECORD_BYTES); }
        assert_eq!(buffer.records.len(), MAX_BYTES / RECORD_BYTES);
        assert_eq!(buffer.bytes, MAX_BYTES);
        assert_eq!(buffer.evicted, 100 - (MAX_BYTES / RECORD_BYTES) as u64);
    }

    #[test]
    fn held_ui_lock_never_blocks_a_producer_or_claims_a_complete_snapshot() {
        let shared = shared();
        let held = shared.live.lock().unwrap();
        record(&shared, &event(1));
        assert_eq!(shared.live_missed.load(Ordering::Relaxed), 1);
        assert_eq!(response(&shared).status(), StatusCode::SERVICE_UNAVAILABLE);
        drop(held);
        record(&shared, &event(2));
        assert_eq!(snapshot(&shared).unwrap()["events"][0]["sequence"], 2);
    }

    #[test]
    fn invalid_fields_cannot_enter_live_view_and_recovery_is_independent() {
        let shared = shared();
        let mut invalid_event = event(1);
        invalid_event.event = Event::Lifecycle { phase:"PRIVATE-NUMBER-AND-TOKEN".into() };
        record(&shared, &invalid_event);
        let gate = crate::module_recovery::Gate::quarantined_for_tests();
        shared.recovery.lock().unwrap().bind(&gate);
        let value = snapshot(&shared).unwrap();
        assert!(value["events"].as_array().unwrap().is_empty());
        assert_eq!(value["counters"]["live_missed"], 1);
        assert_eq!(value["module_recovery"]["state"], "legacy_recovery_required");
        assert_eq!(value["module_recovery"]["historical_recovery_confirmed"], false);
        assert!(!value.to_string().contains("PRIVATE-NUMBER"));
    }
    #[test]
    fn concurrent_arrival_order_is_sorted_outside_the_producer_lock() {
        let shared = shared();
        for sequence in [3, 1, 2] { record(&shared, &event(sequence)); }
        let value = snapshot(&shared).unwrap();
        let sequences = value["events"].as_array().unwrap().iter()
            .map(|record| record["sequence"].as_u64().unwrap()).collect::<Vec<_>>();
        assert_eq!(sequences, [1, 2, 3]);
    }

    #[test]
    fn recovery_observation_contention_returns_without_waiting() {
        let shared = shared();
        let held = shared.recovery.lock().unwrap();
        assert_eq!(response(&shared).status(), StatusCode::SERVICE_UNAVAILABLE);
        drop(held);
        assert_eq!(response(&shared).status(), StatusCode::OK);
    }

}
