// Real child processes use the production lifecycle, journal, owner and byte
// transport. Only physical USB transfers are scripted. Synthetic targets are
// ownership fixtures and cannot enter native USB discovery/re-enumeration.
use super::*;
use crate::availability::BackendAvailability;
use crate::call_mode_restore_journal::{self as journal, DiagnosticSummary, TestPhase};
use crate::usb_at_transport::{NativeTransport, PhysicalTarget, Transport};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;

const PRODUCER: &str = "restore-journal-child";
const IMEI: &str = "123456789012345";
const EVENT: &str = "SWITCH_RESTORE_JOURNAL_EVENT ";

#[derive(Default)]
struct Wire {
    reply: VecDeque<u8>,
    commands: Vec<String>,
    writes: usize,
    ims: u8,
    disabled: u8,
}
fn transport(target: PhysicalTarget, wire: Arc<StdMutex<Wire>>) -> Box<dyn Transport> {
    let reader = wire.clone();
    Box::new(NativeTransport::scripted_for_tests(
        target,
        Box::new(move |output, _| {
            let mut wire = reader.lock().unwrap();
            if wire.reply.is_empty() {
                return Err(());
            }
            let length = output.len().min(wire.reply.len());
            for byte in &mut output[..length] {
                *byte = wire.reply.pop_front().unwrap();
            }
            Ok(length)
        }),
        Box::new(move |bytes, _| {
            let command = std::str::from_utf8(bytes)
                .unwrap()
                .strip_suffix('\r')
                .unwrap();
            let mut wire = wire.lock().unwrap();
            assert!(
                wire.reply.is_empty(),
                "new payload before previous framed reply drained"
            );
            wire.commands.push(command.into());
            let reply = match command {
                "AT+CGSN" => format!("{IMEI}\r\nOK\r\n"),
                "ATI" => "journal-child firmware\r\nOK\r\n".into(),
                "AT+CLCC" => "\r\nOK\r\n".into(),
                // The target disables ADB; status still performs its bounded
                // read-only capability query. Unsupported is a real reply,
                // never an authorization or a fabricated successful probe.
                "AT+QADBKEY?" => "\r\nERROR\r\n".into(),
                "AT+CFUN?" => "+CFUN: 1\r\nOK\r\n".into(),
                "AT+QCFG=\"USBCFG\"" | "AT+QCFG=\"usbcfg\"" => {
                    "+QCFG: \"usbcfg\",0x2C7C,0x2400,1,0,1,0,1,0,0\r\nOK\r\n".into()
                }
                "AT+QCFG=\"ims\"" => format!("+QCFG: \"ims\",{},1\r\nOK\r\n", wire.ims),
                "AT+QCFG=\"volte_disable\"" => {
                    format!("+QCFG: \"volte_disable\",{}\r\nOK\r\n", wire.disabled)
                }
                "AT+QCFG=\"ims\",1" => {
                    wire.writes += 1;
                    wire.ims = 1;
                    "\r\nOK\r\n".into()
                }
                "AT+QCFG=\"volte_disable\",0" => {
                    wire.writes += 1;
                    wire.disabled = 0;
                    "\r\nOK\r\n".into()
                }
                "AT+CFUN=1,1" => {
                    wire.writes += 1;
                    "\r\nOK\r\n".into()
                }
                other => panic!("unplanned physical payload in restore child: {other}"),
            };
            wire.reply.extend(reply.bytes());
            Ok(bytes.len())
        }),
    ))
}

fn root_from_environment() -> PathBuf {
    let path = PathBuf::from(std::env::var_os("SWITCH_RESTORE_JOURNAL_TEST_ROOT").unwrap());
    let path = path.canonicalize().unwrap();
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .join("build/runtime")
        .canonicalize()
        .unwrap();
    assert!(path.starts_with(&runtime) && path != runtime);
    path
}
fn phase_from_environment() -> String {
    let phase = std::env::var("SWITCH_RESTORE_JOURNAL_TEST_PHASE").unwrap();
    assert!(matches!(
        phase.as_str(),
        "intent"
            | "prepared_before_io"
            | "after_write_before_terminal"
            | "second_voice"
            | "verified_before_release"
            | "released_before_receipt"
            | "receipt_before_archive"
            | "archive_before_publish"
    ));
    phase
}
fn emit(value: serde_json::Value) {
    let mut output = std::io::stdout().lock();
    writeln!(output, "\n{EVENT}{value}").unwrap();
    output.flush().unwrap();
}
fn selected(phase: &str, actual: TestPhase, summary: &DiagnosticSummary, writes: usize) -> bool {
    match phase {
        "intent" => actual == TestPhase::Intent,
        "prepared_before_io" => {
            actual == TestPhase::Prepared && summary.prepared_steps == 1 && writes == 0
        }
        "after_write_before_terminal" => actual == TestPhase::BeforeOutcomePersist && writes == 1,
        "second_voice" => {
            actual == TestPhase::Prepared && summary.prepared_steps == 2 && writes == 1
        }
        "verified_before_release" => actual == TestPhase::TargetVerified,
        "released_before_receipt" => actual == TestPhase::Released,
        "receipt_before_archive" => actual == TestPhase::Receipt,
        "archive_before_publish" => actual == TestPhase::Archived,
        _ => false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "owned child only; run through check-restore-journal-crash.py in its mandatory sandbox"]
async fn crash_worker() {
    let root = root_from_environment();
    let phase = phase_from_environment();
    let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
    let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
    assert!(!gate.is_admission_revoked());
    let wire = Arc::new(StdMutex::new(Wire {
        disabled: 1,
        ..Wire::default()
    }));
    let source = PhysicalTarget::synthetic_for_tests();
    let factory_wire = wire.clone();
    let factory_source = source.clone();
    let usb = crate::usb_at::Owner::with_factory(
        PRODUCER.into(),
        Arc::new(move || Ok(transport(factory_source.clone(), factory_wire.clone()))),
    )
    .with_recovery_gate_for_tests(gate.clone());
    usb.claim_native_management().unwrap();
    let session = usb.open_native_session().await.unwrap();
    let successor_wire = wire.clone();
    let original_source = source.clone();
    crate::usb_at::reconnect_tests::set_opener(
        &usb,
        Arc::new(move |target, anchor, _, guard| {
            assert!(target.same_connection(&original_source));
            assert!(anchor.unwrap().same_source(target));
            assert!(guard());
            Ok(transport(
                PhysicalTarget::synthetic_for_tests(),
                successor_wire.clone(),
            ))
        }),
    );
    let modem = crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap();
    // This isolated fixture explicitly permits its scripted RF/reboot path;
    // default unconfigured preferences intentionally suppress such commands.
    crate::module_radio_store::Store::new(root.clone())
        .save(crate::module_radio_store::Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        })
        .unwrap();
    let radio = Radio::new(
        root.clone(),
        usb.clone(),
        modem.clone(),
        BackendAvailability::with_module_recovery(gate),
        PRODUCER.into(),
    );
    radio.configure_native_usb(&session).await.unwrap();
    let service = Service::new(
        &root,
        radio,
        crate::message_archive::InboxState::for_go_producer(PRODUCER.into()),
        usb,
    );
    let id = crate::call_mode_backups::create_native_backup(
        &root,
        crate::module_radio_transport::CallModeModuleIdentity {
            imei: IMEI.into(),
            firmware: "journal-child firmware".into(),
        },
        UsbComposition {
            vendor_id: 0x2c7c,
            product_id: 0x2400,
            flags: [1, 0, 1, 0, 1, 0, 0],
        },
        VoiceConfiguration {
            ims: 1,
            volte_capability: 1,
            volte_disabled: 0,
        },
        false,
        &lifetime.root_proof(),
    )
    .await
    .unwrap();
    let hook_service = service.clone();
    let hook_root = root.clone();
    let hook_wire = wire.clone();
    let hook_phase = phase.clone();
    let _hook = journal::set_test_hook(&root, Arc::new(move |actual, summary| {
        let writes = hook_wire.lock().unwrap().writes;
        if !selected(&hook_phase, actual, summary, writes) {
            return;
        }
        emit(json!({"kind":"boundary", "source":"production_restore_journal",
            "pid":std::process::id(), "phase":hook_phase, "run_id":summary.run_id,
            "sequence":summary.sequence, "prepared_steps":summary.prepared_steps,
            "unknown_steps":summary.unknown_steps, "physical_writes":writes,
            "verified":summary.target_verified, "released":summary.released,
            "receipt_present":summary.receipt_present, "archived":actual == TestPhase::Archived,
            "active_present":std::fs::symlink_metadata(hook_root.join(journal::ACTIVE_NAME)).is_ok(),
            "success_published":hook_service.0.state.snapshot().last_restore.is_some()}));
        // The parent kills this exact owned PID. A broken parent cannot leave
        // a permanently parked worker or let this boundary silently continue.
        std::thread::park_timeout(Duration::from_secs(60));
        panic!("parent did not SIGKILL the acknowledged restore boundary");
    })).unwrap();
    let reply = service.start_restore(id).await;
    assert_eq!(reply.status(), StatusCode::ACCEPTED);
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            tokio::time::sleep(Duration::from_millis(10)).await;
            assert!(
                service.0.state.running(),
                "restore finished/failed before the requested crash boundary: {:?}",
                service.0.state.snapshot()
            );
        }
    })
    .await
    .expect("requested real restore boundary was not reached");
}

// A bounded, nofollow fingerprint of only this fixture's real evidence. It
// cannot turn an invalid journal into success; the production scanner below
// independently validates the complete durable chain first.
fn evidence_fingerprint(root: &std::fs::File) -> Vec<serde_json::Value> {
    use nix::fcntl::{AtFlags, OFlag, openat};
    use nix::sys::stat::{Mode, fstatat};
    use sha2::{Digest, Sha256};
    fn visit(
        parent: &std::fs::File,
        name: &std::ffi::OsStr,
        path: &str,
        rows: &mut Vec<serde_json::Value>,
        depth: usize,
        remaining: &mut u64,
    ) {
        assert!(depth <= 4 && rows.len() < 512);
        let named = match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(named) => named,
            Err(nix::errno::Errno::ENOENT) => return,
            Err(error) => panic!("evidence lookup failed: {error}"),
        };
        let kind = named.st_mode as u32 & nix::libc::S_IFMT as u32;
        let directory = kind == nix::libc::S_IFDIR as u32;
        assert!(directory || kind == nix::libc::S_IFREG as u32);
        let flags = OFlag::O_RDONLY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK
            | if directory {
                OFlag::O_DIRECTORY
            } else {
                OFlag::empty()
            };
        let mut file = std::fs::File::from(openat(parent, name, flags, Mode::empty()).unwrap());
        let before = file.metadata().unwrap();
        assert_eq!(
            (before.dev(), before.ino()),
            (named.st_dev as u64, named.st_ino as u64)
        );
        let digest = if directory {
            String::new()
        } else {
            assert!(before.len() <= *remaining);
            *remaining -= before.len();
            let mut bytes = Vec::new();
            (&mut file)
                .take(before.len() + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes.len() as u64, before.len());
            format!("{:x}", Sha256::digest(bytes))
        };
        rows.push(
            json!({"path":path, "device":before.dev(), "inode":before.ino(),
            "mode":before.mode(), "links":before.nlink(), "length":before.len(), "sha256":digest}),
        );
        if directory {
            let mut names = proletarius_platform_fs::directory_names(&file, 256).unwrap();
            names.sort(); // Comparison order only, never selection/authorization.
            for child in names {
                let child_path = format!("{path}/{}", child.to_str().unwrap());
                visit(&file, &child, &child_path, rows, depth + 1, remaining);
            }
        }
        let after = file.metadata().unwrap();
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.mode(),
                before.len(),
                before.nlink(),
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec()
            ),
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.len(),
                after.nlink(),
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec()
            )
        );
    }
    let mut rows = Vec::new();
    let mut remaining = 16 * 1024 * 1024;
    for name in [
        journal::ACTIVE_NAME,
        journal::ARCHIVE_NAME,
        crate::module_recovery::LEGACY_EXECUTION_PENDING,
        "module-backups",
    ] {
        visit(
            root,
            std::ffi::OsStr::new(name),
            name,
            &mut rows,
            0,
            &mut remaining,
        );
    }
    rows
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "owned independent restart only; run through check-restore-journal-crash.py"]
async fn restart_observer() {
    let root = root_from_environment();
    let phase = phase_from_environment();
    let run_id = std::env::var("SWITCH_RESTORE_JOURNAL_TEST_RUN_ID").unwrap();
    let lifetime = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
    let proof = lifetime.root_proof();
    let (summary, archived, before) = proof
        .with_root(&root, |held| {
            let active = journal::diagnostic_summary(held).unwrap();
            let archived = active.is_none();
            let summary = active
                .or_else(|| journal::diagnostic_archived_summary(held, &run_id).unwrap())
                .unwrap();
            assert_eq!(summary.run_id, run_id);
            (summary, archived, evidence_fingerprint(held))
        })
        .unwrap();
    let gate = crate::module_recovery::Gate::inspect_with_lifetime(&lifetime);
    assert!(gate.is_quarantined() && gate.is_admission_revoked());
    assert!(gate.admit_operation(PRODUCER).is_err());
    let reasons = gate.status().unwrap().clone();
    let factories = Arc::new(AtomicUsize::new(0));
    let calls = factories.clone();
    let usb = crate::usb_at::Owner::with_factory(
        PRODUCER.into(),
        Arc::new(move || {
            calls.fetch_add(1, Ordering::AcqRel);
            panic!("startup quarantine must reject before any physical factory")
        }),
    )
    .with_recovery_gate_for_tests(gate.clone());
    let modem = crate::modem_actor::ModemActor::new_with_module_recovery(gate.clone()).unwrap();
    let radio = Radio::new(
        root.clone(),
        usb.clone(),
        modem.clone(),
        BackendAvailability::with_module_recovery(gate),
        PRODUCER.into(),
    );
    let service = Service::new(
        &root,
        radio,
        crate::message_archive::InboxState::for_go_producer(PRODUCER.into()),
        usb.clone(),
    );
    assert_eq!(
        service
            .0
            .radio
            .reserve_call_mode_operation()
            .await
            .unwrap_err()
            .code,
        crate::module_recovery::CODE
    );
    assert_eq!(
        usb.claim_native_management().unwrap_err().code,
        crate::module_recovery::CODE
    );
    assert!(usb.open_native_session().await.is_err());
    assert!(service.0.state.snapshot().last_restore.is_none());
    usb.shutdown().await.unwrap();
    service.0.radio.begin_shutdown();
    modem.shutdown().unwrap();
    let after = proof.with_root(&root, evidence_fingerprint).unwrap();
    assert_eq!(
        before, after,
        "restart must not rewrite, replay, archive or delete evidence"
    );
    assert_eq!(factories.load(Ordering::Acquire), 0);
    emit(
        json!({"kind":"restart", "pid":std::process::id(), "phase":phase,
        "run_id":summary.run_id, "sequence":summary.sequence, "archived":archived,
        "ordinary_admission_denied":true, "historical_recovery_confirmed":reasons.historical_recovery_confirmed,
        "reason_codes":reasons.reason_codes, "physical_writes":0,
        "factory_calls":factories.load(Ordering::Acquire), "evidence_unchanged":before == after}),
    );
}
