//! Private, unreachable B1 retained-authority slice. No runtime, listener,
//! production owner, or shipping enable mechanism exists here.
#[path = "phase_b/checked_fd.rs"]
mod checked_fd;
#[path = "phase_b/claim.rs"]
mod claim;
#[path = "phase_b/reports.rs"]
mod reports;

use super::*;
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
};

const TOKEN_ENV: &str = "PROLETARIUS_APP_TOKEN";

enum Entry {
    Production,
    Acceptance(AcceptanceAdmission),
}
struct Candidate {
    capsule: String,
    socket: String,
}
enum Classification {
    Production,
    Candidate(Candidate),
}

/// Only lexical shape. It carries no authorization and never reads environment.
fn classify(arguments: &[OsString]) -> Result<Classification> {
    if !arguments.iter().any(|a| {
        a.as_bytes().starts_with(b"--switch-acceptance")
            || a.as_bytes().starts_with(b"-switch-acceptance")
    }) {
        return Ok(Classification::Production);
    }
    check(arguments.len() == 4)?;
    check(arguments[0].as_bytes() == b"-listen" && arguments[2].as_bytes() == b"-backend=at")?;
    let socket = arguments[1]
        .as_bytes()
        .strip_prefix(b"unix:")
        .ok_or("acceptance shape")?;
    let socket = std::str::from_utf8(socket).map_err(|_| "acceptance shape")?;
    canonical(socket)?;
    canonical(
        socket
            .strip_suffix("/switch.sock")
            .ok_or("acceptance shape")?,
    )?;
    let capsule = arguments[3]
        .as_bytes()
        .strip_prefix(b"--switch-acceptance-capsule=")
        .ok_or("acceptance shape")?;
    let capsule = std::str::from_utf8(capsule).map_err(|_| "acceptance shape")?;
    canonical(capsule)?;
    Ok(Classification::Candidate(Candidate {
        capsule: capsule.into(),
        socket: socket.into(),
    }))
}

/// A private observation, never accepted from JSON/DTO/CLI as process proof.
#[derive(PartialEq, Eq)]
struct ProcessSnapshot {
    pid: i32,
    parent: i32,
    start: (u64, u64),
    executable: PathBuf,
    exec_identity: [u32; 8],
}
trait Proof {
    fn pid(&self) -> i32;
    fn identity(&self, pid: i32) -> Result<ProcessSnapshot>;
    fn environment(&self, name: &str) -> Option<OsString>;
    fn now(&self) -> Result<u64>;
}
struct KernelProof;
impl Proof for KernelProof {
    fn pid(&self) -> i32 {
        std::process::id() as i32
    }
    fn identity(&self, pid: i32) -> Result<ProcessSnapshot> {
        let p = io(proletarius_platform_fs::process_start_identity(pid))?;
        Ok(ProcessSnapshot {
            pid: p.pid(),
            parent: p.parent_pid(),
            start: (p.start_seconds(), p.start_microseconds()),
            executable: p.executable().into(),
            exec_identity: *p.exec_identity(),
        })
    }
    fn environment(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
    fn now(&self) -> Result<u64> {
        now_ms()
    }
}

// Authority is retained only here. No Clone/Default/Deserialize, public field,
// DTO conversion or externally callable constructor is provided.
struct RunBinding {
    manifest: Manifest,
    held: Vec<Held>,
    processes: [ProcessSnapshot; 3], // daemon, App, guardian
    capsule: String,
    socket: String,
    token: OsString,
}
struct AcceptanceAdmission {
    binding: RunBinding,
    terminal: bool,
}
/// Borrowed evidence only; cannot construct or recover an admission.
struct RuntimeProofView<'a> {
    binding: &'a RunBinding,
}
impl RuntimeProofView<'_> {
    fn run_id(&self) -> &str {
        &self.binding.manifest.run_id
    }
    fn daemon_pid(&self) -> i32 {
        self.binding.processes[0].pid
    }
    fn app_pid(&self) -> i32 {
        self.binding.processes[1].pid
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Load,
    BeforeClaim,
    ClaimCreated,
    ClaimWritten,
    ClaimSynced,
    RootSynced,
    Admitted,
}
/// Private injection seam; shipping code has no caller or environment gate.
fn classify_and_admit(
    arguments: &[OsString],
    proof: &impl Proof,
    mut stage: impl FnMut(Stage) -> Result<()>,
) -> Result<Entry> {
    match classify(arguments)? {
        Classification::Production => Ok(Entry::Production),
        Classification::Candidate(candidate) => {
            stage(Stage::Load)?;
            let mut binding = load_binding(candidate, proof)?;
            binding.validate_admission(proof)?;
            stage(Stage::BeforeClaim)?;
            // Atomic create is attempted once. Every later failure preserves
            // the consumed entry and returns Err; no retry/reload/fallback.
            let root = binding
                .held
                .iter()
                .find(|h| h.path == binding.manifest.data_root && h.directory)
                .ok_or("claim root")?;
            let name = format!(
                ".consumed-child-{}",
                binding.manifest.child_nonce.as_ref().ok_or("nonce")?
            );
            let mut claim = File::from(
                openat(
                    &root.file,
                    name.as_str(),
                    OFlag::O_WRONLY
                        | OFlag::O_CREAT
                        | OFlag::O_EXCL
                        | OFlag::O_NOFOLLOW
                        | OFlag::O_CLOEXEC,
                    Mode::from_bits_truncate(0o600),
                )
                .map_err(|_| "acceptance nonce rejected")?,
            );
            stage(Stage::ClaimCreated)?;
            Identity::of(&claim)?.regular(true, binding.manifest.uid as u32)?;
            io(claim.write_all(b"consumed\n"))?;
            stage(Stage::ClaimWritten)?;
            io(claim.sync_all())?;
            stage(Stage::ClaimSynced)?;
            io(root.file.sync_all())?;
            stage(Stage::RootSynced)?;
            binding.held.push(Held {
                path: format!("{}/{}", binding.manifest.data_root, name),
                identity: Identity::of(&claim)?,
                file: claim,
                directory: false,
            });
            binding.validate_admission(proof)?;
            stage(Stage::Admitted)?;
            Ok(Entry::Acceptance(AcceptanceAdmission {
                binding,
                terminal: false,
            }))
        }
    }
}

fn load_binding(candidate: Candidate, proof: &impl Proof) -> Result<RunBinding> {
    let uid = getuid().as_raw();
    let now = proof.now()?;
    let (m, child_value, mut held, _) = read_manifest(&candidate.capsule, uid, true)?;
    common(&m, now, uid, 30000)?;
    check(m.role == "daemon" && m.child_nonce.as_deref().is_some_and(|s| hex(s, 64)))?;
    // Exact argv bytes bind to the validated roots before any nonce attempt.
    check(
        candidate.capsule
            == format!(
                "{}/daemon-{}.json",
                m.data_root,
                m.child_nonce.as_ref().unwrap()
            )
            && candidate.socket == format!("{}/switch.sock", m.data_root),
    )?;
    let parent_path = m.parent_manifest_path.as_deref().ok_or("parent path")?;
    check(parent_path == format!("{}/app.json", m.capsule_root))?;
    let (parent, parent_value, mut parent_held, parent_bytes) =
        read_manifest(parent_path, uid, false)?;
    common(&parent, now, uid, 120000)?;
    check(
        parent.role == "app"
            && m.expires_unix_ms <= parent.expires_unix_ms
            && m.created_unix_ms >= parent.created_unix_ms
            && m.parent_manifest_inode == Some(parent_held.last().unwrap().identity.ino)
            && m.parent_manifest_sha256
                .as_deref()
                .is_some_and(|s| hex(s, 64) && s == format!("{:x}", Sha256::digest(&parent_bytes))),
    )?;
    let mut inherited = child_value.as_object().unwrap().clone();
    for key in CHILD {
        inherited.remove(key);
    }
    for key in ["role", "created_unix_ms", "expires_unix_ms"] {
        inherited.remove(key);
    }
    let mut p_inherited = parent_value.as_object().unwrap().clone();
    for key in ["role", "created_unix_ms", "expires_unix_ms"] {
        p_inherited.remove(key);
    }
    check(inherited == p_inherited)?;
    held.append(&mut parent_held);
    for (root, dev, ino) in [
        (&m.capsule_root, m.capsule_root_dev, m.capsule_root_ino),
        (&m.data_root, m.data_root_dev, m.data_root_ino),
        (&m.evidence_root, m.evidence_root_dev, m.evidence_root_ino),
        (&m.tmp_root, m.tmp_root_dev, m.tmp_root_ino),
    ] {
        let mut directory = walk(root, true)?;
        let id = &directory.last().unwrap().identity;
        check(id.uid == uid && id.mode & 0o7777 == 0o700 && id.dev == dev && id.ino == ino)?;
        held.append(&mut directory);
    }
    let processes = process_chain(&m, proof)?;
    for (path, expected) in [
        (&m.app_path, &m.app_sha256),
        (&m.backend_path, &m.backend_sha256),
        (&m.guardian_path, &m.guardian_sha256),
    ] {
        check(hex(expected, 64) && hash(path, &mut held, uid)? == *expected)?;
    }
    let token = proof
        .environment(TOKEN_ENV)
        .ok_or("acceptance environment")?;
    check(
        (16..=4096).contains(&token.as_bytes().len())
            && token.as_bytes().iter().all(|b| (0x21..=0x7e).contains(b)),
    )?;
    Ok(RunBinding {
        manifest: m,
        held,
        processes,
        capsule: candidate.capsule,
        socket: candidate.socket,
        token,
    })
}

fn process_chain(m: &Manifest, proof: &impl Proof) -> Result<[ProcessSnapshot; 3]> {
    let daemon = proof.identity(proof.pid())?;
    check(
        daemon.pid == proof.pid()
            && daemon.pid > 1
            && daemon.parent > 1
            && m.app_pid == Some(daemon.parent as u64)
            && daemon.executable == PathBuf::from(&m.backend_path),
    )?;
    let app = proof.identity(daemon.parent)?;
    check(
        app.pid == daemon.parent
            && app.parent > 1
            && m.guardian_pid == app.parent as u64
            && app.executable == PathBuf::from(&m.app_path),
    )?;
    let guardian = proof.identity(app.parent)?;
    check(guardian.pid == app.parent && guardian.executable == PathBuf::from(&m.guardian_path))?;
    for p in [&daemon, &app, &guardian] {
        check(
            p.pid > 1
                && p.parent > 0
                && p.parent != p.pid
                && p.start.0 > 0
                && p.start.1 < 1_000_000
                && p.exec_identity != [0; 8],
        )?;
    }
    Ok([daemon, app, guardian])
}
impl RunBinding {
    fn validate_environment(&self, proof: &impl Proof) -> Result<()> {
        let m = &self.manifest;
        for (name, expected) in [
            ("HOME", m.data_root.as_str()),
            ("CFFIXED_USER_HOME", m.data_root.as_str()),
            ("SWITCH_DATA_DIR", m.data_root.as_str()),
            ("TMPDIR", m.tmp_root.as_str()),
            ("BLE_SOCKET", &format!("{}/estk-ble.sock", m.data_root)),
        ] {
            check(proof.environment(name).as_deref() == Some(OsStr::new(expected)))?;
        }
        check(proof.environment(TOKEN_ENV).as_deref() == Some(self.token.as_os_str()))
    }
    fn revalidate(&self, proof: &impl Proof) -> Result<()> {
        revalidate_all(&self.held)?;
        check(self.processes == process_chain(&self.manifest, proof)?)?;
        self.validate_environment(proof)
    }
    fn validate_admission(&self, proof: &impl Proof) -> Result<()> {
        let now = proof.now()?;
        check(self.manifest.created_unix_ms <= now && self.manifest.expires_unix_ms > now)?;
        self.revalidate(proof)?;
        // Work may have crossed expiry; do not authorize using the earlier clock.
        let now = proof.now()?;
        check(self.manifest.created_unix_ms <= now && self.manifest.expires_unix_ms > now)
    }
}
impl AcceptanceAdmission {
    fn runtime_proof(&mut self, proof: &impl Proof) -> Result<RuntimeProofView<'_>> {
        check(!self.terminal)?;
        // Admission TTL is intentionally never consulted in runtime. The
        // retained start/file/environment binding is the sole authority.
        if let Err(error) = self.binding.revalidate(proof) {
            self.terminal = true;
            return Err(error);
        }
        Ok(RuntimeProofView {
            binding: &self.binding,
        })
    }
}

#[cfg(test)]
#[path = "phase_b_tests.rs"]
mod tests;
