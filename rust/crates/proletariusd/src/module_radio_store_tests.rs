#[cfg(test)]
mod tests {
    use super::super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    fn root() -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .join("build/runtime/radio-store-tests")
            .join(format!(
                "{}-{}",
                crate::snapshot::new_instance_id(),
                ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        root
    }
    #[test]
    fn object_format_defaults_explicit_choice_and_private_atomic_write() {
        let root = root();
        let store = Store::new(root.clone());
        assert_eq!(store.load().unwrap(), Preferences::default());
        fs::write(&store.path, b"{}").unwrap();
        assert!(store.load().unwrap().suppressed());
        let selected = Preferences {
            enabled: false,
            restore_cfun: 1,
            user_configured: true,
        };
        store.save(selected).unwrap();
        assert_eq!(store.load().unwrap(), selected);
        assert!(!selected.suppressed());
        assert_eq!(
            fs::metadata(&store.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&store.path).unwrap())
                .unwrap()
                .is_object()
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn corrupt_or_invalid_restore_is_never_overwritten() {
        let root = root();
        let store = Store::new(root.clone());
        for bytes in [
            "[]",
            "null",
            "{",
            r#"{"enabled":true,"restore_cfun":4}"#,
            r#"{"enabled":"yes"}"#,
        ] {
            fs::write(&store.path, bytes).unwrap();
            assert!(store.load().is_err());
            assert!(store.save(Preferences::default()).is_err());
            assert_eq!(fs::read_to_string(&store.path).unwrap(), bytes);
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn symlink_and_oversized_preferences_fail_closed() {
        let root = root();
        let store = Store::new(root.clone());
        let outside = root.join("original");
        fs::write(&outside, b"{}").unwrap();
        std::os::unix::fs::symlink(&outside, &store.path).unwrap();
        assert!(store.load().is_err());
        assert!(store.save(Preferences::default()).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"{}");
        fs::remove_file(&store.path).unwrap();
        fs::write(&store.path, vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert!(store.load().is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
