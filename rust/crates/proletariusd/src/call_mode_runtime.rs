//! Verification and native deployment of the module-side voice runtime.
//! Rust opens installed artifacts without following symlinks, verifies the
//! bytes from opened file handles, and keeps them in-process for ADB transfer.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::call_mode_runtime_manifest::{Artifact, MAX_ARTIFACT_BYTES, manifest};

pub(crate) const INTERNAL_VERIFY_PATH: &str = "/api/internal/call-mode/runtime/verify";

static FILE_OPERATIONS: Semaphore = Semaphore::const_new(4);

/// Our Rust child owner is built locally, independently of the pinned six-file
/// third-party runtime. It is embedded in switchd-rs and verified before push.
pub(crate) struct DeploymentArtifact {
    pub(crate) name: &'static str,
    pub(crate) mode: u32,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) fn supervisor_artifact() -> Result<DeploymentArtifact, String> {
    let bytes = include_bytes!(env!("SWITCH_MODULE_SUPERVISOR_ARTIFACT"));
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != env!("SWITCH_MODULE_SUPERVISOR_SHA256")
        || bytes.len() > MAX_ARTIFACT_BYTES as usize
        || !bytes.starts_with(b"\x7fELF\x01\x01\x01")
    {
        return Err("Rust 语音会话控制程序完整性校验失败".into());
    }
    Ok(DeploymentArtifact {
        name: "switch-voice-supervisor.armv7",
        mode: 0o755,
        bytes: bytes.to_vec(),
    })
}

/// Verifies the installed runtime in-process for Rust-owned readiness checks.
/// The same bounded worker and no-follow verifier back the public status API.
pub(crate) async fn installed_runtime_verified(data_root: &Path) -> bool {
    installed_runtime_verification(data_root)
        .await
        .runtime_downloaded
}

/// Native inspection returns verified byte counts and the original Chinese
/// integrity error, through the same bounded no-follow file worker.
pub(crate) async fn installed_runtime_verification(data_root: &Path) -> RuntimeVerification {
    let total = manifest().files.iter().map(|file| file.size).sum();
    let fallback = || RuntimeVerification {
        runtime_version: manifest().version.clone(),
        runtime_downloaded: false,
        downloaded_bytes: 0,
        total_bytes: total,
        error: "语音运行时文件校验未完成".into(),
    };
    let Ok(permit) = FILE_OPERATIONS.acquire().await else {
        return fallback();
    };
    let directory = data_root.join("voice-runtime").join(&manifest().version);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        verify_runtime_files(&directory, &manifest().files)
    })
    .await
    .unwrap_or_else(|_| fallback())
}

#[derive(Debug, Serialize)]
pub(crate) struct RuntimeVerification {
    pub(crate) runtime_version: String,
    pub(crate) runtime_downloaded: bool,
    pub(crate) downloaded_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) error: String,
}

pub(crate) async fn handle_internal(
    request: Request<Incoming>,
    data_root: &Path,
) -> Response<Full<Bytes>> {
    let verify = request.uri().path() == INTERNAL_VERIFY_PATH;
    if request.method() != Method::GET || !verify || request.uri().query().is_some() {
        return response(
            StatusCode::NOT_FOUND,
            &serde_json::json!({"code":"route_not_found","error":"route not found"}),
        );
    }
    let data_root = data_root.to_path_buf();
    let permit = match FILE_OPERATIONS.acquire().await {
        Ok(permit) => permit,
        Err(_) => {
            return response(
                StatusCode::SERVICE_UNAVAILABLE,
                &serde_json::json!({"code":"runtime_worker_unavailable","error":"语音运行时文件服务暂不可用"}),
            );
        }
    };
    // The file reads and hashing can run during status polling or deployment;
    // keep them off Tokio's event loop, bounded across both endpoints.
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let directory = data_root.join("voice-runtime").join(&manifest().version);
        response(
            StatusCode::OK,
            &verify_runtime_files(&directory, &manifest().files),
        )
    })
    .await
    {
        Ok(response) => response,
        Err(_) => response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &serde_json::json!({"code":"runtime_worker_failed","error":"语音运行时文件校验未完成"}),
        ),
    }
}

pub(crate) fn read_deployment_artifacts(
    data_root: &Path,
) -> Result<Vec<(Artifact, Vec<u8>)>, String> {
    let directory = data_root.join("voice-runtime").join(&manifest().version);
    load_verified_artifacts(&directory, &manifest().files).map(|files| {
        files
            .into_iter()
            .filter(|(artifact, _)| artifact.deploy)
            .collect()
    })
}

fn load_verified_artifacts(
    directory: &Path,
    expected: &[Artifact],
) -> Result<Vec<(Artifact, Vec<u8>)>, String> {
    validate_runtime_directory(directory)?;
    expected
        .iter()
        .map(|artifact| {
            let data = read_verified_artifact(directory, artifact)?;
            Ok((artifact.clone(), data))
        })
        .collect()
}

pub(crate) fn verify_runtime_files(directory: &Path, expected: &[Artifact]) -> RuntimeVerification {
    let mut result = RuntimeVerification {
        runtime_version: manifest().version.clone(),
        runtime_downloaded: false,
        downloaded_bytes: 0,
        total_bytes: expected.iter().map(|artifact| artifact.size).sum(),
        error: String::new(),
    };
    if let Err(error) = validate_runtime_directory(directory) {
        result.error = error;
        return result;
    }
    for artifact in expected {
        match read_verified_artifact(directory, artifact) {
            Ok(_) => result.downloaded_bytes += artifact.size,
            Err(error) if result.error.is_empty() => result.error = error,
            Err(_) => {}
        }
    }
    result.runtime_downloaded = result.error.is_empty() && !expected.is_empty();
    result
}

pub(crate) fn validate_runtime_directory(directory: &Path) -> Result<(), String> {
    open_runtime_directory(directory).map(|_| ())
}

fn open_runtime_directory(directory: &Path) -> Result<File, String> {
    let parent = directory
        .parent()
        .ok_or_else(|| "本地语音运行时目录不可用".to_owned())?;
    let data_root = parent
        .parent()
        .ok_or_else(|| "Switch 数据目录不可用".to_owned())?;
    let flags = nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_DIRECTORY;
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(data_root)
        .map_err(|_| "Switch 数据目录不可用或类型无效".to_owned())?;
    let parent_name = parent
        .file_name()
        .ok_or_else(|| "本地语音运行时父目录不可用".to_owned())?;
    let runtime_parent = open_child_no_follow(&root, parent_name, true).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "本地语音运行时尚未下载，请选择下载渠道".to_owned()
        } else {
            "本地语音运行时父目录不可用或类型无效".to_owned()
        }
    })?;
    let directory_name = directory
        .file_name()
        .ok_or_else(|| "本地语音运行时目录不可用".to_owned())?;
    open_child_no_follow(&runtime_parent, directory_name, true).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "本地语音运行时尚未下载，请选择下载渠道".to_owned()
        } else {
            "本地语音运行时目录不可用或类型无效".to_owned()
        }
    })
}

pub(crate) fn read_verified_artifact(
    directory: &Path,
    artifact: &Artifact,
) -> Result<Vec<u8>, String> {
    if artifact.size == 0 || artifact.size > MAX_ARTIFACT_BYTES {
        return Err("语音运行时清单包含无效文件大小".into());
    }
    let runtime = open_runtime_directory(directory)?;
    let file = open_child_no_follow(&runtime, std::ffi::OsStr::new(&artifact.name), false)
        .map_err(|_| format!("{} 不存在或不是普通文件", artifact.name))?;
    let metadata = file
        .metadata()
        .map_err(|_| format!("无法读取 {} 的文件状态", artifact.name))?;
    if !metadata.is_file() || metadata.len() != artifact.size {
        return Err(format!("{} 的类型或大小不匹配", artifact.name));
    }
    let mut data = Vec::with_capacity(artifact.size as usize);
    file.take(artifact.size + 1)
        .read_to_end(&mut data)
        .map_err(|_| format!("读取 {} 失败", artifact.name))?;
    if data.len() as u64 != artifact.size {
        return Err(format!("{} 的内容长度不匹配", artifact.name));
    }
    if let Err(error) = verify_artifact_bytes(artifact, &data) {
        data.fill(0);
        return Err(error);
    }
    Ok(data)
}

pub(crate) fn verify_artifact_bytes(artifact: &Artifact, data: &[u8]) -> Result<(), String> {
    if artifact.size == 0
        || artifact.size > MAX_ARTIFACT_BYTES
        || data.len() as u64 != artifact.size
    {
        return Err(format!("{} 的内容长度不匹配", artifact.name));
    }
    if !constant_hex_eq(&Sha256::digest(data), &artifact.sha256) {
        return Err(format!("{} 的 SHA-256 不匹配", artifact.name));
    }
    Ok(())
}

fn open_child_no_follow(
    parent: &File,
    name: &std::ffi::OsStr,
    directory: bool,
) -> std::io::Result<File> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "runtime entry must be a single name",
        ));
    }
    use nix::fcntl::OFlag;
    let flags = OFlag::O_RDONLY
        | OFlag::O_NOFOLLOW
        | OFlag::O_CLOEXEC
        | OFlag::O_NONBLOCK
        | if directory {
            OFlag::O_DIRECTORY
        } else {
            OFlag::empty()
        };
    // Directory handles keep the checked ancestry stable across rename. Nix's
    // owned descriptor preserves the daemon's unsafe-code prohibition.
    nix::fcntl::openat(parent, name, flags, nix::sys::stat::Mode::empty())
        .map(File::from)
        .map_err(std::io::Error::from)
}

fn constant_hex_eq(digest: &[u8], expected: &str) -> bool {
    if expected.len() != digest.len() * 2 {
        return false;
    }
    let mut difference = 0u8;
    for (index, byte) in digest.iter().enumerate() {
        let pair = &expected.as_bytes()[index * 2..index * 2 + 2];
        let Some(high) = hex_value(pair[0]) else {
            return false;
        };
        let Some(low) = hex_value(pair[1]) else {
            return false;
        };
        difference |= byte ^ ((high << 4) | low);
    }
    difference == 0
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn response<T: Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("static response headers are valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn fixture_file(name: &str, data: &[u8]) -> Artifact {
        let digest = format!("{:x}", Sha256::digest(data));
        Artifact {
            name: name.into(),
            size: data.len() as u64,
            sha256: digest,
            mode: 0o644,
            deploy: true,
        }
    }

    #[test]
    fn absent_runtime_directories_report_pending_download_without_creating_paths() {
        let root =
            std::env::temp_dir().join(format!("switch-runtime-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let parent = root.join("voice-runtime");
        let directory = parent.join("version");
        let expected = [fixture_file("driver.ko", b"driver")];
        let unavailable = verify_runtime_files(&directory, &expected);
        assert!(!unavailable.runtime_downloaded);
        assert_eq!(unavailable.error, "Switch 数据目录不可用或类型无效");
        assert!(!root.exists());

        fs::create_dir(&root).unwrap();
        for parent_exists in [false, true] {
            if parent_exists {
                fs::create_dir(&parent).unwrap();
            }
            let result = verify_runtime_files(&directory, &expected);
            assert!(!result.runtime_downloaded);
            assert_eq!(result.downloaded_bytes, 0);
            assert_eq!(result.total_bytes, 6);
            assert_eq!(result.error, "本地语音运行时尚未下载，请选择下载渠道");
            assert_eq!(parent.exists(), parent_exists);
            assert!(!directory.exists());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_runtime_directory_entries_are_not_reported_as_missing_or_modified() {
        let root =
            std::env::temp_dir().join(format!("switch-runtime-invalid-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("outside")).unwrap();
        fs::write(root.join("outside/sentinel"), b"untouched").unwrap();
        let expected = [fixture_file("driver.ko", b"driver")];
        for (entry, directory, detail) in [
            (
                root.join("voice-runtime"),
                root.join("voice-runtime/version"),
                "本地语音运行时父目录不可用或类型无效",
            ),
            (
                root.join("valid-parent/version"),
                root.join("valid-parent/version"),
                "本地语音运行时目录不可用或类型无效",
            ),
        ] {
            fs::create_dir_all(entry.parent().unwrap()).unwrap();
            for linked in [false, true] {
                if linked {
                    symlink(root.join("outside"), &entry).unwrap();
                } else {
                    fs::write(&entry, b"existing entry").unwrap();
                }
                let result = verify_runtime_files(&directory, &expected);
                assert!(!result.runtime_downloaded);
                assert_eq!(result.downloaded_bytes, 0);
                assert_eq!(result.error, detail);
                assert_eq!(fs::symlink_metadata(&entry).unwrap().is_symlink(), linked);
                if !linked {
                    assert_eq!(fs::read(&entry).unwrap(), b"existing entry");
                }
                assert_eq!(
                    fs::read(root.join("outside/sentinel")).unwrap(),
                    b"untouched"
                );
                fs::remove_file(&entry).unwrap();
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_returns_manifest_matched_bytes_from_opened_regular_files() {
        let root = std::env::temp_dir().join(format!("switch-runtime-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let contents: [&[u8]; 3] = [b"module-a", b"module-b", b"helper"];
        let expected = [
            fixture_file("a.ko", contents[0]),
            fixture_file("b.ko", contents[1]),
            fixture_file("helper", contents[2]),
        ];
        for (artifact, data) in expected.iter().zip(contents) {
            fs::write(root.join(&artifact.name), data).unwrap();
        }
        let files = load_verified_artifacts(&root, &expected).unwrap();
        assert_eq!(files.len(), 3);
        for (index, ((artifact, data), expected)) in files.iter().zip(expected.iter()).enumerate() {
            assert_eq!(artifact.name, expected.name);
            assert_eq!(artifact.size, expected.size);
            assert_eq!(data, contents[index]);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_symlinks_mismatched_hashes_and_symlinked_runtime_directories() {
        let parent =
            std::env::temp_dir().join(format!("switch-runtime-safe-{}", std::process::id()));
        let root = parent.join("runtime");
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&root).unwrap();
        let data = b"driver-bytes";
        let expected = [fixture_file("driver.ko", data)];
        fs::write(parent.join("outside.ko"), data).unwrap();
        symlink(parent.join("outside.ko"), root.join("driver.ko")).unwrap();
        assert!(load_verified_artifacts(&root, &expected).is_err());

        fs::remove_file(root.join("driver.ko")).unwrap();
        fs::write(root.join("driver.ko"), b"tampered-data").unwrap();
        assert!(load_verified_artifacts(&root, &expected).is_err());

        let linked = parent.join("linked-runtime");
        symlink(&root, &linked).unwrap();
        assert!(load_verified_artifacts(&linked, &expected).is_err());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn rejects_symlinked_runtime_parent_directory() {
        let root =
            std::env::temp_dir().join(format!("switch-runtime-parent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("outside")).unwrap();
        let expected = [fixture_file("driver.ko", b"driver")];
        fs::write(root.join("outside/driver.ko"), b"driver").unwrap();
        symlink(root.join("outside"), root.join("linked")).unwrap();
        assert!(load_verified_artifacts(&root.join("linked/version"), &expected).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn complete_runtime_requires_support_files_and_counts_only_verified_bytes() {
        let parent =
            std::env::temp_dir().join(format!("switch-runtime-count-{}", std::process::id()));
        let root = parent.join("version");
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&root).unwrap();
        let data: [&[u8]; 6] = [
            b"driver-a",
            b"driver-b",
            b"bridge",
            b"manifest",
            b"license",
            b"report",
        ];
        let expected: Vec<_> = data
            .iter()
            .enumerate()
            .map(|(index, content)| {
                let mut file = fixture_file(&format!("file-{index}"), content);
                file.deploy = index < 3;
                file
            })
            .collect();
        for (file, content) in expected.iter().zip(data) {
            fs::write(root.join(&file.name), content).unwrap();
        }
        let valid = verify_runtime_files(&root, &expected);
        assert!(valid.runtime_downloaded);
        assert!(valid.error.is_empty());
        assert_eq!(valid.downloaded_bytes, valid.total_bytes);

        // A missing licence must invalidate the entire runtime, even though all
        // three deployment binaries are still intact.
        fs::remove_file(root.join(&expected[4].name)).unwrap();
        let missing = verify_runtime_files(&root, &expected);
        assert!(!missing.runtime_downloaded);
        assert_eq!(
            missing.downloaded_bytes,
            valid.total_bytes - expected[4].size
        );
        assert!(missing.error.contains(&expected[4].name));

        // A same-length replacement cannot contribute to downloaded_bytes.
        fs::write(root.join(&expected[4].name), b"LICENSE").unwrap();
        let tampered = verify_runtime_files(&root, &expected);
        assert!(!tampered.runtime_downloaded);
        assert_eq!(tampered.downloaded_bytes, missing.downloaded_bytes);
        assert!(tampered.error.contains("SHA-256"));
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn non_regular_artifact_does_not_block_a_file_worker() {
        let parent =
            std::env::temp_dir().join(format!("switch-runtime-fifo-{}", std::process::id()));
        let root = parent.join("version");
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&root).unwrap();
        let file = fixture_file("fifo", b"data");
        nix::unistd::mkfifo(
            &root.join(&file.name),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(read_verified_artifact(&root, &file).is_err());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn directory_handle_stays_on_verified_directory_during_path_replacement() {
        let parent =
            std::env::temp_dir().join(format!("switch-runtime-race-{}", std::process::id()));
        let root = parent.join("version");
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(parent.join("outside")).unwrap();
        fs::write(root.join("driver"), b"verified").unwrap();
        fs::write(parent.join("outside/driver"), b"outside").unwrap();
        let directory = open_runtime_directory(&root).unwrap();
        fs::rename(&root, parent.join("saved")).unwrap();
        symlink(parent.join("outside"), &root).unwrap();
        let mut file =
            open_child_no_follow(&directory, std::ffi::OsStr::new("driver"), false).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"verified");
        assert!(open_runtime_directory(&root).is_err());
        fs::remove_dir_all(parent).unwrap();
    }
}
