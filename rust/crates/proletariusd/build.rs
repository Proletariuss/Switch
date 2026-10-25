//! Embed only the source-matched, audited module owner built by the recipe.
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};

fn hash(path: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(fs::read(path).expect("read supervisor build input"))
    )
}

fn main() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap();
    let output = source.parent().unwrap().join("build/module-supervisor");
    let manifest_path = output.join("manifest.json");
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect(
            "first run scripts/build-module-supervisor.py (workspace-local ARM dependency)",
        ))
        .expect("valid module supervisor build manifest");
    assert_eq!(value["schema"], 1);
    assert_eq!(value["target"], "armv7-unknown-linux-musleabi");
    assert_eq!(value["rust_version"], "1.98.0");
    assert_eq!(value["artifact"], "switch-voice-supervisor.armv7");
    let mut expected = BTreeMap::new();
    fn collect(root: &Path, path: &Path, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(path).expect("read supervisor source") {
            let path = entry.expect("source entry").path();
            let kind = fs::symlink_metadata(&path)
                .expect("source metadata")
                .file_type();
            assert!(!kind.is_symlink(), "linked supervisor source");
            if kind.is_dir() {
                collect(root, &path, out);
            } else {
                assert!(kind.is_file(), "special supervisor source");
                out.insert(
                    path.strip_prefix(root).unwrap().to_str().unwrap().into(),
                    hash(&path),
                );
            }
        }
    }
    println!(
        "cargo:rerun-if-changed={}",
        source.join("rust/crates/switch-voice-supervisor").display()
    );
    collect(
        source,
        &source.join("rust/crates/switch-voice-supervisor"),
        &mut expected,
    );
    for name in [
        "rust/Cargo.toml",
        "rust/Cargo.lock",
        "scripts/build-module-supervisor.py",
        "scripts/prepare-module-supervisor.py",
    ] {
        expected.insert(name.into(), hash(&source.join(name)));
    }
    for name in expected.keys() {
        println!("cargo:rerun-if-changed={}", source.join(name).display());
    }
    let recorded: BTreeMap<String, String> =
        serde_json::from_value(value["source_files"].clone()).expect("supervisor source hashes");
    assert_eq!(
        recorded, expected,
        "stale module supervisor; rerun scripts/build-module-supervisor.py"
    );
    let artifact = output.join("switch-voice-supervisor.armv7");
    println!("cargo:rerun-if-changed={}", artifact.display());
    let bytes = fs::read(&artifact).expect("read built module supervisor");
    assert!((52..=1_100_000).contains(&bytes.len()));
    assert_eq!(&bytes[..7], b"\x7fELF\x01\x01\x01");
    assert_eq!(u16::from_le_bytes(bytes[16..18].try_into().unwrap()), 2);
    assert_eq!(u16::from_le_bytes(bytes[18..20].try_into().unwrap()), 40);
    let flags = u32::from_le_bytes(bytes[36..40].try_into().unwrap());
    assert_eq!(
        flags & 0xff000600,
        0x05000200,
        "expected ARM EABI5 soft-float"
    );
    let offset = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
    let size = u16::from_le_bytes(bytes[42..44].try_into().unwrap()) as usize;
    let count = u16::from_le_bytes(bytes[44..46].try_into().unwrap()) as usize;
    assert!(size == 32 && (1..=64).contains(&count) && offset + size * count <= bytes.len());
    for index in 0..count {
        let item = offset + size * index;
        let kind = u32::from_le_bytes(bytes[item..item + 4].try_into().unwrap());
        assert!(
            ![2, 3].contains(&kind),
            "dynamic ARM module dependency forbidden"
        );
    }
    let sha = hash(&artifact);
    assert_eq!(
        value["sha256"].as_str(),
        Some(sha.as_str()),
        "module supervisor hash changed"
    );
    println!(
        "cargo:rustc-env=SWITCH_MODULE_SUPERVISOR_ARTIFACT={}",
        artifact.display()
    );
    println!("cargo:rustc-env=SWITCH_MODULE_SUPERVISOR_SHA256={sha}");
}
