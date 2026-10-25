//! Real owned-process exits at production backup transaction boundaries.
//! Hooks cannot return success, mint authority, or substitute a receipt.
use super::*;
use crate::daemon_lifetime::Lease;
use crate::module_recovery::Gate;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::sync::{Mutex, OnceLock};

const EVENT: &str = "SWITCH_BACKUP_LIFECYCLE_EVENT ";
#[derive(Clone, Copy, Debug)]
pub(super) enum PublicationPoint {
    BeforeGeneration,
    AfterGeneration,
    BeforeIntent,
    AfterIntent,
    BeforeTemp,
    AfterTempAllocated,
    AfterTempWrite,
    AfterTempSync,
    AfterReady,
    BeforePrepare,
    AfterPrepare,
    BeforeDirectoryRename,
    AfterDirectoryRename,
    BeforeRename,
    AfterRename,
    BeforeSourceSync,
    AfterSourceSync,
    BeforeDestinationSync,
    AfterDestinationSync,
    BeforeRootSync,
    AfterRootSync,
    BeforeCommit,
    AfterCommit,
}
impl PublicationPoint {
    fn name(self) -> &'static str {
        match self {
            Self::BeforeGeneration => "before-generation",
            Self::AfterGeneration => "after-generation",
            Self::BeforeIntent => "before-intent",
            Self::AfterIntent => "after-intent",
            Self::BeforeTemp => "before-temp",
            Self::AfterTempAllocated => "after-temp-allocated",
            Self::AfterTempWrite => "after-temp-write",
            Self::AfterTempSync => "after-temp-sync",
            Self::AfterReady => "after-ready",
            Self::BeforePrepare => "before-prepare",
            Self::AfterPrepare => "after-prepare",
            Self::BeforeDirectoryRename => "before-directory-rename",
            Self::AfterDirectoryRename => "after-directory-rename",
            Self::BeforeRename => "before-rename",
            Self::AfterRename => "after-rename",
            Self::BeforeSourceSync => "before-source-sync",
            Self::AfterSourceSync => "after-source-sync",
            Self::BeforeDestinationSync => "before-destination-sync",
            Self::AfterDestinationSync => "after-destination-sync",
            Self::BeforeRootSync => "before-root-sync",
            Self::AfterRootSync => "after-root-sync",
            Self::BeforeCommit => "before-commit",
            Self::AfterCommit => "after-commit",
        }
    }
}
struct Armed {
    phase: String,
}
// HTTP handlers use the real archive-I/O blocking worker. Thread-local hooks
// would miss that execution; only one exact ignored test runs in each child.
static ARMED: OnceLock<Mutex<Option<Armed>>> = OnceLock::new();
fn emit(value: Value) {
    let mut output = std::io::stdout().lock();
    writeln!(output, "\n{EVENT}{value}").unwrap();
    output.flush().unwrap();
}
pub(super) fn observe_publication(point: PublicationPoint, generation: u64) -> io::Result<()> {
    let phase = point.name();
    let reached = ARMED
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|a| a.phase == phase);
    if reached {
        emit(
            json!({"kind":"boundary", "phase":phase, "generation":generation,
            "pid":std::process::id(), "process_exit_only_not_power_loss":true}),
        );
        std::process::exit(86);
    }
    Ok(())
}
fn phase() -> String {
    let value = std::env::var("SWITCH_BACKUP_LIFECYCLE_PHASE").unwrap();
    assert!(
        [
            "before-generation",
            "after-generation",
            "before-intent",
            "after-intent",
            "before-temp",
            "after-temp-allocated",
            "after-temp-write",
            "after-temp-sync",
            "after-ready",
            "before-prepare",
            "after-prepare",
            "before-directory-rename",
            "after-directory-rename",
            "before-rename",
            "after-rename",
            "before-source-sync",
            "after-source-sync",
            "before-destination-sync",
            "after-destination-sync",
            "before-root-sync",
            "after-root-sync",
            "before-commit",
            "after-commit"
        ]
        .contains(&value.as_str())
    );
    value
}
fn root() -> PathBuf {
    let root = PathBuf::from(std::env::var_os("SWITCH_BACKUP_LIFECYCLE_ROOT").unwrap());
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime")
        .canonicalize()
        .unwrap();
    assert!(root.is_absolute() && root.starts_with(&runtime) && root != runtime);
    assert_eq!(root.canonicalize().unwrap(), root);
    let m = fs::symlink_metadata(&root).unwrap();
    assert!(m.is_dir() && !m.file_type().is_symlink());
    assert_eq!(m.mode() & 0o7777, 0o700);
    root
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    sha256: String,
    size: u64,
}
type Files = BTreeMap<String, Entry>;
fn files(root: &Path) -> Files {
    fn visit(root: &Path, path: &Path, out: &mut Files) {
        for row in fs::read_dir(path).unwrap() {
            let path = row.unwrap().path();
            let m = fs::symlink_metadata(&path).unwrap();
            assert!(!m.file_type().is_symlink());
            if m.is_dir() {
                visit(root, &path, out);
            } else {
                assert!(m.is_file());
                assert!(m.len() <= 1024 * 1024);
                let raw = fs::read(&path).unwrap();
                out.insert(
                    path.strip_prefix(root).unwrap().to_str().unwrap().into(),
                    Entry {
                        dev: m.dev(),
                        ino: m.ino(),
                        mode: m.mode(),
                        uid: m.uid(),
                        gid: m.gid(),
                        links: m.nlink(),
                        sha256: storage::digest(&raw),
                        size: raw.len() as u64,
                    },
                );
            }
            assert!(out.len() < 8192);
        }
    }
    let mut out = Files::new();
    visit(root, root, &mut out);
    out
}
fn historical(root: &Path) -> Files {
    files(root)
        .into_iter()
        .filter(|(path, _)| {
            path.starts_with("module-recovery-v1/")
                && !path.starts_with("module-recovery-v1/backup-lifecycle-v1/")
        })
        .collect()
}
fn assert_historical_extension(root: &Path, before: &Files, expect_anchor: bool) {
    let after = historical(root);
    for (name, entry) in before {
        assert_eq!(
            after.get(name),
            Some(entry),
            "original history changed: {name}"
        );
    }
    let held = File::open(root).unwrap();
    let anchors = storage::origins(&held)
        .unwrap()
        .into_iter()
        .filter(|value| value["kind"] == "backup_lifecycle")
        .collect::<Vec<_>>();
    assert_eq!(anchors.len(), if expect_anchor { 1 } else { 0 });
    let mut allowed_new = Vec::new();
    for anchor in anchors {
        let keys = anchor
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            [
                "schema",
                "publication",
                "receipt_sha256",
                "kind",
                "id",
                "sha256",
                "entry"
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(anchor["schema"], 1);
        assert_eq!(anchor["id"], NAME);
        assert_eq!(anchor["entry"], namespace_anchor(&held).unwrap());
        assert_eq!(
            anchor["sha256"],
            storage::digest(&serde_json::to_vec(&anchor["entry"]).unwrap())
        );
        // Bind the new anchor to the already genuinely published, unchanged
        // original receipt. The general ledger reader intentionally rejects
        // unprepared allocation gaps, so it cannot be used to waive Pending.
        let publication = anchor["publication"].as_str().unwrap();
        let prefix = format!("{}/{publication}/", storage::NAME);
        let (receipt_path, receipt) = before
            .iter()
            .filter(|(name, _)| {
                name.strip_prefix(prefix.as_str()).is_some_and(|part| {
                    part.len() == 11
                        && part.ends_with(".json")
                        && part.as_bytes()[..6].iter().all(u8::is_ascii_digit)
                })
            })
            .last()
            .expect("anchor must bind the pre-crash genuine completion receipt");
        assert_eq!(anchor["receipt_sha256"], receipt.sha256);
        let envelope: ledger::Envelope =
            serde_json::from_slice(&fs::read(root.join(receipt_path)).unwrap()).unwrap();
        assert!(matches!(
            envelope.event,
            ledger::Event::CompletionReceipt { .. }
        ));
        let raw = serde_json::to_vec(&anchor).unwrap();
        let sha = storage::digest(&raw);
        let name = format!("{}/origins/{sha}.json", storage::NAME);
        let actual = after.get(&name).expect("canonical anchor path missing");
        assert_eq!(actual.sha256, sha);
        assert_eq!(actual.links, 1);
        assert_eq!(actual.mode & 0o7777, 0o600);
        if !before.contains_key(&name) {
            allowed_new.push(name);
        }
    }
    for name in after.keys().filter(|name| !before.contains_key(*name)) {
        assert!(
            allowed_new.contains(name),
            "unrelated history extension: {name}"
        );
    }
    assert_eq!(after.len(), before.len() + allowed_new.len());
}
fn lifecycle(root: &Path) -> PathBuf {
    root.join(storage::NAME).join(NAME)
}
fn native_entries() -> usize {
    crate::usb_at_transport::recovery::reconcile::reconciliation_native_entry_count()
}
fn original(root: &Path) -> (String, Entry) {
    let all = files(root)
        .into_iter()
        .filter(|(p, _)| p.starts_with("module-backups/"))
        .collect::<Vec<_>>();
    assert_eq!(all.len(), 1);
    let (path, entry) = all.into_iter().next().unwrap();
    (path.strip_prefix("module-backups/").unwrap().into(), entry)
}
fn assert_one_inode(root: &Path, expected: &Entry, public: bool) {
    let matches = files(root)
        .into_iter()
        .filter(|(_, v)| (v.dev, v.ino) == (expected.dev, expected.ino))
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "original inode was lost or multiply linked"
    );
    assert_eq!(
        &matches[0].1, expected,
        "original raw/inode/mode/owner/link count changed"
    );
    assert_eq!(matches[0].0.starts_with("module-backups/"), public);
}
fn prefix_records(root: &Path) -> Files {
    files(root)
        .into_iter()
        .filter(|(p, _)| p.starts_with("module-recovery-v1/") && p.ends_with(".json"))
        .collect()
}
fn unchanged_prefix(root: &Path, before: &Files) {
    let after = files(root);
    for (name, entry) in before {
        assert_eq!(
            after.get(name),
            Some(entry),
            "old durable record changed: {name}"
        );
    }
}
fn completed_generation(root: &Path) -> PathBuf {
    let mut dirs = fs::read_dir(lifecycle(root))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    assert!(!dirs.is_empty());
    dirs.pop().unwrap()
}

#[tokio::test]
#[ignore = "owned child only; mandatory hardware/network sandbox via iteration-594-backup-lifecycle-crash-gate.py"]
async fn backup_lifecycle_crash_worker() {
    let root = root();
    let phase = phase();
    let variant = std::env::var("SWITCH_BACKUP_LIFECYCLE_VARIANT").unwrap();
    let operation = std::env::var("SWITCH_BACKUP_LIFECYCLE_OPERATION").unwrap();
    assert!(matches!(variant.as_str(), "some" | "none" | "fresh"));
    assert!(matches!(operation.as_str(), "delete" | "import"));
    let (lease, gate) = if variant == "fresh" {
        let lease = Lease::acquire(&root).unwrap();
        let gate = Gate::inspect_with_lifetime(&lease);
        (lease, gate)
    } else {
        super::super::tests::prepare_completed_backup_lifecycle(&root, variant == "some").await
    };
    // Mirror the real fresh daemon's production lifecycle initialization while
    // all eight evidence slots are still empty. Its actual constructor owns
    // pristine-marker creation; this child never manufactures that evidence.
    // Retain the real service/actor through the deliberate process-exit hook.
    let fresh_lifecycle = if variant == "fresh" {
        Some(
            crate::call_mode_backups::lifecycle_tests::initialize_fresh_call_mode_lifecycle(
                &root, &gate,
            ),
        )
    } else {
        None
    };
    assert!(!gate.is_quarantined());
    let old = if variant == "fresh" {
        None
    } else {
        Some(original(&root))
    };
    let history = historical(&root);
    let next_generation = scan(&File::open(&root).unwrap())
        .unwrap()
        .map_or(1, |inventory| inventory.transactions.len() as u64 + 1);
    if variant != "fresh" {
        assert!(!history.is_empty());
        let journals = fs::read_dir(root.join(storage::NAME))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.join("recovered-original/journal").is_dir())
            .collect::<Vec<_>>();
        assert_eq!(journals.len(), 1);
        let journal = crate::call_mode_restore_journal::inspect_readonly_directory(
            &File::open(journals[0].join("recovered-original/journal")).unwrap(),
        )
        .unwrap();
        assert_eq!(journal.summary.unknown_steps, 1);
        assert!(!journal.summary.receipt_present);
        assert_eq!(
            journal.intent.voice_recovery_binding.is_some(),
            variant == "some"
        );
        assert!(native_entries() > 0, "genuine full executor never reached");
    }
    emit(
        json!({"kind":"ready","pid":std::process::id(),"root":root,"phase":phase,
        "variant":variant,"operation":operation,"genuine_completed_recovery":variant!="fresh",
        "original_voice_some":variant=="some","actual_join_returned":variant!="fresh",
        "fresh_production_lifecycle_initialized":fresh_lifecycle.is_some(),
        "reconciliation_native_entries":native_entries(),"next_generation":next_generation,"historical":history,"original":old}),
    );
    *ARMED.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(Armed {
        phase: phase.clone(),
    });
    let proof = gate.root_proof_for(&root).unwrap();
    if operation == "delete" {
        assert!(variant != "fresh");
        crate::call_mode_backups::lifecycle_tests::process_delete(&root, proof, &old.unwrap().0)
            .await;
    } else {
        crate::call_mode_backups::lifecycle_tests::process_import(&root, proof).await;
    }
    // Reaching here is an unverified hook, never a successful crash scenario.
    // Still perform the actual service shutdown and actor close ACK before
    // releasing this lifetime; the actor API does not expose a thread join.
    if let Some(fresh_lifecycle) = fresh_lifecycle {
        fresh_lifecycle.shutdown().await;
    }
    drop((gate, lease));
    panic!("requested actual backup publication boundary not reached: {phase}");
}

fn inject_negative(root: &Path, case: &str, original: &(String, Entry)) {
    let source = root.join("module-backups").join(&original.0);
    let target = completed_generation(root).join("archive");
    assert!(source.is_file() && !target.exists());
    match case {
        "both-present" => fs::write(target, b"controlled foreign archive").unwrap(),
        "both-absent" => fs::rename(source, root.join("test-retained-original")).unwrap(),
        "foreign-destination" => {
            fs::rename(source, root.join("test-retained-original")).unwrap();
            fs::write(target, b"controlled foreign archive").unwrap();
        }
        "tamper" => {
            let mut f = OpenOptions::new().append(true).open(source).unwrap();
            f.write_all(b" ").unwrap();
            f.sync_all().unwrap();
        }
        _ => panic!("unsupported negative"),
    }
}
#[tokio::test]
#[ignore = "fresh-lifetime observer only; same exact owned iteration-594-backup-lifecycle-crash-gate.py child"]
async fn backup_lifecycle_restart_observer() {
    let root = root();
    let phase = phase();
    let baseline: Value =
        serde_json::from_str(&std::env::var("SWITCH_BACKUP_LIFECYCLE_BASELINE").unwrap()).unwrap();
    let history: Files = serde_json::from_value(baseline["historical"].clone()).unwrap();
    let old: Option<(String, Entry)> =
        serde_json::from_value(baseline["original"].clone()).unwrap();
    let negative = std::env::var("SWITCH_BACKUP_LIFECYCLE_NEGATIVE").unwrap_or_default();
    let operation = baseline["operation"].as_str().unwrap();
    assert_eq!(native_entries(), 0);
    if !negative.is_empty() {
        inject_negative(&root, &negative, old.as_ref().unwrap());
    }
    let before = files(&root);
    let prefix = prefix_records(&root);
    let blocked = !negative.is_empty()
        || matches!(
            phase.as_str(),
            "after-generation"
                | "before-intent"
                | "after-temp-allocated"
                | "after-temp-write"
                | "after-temp-sync"
        );
    let no_op = phase == "before-generation";
    let aborted = matches!(phase.as_str(), "after-intent" | "before-temp")
        || (phase == "before-prepare" && operation == "delete");
    let lease = Lease::acquire(&root).unwrap();
    let resumed = resume(&root, &lease.root_proof());
    let gate = Gate::inspect_with_lifetime(&lease);
    assert_eq!(native_entries(), 0);
    assert_historical_extension(&root, &history, baseline["variant"] != "fresh");
    unchanged_prefix(&root, &prefix);
    if blocked {
        assert!(resumed.is_err());
        assert!(gate.is_quarantined());
        assert_eq!(files(&root), before);
    } else {
        resumed.unwrap();
        assert!(!gate.is_quarantined());
        if !no_op {
            let dir = completed_generation(&root);
            assert_eq!(dir.join("abort.json").exists(), aborted);
            assert_eq!(dir.join("commit.json").exists(), !aborted);
            if !aborted {
                let intent: Value =
                    serde_json::from_slice(&fs::read(dir.join("intent.json")).unwrap()).unwrap();
                let id = intent["id"].as_str().unwrap();
                assert_eq!(
                    root.join("module-backups").join(id).is_file(),
                    operation != "delete"
                );
                assert_eq!(dir.join("archive").is_file(), operation == "delete");
                if operation == "import" {
                    let expected: Vec<u8> =
                        serde_json::from_value(intent["payload"].clone()).unwrap();
                    assert_eq!(
                        fs::read(root.join("module-backups").join(id)).unwrap(),
                        expected
                    );
                }
            }
        }
        if let Some((_, entry)) = &old {
            assert_one_inode(&root, entry, operation != "delete" || no_op || aborted);
        }
    }
    if !blocked {
        // The first fresh metadata resume must leave normal real handlers usable.
        // These are new typed transactions, never a replay of the crashed one.
        let proof = gate.root_proof_for(&root).unwrap();
        let imported =
            crate::call_mode_backups::lifecycle_tests::process_import(&root, proof.clone()).await;
        crate::call_mode_backups::lifecycle_tests::process_delete(&root, proof, &imported).await;
        assert!(!root.join("module-backups").join(imported).exists());
        assert_historical_extension(&root, &history, baseline["variant"] != "fresh");
        unchanged_prefix(&root, &prefix);
        if let Some((_, entry)) = &old {
            assert_one_inode(&root, entry, operation != "delete" || no_op || aborted);
        }
    }
    let after = files(&root);
    drop(gate);
    drop(lease);
    let lease = Lease::acquire(&root).unwrap();
    let second = resume(&root, &lease.root_proof());
    let gate = Gate::inspect_with_lifetime(&lease);
    if blocked {
        assert!(second.is_err());
        assert!(gate.is_quarantined());
    } else {
        second.unwrap();
        assert!(!gate.is_quarantined());
    }
    assert_eq!(files(&root), after);
    assert_eq!(native_entries(), 0);
    emit(
        json!({"kind":"restart","pid":std::process::id(),"phase":phase,"negative":negative,
        "pending":blocked,"aborted":aborted&&!blocked,"no_op":no_op&&!blocked,
        "metadata_resume_only":true,"reconciliation_native_entries":0,"historical_raw_inodes_unchanged":true,
        "prefix_raw_inodes_unchanged":true,"second_lifetime_same_result":true,"no_overwrite":true,
        "actual_public_path_and_archive_checked":!blocked,"post_restart_import_delete_verified":!blocked,"controlled_tamper_not_original_preservation_claim":!negative.is_empty()}),
    );
}
