#![deny(unsafe_op_in_unsafe_fn)]
//! Immutable Swift admission checkpoint authority. Raw bytes retain the old
//! format; only a confirmed physical checkpoint can produce a POST snapshot.
use nix::{
    fcntl::{AtFlags, OFlag, open, openat},
    libc,
    sys::stat::{Mode, fchmod, fstatat},
};
use proletarius_api_contract::{SmsForwardingHandoffError, SmsForwardingHandoffV1};
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    fs::{File, Metadata},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStringExt, fs::MetadataExt},
    },
    path::PathBuf,
    sync::Arc,
};

mod forwarding_diagnostic;
mod forwarding_seed;

pub const MAX: usize = 8 * 1024 * 1024;
const FILE: &str = "sms-forwarding-swift-checkpoint-v1.json";
const LOCK: &str = ".sms-forwarding-swift-checkpoint-v1.lock";
type Result<T> = std::result::Result<T, i32>;
const UNAVAILABLE: i32 = -1;
const BUSY: i32 = -2;
const TOO_LARGE: i32 = -3;
const INVALID: i32 = -10;

#[unsafe(no_mangle)]
pub extern "C" fn switch_handoff_link_anchor() {}

fn uuid() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|_| UNAVAILABLE)?;
    b[6] = (b[6] & 15) | 64;
    b[8] = (b[8] & 63) | 128;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    ))
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
fn safe_dir(m: &Metadata) -> bool {
    m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o022 == 0
}
fn safe_file(m: &Metadata) -> bool {
    m.is_file()
        && m.uid() == unsafe { libc::geteuid() }
        && m.nlink() == 1
        && m.mode() & 0o7777 == 0o600
}
fn stamp(m: &Metadata) -> (u64, i64, i64, i64, i64) {
    (
        m.size(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}

struct Root {
    path: PathBuf,
    file: File,
}
struct Checkpoint {
    root: Arc<Root>,
    #[cfg(feature = "fault-injection")]
    fault: u8,
}
impl Checkpoint {
    fn open(path: PathBuf) -> Result<Self> {
        if !path.is_absolute() {
            return Err(UNAVAILABLE);
        }
        let file: File = open(
            &path,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| UNAVAILABLE)?
        .into();
        let s = Self {
            root: Arc::new(Root { path, file }),
            #[cfg(feature = "fault-injection")]
            fault: 0,
        };
        s.validate_root()?;
        Ok(s)
    }
    fn validate_root(&self) -> Result<()> {
        let opened = self.root.file.metadata().map_err(|_| UNAVAILABLE)?;
        let named = std::fs::symlink_metadata(&self.root.path).map_err(|_| UNAVAILABLE)?;
        if safe_dir(&opened) && safe_dir(&named) && same(&opened, &named) {
            Ok(())
        } else {
            Err(UNAVAILABLE)
        }
    }
    fn checked(&self, file: &File, name: &str) -> Result<Metadata> {
        let opened = file.metadata().map_err(|_| UNAVAILABLE)?;
        let named = fstatat(&self.root.file, name, AtFlags::AT_SYMLINK_NOFOLLOW)
            .map_err(|_| UNAVAILABLE)?;
        if !safe_file(&opened)
            || named.st_mode & libc::S_IFMT != libc::S_IFREG
            || named.st_uid != unsafe { libc::geteuid() }
            || named.st_nlink != 1
            || named.st_mode & 0o7777 != 0o600
            || opened.dev() != named.st_dev as u64
            || opened.ino() != named.st_ino
        {
            return Err(UNAVAILABLE);
        }
        Ok(opened)
    }
    fn validate(&self, lock: &File) -> Result<()> {
        self.validate_root()?;
        self.checked(lock, LOCK)?;
        Ok(())
    }
    fn sync(&self, file: &File, directory: bool) -> Result<()> {
        #[cfg(feature = "fault-injection")]
        if self.fault == if directory { 2 } else { 1 } {
            return Err(UNAVAILABLE);
        }
        let _ = directory;
        loop {
            match file.sync_all() {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(UNAVAILABLE),
            }
        }
    }
    fn with_lock<T>(&self, body: impl FnOnce(&File) -> Result<T>) -> Result<T> {
        self.validate_root()?;
        let create = openat(
            &self.root.file,
            LOCK,
            OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        );
        let lock: File = match create {
            Ok(fd) => {
                fchmod(&fd, Mode::from_bits_truncate(0o600)).map_err(|_| UNAVAILABLE)?;
                fd.into()
            }
            Err(nix::errno::Errno::EEXIST) => openat(
                &self.root.file,
                LOCK,
                OFlag::O_RDWR | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| UNAVAILABLE)?
            .into(),
            Err(_) => return Err(UNAVAILABLE),
        };
        self.checked(&lock, LOCK)?;
        loop {
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let e = std::io::Error::last_os_error().raw_os_error();
            if e == Some(libc::EINTR) {
                continue;
            }
            return Err(if e == Some(libc::EWOULDBLOCK) || e == Some(libc::EAGAIN) {
                BUSY
            } else {
                UNAVAILABLE
            });
        }
        struct Unlock<'a>(&'a File);
        impl Drop for Unlock<'_> {
            fn drop(&mut self) {
                unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
            }
        }
        let _unlock = Unlock(&lock);
        self.validate(&lock)?;
        let value = body(&lock)?;
        self.validate(&lock)?;
        Ok(value)
    }
    fn read(&self, lock: &File) -> Result<Option<Vec<u8>>> {
        self.validate(lock)?;
        let mut file: File = match openat(
            &self.root.file,
            FILE,
            OFlag::O_RDWR | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd.into(),
            Err(nix::errno::Errno::ENOENT) => return Ok(None),
            Err(_) => return Err(UNAVAILABLE),
        };
        let before = self.checked(&file, FILE)?;
        if before.size() > MAX as u64 {
            return Err(TOO_LARGE);
        }
        let mut bytes = Vec::with_capacity(before.size() as usize);
        (&mut file)
            .take((MAX + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| UNAVAILABLE)?;
        if bytes.len() > MAX {
            return Err(TOO_LARGE);
        }
        let after = self.checked(&file, FILE)?;
        if stamp(&before) != stamp(&after) || bytes.len() as u64 != after.size() {
            return Err(UNAVAILABLE);
        }
        self.validate(lock)?;
        self.sync(&file, false)?;
        self.sync(&self.root.file, true)?;
        if stamp(&after) != stamp(&self.checked(&file, FILE)?) {
            return Err(UNAVAILABLE);
        }
        self.validate(lock)?;
        Ok(Some(bytes))
    }
    fn load(&self) -> Result<Option<Vec<u8>>> {
        self.with_lock(|lock| {
            if let Some(b) = self.read(lock)? {
                return Ok(Some(b));
            }
            self.validate(lock)?;
            self.sync(&self.root.file, true)?;
            self.validate(lock)?;
            Ok(None)
        })
    }
    fn confirm(&self, candidate: &[u8]) -> Result<Vec<u8>> {
        if candidate.len() > MAX {
            return Err(TOO_LARGE);
        }
        self.with_lock(|lock| {
            if let Some(b) = self.read(lock)? {
                return Ok(b);
            }
            let name = format!(".sms-forwarding-swift-checkpoint-v1.{}.tmp", uuid()?);
            let mut file: File = openat(
                &self.root.file,
                name.as_str(),
                OFlag::O_RDWR
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|_| UNAVAILABLE)?
            .into();
            let operation = (|| {
                fchmod(&file, Mode::from_bits_truncate(0o600)).map_err(|_| UNAVAILABLE)?;
                self.validate(lock)?;
                self.checked(&file, &name)?;
                file.write_all(candidate).map_err(|_| UNAVAILABLE)?;
                self.sync(&file, false)?;
                if self.checked(&file, &name)?.size() != candidate.len() as u64 {
                    return Err(UNAVAILABLE);
                }
                self.validate(lock)?;
                match proletarius_platform_fs::rename_regular_noreplace(
                    &self.root.file,
                    &name,
                    &self.root.file,
                    FILE,
                ) {
                    Ok(()) => {}
                    Err(e) if e.raw_os_error() == Some(libc::EEXIST) => {
                        return self.read(lock)?.ok_or(UNAVAILABLE);
                    }
                    Err(_) => return Err(UNAVAILABLE),
                }
                self.validate(lock)?;
                self.sync(&self.root.file, true)?;
                let confirmed = self.read(lock)?.ok_or(UNAVAILABLE)?;
                if confirmed != candidate {
                    return Err(UNAVAILABLE);
                }
                Ok(confirmed)
            })();
            // Only our still-named single-link temporary is removable. After
            // rename/directory-sync failure the visible authority is retained.
            if self.checked(&file, &name).is_ok() {
                let _ = nix::unistd::unlinkat(
                    &self.root.file,
                    name.as_str(),
                    nix::unistd::UnlinkatFlags::NoRemoveDir,
                );
            }
            operation
        })
    }
}

// Foundation JSONDecoder takes the FIRST occurrence of duplicate object keys.
// Preserve that legacy decoding behavior recursively instead of serde Value's
// last-wins map. Known fields are projected below, as Swift ignored unknowns.
struct LegacyValue(Value);
impl<'de> serde::Deserialize<'de> for LegacyValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Value;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_bool<E>(self, v: bool) -> std::result::Result<Value, E> {
                Ok(v.into())
            }
            fn visit_i64<E>(self, v: i64) -> std::result::Result<Value, E> {
                Ok(v.into())
            }
            fn visit_u64<E>(self, v: u64) -> std::result::Result<Value, E> {
                Ok(v.into())
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Value, E> {
                serde_json::Number::from_f64(v)
                    .map(Value::Number)
                    .ok_or_else(|| E::custom("not finite"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Value, E> {
                Ok(v.into())
            }
            fn visit_string<E>(self, v: String) -> std::result::Result<Value, E> {
                Ok(v.into())
            }
            fn visit_none<E>(self) -> std::result::Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_unit<E>(self) -> std::result::Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Value, A::Error> {
                let mut v = Vec::new();
                while let Some(x) = a.next_element::<LegacyValue>()? {
                    v.push(x.0)
                }
                Ok(Value::Array(v))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Value, A::Error> {
                let mut v = serde_json::Map::new();
                while let Some((k, x)) = a.next_entry::<String, LegacyValue>()? {
                    v.entry(k).or_insert(x.0);
                }
                Ok(Value::Object(v))
            }
        }
        d.deserialize_any(V).map(Self)
    }
}
fn parse(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > MAX {
        return Err(INVALID);
    }
    serde_json::from_slice::<LegacyValue>(bytes)
        .map(|x| x.0)
        .map_err(|_| INVALID)
}
fn project(value: &Value, keys: &[&str]) -> Value {
    let mut m = serde_json::Map::new();
    if let Some(o) = value.as_object() {
        for k in keys {
            if let Some(v) = o.get(*k) {
                m.insert((*k).into(), v.clone());
            }
        }
    }
    Value::Object(m)
}
fn decode(bytes: &[u8]) -> Result<SmsForwardingHandoffV1> {
    let v = parse(bytes)?;
    let mut p = project(&v, &["schema_version", "handoff_id"]);
    if p["schema_version"].as_f64() == Some(1.0) {
        p["schema_version"] = json!(1)
    }
    p["device_sms"] = project(
        &v["device_sms"],
        &["accepted_notification_ids", "was_enabled"],
    );
    p["nwait_sms"] = project(&v["nwait_sms"], &["cursor", "seeded", "was_enabled"]);
    let h: SmsForwardingHandoffV1 = serde_json::from_value(p).map_err(|_| INVALID)?;
    h.validate().map_err(|_| INVALID)?;
    Ok(h)
}
fn canonical(bytes: &[u8]) -> Result<Vec<u8>> {
    serde_json::to_vec(&decode(bytes)?).map_err(|_| INVALID)
}
struct Authority {
    checkpoint: Arc<Checkpoint>,
    pending: Option<Vec<u8>>,
}
impl Authority {
    fn persist(&mut self, bytes: Vec<u8>) -> Result<Vec<u8>> {
        decode(&bytes)?;
        self.pending = Some(bytes);
        let confirmed = self.checkpoint.confirm(self.pending.as_ref().unwrap())?;
        let result = canonical(&confirmed)?;
        self.pending = Some(confirmed);
        Ok(result)
    }
    fn local(&mut self, legacy: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
        if let Some(b) = self.checkpoint.load()? {
            let result = canonical(&b)?;
            self.pending = Some(b);
            return Ok(Some(result));
        }
        if let Some(p) = self.pending.clone() {
            return self.persist(p).map(Some);
        }
        if let Some(b) = legacy {
            return self.persist(b.to_vec()).map(Some);
        }
        Ok(None)
    }
    fn remote(
        &mut self,
        accepted: bool,
        snapshot: Option<&[u8]>,
        source: &[u8],
    ) -> Result<Vec<u8>> {
        if accepted {
            return self.persist(snapshot.ok_or(INVALID)?.to_vec());
        }
        if snapshot.is_some() {
            return Err(INVALID);
        }
        // Raw legacy observations precede deduplication; only frozen bytes
        // have the 8 MiB limit, as in the original Swift admission path.
        let mut v = serde_json::from_slice::<LegacyValue>(source)
            .map_err(|_| INVALID)?
            .0;
        let ids = v["device_sms"]["accepted_notification_ids"]
            .as_array()
            .ok_or(INVALID)?;
        let ids: std::collections::BTreeSet<String> = ids
            .iter()
            .map(|x| x.as_str().map(String::from).ok_or(INVALID))
            .collect::<Result<_>>()?;
        v["device_sms"]["accepted_notification_ids"] = json!(ids);
        v["schema_version"] = json!(1);
        v["handoff_id"] = json!("123e4567-e89b-12d3-a456-426614174000");
        let h: SmsForwardingHandoffV1 = serde_json::from_value(v.clone()).map_err(|_| INVALID)?;
        h.validate().map_err(|e| match e {
            SmsForwardingHandoffError::TooManyAcceptedIds => -11,
            SmsForwardingHandoffError::InvalidAcceptedId => -12,
            SmsForwardingHandoffError::CursorTooLong => -13,
            _ => INVALID,
        })?;
        v["handoff_id"] = json!(uuid()?);
        self.persist(serde_json::to_vec(&v).map_err(|_| INVALID)?)
    }
}

struct CheckpointHandle(Option<Arc<Checkpoint>>);
struct Bytes {
    status: i32,
    data: Vec<u8>,
}
fn result(r: Result<Option<Vec<u8>>>) -> *mut c_void {
    let b = match r {
        Ok(Some(data)) => Bytes { status: 1, data },
        Ok(None) => Bytes {
            status: 0,
            data: vec![],
        },
        Err(status) => Bytes {
            status,
            data: vec![],
        },
    };
    Box::into_raw(Box::new(b)).cast()
}
unsafe fn input<'a>(p: *const u8, n: usize) -> Result<&'a [u8]> {
    if n > MAX || (n != 0 && (p.is_null() || (p as usize).checked_add(n).is_none())) {
        return Err(TOO_LARGE);
    }
    Ok(if n == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(p, n) }
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_checkpoint_create(
    path: *const u8,
    n: usize,
) -> *mut c_void {
    let root = unsafe { input(path, n) }
        .ok()
        .and_then(|b| {
            Checkpoint::open(PathBuf::from(std::ffi::OsString::from_vec(b.to_vec()))).ok()
        })
        .map(Arc::new);
    Box::into_raw(Box::new(CheckpointHandle(root))).cast()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_checkpoint_destroy(h: *mut c_void) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h.cast::<CheckpointHandle>()) })
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_checkpoint_io(
    h: *const c_void,
    p: *const u8,
    n: usize,
    confirm: u8,
) -> *mut c_void {
    result((|| {
        let c = unsafe { h.cast::<CheckpointHandle>().as_ref() }
            .and_then(|x| x.0.as_ref())
            .ok_or(UNAVAILABLE)?;
        if confirm == 0 {
            c.load()
        } else if confirm == 1 {
            c.confirm(unsafe { input(p, n) }?).map(Some)
        } else {
            Err(UNAVAILABLE)
        }
    })())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_checkpoint_fault_clone(
    h: *const c_void,
    fault: u8,
) -> *mut c_void {
    #[cfg(feature = "fault-injection")]
    if let Some(c) = unsafe { h.cast::<CheckpointHandle>().as_ref() }
        .and_then(|x| x.0.as_ref())
        .filter(|_| matches!(fault, 1 | 2))
    {
        return Box::into_raw(Box::new(CheckpointHandle(Some(Arc::new(Checkpoint {
            root: c.root.clone(),
            fault,
        })))))
        .cast();
    }
    let _ = (h, fault);
    std::ptr::null_mut()
}
#[unsafe(no_mangle)]
pub extern "C" fn switch_handoff_test_lock(fd: i32, operation: i32) -> i32 {
    #[cfg(feature = "fault-injection")]
    {
        return unsafe { libc::flock(fd, operation) };
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        let _ = (fd, operation);
        -1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_authority_create(h: *const c_void) -> *mut c_void {
    unsafe { h.cast::<CheckpointHandle>().as_ref() }
        .and_then(|x| x.0.clone())
        .map_or(std::ptr::null_mut(), |checkpoint| {
            Box::into_raw(Box::new(Authority {
                checkpoint,
                pending: None,
            }))
            .cast()
        })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_authority_destroy(h: *mut c_void) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h.cast::<Authority>()) })
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_authority_pending_for_test(
    h: *const c_void,
) -> *mut c_void {
    #[cfg(feature = "fault-injection")]
    {
        return result(
            unsafe { h.cast::<Authority>().as_ref() }
                .map(|a| a.pending.clone())
                .ok_or(UNAVAILABLE),
        );
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        let _ = h;
        result(Err(UNAVAILABLE))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_authority_local(
    h: *mut c_void,
    p: *const u8,
    n: usize,
    present: u8,
) -> *mut c_void {
    result((|| {
        let a = unsafe { h.cast::<Authority>().as_mut() }.ok_or(UNAVAILABLE)?;
        if present > 1 {
            return Err(INVALID);
        }
        a.local(if present == 1 {
            Some(unsafe { input(p, n) }?)
        } else {
            None
        })
    })())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_authority_remote(
    h: *mut c_void,
    accepted: u8,
    p: *const u8,
    n: usize,
    present: u8,
    source: *const u8,
    source_n: usize,
) -> *mut c_void {
    result((|| {
        let a = unsafe { h.cast::<Authority>().as_mut() }.ok_or(UNAVAILABLE)?;
        if accepted > 1 || present > 1 {
            return Err(INVALID);
        }
        let source = if accepted == 1 {
            &[][..]
        } else {
            if source_n > isize::MAX as usize
                || (source_n != 0
                    && (source.is_null() || (source as usize).checked_add(source_n).is_none()))
            {
                return Err(INVALID);
            }
            if source_n == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(source, source_n) }
            }
        };
        a.remote(
            accepted == 1,
            if present == 1 {
                Some(unsafe { input(p, n) }?)
            } else {
                None
            },
            source,
        )
        .map(Some)
    })())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_validate(p: *const u8, n: usize) -> *mut c_void {
    result(
        unsafe { input(p, n) }
            .map_err(|_| INVALID)
            .and_then(canonical)
            .map(Some),
    )
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_bytes_status(h: *const c_void) -> i32 {
    unsafe { h.cast::<Bytes>().as_ref() }.map_or(UNAVAILABLE, |x| x.status)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_bytes_length(h: *const c_void) -> usize {
    unsafe { h.cast::<Bytes>().as_ref() }.map_or(0, |x| x.data.len())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_bytes_data(h: *const c_void) -> *const u8 {
    unsafe { h.cast::<Bytes>().as_ref() }.map_or(std::ptr::null(), |x| x.data.as_ptr())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn switch_handoff_bytes_destroy(h: *mut c_void) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h.cast::<Bytes>()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("handoff-core-{}", uuid().unwrap()));
            std::fs::create_dir(&p).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(p)
        }
        fn authority(&self) -> Authority {
            Authority {
                checkpoint: Arc::new(Checkpoint::open(self.0.clone()).unwrap()),
                pending: None,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn source() -> Vec<u8> {
        serde_json::to_vec(&json!({"device_sms":{"accepted_notification_ids":["a".repeat(64),"a".repeat(64)],"was_enabled":true},"nwait_sms":{"seeded":true,"cursor":"opaque"}})).unwrap()
    }
    fn snapshot() -> Vec<u8> {
        let f = Fixture::new();
        f.authority().remote(false, None, &source()).unwrap()
    }
    #[test]
    fn confirmed_file_has_priority_over_legacy_and_remote_candidates() {
        let f = Fixture::new();
        let mut a = f.authority();
        assert!(a.local(None).unwrap().is_none());
        let first = a.remote(false, None, &source()).unwrap();
        let raw = a.checkpoint.load().unwrap().unwrap();
        let mut b = f.authority();
        assert_eq!(b.local(Some(b"corrupt legacy")).unwrap().unwrap(), first);
        assert_eq!(b.remote(true, Some(&snapshot()), &[]).unwrap(), first);
        assert_eq!(b.checkpoint.load().unwrap().unwrap(), raw);
    }
    #[test]
    fn legacy_bytes_and_id_remain_exact_while_post_drops_unknown_fields() {
        let mut v: Value = serde_json::from_slice(&snapshot()).unwrap();
        v["ignored_legacy"] = json!({"not_forwarded":true});
        v["device_sms"]["old_ui_field"] = json!(3);
        let raw = serde_json::to_vec_pretty(&v).unwrap();
        let f = Fixture::new();
        let mut a = f.authority();
        let post = a.local(Some(&raw)).unwrap().unwrap();
        assert_eq!(a.checkpoint.load().unwrap().unwrap(), raw);
        assert_eq!(
            decode(&post).unwrap().handoff_id,
            v["handoff_id"].as_str().unwrap()
        );
        assert!(
            serde_json::from_slice::<Value>(&post)
                .unwrap()
                .get("ignored_legacy")
                .is_none()
        );
    }
    #[test]
    fn legacy_first_duplicate_key_and_integral_float_match_foundation() {
        let s = String::from_utf8(snapshot()).unwrap();
        let id = decode(s.as_bytes()).unwrap().handoff_id;
        let s = s
            .replacen(
                "\"schema_version\":1",
                "\"schema_version\":1.0,\"schema_version\":2",
                1,
            )
            .replacen(
                &format!("\"handoff_id\":\"{id}\""),
                &format!("\"handoff_id\":\"{id}\",\"handoff_id\":\"invalid-later-key\""),
                1,
            );
        assert_eq!(decode(s.as_bytes()).unwrap().handoff_id, id);
    }
    #[test]
    fn corrupt_and_truncated_checkpoints_never_rebase_authority() {
        for raw in [b"{".to_vec(), snapshot()[..10].to_vec(), vec![]] {
            let f = Fixture::new();
            let mut a = f.authority();
            a.checkpoint.confirm(&raw).unwrap();
            let before = std::fs::metadata(f.0.join(FILE)).unwrap();
            assert_eq!(a.local(Some(&snapshot())), Err(INVALID));
            assert_eq!(a.remote(false, None, &source()), Err(INVALID));
            assert_eq!(a.checkpoint.load().unwrap().unwrap(), raw);
            assert_eq!(
                std::fs::metadata(f.0.join(FILE)).unwrap().ino(),
                before.ino()
            );
        }
    }
    #[test]
    fn inode_and_lock_replacement_are_rejected_under_the_actual_lock() {
        let f = Fixture::new();
        let c = f.authority().checkpoint;
        let raw = snapshot();
        c.confirm(&raw).unwrap();
        let result = c.with_lock(|lock| {
            let held: File = openat(
                &c.root.file,
                FILE,
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW,
                Mode::empty(),
            )
            .unwrap()
            .into();
            std::fs::rename(f.0.join(FILE), f.0.join("original")).unwrap();
            std::fs::write(f.0.join(FILE), &raw).unwrap();
            std::fs::set_permissions(f.0.join(FILE), std::fs::Permissions::from_mode(0o600))
                .unwrap();
            assert_eq!(c.checked(&held, FILE).unwrap_err(), UNAVAILABLE);
            std::fs::rename(f.0.join(LOCK), f.0.join("old-lock")).unwrap();
            std::fs::write(f.0.join(LOCK), []).unwrap();
            std::fs::set_permissions(f.0.join(LOCK), std::fs::Permissions::from_mode(0o600))
                .unwrap();
            assert_eq!(c.read(lock), Err(UNAVAILABLE));
            Ok(())
        });
        assert_eq!(result, Err(UNAVAILABLE));
    }
    #[cfg(feature = "fault-injection")]
    #[test]
    fn pending_memory_never_returns_post_bytes_until_real_durable_confirmation() {
        let f = Fixture::new();
        let mut a = f.authority();
        a.checkpoint = Arc::new(Checkpoint {
            root: a.checkpoint.root.clone(),
            fault: 1,
        });
        let failed = a.remote(false, None, &source());
        assert_eq!(failed, Err(UNAVAILABLE));
        assert!(!f.0.join(FILE).exists());
        let pending = a.pending.clone().unwrap();
        let pending_id = decode(&pending).unwrap().handoff_id;
        let reply = result(failed.map(Some));
        unsafe {
            assert_eq!((*reply.cast::<Bytes>()).status, UNAVAILABLE);
            assert!((*reply.cast::<Bytes>()).data.is_empty());
            switch_handoff_bytes_destroy(reply);
        }
        assert_eq!(a.local(Some(&snapshot())), Err(UNAVAILABLE));
        a.checkpoint = Arc::new(Checkpoint {
            root: a.checkpoint.root.clone(),
            fault: 0,
        });
        let confirmed = a.local(Some(b"changed legacy")).unwrap().unwrap();
        assert_eq!(decode(&confirmed).unwrap().handoff_id, pending_id);
        assert_eq!(a.checkpoint.load().unwrap().unwrap(), pending);
    }
    #[cfg(feature = "fault-injection")]
    #[test]
    fn directory_sync_failure_retains_exact_first_commit_without_ack() {
        let f = Fixture::new();
        let mut a = f.authority();
        a.checkpoint = Arc::new(Checkpoint {
            root: a.checkpoint.root.clone(),
            fault: 2,
        });
        assert_eq!(a.remote(false, None, &source()), Err(UNAVAILABLE));
        let raw = std::fs::read(f.0.join(FILE)).unwrap();
        assert_eq!(raw, a.pending.clone().unwrap());
        assert_eq!(std::fs::metadata(f.0.join(FILE)).unwrap().nlink(), 1);
        assert_eq!(a.local(None), Err(UNAVAILABLE));
        let mut real = f.authority();
        let confirmed = real.local(None).unwrap().unwrap();
        assert_eq!(
            decode(&confirmed).unwrap().handoff_id,
            decode(&raw).unwrap().handoff_id
        );
        assert_eq!(real.checkpoint.confirm(&snapshot()).unwrap(), raw);
    }
    #[test]
    fn raw_legacy_duplicates_are_deduplicated_before_checkpoint_size_limit() {
        let f = Fixture::new();
        let mut a = f.authority();
        let mut v: Value = serde_json::from_slice(&source()).unwrap();
        v["device_sms"]["accepted_notification_ids"] = json!(vec!["a".repeat(64); 130_000]);
        let source = serde_json::to_vec(&v).unwrap();
        assert!(source.len() > MAX);
        let post = a.remote(false, None, &source).unwrap();
        assert_eq!(
            decode(&post)
                .unwrap()
                .device_sms
                .accepted_notification_ids
                .len(),
            1
        );
    }
}
