//! Offline native storage compatibility and physical binding regressions.
use super::*;
use crate::module_radio_transport::{
    CallModeModuleIdentity, UsbComposition as NativeUsb, VoiceConfiguration as NativeVoice,
};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};
struct Root(PathBuf, Option<crate::daemon_lifetime::Lease>);
impl Root {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap();
        let root = workspace
            .join("build/native-call-mode-backup-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        let lease = crate::daemon_lifetime::Lease::acquire(&root).unwrap();
        Self(root, Some(lease))
    }
    fn proof(&self) -> crate::daemon_lifetime::RootProof {
        self.1.as_ref().unwrap().root_proof()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn identity() -> CallModeModuleIdentity {
    CallModeModuleIdentity {
        imei: "123456789012345".into(),
        firmware: "fixture-firmware".into(),
    }
}
fn usb() -> NativeUsb {
    NativeUsb {
        vendor_id: 0x2c7c,
        product_id: 0x2400,
        flags: [1, 0, 1, 0, 1, 0, 0],
    }
}
fn voice() -> NativeVoice {
    NativeVoice {
        ims: 2,
        volte_capability: 1,
        volte_disabled: 1,
    }
}
fn command() -> &'static str {
    "AT+QCFG=\"USBCFG\",0x2C7C,0x2400,1,0,1,0,1,0,0"
}

fn root_entries(root: &Path) -> Vec<std::ffi::OsString> {
    let mut entries: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    entries
}

#[tokio::test]
async fn native_creation_rejects_foreign_root_proof_without_creating_any_backup_or_temp() {
    for before_restore in [false, true] {
        let root = Root::new();
        let foreign = Root::new();
        let before = root_entries(&root.0);
        let foreign_before = root_entries(&foreign.0);
        let result = create_native_backup(
            &root.0,
            identity(),
            usb(),
            voice(),
            before_restore,
            &foreign.proof(),
        )
        .await;
        assert!(
            matches!(result, Err(error) if error.starts_with("模块备份根目录授权已失效或不匹配："))
        );
        assert_eq!(root_entries(&root.0), before);
        assert_eq!(root_entries(&foreign.0), foreign_before);
        assert!(!backup_directory(&root.0).exists());
        assert!(!backup_directory(&foreign.0).exists());
    }
}

#[tokio::test]
async fn native_creation_rejects_dropped_authority_even_after_same_path_reacquisition() {
    let mut root = Root::new();
    let proof = root.proof();
    let before = root_entries(&root.0);
    drop(root.1.take());
    for reacquired in [false, true] {
        if reacquired {
            root.1 = Some(crate::daemon_lifetime::Lease::acquire(&root.0).unwrap());
        }
        let result = create_native_backup(&root.0, identity(), usb(), voice(), true, &proof).await;
        assert!(
            matches!(result, Err(error) if error.starts_with("模块备份根目录授权已失效或不匹配："))
        );
        assert_eq!(root_entries(&root.0), before);
        assert!(!backup_directory(&root.0).exists());
    }
}

#[tokio::test]
async fn captured_restore_preserves_original_bytes_schema_and_configuration() {
    for schema in [2, 3] {
        let root = Root::new();
        let id = create_native_backup(&root.0, identity(), usb(), voice(), false, &root.proof())
            .await
            .unwrap();
        let path = backup_directory(&root.0).join(&id);
        if schema == 2 {
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            value["schema_version"] = json!(2);
            value.as_object_mut().unwrap().remove("voice");
            value["module"].as_object_mut().unwrap().remove("firmware");
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let original = fs::read(&path).unwrap();
        let captured = capture_native_backup(&root.0, &id, &root.proof())
            .await
            .unwrap();
        assert_eq!(captured.id(), id);
        assert_eq!(captured.digest, backup_digest(&original));
        assert_eq!(captured.sha256().len(), 64);
        assert_eq!(captured.backup().module.imei, identity().imei);
        assert_eq!(captured.backup().usb, usb());
        assert_eq!(captured.backup().voice, (schema == 3).then(voice));
        assert_eq!(
            captured.backup().module.firmware,
            if schema == 2 { "" } else { "fixture-firmware" }
        );
        captured.verify_unchanged().await.unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        if schema == 2 {
            assert_tampered_lifecycle_blocks_new_writer(&root).await;
        }
    }
}

#[tokio::test]
async fn previous_schema_never_promotes_extra_voice_fields_to_restore_targets() {
    for voice_value in [
        json!({"ims":0,"volte_capability":0,"volte_disabled":1}),
        json!({"ims":-99,"volte_capability":77,"volte_disabled":42}),
    ] {
        let root = Root::new();
        let id = create_native_backup(&root.0, identity(), usb(), voice(), false, &root.proof())
            .await
            .unwrap();
        let path = backup_directory(&root.0).join(&id);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["schema_version"] = json!(2);
        value["voice"] = voice_value;
        let raw = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &raw).unwrap();
        let captured = capture_native_backup(&root.0, &id, &root.proof())
            .await
            .unwrap();
        assert_eq!(captured.backup().usb, usb());
        assert!(captured.backup().voice.is_none());
        assert!(
            read_native_backup(&root.0, &id)
                .await
                .unwrap()
                .voice
                .is_none()
        );
        assert!(
            read_restorable_backup(&root.0, &id)
                .unwrap()
                .voice
                .is_none()
        );
        let summary = summarize(&id, OffsetDateTime::UNIX_EPOCH, &raw).0;
        assert!(summary.restorable && !summary.voice_included && !summary.ims_configured);
        captured.verify_unchanged().await.unwrap();
        assert_eq!(read_export(&root.0, &id).unwrap().0, raw);
        assert_eq!(fs::read(&path).unwrap(), raw);
        assert_tampered_lifecycle_blocks_new_writer(&root).await;
    }
}

#[tokio::test]
async fn captured_restore_never_adopts_replacement_or_in_place_changed_values() {
    for replace_inode in [false, true] {
        let root = Root::new();
        let id = create_native_backup(&root.0, identity(), usb(), voice(), false, &root.proof())
            .await
            .unwrap();
        let path = backup_directory(&root.0).join(&id);
        let captured = capture_native_backup(&root.0, &id, &root.proof())
            .await
            .unwrap();
        let mut changed: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        changed["usb"]["flags"][6] = json!(1);
        changed["restore_command"] = json!("AT+QCFG=\"USBCFG\",0x2C7C,0x2400,1,0,1,0,1,0,1");
        let replacement = serde_json::to_vec(&changed).unwrap();
        if replace_inode {
            let pending = path.with_extension("replacement");
            fs::write(&pending, &replacement).unwrap();
            fs::rename(pending, &path).unwrap();
        } else {
            fs::write(&path, &replacement).unwrap();
        }
        // The replacement is itself a valid backup. It still cannot become
        // the already selected restore target through a second path read.
        assert_ne!(read_native_backup(&root.0, &id).await.unwrap().usb, usb());
        assert!(captured.verify_unchanged().await.is_err());
        assert_eq!(captured.backup().usb, usb());
        assert_eq!(captured.backup().voice, Some(voice()));
        assert_eq!(fs::read(&path).unwrap(), replacement);
        assert_tampered_lifecycle_blocks_new_writer(&root).await;
    }
}

#[tokio::test]
async fn captured_restore_rejects_identity_or_namespace_changes_without_adopting_aliases() {
    use std::os::unix::fs::symlink;
    for mutation in [
        "identical-file",
        "file-symlink",
        "file-hardlink",
        "directory",
        "directory-symlink",
        "root",
    ] {
        let root = Root::new();
        let id = create_native_backup(&root.0, identity(), usb(), voice(), false, &root.proof())
            .await
            .unwrap();
        let path = backup_directory(&root.0).join(&id);
        let original = fs::read(&path).unwrap();
        let captured = capture_native_backup(&root.0, &id, &root.proof())
            .await
            .unwrap();
        let saved_directory = root.0.join("saved-backups");
        let moved_root = root.0.with_extension("moved");
        match mutation {
            "identical-file" => {
                let new = root.0.join("identical");
                fs::write(&new, &original).unwrap();
                fs::rename(new, &path).unwrap();
            }
            "file-symlink" => {
                let target = root.0.join("original");
                fs::rename(&path, &target).unwrap();
                symlink(target, &path).unwrap();
            }
            "file-hardlink" => {
                fs::hard_link(&path, root.0.join("alias")).unwrap();
            }
            "directory" | "directory-symlink" => {
                fs::rename(backup_directory(&root.0), &saved_directory).unwrap();
                if mutation == "directory-symlink" {
                    symlink(&saved_directory, backup_directory(&root.0)).unwrap();
                } else {
                    fs::create_dir(backup_directory(&root.0)).unwrap();
                    fs::write(&path, &original).unwrap();
                }
            }
            "root" => {
                fs::rename(&root.0, &moved_root).unwrap();
                fs::create_dir_all(backup_directory(&root.0)).unwrap();
                fs::write(&path, &original).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            captured.verify_unchanged().await.is_err(),
            "adopted {mutation}"
        );
        assert_eq!(captured.backup().usb, usb());
        if mutation == "root" {
            assert_eq!(
                fs::read(backup_directory(&moved_root).join(&id)).unwrap(),
                original
            );
            fs::remove_dir_all(&moved_root).unwrap();
        }
        if saved_directory.exists() {
            assert_eq!(fs::read(saved_directory.join(&id)).unwrap(), original);
        }
    }
}

#[tokio::test]
async fn captured_restore_rejects_foreign_root_authority_without_adopting_valid_backup() {
    let root = Root::new();
    let foreign = Root::new();
    let id = create_native_backup(&root.0, identity(), usb(), voice(), false, &root.proof())
        .await
        .unwrap();
    let path = backup_directory(&root.0).join(&id);
    let bytes = fs::read(&path).unwrap();
    let captured = capture_native_backup(&root.0, &id, &root.proof())
        .await
        .unwrap();
    captured
        .verify_for_root(&root.0, &root.proof())
        .await
        .unwrap();
    assert!(
        captured
            .verify_for_root(&foreign.0, &foreign.proof())
            .await
            .unwrap_err()
            .starts_with("模块备份根目录授权已失效或不匹配：")
    );
    assert!(
        captured
            .verify_for_root(&foreign.0, &root.proof())
            .await
            .unwrap_err()
            .starts_with("模块备份根目录授权已失效或不匹配：")
    );
    let result = capture_native_backup(&root.0, &id, &foreign.proof()).await;
    assert!(
        matches!(result, Err(error) if error.starts_with("模块备份根目录授权已失效或不匹配："))
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(!foreign.0.join(BACKUP_DIRECTORY).exists());
}

#[tokio::test]
async fn captured_restore_authority_ends_with_lease_and_cannot_adopt_a_new_lease() {
    let mut root = Root::new();
    let id = create_native_backup(&root.0, identity(), usb(), voice(), false, &root.proof())
        .await
        .unwrap();
    let path = backup_directory(&root.0).join(&id);
    let bytes = fs::read(&path).unwrap();
    let proof = root.proof();
    let captured = capture_native_backup(&root.0, &id, &proof).await.unwrap();
    drop(root.1.take());
    assert!(
        captured
            .verify_unchanged()
            .await
            .unwrap_err()
            .starts_with("模块备份根目录授权已失效或不匹配：")
    );
    root.1 = Some(crate::daemon_lifetime::Lease::acquire(&root.0).unwrap());
    assert!(matches!(capture_native_backup(&root.0, &id, &proof).await,
        Err(error) if error.starts_with("模块备份根目录授权已失效或不匹配：")));
    assert!(captured.verify_unchanged().await.is_err());
    assert!(
        captured
            .verify_for_root(&root.0, &root.proof())
            .await
            .is_err()
    );
    assert!(!proof.same_authority(&root.proof()));
    capture_native_backup(&root.0, &id, &root.proof())
        .await
        .unwrap()
        .verify_unchanged()
        .await
        .unwrap();
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[tokio::test]
async fn captured_restore_rejects_lifetime_lock_replacement_and_keeps_backup_bytes() {
    let root = Root::new();
    let id = create_native_backup(&root.0, identity(), usb(), voice(), false, &root.proof())
        .await
        .unwrap();
    let path = backup_directory(&root.0).join(&id);
    let bytes = fs::read(&path).unwrap();
    let proof = root.proof();
    let captured = capture_native_backup(&root.0, &id, &proof).await.unwrap();
    fs::rename(
        root.0.join(".switchd-lifetime.lock"),
        root.0.join("old-lock"),
    )
    .unwrap();
    fs::write(root.0.join(".switchd-lifetime.lock"), []).unwrap();
    assert!(captured.verify_unchanged().await.is_err());
    assert!(matches!(capture_native_backup(&root.0, &id, &proof).await,
        Err(error) if error.starts_with("模块备份根目录授权已失效或不匹配：")));
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[tokio::test]
async fn native_schema_three_create_round_trips_through_existing_reader_and_export() {
    let root = Root::new();
    for before_restore in [false, true] {
        let id = create_native_backup(
            &root.0,
            identity(),
            usb(),
            voice(),
            before_restore,
            &root.proof(),
        )
        .await
        .unwrap();
        assert!(id.starts_with(if before_restore {
            "usb-before-restore-"
        } else {
            "usb-before-call-mode-"
        }));
        let existing = read_restorable_backup(&root.0, &id).unwrap();
        assert_eq!(existing.schema_version, 3);
        assert_eq!(existing.restore_command.as_deref(), Some(command()));
        assert_eq!(
            existing.reason.as_deref(),
            Some(if before_restore {
                "before_restore"
            } else {
                "before_call_mode"
            })
        );
        let native = read_native_backup(&root.0, &id).await.unwrap();
        assert_eq!(native.module.imei, identity().imei);
        assert_eq!(native.module.firmware, identity().firmware);
        assert_eq!(native.usb, usb());
        assert_eq!(native.voice, Some(voice()));
        assert!(!native.saved_at.is_empty());
        let (bytes, _) = read_export(&root.0, &id).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload["restore_command"], command());
        assert_eq!(
            fs::metadata(backup_directory(&root.0).join(id))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn native_previous_schema_read_accepts_missing_or_null_firmware_and_absent_voice() {
    let root = Root::new();
    fs::create_dir_all(backup_directory(&root.0)).unwrap();
    for (index, firmware) in [None, Some(serde_json::Value::Null)]
        .into_iter()
        .enumerate()
    {
        let mut payload = json!({
            "schema_version":2,"kind":BACKUP_KIND,"reason":"before_call_mode",
            "saved_at":"2026-10-13T00:00:00Z","module":{"imei":identity().imei},
            "usb":usb(),"restore_command":command()
        });
        if let Some(firmware) = firmware {
            payload["module"]["firmware"] = firmware;
        }
        let id = format!("usb-before-call-mode-legacy-{index}.json");
        fs::write(
            backup_directory(&root.0).join(&id),
            serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
        let native = read_native_backup(&root.0, &id).await.unwrap();
        assert_eq!(native.module.firmware, "");
        assert_eq!(native.usb, usb());
        assert!(native.voice.is_none());
        let existing = read_restorable_backup(&root.0, &id).unwrap();
        assert_eq!(existing.restore_command.as_deref(), Some(command()));
        assert!(existing.voice.is_none());
    }
}

#[tokio::test]
async fn native_backup_cannot_restore_to_wrong_imei_or_usb_identity() {
    let root = Root::new();
    let id = create_native_backup(&root.0, identity(), usb(), voice(), false, &root.proof())
        .await
        .unwrap();
    let stored = read_restorable_backup(&root.0, &id).unwrap();
    assert_eq!(
        restore_binding_error(&stored, &identity().imei, 0x2c7c, 0x2400),
        None
    );
    for bad_imei in ["987654321098765", "123", "12345678901234x"] {
        assert_eq!(
            restore_binding_error(&stored, bad_imei, 0x2c7c, 0x2400),
            Some("备份所属模块与当前连接模块的 IMEI 不一致")
        );
    }
    for (vid, pid) in [(0x2c7d, 0x2400), (0x2c7c, 0x2401), (0, 0x2400)] {
        assert_eq!(
            restore_binding_error(&stored, &identity().imei, vid, pid),
            Some("备份与当前模块的 USB VID/PID 不一致")
        );
    }
    let mut malformed = identity();
    malformed.imei = "123".into();
    assert!(
        create_native_backup(&root.0, malformed, usb(), voice(), false, &root.proof())
            .await
            .is_err()
    );
    let mut malformed_usb = usb();
    malformed_usb.vendor_id = 0;
    assert!(
        create_native_backup(
            &root.0,
            identity(),
            malformed_usb,
            voice(),
            false,
            &root.proof()
        )
        .await
        .is_err()
    );
    assert_eq!(fs::read_dir(backup_directory(&root.0)).unwrap().count(), 1);
}

// Independent read/capture cases above deliberately edit a previously committed
// lifecycle head. They must not accidentally rely on a later writer accepting
// that edit merely because the next test case happens to share the same root.
async fn assert_tampered_lifecycle_blocks_new_writer(root: &Root) {
    let before = list_backups(&root.0).unwrap();
    let result =
        create_native_backup(&root.0, identity(), usb(), voice(), true, &root.proof()).await;
    assert!(
        result.is_err(),
        "tampered lifecycle head admitted a new backup writer"
    );
    assert!(
        list_backups(&root.0).unwrap() == before,
        "rejected writer changed public backup inventory"
    );
    assert!(
        crate::module_recovery::Gate::inspect_with_lifetime(root.1.as_ref().unwrap())
            .is_quarantined(),
        "tampered lifecycle head lost startup quarantine"
    );
}
