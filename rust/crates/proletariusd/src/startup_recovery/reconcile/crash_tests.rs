//! Owned-process publication crashes. No hook can manufacture a successful
//! record, native result, scope, resource release, or receipt. Release builds
//! contain neither this module nor its callers.
use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

const EVENT: &str = "SWITCH_PUBLICATION_EVENT ";
const EXIT_CODE: i32 = 86;

#[derive(Clone, Copy, Debug)]
pub(super) enum PublicationPoint {
    BeforePrepare,
    AfterPrepare,
    BeforeRename,
    AfterRename,
    BeforeArchiveSync,
    AfterArchiveSync,
    BeforeAttemptSync,
    AfterAttemptSync,
    BeforeNamespaceSync,
    AfterNamespaceSync,
    BeforeRootSync,
    AfterRootSync,
    BeforePlacement,
    AfterPlacement,
    BeforeManifest,
    AfterManifest,
    BeforePublication,
    AfterPublication,
    BeforeReceipt,
    AfterReceipt,
}
impl PublicationPoint {
    fn name(self) -> &'static str {
        match self {
            Self::BeforePrepare => "before-prepare",
            Self::AfterPrepare => "after-prepare",
            Self::BeforeRename => "before-rename",
            Self::AfterRename => "after-rename",
            Self::BeforeArchiveSync => "before-archive-sync",
            Self::AfterArchiveSync => "after-archive-sync",
            Self::BeforeAttemptSync => "before-attempt-sync",
            Self::AfterAttemptSync => "after-attempt-sync",
            Self::BeforeNamespaceSync => "before-namespace-sync",
            Self::AfterNamespaceSync => "after-namespace-sync",
            Self::BeforeRootSync => "before-root-sync",
            Self::AfterRootSync => "after-root-sync",
            Self::BeforePlacement => "before-placement",
            Self::AfterPlacement => "after-placement",
            Self::BeforeManifest => "before-manifest",
            Self::AfterManifest => "after-manifest",
            Self::BeforePublication => "before-publication",
            Self::AfterPublication => "after-publication",
            Self::BeforeReceipt => "before-receipt",
            Self::AfterReceipt => "after-receipt",
        }
    }
}
struct Armed {
    phase: String,
    fail_only: bool,
}
thread_local! { static ARMED: RefCell<Option<Armed>> = const { RefCell::new(None) }; }
fn emit(value: serde_json::Value) {
    let mut output = std::io::stdout().lock();
    writeln!(output, "\n{EVENT}{value}").unwrap();
    output.flush().unwrap();
}
pub(super) fn observe_publication(point: PublicationPoint, slot: &str) -> io::Result<()> {
    let phase = format!("{slot}:{}", point.name());
    let action = ARMED.with(|armed| {
        armed
            .borrow()
            .as_ref()
            .filter(|a| a.phase == phase)
            .map(|a| a.fail_only)
    });
    if let Some(fail_only) = action {
        emit(serde_json::json!({"kind":"boundary", "phase":phase,
            "pid":std::process::id(), "process_exit_only_not_power_loss":true}));
        if fail_only {
            return Err(io::Error::other(
                "publication observation requested failure",
            ));
        }
        std::process::exit(EXIT_CODE);
    }
    Ok(())
}
fn phase() -> String {
    let value = std::env::var("SWITCH_PUBLICATION_PHASE").unwrap();
    let (slot, point) = value.split_once(':').unwrap();
    let slot_points = [
        "before-prepare",
        "after-prepare",
        "before-rename",
        "after-rename",
        "before-archive-sync",
        "after-archive-sync",
        "before-attempt-sync",
        "after-attempt-sync",
        "before-namespace-sync",
        "after-namespace-sync",
        "before-root-sync",
        "after-root-sync",
        "before-placement",
        "after-placement",
    ];
    let publication_points = [
        "before-manifest",
        "after-manifest",
        "before-publication",
        "after-publication",
        "before-receipt",
        "after-receipt",
    ];
    assert!(
        (matches!(slot, "journal" | "voice-control") && slot_points.contains(&point))
            || (slot == "publication" && publication_points.contains(&point))
    );
    value
}
fn root() -> PathBuf {
    let root = PathBuf::from(std::env::var_os("SWITCH_PUBLICATION_ROOT").unwrap());
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime")
        .canonicalize()
        .unwrap();
    assert!(root.is_absolute() && root.starts_with(&runtime) && root != runtime);
    assert_eq!(root.canonicalize().unwrap(), root);
    assert!(root.is_dir() && !root.is_symlink());
    assert_eq!(fs::metadata(&root).unwrap().mode() & 0o7777, 0o700);
    root
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    file_sha256: Option<String>,
    file_size: Option<u64>,
}
type Tree = BTreeMap<String, Entry>;
fn tree(root: &Path) -> Tree {
    fn visit(path: &Path, name: String, output: &mut Tree) {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert!(!metadata.file_type().is_symlink());
        assert!(metadata.is_dir() || metadata.is_file());
        let raw = metadata.is_file().then(|| fs::read(path).unwrap());
        output.insert(
            name.clone(),
            Entry {
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                file_sha256: raw.as_ref().map(|b| storage::digest(b)),
                file_size: raw.as_ref().map(|b| b.len() as u64),
            },
        );
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                let part = child.file_name().unwrap().to_str().unwrap();
                visit(
                    &child,
                    if name.is_empty() {
                        part.into()
                    } else {
                        format!("{name}/{part}")
                    },
                    output,
                );
            }
        }
    }
    let mut output = Tree::new();
    visit(root, String::new(), &mut output);
    output
}
fn attempt(root: &Path) -> PathBuf {
    let typed = storage::attempts(&File::open(root).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(typed.len(), 1);
    let mut names = fs::read_dir(root.join(storage::NAME))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|path| {
            path.file_name().unwrap().to_str().is_some_and(|name| {
                name.len() == 64
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        })
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names.len(), 1);
    names.pop().unwrap()
}
fn records(root: &Path) -> Vec<(String, Vec<u8>, u64)> {
    let mut rows = fs::read_dir(attempt(root))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name().unwrap().to_str().unwrap().len() == 11
                && p.extension().is_some_and(|e| e == "json")
        })
        .map(|p| {
            (
                p.file_name().unwrap().to_str().unwrap().into(),
                fs::read(&p).unwrap(),
                fs::metadata(p).unwrap().ino(),
            )
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}
fn event_counts(root: &Path) -> BTreeMap<String, usize> {
    let mut result = BTreeMap::new();
    for (_, bytes, _) in records(root) {
        let row: ledger::Envelope = serde_json::from_slice(&bytes).unwrap();
        let key = match row.event {
            ledger::Event::RetirementPrepare { slot, .. } => format!("prepare:{slot}"),
            ledger::Event::PlacementCommitted { slot, .. } => format!("placement:{slot}"),
            ledger::Event::PhysicalSettled => "physical".into(),
            ledger::Event::PublicationCommitted { .. } => "publication".into(),
            ledger::Event::CompletionReceipt { .. } => "receipt".into(),
            _ => continue,
        };
        *result.entry(key).or_insert(0) += 1;
    }
    result
}
fn native_entries() -> usize {
    crate::usb_at_transport::recovery::reconcile::reconciliation_native_entry_count()
}

#[tokio::test]
#[ignore = "owned child only; run iteration-593-publication-crash-gate.py under mandatory hardware/network sandbox"]
async fn publication_crash_worker() {
    let root = root();
    let phase = phase();
    let (lease, gate, settled) = super::tests::prepare_publication_crash(&root).await;
    let original = crate::call_mode_restore_journal::inspect_readonly_directory(
        &File::open(root.join("call-mode-restore.active")).unwrap(),
    )
    .unwrap();
    assert!(original.intent.voice_recovery_binding.is_some());
    assert_eq!(original.summary.unknown_steps, 1);
    assert!(!original.summary.receipt_present);
    let baseline = serde_json::json!({"journal":tree(&root.join("call-mode-restore.active")),
        "voice-control":tree(&root.join("voice-control"))});
    emit(
        serde_json::json!({"kind":"ready", "pid":std::process::id(), "phase":phase,
        "root":root, "original_voice_some":true, "baseline":baseline,
        "actual_reconciliation_native_entries":native_entries(), "actual_join_returned":true}),
    );
    ARMED.with(|armed| {
        *armed.borrow_mut() = Some(Armed {
            phase: phase.clone(),
            fail_only: false,
        })
    });
    let result = settled.finalize();
    drop((lease, gate));
    panic!(
        "requested publication boundary was not reached: {phase}, result {}",
        result.is_ok()
    );
}

fn inject_negative(root: &Path, case: &str) {
    let (slot, mode) = case.split_once(':').unwrap();
    let (live, archive_name) = match slot {
        "journal" => ("call-mode-restore.active", "journal"),
        "voice-control" => ("voice-control", "voice-control"),
        _ => panic!("unsupported negative slot"),
    };
    let archive = attempt(root).join("recovered-original").join(archive_name);
    let source = root.join(live);
    match mode {
        "both-present" => {
            assert!(source.is_dir() && !archive.exists());
            fs::create_dir(&archive).unwrap();
            fs::set_permissions(&archive, fs::Permissions::from_mode(0o700)).unwrap();
        }
        "both-absent" => {
            assert!(source.is_dir() && !archive.exists());
            fs::rename(&source, root.join(format!("test-retained-{slot}"))).unwrap();
        }
        "foreign-destination" => {
            assert!(source.is_dir() && !archive.exists());
            fs::rename(&source, root.join(format!("test-retained-{slot}"))).unwrap();
            fs::create_dir(&archive).unwrap();
            fs::set_permissions(&archive, fs::Permissions::from_mode(0o700)).unwrap();
        }
        "tamper" => {
            let file = fs::read_dir(&source)
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.is_file())
                .unwrap();
            let mut output = OpenOptions::new().append(true).open(file).unwrap();
            output.write_all(b" ").unwrap();
            output.sync_all().unwrap();
        }
        _ => panic!("unsupported negative mode"),
    }
}
#[tokio::test]
#[ignore = "independent fresh-lifetime observer only; run iteration-593-publication-crash-gate.py after its exact owned crash child"]
async fn publication_restart_observer() {
    let root = root();
    let phase = phase();
    let baseline: serde_json::Value =
        serde_json::from_str(&std::env::var("SWITCH_PUBLICATION_BASELINE").unwrap()).unwrap();
    let negative = std::env::var("SWITCH_PUBLICATION_NEGATIVE").unwrap_or_default();
    assert_eq!(native_entries(), 0);
    if !negative.is_empty() {
        inject_negative(&root, &negative);
    }
    let before = tree(&root);
    let prefix = records(&root);
    let lease = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
    let gate = Gate::inspect_with_lifetime(&lease);
    let completed =
        Coordinator::reconcile_with_cancel(&gate, &root, Arc::new(AtomicBool::new(false))).await;
    if !negative.is_empty() {
        assert!(completed.is_err());
        assert!(gate.is_quarantined());
        assert_eq!(native_entries(), 0);
        assert_eq!(tree(&root), before);
        emit(
            serde_json::json!({"kind":"restart", "pid":std::process::id(), "phase":phase,
            "negative":negative, "pending":true, "no_overwrite":true,
            "reconciliation_native_entries":0, "controlled_tamper_not_original_preservation_claim":true}),
        );
        return;
    }
    if phase == "publication:after-receipt" {
        assert!(completed.unwrap().is_none());
        assert!(!gate.is_quarantined());
    } else {
        let completed = completed.unwrap().unwrap();
        gate.consume_reconciliation(&completed).unwrap();
        assert!(!gate.is_quarantined());
    }
    assert_eq!(native_entries(), 0);
    let after = records(&root);
    assert!(after.starts_with(&prefix));
    let archive = attempt(&root).join("recovered-original");
    for slot in ["journal", "voice-control"] {
        let actual = serde_json::to_value(tree(&archive.join(slot))).unwrap();
        assert_eq!(actual, baseline[slot]);
    }
    let historical = crate::call_mode_restore_journal::inspect_readonly_directory(
        &File::open(archive.join("journal")).unwrap(),
    )
    .unwrap();
    assert_eq!(historical.summary.unknown_steps, 1);
    assert!(!historical.summary.receipt_present);
    assert!(historical.intent.voice_recovery_binding.is_some());
    assert!(!root.join("call-mode-restore.active").exists());
    assert!(!root.join("voice-control").exists());
    let counts = event_counts(&root);
    for key in [
        "physical",
        "prepare:call-mode-restore.active",
        "prepare:voice-control",
        "placement:call-mode-restore.active",
        "placement:voice-control",
        "publication",
        "receipt",
    ] {
        assert_eq!(counts.get(key), Some(&1), "not exactly once: {key}");
    }
    drop(gate);
    drop(lease);
    let lease = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
    let gate = Gate::inspect_with_lifetime(&lease);
    assert!(!gate.is_quarantined());
    assert_eq!(native_entries(), 0);
    assert_eq!(records(&root), after);
    emit(
        serde_json::json!({"kind":"restart", "pid":std::process::id(), "phase":phase,
        "negative":"", "metadata_resume_only":true, "reconciliation_native_entries":0,
        "prefix_raw_inodes_unchanged":true, "both_original_trees_raw_inodes_preserved":true,
        "events_exactly_once":true, "original_unknown1_not_rewritten":true, "fresh_gate_activated":true, "second_lifetime_accepted":true}),
    );
}
