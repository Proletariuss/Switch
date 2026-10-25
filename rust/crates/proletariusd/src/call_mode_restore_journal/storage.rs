use super::*;
use nix::fcntl::{AtFlags, OFlag, openat};
use nix::sys::stat::{FileStat, Mode, fstatat, mkdirat};
use nix::unistd::{UnlinkatFlags, linkat, unlinkat};
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::io::{self, Write};
use std::os::unix::fs::{FileExt, MetadataExt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity(u64, u64);
impl Identity {
    fn file(meta: &Metadata) -> Self {
        Self(meta.dev(), meta.ino())
    }
    fn named(meta: &FileStat) -> Self {
        Self(meta.st_dev as u64, meta.st_ino as u64)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stamp {
    identity: Identity,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    size: u64,
    mt: (i64, i64),
    ct: (i64, i64),
}
impl Stamp {
    fn of(meta: &Metadata) -> Self {
        Self {
            identity: Identity::file(meta),
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            links: meta.nlink(),
            size: meta.len(),
            mt: (meta.mtime(), meta.mtime_nsec()),
            ct: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn directory(meta: &Metadata, private: bool) -> io::Result<()> {
    if !meta.is_dir()
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o7022 != 0
        || private && meta.mode() & 0o777 != 0o700
    {
        return Err(invalid(
            "restore journal directory shape or ownership changed",
        ));
    }
    Ok(())
}
fn named_directory(meta: &FileStat, private: bool) -> io::Result<()> {
    if meta.st_mode & nix::libc::S_IFMT != nix::libc::S_IFDIR
        || meta.st_uid != nix::unistd::geteuid().as_raw()
        || meta.st_mode as u32 & 0o7022 != 0
        || private && meta.st_mode as u32 & 0o777 != 0o700
    {
        return Err(invalid("restore journal named directory is unsafe"));
    }
    Ok(())
}
fn open_directory(parent: &File, name: &str, private: bool) -> io::Result<File> {
    let named = fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    named_directory(&named, private)?;
    let file = File::from(openat(
        parent,
        name,
        OFlag::O_RDONLY
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    directory(&metadata, private)?;
    if Identity::file(&metadata) != Identity::named(&named) {
        return Err(invalid("restore journal directory changed while opening"));
    }
    Ok(file)
}
fn optional_directory(parent: &File, name: &str) -> io::Result<Option<File>> {
    match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Err(nix::errno::Errno::ENOENT) => Ok(None),
        Err(error) => Err(error.into()),
        Ok(_) => open_directory(parent, name, true).map(Some),
    }
}
fn check_directory_name(
    parent: &File,
    name: &str,
    held: &File,
    identity: Identity,
) -> io::Result<()> {
    let metadata = held.metadata()?;
    directory(&metadata, true)?;
    let named = fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    named_directory(&named, true)?;
    if Identity::file(&metadata) != identity || Identity::named(&named) != identity {
        return Err(invalid("restore journal directory namespace changed"));
    }
    Ok(())
}
fn record_name(sequence: u64) -> String {
    format!("{sequence:04}.json")
}
struct Saved {
    file: File,
    name: String,
    identity: Identity,
    payload: Vec<u8>,
    links: u64,
    stamp: Stamp,
}
impl Saved {
    fn check(&self, parent: &File) -> io::Result<()> {
        let before = self.file.metadata()?;
        check_record_shape(&before, self.links)?;
        if Stamp::of(&before) != self.stamp {
            return Err(invalid("immutable restore journal record metadata changed"));
        }
        let named = fstatat(parent, self.name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)?;
        if Identity::file(&before) != self.identity
            || Identity::named(&named) != self.identity
            || named.st_mode & nix::libc::S_IFMT != nix::libc::S_IFREG
            || named.st_nlink as u64 != self.links
            || named.st_mode as u32 & 0o7777 != 0o600
            || named.st_uid != before.uid()
            || named.st_size as u64 != self.payload.len() as u64
            || before.len() != self.payload.len() as u64
        {
            return Err(invalid("restore journal record namespace or shape changed"));
        }
        if read_bounded(&self.file)? != self.payload {
            return Err(invalid("restore journal record bytes changed"));
        }
        let after = self.file.metadata()?;
        let after_named = fstatat(parent, self.name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)?;
        if Stamp::of(&before) != Stamp::of(&after)
            || Identity::named(&after_named) != self.identity
            || after_named.st_nlink as u64 != self.links
            || after_named.st_size as u64 != after.len()
            || after_named.st_mode as u32 != after.mode()
            || after_named.st_uid != after.uid()
        {
            return Err(invalid("restore journal record changed during readback"));
        }
        Ok(())
    }
}
fn check_record_shape(meta: &Metadata, links: u64) -> io::Result<()> {
    if !meta.is_file()
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o7777 != 0o600
        || meta.nlink() != links
        || meta.len() == 0
        || meta.len() > MAX_RECORD_BYTES as u64
    {
        return Err(invalid(
            "restore journal record is not bounded private evidence",
        ));
    }
    Ok(())
}
fn read_bounded(file: &File) -> io::Result<Vec<u8>> {
    let length = file.metadata()?.len();
    if length == 0 || length > MAX_RECORD_BYTES as u64 {
        return Err(invalid("restore journal record exceeds byte bound"));
    }
    let mut bytes = vec![0; length as usize + 1];
    let mut offset = 0;
    while offset < bytes.len() {
        match file.read_at(&mut bytes[offset..], offset as u64) {
            Ok(0) => break,
            Ok(count) => offset += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    if offset != length as usize {
        return Err(invalid("restore journal record length changed"));
    }
    bytes.truncate(offset);
    Ok(bytes)
}
fn open_saved(parent: &File, name: String) -> io::Result<Saved> {
    let named = fstatat(parent, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)?;
    if named.st_mode & nix::libc::S_IFMT != nix::libc::S_IFREG
        || named.st_nlink != 1
        || named.st_uid != nix::unistd::geteuid().as_raw()
        || named.st_mode as u32 & 0o7777 != 0o600
        || named.st_size <= 0
        || named.st_size as usize > MAX_RECORD_BYTES
    {
        return Err(invalid("unsafe restore journal record entry"));
    }
    let file = File::from(openat(
        parent,
        name.as_str(),
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let payload = read_bounded(&file)?;
    let stamp = Stamp::of(&file.metadata()?);
    let saved = Saved {
        identity: Identity::named(&named),
        file,
        name,
        payload,
        links: 1,
        stamp,
    };
    saved.check(parent)?;
    Ok(saved)
}

pub(super) struct Store {
    active: File,
    root_identity: Identity,
    active_identity: Identity,
    active_stamp: Stamp,
    records: Vec<Saved>,
    run_id: String,
    total_bytes: usize,
    archived: Option<Archived>,
    #[cfg(test)]
    hook: Option<TestHook>,
    #[cfg(test)]
    fault: Option<Box<dyn FnMut(Point) -> io::Result<()> + Send>>,
}
struct Archived {
    parent: File,
    parent_identity: Identity,
    run: File,
    run_identity: Identity,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Point {
    FileSync,
    DirectorySync,
    RootSync,
    Readback,
    ArchiveLink,
    ArchiveSync,
    ActiveRemove,
    FinalRootSync,
}
impl Store {
    pub(super) fn create(
        root: &File,
        intent: Intent,
        backup_id: String,
        backup_sha256: String,
    ) -> Result<Self, Error> {
        directory(&root.metadata()?, false)?;
        if !crate::call_mode_backups::valid_backup_id(&backup_id) || !digest(&backup_sha256) {
            return Err(Error::invalid("invalid captured backup journal identity"));
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| Error::invalid("cannot generate restore journal id"))?;
        let run_id: String = random.iter().map(|b| format!("{b:02x}")).collect();
        #[cfg(test)]
        let hook = lookup_hook(root)?;
        #[cfg(test)]
        if let Some(hook) = &hook {
            hook(
                TestPhase::BeforeIntent,
                &DiagnosticSummary {
                    run_id: run_id.clone(),
                    sequence: 0,
                    prepared_steps: 0,
                    unknown_steps: 0,
                    receipt_present: false,
                    target_verified: false,
                    released: false,
                },
            );
        }
        let recovery_origin =
            crate::startup_recovery::reconcile::ledger::capture_normal_journal_origin(root)?;
        // Existing evidence is never adopted, truncated, repaired or removed.
        mkdirat(root, ACTIVE_NAME, Mode::S_IRWXU)?;
        let active = open_directory(root, ACTIVE_NAME, true)?;
        let active_identity = Identity::file(&active.metadata()?);
        let mut store = Self {
            root_identity: Identity::file(&root.metadata()?),
            active_stamp: Stamp::of(&active.metadata()?),
            active,
            active_identity,
            records: Vec::new(),
            run_id,
            total_bytes: 0,
            archived: None,
            #[cfg(test)]
            hook,
            #[cfg(test)]
            fault: None,
        };
        store.append(
            root,
            Event::Intent {
                recovery_origin,
                intent,
                backup_id,
                backup_sha256,
            },
        )?;
        Ok(store)
    }
    pub(super) fn validate(&self, root: &File) -> io::Result<()> {
        let meta = root.metadata()?;
        directory(&meta, false)?;
        if Identity::file(&meta) != self.root_identity {
            return Err(invalid("restore journal held root changed"));
        }
        check_directory_name(root, ACTIVE_NAME, &self.active, self.active_identity)?;
        if Stamp::of(&self.active.metadata()?) != self.active_stamp {
            return Err(invalid(
                "restore journal active directory changed outside writer",
            ));
        }
        let mut names = proletarius_platform_fs::directory_names(&self.active, MAX_RECORDS)?;
        names.sort();
        let expected: Vec<_> = self
            .records
            .iter()
            .map(|s| std::ffi::OsString::from(&s.name))
            .collect();
        if names != expected {
            return Err(invalid("restore journal inventory changed"));
        }
        for record in &self.records {
            record.check(&self.active)?;
        }
        check_directory_name(root, ACTIVE_NAME, &self.active, self.active_identity)?;
        if Stamp::of(&self.active.metadata()?) != self.active_stamp {
            return Err(invalid(
                "restore journal active directory changed while validating",
            ));
        }
        Ok(())
    }
    fn at(&mut self, point: Point) -> io::Result<()> {
        #[cfg(test)]
        if let Some(fault) = &mut self.fault {
            fault(point)?;
        }
        #[cfg(not(test))]
        let _ = point;
        Ok(())
    }
    fn sync_file(&mut self, file: &File, point: Point) -> io::Result<()> {
        self.at(point)?;
        file.sync_all()
    }
    pub(super) fn append(&mut self, root: &File, event: Event) -> Result<u64, Error> {
        self.validate(root)?;
        if self.records.len() >= MAX_RECORDS {
            return Err(Error::invalid("restore journal sequence bound reached"));
        }
        let sequence = self.records.len() as u64 + 1;
        let record = Record {
            schema: 1,
            run_id: self.run_id.clone(),
            sequence,
            previous_sha256: self.records.last().map(|s| hash(&s.payload)),
            event,
        };
        let mut payload = serde_json::to_vec(&record)
            .map_err(|_| Error::invalid("cannot encode restore journal record"))?;
        payload.push(b'\n');
        if payload.len() > MAX_RECORD_BYTES || self.total_bytes + payload.len() > MAX_TOTAL_BYTES {
            return Err(Error::invalid("restore journal byte bound reached"));
        }
        let root_before = Stamp::of(&root.metadata()?);
        let name = record_name(sequence);
        let mut file = File::from(openat(
            &self.active,
            name.as_str(),
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_EXCL
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )?);
        // On every error leave the incomplete immutable entry in the active
        // namespace; restart must diagnose it, never infer NotSubmitted.
        file.write_all(&payload)?;
        let stamp = Stamp::of(&file.metadata()?);
        let saved = Saved {
            identity: Identity::file(&file.metadata()?),
            file,
            name,
            payload,
            links: 1,
            stamp,
        };
        self.sync_file(&saved.file, Point::FileSync)?;
        self.at(Point::Readback)?;
        saved.check(&self.active)?;
        self.at(Point::DirectorySync)?;
        self.active.sync_all()?;
        saved.check(&self.active)?;
        self.sync_file(root, Point::RootSync)?;
        saved.check(&self.active)?;
        if Stamp::of(&root.metadata()?) != root_before {
            return Err(Error::invalid(
                "restore journal root namespace changed during record persistence",
            ));
        }
        self.active_stamp = Stamp::of(&self.active.metadata()?);
        self.total_bytes += saved.payload.len();
        self.records.push(saved);
        self.validate(root)?;
        #[cfg(test)]
        self.emit_for(&record.event)?;
        Ok(sequence)
    }
    pub(super) fn archive(&mut self, root: &File) -> Result<(), Error> {
        self.validate(root)?;
        // Exclusive run-directory creation is portable no-overwrite. Link each
        // pinned immutable inode before removing only its original active name.
        match mkdirat(root, ARCHIVE_NAME, Mode::S_IRWXU) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
            Err(e) => return Err(e.into()),
        }
        let archive = open_directory(root, ARCHIVE_NAME, true)?;
        let archive_id = Identity::file(&archive.metadata()?);
        mkdirat(&archive, self.run_id.as_str(), Mode::S_IRWXU)?;
        let run = open_directory(&archive, &self.run_id, true)?;
        let run_id = Identity::file(&run.metadata()?);
        for index in 0..self.records.len() {
            self.validate(root)?;
            self.at(Point::ArchiveLink)?;
            let saved = &mut self.records[index];
            linkat(
                &self.active,
                saved.name.as_str(),
                &run,
                saved.name.as_str(),
                AtFlags::empty(),
            )?;
            saved.links = 2;
            saved.stamp = Stamp::of(&saved.file.metadata()?);
            saved.check(&self.active)?;
            saved.check(&run)?;
            saved.file.sync_all()?;
        }
        self.at(Point::ArchiveSync)?;
        run.sync_all()?;
        archive.sync_all()?;
        root.sync_all()?;
        check_directory_name(root, ARCHIVE_NAME, &archive, archive_id)?;
        check_directory_name(&archive, &self.run_id, &run, run_id)?;
        for saved in &self.records {
            saved.check(&self.active)?;
            saved.check(&run)?;
        }
        self.at(Point::ActiveRemove)?;
        for saved in &mut self.records {
            saved.check(&self.active)?;
            unlinkat(
                &self.active,
                saved.name.as_str(),
                UnlinkatFlags::NoRemoveDir,
            )?;
            saved.links = 1;
            saved.stamp = Stamp::of(&saved.file.metadata()?);
            saved.check(&run)?;
        }
        self.active.sync_all()?;
        check_directory_name(root, ACTIVE_NAME, &self.active, self.active_identity)?;
        unlinkat(root, ACTIVE_NAME, UnlinkatFlags::RemoveDir)?;
        self.sync_file(root, Point::FinalRootSync)?;
        check_directory_name(root, ARCHIVE_NAME, &archive, archive_id)?;
        check_directory_name(&archive, &self.run_id, &run, run_id)?;
        for saved in &self.records {
            saved.check(&run)?;
            saved.file.sync_all()?;
            saved.check(&run)?;
        }
        run.sync_all()?;
        archive.sync_all()?;
        root.sync_all()?;
        self.archived = Some(Archived {
            parent: archive,
            parent_identity: archive_id,
            run,
            run_identity: run_id,
        });
        #[cfg(test)]
        self.emit(TestPhase::Archived)?;
        Ok(())
    }
    pub(super) fn validate_archived(&self, root: &File) -> io::Result<()> {
        let Some(archived) = &self.archived else {
            return Err(invalid("restore archive was not completed by this writer"));
        };
        let root_before = Stamp::of(&root.metadata()?);
        directory(&root.metadata()?, false)?;
        if root_before.identity != self.root_identity {
            return Err(invalid("completed restore belongs to another root"));
        }
        check_directory_name(
            root,
            ARCHIVE_NAME,
            &archived.parent,
            archived.parent_identity,
        )?;
        check_directory_name(
            &archived.parent,
            &self.run_id,
            &archived.run,
            archived.run_identity,
        )?;
        let run_before = Stamp::of(&archived.run.metadata()?);
        let mut names = proletarius_platform_fs::directory_names(&archived.run, MAX_RECORDS)?;
        names.sort();
        let expected: Vec<_> = self
            .records
            .iter()
            .map(|s| std::ffi::OsString::from(&s.name))
            .collect();
        if names != expected {
            return Err(invalid("completed restore archive inventory changed"));
        }
        for saved in &self.records {
            saved.check(&archived.run)?;
        }
        let diagnostic = read_summary(&archived.run)?;
        if diagnostic.run_id != self.run_id
            || diagnostic.sequence != self.records.len() as u64
            || !diagnostic.receipt_present
            || !diagnostic.target_verified
            || !diagnostic.released
            || diagnostic.unknown_steps != 0
        {
            return Err(invalid(
                "completed restore archive no longer has its verified receipt chain",
            ));
        }
        check_directory_name(
            root,
            ARCHIVE_NAME,
            &archived.parent,
            archived.parent_identity,
        )?;
        check_directory_name(
            &archived.parent,
            &self.run_id,
            &archived.run,
            archived.run_identity,
        )?;
        if Stamp::of(&root.metadata()?) != root_before
            || Stamp::of(&archived.run.metadata()?) != run_before
        {
            return Err(invalid(
                "completed restore archive changed during validation",
            ));
        }
        match fstatat(root, ACTIVE_NAME, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Err(nix::errno::Errno::ENOENT) => Ok(()),
            Err(error) => Err(error.into()),
            Ok(_) => Err(invalid("a restore active namespace exists after archival")),
        }
    }
}

fn summary(records: &[Record]) -> io::Result<DiagnosticSummary> {
    let Some(first) = records.first() else {
        return Err(invalid("active restore journal lacks durable intent"));
    };
    let Event::Intent {
        intent,
        backup_id,
        backup_sha256,
        ..
    } = &first.event
    else {
        return Err(invalid("restore journal first record is not intent"));
    };
    intent
        .validate()
        .map_err(|_| invalid("invalid restore intent"))?;
    if !crate::call_mode_backups::valid_backup_id(backup_id) || !digest(backup_sha256) {
        return Err(invalid("invalid restore backup evidence"));
    }
    let mut steps: Vec<(u64, Mutation, Option<Outcome>)> = Vec::new();
    let mut safety = false;
    let mut target: Option<Receipt> = None;
    let mut released = false;
    let mut receipt = false;
    for record in &records[1..] {
        if receipt {
            return Err(invalid("restore journal has events after final receipt"));
        }
        match &record.event {
            Event::Intent { .. } => return Err(invalid("duplicate restore intent")),
            Event::SafetyCheckpoint {
                backup_id,
                backup_sha256,
            } => {
                if safety
                    || !steps.is_empty()
                    || target.is_some()
                    || released
                    || !crate::call_mode_backups::valid_backup_id(backup_id)
                    || !digest(backup_sha256)
                {
                    return Err(invalid("invalid restore safety checkpoint order"));
                }
                safety = true;
            }
            Event::Prepare { mutation } => {
                mutation
                    .validate(intent)
                    .map_err(|_| invalid("mutation outside restore intent"))?;
                if !safety
                    || target.is_some()
                    || released
                    || steps.len() >= MAX_MUTATIONS
                    || steps.iter().any(|(_, m, o)| {
                        m.operation_id == mutation.operation_id
                            || !matches!(
                                o,
                                Some(
                                    Outcome::VerifiedReadback
                                        | Outcome::NotSubmitted
                                        | Outcome::KnownRejected
                                )
                            )
                    })
                {
                    return Err(invalid("unresolved or repeated restore mutation"));
                }
                steps.push((record.sequence, mutation.clone(), None));
            }
            Event::Outcome {
                prepare_sequence,
                outcome,
            } => {
                if target.is_some() || released {
                    return Err(invalid("outcome after target or release checkpoint"));
                }
                if *outcome == Outcome::VerifiedReadback {
                    return Err(invalid(
                        "untyped verified outcome is not durable readback evidence",
                    ));
                }
                let step = steps
                    .iter_mut()
                    .find(|s| s.0 == *prepare_sequence)
                    .ok_or_else(|| invalid("outcome has no prepared mutation"))?;
                if step.2.is_some()
                    && !(step.2 == Some(Outcome::Acknowledged)
                        && matches!(outcome, Outcome::SubmittedUnknown))
                {
                    return Err(invalid("restore outcome overwritten"));
                }
                step.2 = Some(*outcome);
            }
            Event::VerifiedReadback {
                prepare_sequence,
                readback,
            } => {
                if target.is_some() || released {
                    return Err(invalid("typed readback after target/release checkpoint"));
                }
                let step = steps
                    .iter_mut()
                    .find(|s| s.0 == *prepare_sequence)
                    .ok_or_else(|| invalid("readback has no prepared mutation"))?;
                if step.2 != Some(Outcome::Acknowledged) || !readback.validate(&step.1, intent) {
                    return Err(invalid(
                        "typed readback lacks ACK or exceeds original/target values",
                    ));
                }
                step.2 = Some(Outcome::VerifiedReadback);
            }
            Event::RebootSuccessorVerified { prepare_sequence } => {
                if target.is_some() || released {
                    return Err(invalid("successor after target or release checkpoint"));
                }
                let step = steps
                    .iter_mut()
                    .find(|s| s.0 == *prepare_sequence)
                    .ok_or_else(|| invalid("successor has no prepared reboot"))?;
                if step.1.kind != MutationKind::Reboot
                    || !matches!(
                        step.2,
                        Some(Outcome::Acknowledged | Outcome::SubmittedUnknown)
                    )
                {
                    return Err(invalid("invalid reboot successor refinement"));
                }
                step.2 = Some(Outcome::VerifiedReadback);
            }
            Event::TargetVerified { receipt: value } => {
                if target.is_some()
                    || released
                    || steps.iter().any(|s| s.2 != Some(Outcome::VerifiedReadback))
                    || value.module_imei != intent.module_imei
                    || value.usb != intent.target_usb
                    || value.voice != intent.target_voice
                    || value.restored_at.len() > 64
                {
                    return Err(invalid("invalid restore target checkpoint"));
                }
                target = Some(value.clone());
            }
            Event::Released => {
                if released {
                    return Err(invalid("invalid restore release checkpoint"));
                }
                released = true;
            }
            Event::Receipt { receipt: value } => {
                if !released
                    || !target
                        .as_ref()
                        .is_some_and(|target| same_target(target, value))
                    || value.restored_at.is_empty()
                    || value.restored_at.len() > 64
                {
                    return Err(invalid(
                        "final restore receipt lacks target/release evidence",
                    ));
                }
                receipt = true;
            }
        }
    }
    Ok(DiagnosticSummary {
        run_id: first.run_id.clone(),
        sequence: records.len() as u64,
        prepared_steps: steps.len(),
        unknown_steps: steps
            .iter()
            .filter(|s| {
                matches!(
                    s.2,
                    None | Some(Outcome::Acknowledged | Outcome::SubmittedUnknown)
                )
            })
            .count(),
        receipt_present: receipt,
        target_verified: target.is_some(),
        released,
    })
}
struct ParsedRecords {
    summary: DiagnosticSummary,
    records: Vec<Record>,
    files: Vec<Saved>,
    stamp: Stamp,
    names: Vec<std::ffi::OsString>,
}
fn read_records(directory: &File) -> io::Result<ParsedRecords> {
    let before = Stamp::of(&directory.metadata()?);
    directory_check_private(directory)?;
    let mut names = proletarius_platform_fs::directory_names(directory, MAX_RECORDS)?;
    names.sort();
    if names.is_empty() {
        return Err(invalid("active restore journal is empty"));
    }
    let mut records = Vec::new();
    let mut saved_files = Vec::new();
    let mut previous = None;
    let mut bytes = 0;
    for (index, name) in names.iter().enumerate() {
        if name != &std::ffi::OsString::from(record_name(index as u64 + 1)) {
            return Err(invalid("restore journal contains unexpected names"));
        }
        let saved = open_saved(directory, record_name(index as u64 + 1))?;
        bytes += saved.payload.len();
        if bytes > MAX_TOTAL_BYTES {
            return Err(invalid("restore journal exceeds total byte bound"));
        }
        let record: Record = serde_json::from_slice(&saved.payload)
            .map_err(|_| invalid("restore journal record cannot be decoded"))?;
        if record.schema != 1
            || record.sequence != index as u64 + 1
            || record.previous_sha256 != previous
            || record.run_id.len() != 32
            || !record
                .run_id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || records
                .first()
                .is_some_and(|first: &Record| first.run_id != record.run_id)
        {
            return Err(invalid("restore journal sequence or hash chain changed"));
        }
        previous = Some(hash(&saved.payload));
        records.push(record);
        saved_files.push(saved);
    }
    let result = summary(&records)?;
    for saved in &saved_files {
        saved.check(directory)?;
    }
    let mut final_names = proletarius_platform_fs::directory_names(directory, MAX_RECORDS)?;
    final_names.sort();
    if names != final_names || Stamp::of(&directory.metadata()?) != before {
        return Err(invalid("restore journal changed during diagnostic scan"));
    }
    Ok(ParsedRecords {
        summary: result,
        records,
        files: saved_files,
        stamp: before,
        names,
    })
}
fn read_summary(directory: &File) -> io::Result<DiagnosticSummary> {
    read_records(directory).map(|parsed| parsed.summary)
}

/// Pinned immutable input. This is intentionally not a live writable Store:
/// startup readers cannot append outcomes, replay a token or archive evidence.
pub(super) struct RecoveryInput {
    active: File,
    identity: Identity,
    parsed: ParsedRecords,
    digest: String,
}
impl RecoveryInput {
    pub(super) fn capture(root: &File) -> io::Result<Option<Self>> {
        directory(&root.metadata()?, false)?;
        let before = Stamp::of(&root.metadata()?);
        let Some(active) = optional_directory(root, ACTIVE_NAME)? else {
            if Stamp::of(&root.metadata()?) != before {
                return Err(invalid(
                    "root changed during absent recovery journal lookup",
                ));
            }
            return Ok(None);
        };
        let identity = Identity::file(&active.metadata()?);
        let parsed = read_records(&active)?;
        // The last immutable record commits the complete validated hash chain.
        let digest = hash(
            &parsed
                .files
                .last()
                .ok_or_else(|| invalid("empty recovery journal"))?
                .payload,
        );
        let input = Self {
            active,
            identity,
            parsed,
            digest,
        };
        input.verify(root)?;
        if Stamp::of(&root.metadata()?) != before {
            return Err(invalid("root changed during recovery journal capture"));
        }
        Ok(Some(input))
    }
    pub(super) fn verify(&self, root: &File) -> io::Result<()> {
        directory(&root.metadata()?, false)?;
        check_directory_name(root, ACTIVE_NAME, &self.active, self.identity)?;
        if Stamp::of(&self.active.metadata()?) != self.parsed.stamp {
            return Err(invalid("captured recovery journal directory changed"));
        }
        let mut names = proletarius_platform_fs::directory_names(&self.active, MAX_RECORDS)?;
        names.sort();
        if names != self.parsed.names {
            return Err(invalid("captured recovery journal record set changed"));
        }
        for file in &self.parsed.files {
            file.check(&self.active)?;
        }
        let mut after = proletarius_platform_fs::directory_names(&self.active, MAX_RECORDS)?;
        after.sort();
        check_directory_name(root, ACTIVE_NAME, &self.active, self.identity)?;
        if after != names || Stamp::of(&self.active.metadata()?) != self.parsed.stamp {
            return Err(invalid(
                "captured recovery journal changed during verification",
            ));
        }
        Ok(())
    }
    pub(super) fn summary(&self) -> DiagnosticSummary {
        self.parsed.summary.clone()
    }
    pub(super) fn intent(&self) -> &Intent {
        match &self.parsed.records[0].event {
            Event::Intent { intent, .. } => intent,
            _ => unreachable!("validated first event"),
        }
    }
    pub(super) fn events(&self) -> impl Iterator<Item = &Event> {
        self.parsed.records.iter().map(|record| &record.event)
    }
    pub(super) fn backup_evidence(&self) -> (&str, &str) {
        match &self.parsed.records[0].event {
            Event::Intent {
                backup_id,
                backup_sha256,
                ..
            } => (backup_id, backup_sha256),
            _ => unreachable!("validated first event"),
        }
    }
    pub(super) fn evidence_sha256(&self) -> &str {
        &self.digest
    }
}
fn directory_check_private(file: &File) -> io::Result<()> {
    directory(&file.metadata()?, true)
}
pub(super) fn diagnostic_summary(root: &File) -> io::Result<Option<DiagnosticSummary>> {
    directory(&root.metadata()?, false)?;
    let root_before = Stamp::of(&root.metadata()?);
    let Some(active) = optional_directory(root, ACTIVE_NAME)? else {
        if Stamp::of(&root.metadata()?) != root_before {
            return Err(invalid("root changed during absent restore journal lookup"));
        }
        return Ok(None);
    };
    let identity = Identity::file(&active.metadata()?);
    let value = read_summary(&active)?;
    check_directory_name(root, ACTIVE_NAME, &active, identity)?;
    if Stamp::of(&root.metadata()?) != root_before {
        return Err(invalid("root changed during restore journal scan"));
    }
    Ok(Some(value))
}
pub(super) fn diagnostic_archived_summary(
    root: &File,
    run_id: &str,
) -> io::Result<Option<DiagnosticSummary>> {
    if run_id.len() != 32
        || !run_id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(invalid("invalid archived restore run id"));
    }
    directory(&root.metadata()?, false)?;
    let root_before = Stamp::of(&root.metadata()?);
    let Some(archive) = optional_directory(root, ARCHIVE_NAME)? else {
        if Stamp::of(&root.metadata()?) != root_before {
            return Err(invalid("root changed during absent restore archive lookup"));
        }
        return Ok(None);
    };
    let archive_before = Stamp::of(&archive.metadata()?);
    let Some(run) = optional_directory(&archive, run_id)? else {
        if Stamp::of(&root.metadata()?) != root_before
            || Stamp::of(&archive.metadata()?) != archive_before
        {
            return Err(invalid("archive changed during absent run lookup"));
        }
        return Ok(None);
    };
    let identity = Identity::file(&run.metadata()?);
    let value = read_summary(&run)?;
    if value.run_id != run_id || !value.receipt_present || !value.released {
        return Err(invalid("archive is not a complete restore receipt"));
    }
    check_directory_name(&archive, run_id, &run, identity)?;
    check_directory_name(root, ARCHIVE_NAME, &archive, archive_before.identity)?;
    if Stamp::of(&root.metadata()?) != root_before
        || Stamp::of(&archive.metadata()?) != archive_before
    {
        return Err(invalid("archived restore namespace changed during scan"));
    }
    Ok(Some(value))
}

#[cfg(test)]
pub(crate) type TestHook = Arc<dyn Fn(TestPhase, &DiagnosticSummary) + Send + Sync>;
#[cfg(test)]
type Registry = std::collections::HashMap<(u64, u64), TestHook>;
#[cfg(test)]
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: std::sync::OnceLock<Mutex<Registry>> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::new()))
}
#[cfg(test)]
pub(crate) struct TestHookGuard {
    identity: (u64, u64),
    hook: TestHook,
}
#[cfg(test)]
impl Drop for TestHookGuard {
    fn drop(&mut self) {
        let mut hooks = registry().lock().unwrap();
        if hooks
            .get(&self.identity)
            .is_some_and(|hook| Arc::ptr_eq(hook, &self.hook))
        {
            hooks.remove(&self.identity);
        }
    }
}
#[cfg(test)]
pub(crate) fn set_test_hook(root: &Path, hook: TestHook) -> io::Result<TestHookGuard> {
    let metadata = std::fs::metadata(root)?;
    directory(&metadata, false)?;
    let identity = (metadata.dev(), metadata.ino());
    let mut hooks = registry().lock().unwrap();
    if hooks.contains_key(&identity) {
        return Err(invalid("restore test root already has a hook"));
    }
    hooks.insert(identity, hook.clone());
    Ok(TestHookGuard { identity, hook })
}
#[cfg(test)]
fn lookup_hook(root: &File) -> io::Result<Option<TestHook>> {
    let metadata = root.metadata()?;
    Ok(registry()
        .lock()
        .unwrap()
        .get(&(metadata.dev(), metadata.ino()))
        .cloned())
}
#[cfg(test)]
impl Store {
    fn emit(&self, phase: TestPhase) -> Result<(), Error> {
        if let Some(hook) = &self.hook {
            let records: Result<Vec<Record>, _> = self
                .records
                .iter()
                .map(|saved| serde_json::from_slice(&saved.payload))
                .collect();
            let value = summary(
                &records.map_err(|_| Error::invalid("cannot decode own durable journal"))?,
            )?;
            hook(phase, &value);
        }
        Ok(())
    }
    fn emit_for(&self, event: &Event) -> Result<(), Error> {
        let phase = match event {
            Event::Intent { .. } => Some(TestPhase::Intent),
            Event::Prepare { .. } => Some(TestPhase::Prepared),
            Event::TargetVerified { .. } => Some(TestPhase::TargetVerified),
            Event::Released => Some(TestPhase::Released),
            Event::Receipt { .. } => Some(TestPhase::Receipt),
            _ => None,
        };
        if let Some(phase) = phase {
            self.emit(phase)?;
        }
        Ok(())
    }
    pub(super) fn before_outcome(&self) -> Result<(), Error> {
        self.emit(TestPhase::BeforeOutcomePersist)
    }
    pub(super) fn set_fault(
        &mut self,
        fault: impl FnMut(Point) -> io::Result<()> + Send + 'static,
    ) {
        self.fault = Some(Box::new(fault));
    }
}

pub(super) fn inspect_readonly_directory(directory: &File) -> io::Result<ReadonlyJournalEvidence> {
    let parsed = read_records(directory)?;
    let Event::Intent {
        intent,
        backup_id,
        backup_sha256,
        recovery_origin,
    } = &parsed.records[0].event
    else {
        return Err(invalid("missing typed intent"));
    };
    Ok(ReadonlyJournalEvidence {
        intent: intent.clone(),
        backup_id: backup_id.clone(),
        backup_sha256: backup_sha256.clone(),
        chain_sha256: hash(
            &parsed
                .files
                .last()
                .ok_or_else(|| invalid("empty journal"))?
                .payload,
        ),
        summary: parsed.summary,
        recovery_origin: recovery_origin.clone(),
    })
}
