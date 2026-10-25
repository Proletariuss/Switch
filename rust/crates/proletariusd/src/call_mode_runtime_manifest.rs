//! The native daemon embeds the pinned module voice runtime manifest.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::Deserialize;

const MANIFEST_JSON: &str = include_str!("../resources/voice-runtime-manifest.json");

pub(crate) const MAX_ARTIFACT_BYTES: u64 = 1_100_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeManifest {
    pub(crate) commit: String,
    pub(crate) version: String,
    pub(crate) kernel: String,
    pub(crate) files: Vec<Artifact>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Artifact {
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) mode: u32,
    pub(crate) deploy: bool,
}

pub(crate) fn manifest() -> &'static RuntimeManifest {
    static MANIFEST: OnceLock<RuntimeManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        parse_manifest(MANIFEST_JSON).expect("embedded voice runtime manifest must be valid")
    })
}

fn parse_manifest(json: &str) -> Result<RuntimeManifest, String> {
    let manifest: RuntimeManifest = serde_json::from_str(json).map_err(|err| err.to_string())?;
    if !lowercase_hex(&manifest.commit, 40)
        || !safe_component(&manifest.version)
        || manifest.kernel.is_empty()
        || !manifest
            .kernel
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        || manifest.files.is_empty()
    {
        return Err("invalid voice runtime identity".into());
    }
    let mut names = HashSet::new();
    for file in &manifest.files {
        if !safe_component(&file.name)
            || !names.insert(&file.name)
            || file.size == 0
            || file.size > MAX_ARTIFACT_BYTES
            || !lowercase_hex(&file.sha256, 64)
            || !matches!(file.mode, 0o644 | 0o755)
        {
            return Err("invalid voice runtime artifact".into());
        }
    }
    if !manifest.files.iter().any(|file| file.deploy) {
        return Err("voice runtime has no deployment files".into());
    }
    Ok(manifest)
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn lowercase_hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_manifest_retains_complete_pinned_runtime_and_deployment_subset() {
        let manifest = manifest();
        assert_eq!(manifest.version, "qdc507-3.18.44-voice-20260712.5");
        assert_eq!(manifest.kernel, "3.18.44");
        assert_eq!(manifest.files.len(), 6);
        assert_eq!(
            manifest.files.iter().map(|file| file.size).sum::<u64>(),
            1_080_625
        );
        let deployment: Vec<_> = manifest
            .files
            .iter()
            .filter(|file| file.deploy)
            .map(|file| file.name.as_str())
            .collect();
        assert_eq!(
            deployment,
            [
                "qdc507_aprv3.ko",
                "qdc507_voice.ko",
                "mavo-pcm-bridge.armv7"
            ]
        );
        assert_eq!(manifest.files[2].mode, 0o755);
        assert!(
            manifest
                .files
                .iter()
                .enumerate()
                .all(|(index, file)| index == 2 || file.mode == 0o644)
        );
    }

    #[test]
    fn malformed_or_unsafe_canonical_entries_fail_closed() {
        let original: serde_json::Value = serde_json::from_str(MANIFEST_JSON).unwrap();
        let cases: &[(&str, serde_json::Value)] = &[
            ("name", serde_json::json!("../outside")),
            ("name", serde_json::json!("/absolute")),
            ("size", serde_json::json!(MAX_ARTIFACT_BYTES + 1)),
            ("size", serde_json::json!(0)),
            ("sha256", serde_json::json!("not-a-hash")),
            ("mode", serde_json::json!(0o777)),
        ];
        for (key, value) in cases {
            let mut modified = original.clone();
            modified["files"][0][*key] = value.clone();
            assert!(
                parse_manifest(&modified.to_string()).is_err(),
                "accepted {key}={value}"
            );
        }
        let mut duplicate = original.clone();
        duplicate["files"][1]["name"] = duplicate["files"][0]["name"].clone();
        assert!(parse_manifest(&duplicate.to_string()).is_err());
        let mut traversal = original;
        traversal["version"] = serde_json::json!("../escape");
        assert!(parse_manifest(&traversal.to_string()).is_err());
    }
}
