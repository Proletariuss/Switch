use super::*;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap();
        let fixture_root = workspace.join("build/runtime-download-tests");
        std::fs::create_dir_all(&fixture_root).unwrap();
        let path = fixture_root.join(format!(
            "runtime-download-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn version(&self) -> PathBuf {
        self.0.join("voice-runtime").join("fixture-runtime")
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(contents: &[&[u8]]) -> RuntimeManifest {
    RuntimeManifest {
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        version: "fixture-runtime".into(),
        kernel: "3.18.44".into(),
        files: contents
            .iter()
            .enumerate()
            .map(|(index, bytes)| Artifact {
                name: format!("artifact-{index}"),
                size: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(bytes)),
                mode: if index == 2 { 0o755 } else { 0o644 },
                deploy: index < 3,
            })
            .collect(),
    }
}

fn input(source: &str) -> StartRequest {
    StartRequest {
        source: source.into(),
        operation_id: "0123456789abcdef0123456789abcdef".into(),
    }
}

struct FakeTransport {
    replies: Mutex<HashMap<String, VecDeque<Result<DownloadResponse, String>>>>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl FakeTransport {
    fn new() -> Self {
        Self {
            replies: Mutex::new(HashMap::new()),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn add(&self, url: String, data: &[u8]) {
        self.reply(
            url.clone(),
            Ok(DownloadResponse {
                status: 200,
                final_url: url,
                content_length: Some(data.len() as u64),
                body: Box::new(Cursor::new(data.to_vec())),
            }),
        );
    }
    fn reply(&self, url: String, response: Result<DownloadResponse, String>) {
        self.replies
            .lock()
            .unwrap()
            .entry(url)
            .or_default()
            .push_back(response);
    }
}

impl Transport for FakeTransport {
    fn get(&self, url: &str) -> Result<DownloadResponse, String> {
        self.calls.lock().unwrap().push(url.into());
        self.replies
            .lock()
            .unwrap()
            .get_mut(url)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| Err("unexpected network request".into()))
    }
}

#[test]
fn downloads_all_six_pinned_artifacts_with_permissions_and_reuses_verified_cache() {
    let root = TestRoot::new();
    let contents: [&[u8]; 6] = [
        b"driver-one",
        b"driver-two",
        b"executable",
        b"license",
        b"notice",
        b"metadata",
    ];
    let runtime = fixture(&contents);
    let transport = FakeTransport::new();
    for (artifact, bytes) in runtime.files.iter().zip(contents) {
        transport.add(urls(&runtime, "github", artifact)[0].clone(), bytes);
    }
    assert!(!root.0.join("voice-runtime").exists());
    let directory = RuntimeDirectory::open(&root.0).unwrap();
    assert_eq!(
        std::fs::metadata(root.0.join("voice-runtime"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let mut progress = Vec::new();
    download_runtime(
        &directory,
        &runtime,
        &input("github"),
        &transport,
        |file, count, _| progress.push((file.to_owned(), count)),
    )
    .unwrap();
    assert_eq!(transport.calls.lock().unwrap().len(), 6);
    assert_eq!(
        std::fs::metadata(root.version())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        progress.last().unwrap().1,
        contents.iter().map(|bytes| bytes.len() as u64).sum::<u64>()
    );
    for (artifact, bytes) in runtime.files.iter().zip(contents) {
        let path = root.version().join(&artifact.name);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            artifact.mode
        );
    }
    download_runtime(
        &directory,
        &runtime,
        &input("relay"),
        &transport,
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(
        transport.calls.lock().unwrap().len(),
        6,
        "valid cache never requires network"
    );
    assert_eq!(
        std::fs::read_dir(root.version()).unwrap().count(),
        6,
        "no temporary files remain"
    );
}

#[test]
fn failed_artifact_preserves_old_bytes_and_prior_success_is_resumable() {
    let root = TestRoot::new();
    let runtime = fixture(&[b"new-one", b"new-two"]);
    let directory = RuntimeDirectory::open(&root.0).unwrap();
    let target = directory.open_version(&runtime.version).unwrap();
    std::fs::write(root.version().join("artifact-1"), b"old-two").unwrap();
    let transport = FakeTransport::new();
    transport.add(
        urls(&runtime, "github", &runtime.files[0])[0].clone(),
        b"new-one",
    );
    transport.add(
        urls(&runtime, "github", &runtime.files[1])[0].clone(),
        b"bad-two",
    );
    assert!(
        download_runtime(
            &directory,
            &runtime,
            &input("github"),
            &transport,
            |_, _, _| {}
        )
        .is_err()
    );
    assert_eq!(read_cached(&target, &runtime.files[0]).unwrap(), b"new-one");
    assert_eq!(
        std::fs::read(root.version().join("artifact-1")).unwrap(),
        b"old-two"
    );
    transport.add(
        urls(&runtime, "github", &runtime.files[1])[0].clone(),
        b"new-two",
    );
    download_runtime(
        &directory,
        &runtime,
        &input("github"),
        &transport,
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(
        transport.calls.lock().unwrap().len(),
        3,
        "retry skips the previously verified artifact"
    );
    assert_eq!(std::fs::read_dir(root.version()).unwrap().count(), 2);
}

#[test]
fn relay_falls_back_in_fixed_order_after_failure_and_hash_mismatch() {
    let root = TestRoot::new();
    let runtime = fixture(&[b"actual-a", b"actual-b"]);
    let transport = FakeTransport::new();
    let first = urls(&runtime, "relay", &runtime.files[0]);
    let second = urls(&runtime, "relay", &runtime.files[1]);
    transport.reply(first[0].clone(), Err("network timeout".into()));
    transport.add(first[1].clone(), b"actual-a");
    transport.add(second[0].clone(), b"wrong--b");
    transport.add(second[1].clone(), b"actual-b");
    let directory = RuntimeDirectory::open(&root.0).unwrap();
    download_runtime(
        &directory,
        &runtime,
        &input("relay"),
        &transport,
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(
        *transport.calls.lock().unwrap(),
        vec![
            first[0].clone(),
            first[1].clone(),
            second[0].clone(),
            second[1].clone()
        ]
    );
}

#[test]
fn rejects_insecure_urls_size_hash_status_and_bounded_overlong_streams() {
    let runtime = fixture(&[b"verified"]);
    let artifact = &runtime.files[0];
    let url = urls(&runtime, "github", artifact).remove(0);
    let cases: Vec<(u16, &str, Option<u64>, &[u8])> = vec![
        (404, &url, Some(8), b"verified"),
        (200, "http://example.invalid/file", Some(8), b"verified"),
        (200, &url, Some(7), b"verified"),
        (200, &url, None, b"short"),
        (200, &url, None, b"verified-and-unbounded"),
        (200, &url, Some(8), b"mismatch"),
    ];
    for (status, final_url, length, bytes) in cases {
        let transport = FakeTransport::new();
        transport.reply(
            url.clone(),
            Ok(DownloadResponse {
                status,
                final_url: final_url.into(),
                content_length: length,
                body: Box::new(Cursor::new(bytes.to_vec())),
            }),
        );
        assert!(download_file(&transport, &url, artifact, |_| {}).is_err());
    }
    let transport = FakeTransport::new();
    assert!(download_file(&transport, "http://example.invalid/file", artifact, |_| {}).is_err());
    assert!(transport.calls.lock().unwrap().is_empty());
}

#[test]
fn streaming_progress_is_throttled_without_hiding_final_completion() {
    let data = vec![42u8; 300_000];
    let runtime = fixture(&[&data]);
    let transport = FakeTransport::new();
    let url = urls(&runtime, "github", &runtime.files[0]).remove(0);
    transport.add(url.clone(), &data);
    let mut progress = Vec::new();
    assert_eq!(
        download_file(&transport, &url, &runtime.files[0], |count| progress
            .push(count))
        .unwrap(),
        data
    );
    assert_eq!(
        progress.len(),
        1,
        "fast reads should not flood notifications"
    );
    assert!(progress[0] <= runtime.files[0].size);
}

#[test]
fn symlink_directories_targets_and_lock_files_are_rejected() {
    let outside = TestRoot::new();
    let linked_root = TestRoot::new();
    symlink(&outside.0, linked_root.0.join("link")).unwrap();
    assert!(RuntimeDirectory::open(&linked_root.0.join("link")).is_err());
    symlink(&outside.0, linked_root.0.join("voice-runtime")).unwrap();
    assert!(RuntimeDirectory::open(&linked_root.0).is_err());
    let root = TestRoot::new();
    let directory = RuntimeDirectory::open(&root.0).unwrap();
    symlink(&outside.0, root.0.join("voice-runtime/fixture-runtime")).unwrap();
    assert!(directory.open_version("fixture-runtime").is_err());
    std::fs::remove_file(root.0.join("voice-runtime/fixture-runtime")).unwrap();
    let target = directory.open_version("fixture-runtime").unwrap();
    let runtime = fixture(&[b"safe"]);
    std::fs::write(outside.0.join("outside"), b"untouched").unwrap();
    symlink(outside.0.join("outside"), root.version().join("artifact-0")).unwrap();
    assert!(ensure_destination_regular(&target, &runtime.files[0]).is_err());
    assert!(
        install_file(
            &target,
            &runtime.files[0],
            b"safe",
            &input("github").operation_id
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(outside.0.join("outside")).unwrap(),
        b"untouched"
    );
    assert_eq!(
        std::fs::read_dir(root.version()).unwrap().count(),
        1,
        "rejected commit cleans its temporary file"
    );
    symlink(
        outside.0.join("outside"),
        root.0.join("voice-runtime/.download.lock"),
    )
    .unwrap();
    assert!(directory.try_lock().is_err());
    std::fs::remove_file(root.0.join("voice-runtime/.download.lock")).unwrap();
    nix::unistd::mkfifo(
        &root.0.join("voice-runtime/.download.lock"),
        Mode::S_IRUSR | Mode::S_IWUSR,
    )
    .unwrap();
    assert!(
        directory.try_lock().is_err(),
        "FIFO lock must reject immediately rather than block"
    );
}

#[test]
fn directory_handle_pins_writes_if_parent_path_is_replaced() {
    let root = TestRoot::new();
    let outside = TestRoot::new();
    let directory = RuntimeDirectory::open(&root.0).unwrap();
    let target = directory.open_version("fixture-runtime").unwrap();
    std::fs::rename(root.version(), root.0.join("voice-runtime/original")).unwrap();
    symlink(&outside.0, root.version()).unwrap();
    let runtime = fixture(&[b"safe"]);
    install_file(
        &target,
        &runtime.files[0],
        b"safe",
        &input("github").operation_id,
    )
    .unwrap();
    assert!(std::fs::read_dir(&outside.0).unwrap().next().is_none());
    assert_eq!(
        std::fs::read(root.0.join("voice-runtime/original/artifact-0")).unwrap(),
        b"safe"
    );
}

#[test]
fn replacing_published_directory_cannot_report_complete_or_write_through_symlink() {
    struct ReplacingTransport {
        target: PathBuf,
        outside: PathBuf,
    }
    impl Transport for ReplacingTransport {
        fn get(&self, url: &str) -> Result<DownloadResponse, String> {
            std::fs::rename(&self.target, self.target.with_file_name("original")).unwrap();
            symlink(&self.outside, &self.target).unwrap();
            Ok(DownloadResponse {
                status: 200,
                final_url: url.into(),
                content_length: Some(4),
                body: Box::new(Cursor::new(b"safe".to_vec())),
            })
        }
    }
    let root = TestRoot::new();
    let outside = TestRoot::new();
    let directory = RuntimeDirectory::open(&root.0).unwrap();
    let transport = ReplacingTransport {
        target: root.version(),
        outside: outside.0.clone(),
    };
    let runtime = fixture(&[b"safe"]);
    assert!(
        download_runtime(
            &directory,
            &runtime,
            &input("github"),
            &transport,
            |_, _, _| {}
        )
        .is_err()
    );
    assert!(std::fs::read_dir(&outside.0).unwrap().next().is_none());
}

#[test]
fn malformed_start_requests_cannot_choose_urls_or_paths() {
    assert!(input("github").valid());
    assert!(input("relay").valid());
    assert!(!input("https://example.com").valid());
    let mut request = input("github");
    request.operation_id = "../x".into();
    assert!(!request.valid());
    assert!(serde_json::from_str::<StartRequest>(r#"{"source":"github","operation_id":"0123456789abcdef0123456789abcdef","url":"https://attacker.invalid"}"#).is_err());
}

#[test]
fn public_download_requires_explicit_confirmation_and_allowlisted_source() {
    let confirmed: PublicStartRequest =
        serde_json::from_str(r#"{"confirm":true,"source":"relay"}"#).unwrap();
    assert!(confirmed.confirm);
    assert_eq!(confirmed.source, "relay");
    let unchecked: PublicStartRequest =
        serde_json::from_str(r#"{"confirm":false,"source":"github"}"#).unwrap();
    assert!(!unchecked.valid());
    let invalid_source: PublicStartRequest =
        serde_json::from_str(r#"{"confirm":true,"source":"https://attacker.invalid"}"#).unwrap();
    assert!(!invalid_source.valid());
    assert!(
        serde_json::from_str::<PublicStartRequest>(
            r#"{"confirm":true,"source":"github","url":"https://attacker.invalid"}"#
        )
        .is_err()
    );
    let operation_id = new_operation_id().unwrap();
    assert!(input("github").valid());
    assert!(
        StartRequest {
            source: "relay".into(),
            operation_id,
        }
        .valid()
    );
}

#[test]
fn native_generation_change_during_download_stops_before_artifact_publication() {
    use std::sync::atomic::AtomicBool;
    let root = TestRoot::new();
    let data = vec![42u8; 64 * 1024];
    let runtime = fixture(&[data.as_slice()]);
    let directory = RuntimeDirectory::open(&root.0).unwrap();
    let worker_lock = directory.try_lock().unwrap();
    let transport = FakeTransport::new();
    transport.add(
        urls(&runtime, "github", &runtime.files[0])[0].clone(),
        &data,
    );
    let current = AtomicBool::new(true);
    let error = download_runtime_fenced(
        &directory,
        &runtime,
        &input("github"),
        &transport,
        |_, _, _| {
            current.store(false, Ordering::SeqCst);
        },
        &|| current.load(Ordering::SeqCst),
    )
    .unwrap_err();
    assert_eq!(error, "运行时下载操作已失效");
    assert!(!root.version().join(&runtime.files[0].name).exists());
    // Only real worker cleanup releases admission; a stale status is no proof.
    assert!(directory.try_lock().is_err());
    drop(worker_lock);
    assert!(directory.try_lock().is_ok());
}

#[test]
fn stale_native_generation_cannot_reuse_even_a_complete_cache() {
    let root = TestRoot::new();
    let runtime = fixture(&[b"verified bytes"]);
    let directory = RuntimeDirectory::open(&root.0).unwrap();
    let version = directory.open_version(&runtime.version).unwrap();
    install_file(
        &version,
        &runtime.files[0],
        b"verified bytes",
        &input("github").operation_id,
    )
    .unwrap();
    let transport = FakeTransport::new();
    let error = download_runtime_fenced(
        &directory,
        &runtime,
        &input("github"),
        &transport,
        |_, _, _| panic!("stale work must not emit progress"),
        &|| false,
    )
    .unwrap_err();
    assert_eq!(error, "运行时下载操作已失效");
    assert!(transport.calls.lock().unwrap().is_empty());
}

#[test]
fn stale_generation_after_staging_removes_temporary_file_without_replacing_cache() {
    let root = TestRoot::new();
    let runtime = fixture(&[b"new verified bytes"]);
    let directory = RuntimeDirectory::open(&root.0).unwrap();
    let version = directory.open_version(&runtime.version).unwrap();
    std::fs::write(root.version().join(&runtime.files[0].name), b"old cache").unwrap();
    let calls = AtomicU64::new(0);
    let error = install_file_fenced(
        &version,
        &runtime.files[0],
        b"new verified bytes",
        &input("github").operation_id,
        &|| calls.fetch_add(1, Ordering::SeqCst) == 0,
    )
    .unwrap_err();
    assert_eq!(error, "运行时下载操作已失效");
    assert_eq!(
        std::fs::read(root.version().join(&runtime.files[0].name)).unwrap(),
        b"old cache"
    );
    assert_eq!(std::fs::read_dir(root.version()).unwrap().count(), 1);
}
