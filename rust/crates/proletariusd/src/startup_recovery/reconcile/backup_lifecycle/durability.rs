//! A readable terminal record is not evidence that its earlier sync succeeded.
//! Confirm only an already validated, unchanged inventory using its actual FDs.
//! No entry is created, renamed, overwritten or admitted by this fence.
use super::*;

#[derive(PartialEq)]
struct GenerationSnapshot {
    generation: u64,
    stamp: FileStamp,
    names: Vec<String>,
    entries: Vec<Value>,
}

#[derive(PartialEq)]
struct Snapshot {
    root: FileStamp,
    outer: FileStamp,
    namespace: FileStamp,
    names: Vec<String>,
    lock: Option<Value>,
    public: Value,
    generations: Vec<GenerationSnapshot>,
}

fn directory_matches(parent: &File, name: &str, held: &File, stamp: FileStamp) -> io::Result<()> {
    let named = storage::directory(parent, name)?;
    if file_stamp(&held.metadata()?) != stamp || file_stamp(&named.metadata()?) != stamp {
        return Err(invalid());
    }
    Ok(())
}

impl Snapshot {
    // Scan has already checked all schemas, transitions, origins and budgets.
    // Retain full identities/raw digests as one bounded comparison snapshot.
    // Regular FDs are opened one at a time, rather than retaining MAX_NODES FDs.
    fn capture(root: &File, inventory: &Inventory) -> io::Result<Self> {
        let root_stamp = file_stamp(&root.metadata()?);
        let outer = storage::optional_dir(root, storage::NAME)?.ok_or_else(invalid)?;
        let outer_stamp = file_stamp(&outer.metadata()?);
        let namespace = storage::directory(&outer, NAME)?;
        let namespace_stamp = file_stamp(&namespace.metadata()?);
        let namespace_names = names(&namespace, MAX_GENERATIONS + 1)?;
        let lock = if namespace_names.iter().any(|name| name == "inventory.lock") {
            let actual = lock_file(&namespace, false)?;
            let (_, entry) = evidence::pinned_regular_manifest(&namespace, "inventory.lock")?;
            if file_stamp(&actual.metadata()?)
                != file_stamp(&lock_file(&namespace, false)?.metadata()?)
            {
                return Err(invalid());
            }
            Some(entry)
        } else {
            None
        };
        let public = backups(&manifest(root)?).clone();
        let mut generations = Vec::with_capacity(inventory.transactions.len());
        for transaction in &inventory.transactions {
            if transaction.commit.is_none() && transaction.abort.is_none() {
                return Err(invalid());
            }
            let generation = transaction.intent.generation;
            let name = format!("{generation:06}");
            let stamp = file_stamp(&transaction.directory.metadata()?);
            directory_matches(&namespace, &name, &transaction.directory, stamp)?;
            let actual_names = names(&transaction.directory, 8)?;
            let expected = [
                ("intent.json", Some(digest(&transaction.intent)?)),
                (
                    "ready.json",
                    transaction.ready.as_ref().map(digest).transpose()?,
                ),
                (
                    "prepare.json",
                    transaction.prepare.as_ref().map(digest).transpose()?,
                ),
                (
                    "commit.json",
                    transaction.commit.as_ref().map(digest).transpose()?,
                ),
                (
                    "abort.json",
                    transaction.abort.as_ref().map(digest).transpose()?,
                ),
            ];
            let mut entries = Vec::with_capacity(actual_names.len());
            let mut expected_names = Vec::with_capacity(actual_names.len());
            for (record_name, expected_hash) in expected {
                if let Some(expected_hash) = expected_hash {
                    let (_, entry) =
                        evidence::pinned_regular_manifest(&transaction.directory, record_name)?;
                    if entry["sha256"] != expected_hash {
                        return Err(invalid());
                    }
                    expected_names.push(record_name.to_owned());
                    entries.push(entry);
                }
            }
            if transaction.intent.kind == Kind::Delete && transaction.commit.is_some() {
                let (_, entry) =
                    evidence::pinned_regular_manifest(&transaction.directory, "archive")?;
                if transaction.commit.as_ref().map(|commit| &commit.entry) != Some(&entry) {
                    return Err(invalid());
                }
                expected_names.push("archive".to_owned());
                entries.push(entry);
            }
            expected_names.sort();
            if actual_names != expected_names {
                return Err(invalid());
            }
            directory_matches(&namespace, &name, &transaction.directory, stamp)?;
            generations.push(GenerationSnapshot {
                generation,
                stamp,
                names: actual_names,
                entries,
            });
        }
        let mut expected_names = inventory
            .transactions
            .iter()
            .map(|transaction| format!("{:06}", transaction.intent.generation))
            .collect::<Vec<_>>();
        if lock.is_some() {
            expected_names.push("inventory.lock".to_owned());
        }
        expected_names.sort();
        if expected_names != namespace_names {
            return Err(invalid());
        }
        directory_matches(root, storage::NAME, &outer, outer_stamp)?;
        directory_matches(&outer, NAME, &namespace, namespace_stamp)?;
        if file_stamp(&root.metadata()?) != root_stamp
            || names(&namespace, MAX_GENERATIONS + 1)? != namespace_names
            || backups(&manifest(root)?) != &public
        {
            return Err(invalid());
        }
        Ok(Self {
            root: root_stamp,
            outer: outer_stamp,
            namespace: namespace_stamp,
            names: namespace_names,
            lock,
            public,
            generations,
        })
    }
}

fn read_raw(file: &File, expected: FileStamp) -> io::Result<zeroize::Zeroizing<Vec<u8>>> {
    // The existing pinned evidence reader has already applied this same
    // per-file bound. Keep just this one raw comparison buffer across sync.
    if expected.6 > 1_100_000 || file_stamp(&file.metadata()?) != expected {
        return Err(invalid());
    }
    let length = usize::try_from(expected.6).map_err(|_| invalid())?;
    let mut raw = zeroize::Zeroizing::new(vec![0; length + 1]);
    let mut count = 0;
    while count < raw.len() {
        match file.read_at(&mut raw[count..], count as u64) {
            Ok(0) => break,
            Ok(read) => count += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    if count != length || file_stamp(&file.metadata()?) != expected {
        return Err(invalid());
    }
    raw.truncate(count);
    Ok(raw)
}

fn sync_regular(
    _root: &File,
    _generation: Option<u64>,
    parent: &File,
    name: &str,
    expected: &Value,
    placement: &impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    placement()?;
    let (file, before) = evidence::pinned_regular_manifest(parent, name)?;
    let stamp = file_stamp(&file.metadata()?);
    if &before != expected {
        return Err(invalid());
    }
    let raw = read_raw(&file, stamp)?;
    if expected["sha256"] != storage::digest(raw.as_slice()) {
        return Err(invalid());
    }
    #[cfg(test)]
    let _finite_io_context = if let Some(generation) = _generation {
        super::super::finite_io_tests::ContextGuard::backup(
            _root,
            parent,
            generation,
            match name {
                "commit.json" => super::super::finite_io_tests::Record::Commit,
                "ready.json" => super::super::finite_io_tests::Record::Ready,
                _ => super::super::finite_io_tests::Record::Other,
            },
            super::super::finite_io_tests::Mode::Resync,
        )?
    } else {
        super::super::finite_io_tests::ContextGuard::metadata(
            _root,
            parent,
            super::super::finite_io_tests::Record::Other,
        )?
    };
    #[cfg(test)]
    let synced = super::super::finite_io_tests::sync_all(
        if _generation.is_some() {
            super::super::finite_io_tests::SyncPoint::ResyncRecord
        } else {
            super::super::finite_io_tests::SyncPoint::ResyncData
        },
        &file,
    );
    #[cfg(not(test))]
    let synced = file.sync_all();
    let unchanged = (|| {
        placement()?;
        let (_, after) = evidence::pinned_regular_manifest(parent, name)?;
        if &after != expected
            || file_stamp(&file.metadata()?) != stamp
            || read_raw(&file, stamp)?.as_slice() != raw.as_slice()
        {
            return Err(invalid());
        }
        Ok(())
    })();
    synced.and(unchanged)
}

fn sync_directory(
    _root: &File,
    parent: &File,
    name: &str,
    held: &File,
    stamp: FileStamp,
    placement: &impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    placement()?;
    directory_matches(parent, name, held, stamp)?;
    #[cfg(test)]
    let _finite_io_context = super::super::finite_io_tests::ContextGuard::metadata(
        _root,
        parent,
        super::super::finite_io_tests::Record::Other,
    )?;
    #[cfg(test)]
    let synced = super::super::finite_io_tests::sync_all(
        match name {
            NAME => super::super::finite_io_tests::SyncPoint::ResyncNamespace,
            storage::NAME => super::super::finite_io_tests::SyncPoint::ResyncOuter,
            _ => super::super::finite_io_tests::SyncPoint::ResyncContainer,
        },
        held,
    );
    #[cfg(not(test))]
    let synced = held.sync_all();
    let unchanged = placement().and_then(|()| directory_matches(parent, name, held, stamp));
    synced.and(unchanged)
}

pub(super) fn resync_finished(root: &File, inventory: &Inventory) -> io::Result<()> {
    // There is deliberately no new flock here. Normal begin/resume already
    // hold it and origin inspection re-enters these same readers. Pure readers
    // use complete before/after FD/raw snapshots and fail on concurrent drift.
    let before = Snapshot::capture(root, inventory)?;
    let outer = storage::directory(root, storage::NAME)?;
    let namespace = storage::directory(&outer, NAME)?;
    directory_matches(root, storage::NAME, &outer, before.outer)?;
    directory_matches(&outer, NAME, &namespace, before.namespace)?;
    let check_root = || {
        if file_stamp(&root.metadata()?) != before.root {
            return Err(invalid());
        }
        Ok(())
    };
    let check_outer = || {
        check_root()?;
        directory_matches(root, storage::NAME, &outer, before.outer)
    };
    let check_namespace = || {
        check_outer()?;
        directory_matches(&outer, NAME, &namespace, before.namespace)
    };
    for (transaction, snapshot) in inventory.transactions.iter().zip(&before.generations) {
        let name = format!("{:06}", snapshot.generation);
        let check_generation = || {
            check_namespace()?;
            directory_matches(&namespace, &name, &transaction.directory, snapshot.stamp)
        };
        check_generation()?;
        for entry in &snapshot.entries {
            let record_name = entry["name"].as_str().ok_or_else(invalid)?;
            sync_regular(
                root,
                Some(snapshot.generation),
                &transaction.directory,
                record_name,
                entry,
                &check_generation,
            )?;
        }
        sync_directory(
            root,
            &namespace,
            &name,
            &transaction.directory,
            snapshot.stamp,
            &check_namespace,
        )?;
    }
    if let Some(lock) = &before.lock {
        sync_regular(
            root,
            None,
            &namespace,
            "inventory.lock",
            lock,
            &check_namespace,
        )?;
    }
    if !before.public.is_null() {
        let public = public_dir(root, false)?.ok_or_else(invalid)?;
        let stamp = file_stamp(&public.metadata()?);
        if !same_parent(&before.public, &public)? || backups(&manifest(root)?) != &before.public {
            return Err(invalid());
        }
        let check_public = || {
            check_root()?;
            let named = public_dir(root, false)?.ok_or_else(invalid)?;
            if file_stamp(&public.metadata()?) != stamp || file_stamp(&named.metadata()?) != stamp {
                return Err(invalid());
            }
            Ok(())
        };
        for entry in children(&before.public)? {
            let name = entry["name"].as_str().ok_or_else(invalid)?;
            sync_regular(root, None, &public, name, &entry, &check_public)?;
        }
        #[cfg(test)]
        let _finite_io_context = super::super::finite_io_tests::ContextGuard::metadata(
            root,
            root,
            super::super::finite_io_tests::Record::Other,
        )?;
        #[cfg(test)]
        let synced = super::super::finite_io_tests::sync_all(
            super::super::finite_io_tests::SyncPoint::ResyncPublic,
            &public,
        );
        #[cfg(not(test))]
        let synced = public.sync_all();
        let unchanged = (|| {
            let named = public_dir(root, false)?.ok_or_else(invalid)?;
            if file_stamp(&public.metadata()?) != stamp
                || file_stamp(&named.metadata()?) != stamp
                || backups(&manifest(root)?) != &before.public
            {
                return Err(invalid());
            }
            Ok(())
        })();
        synced.and(unchanged)?;
    }
    sync_directory(
        root,
        &outer,
        NAME,
        &namespace,
        before.namespace,
        &check_outer,
    )?;
    sync_directory(root, root, storage::NAME, &outer, before.outer, &check_root)?;
    if file_stamp(&root.metadata()?) != before.root {
        return Err(invalid());
    }
    #[cfg(test)]
    let _finite_io_context = super::super::finite_io_tests::ContextGuard::metadata(
        root,
        root,
        super::super::finite_io_tests::Record::Other,
    )?;
    #[cfg(test)]
    let synced = super::super::finite_io_tests::sync_all(
        super::super::finite_io_tests::SyncPoint::ResyncRoot,
        root,
    );
    #[cfg(not(test))]
    let synced = root.sync_all();
    let unchanged = if file_stamp(&root.metadata()?) == before.root {
        Ok(())
    } else {
        Err(invalid())
    };
    synced.and(unchanged)?;
    verify_finished(root, inventory)?;
    // Full records, directory names, head, archive/raw identities and parent
    // placements must still be exactly the originally validated snapshot.
    if Snapshot::capture(root, inventory)? != before {
        return Err(invalid());
    }
    Ok(())
}
