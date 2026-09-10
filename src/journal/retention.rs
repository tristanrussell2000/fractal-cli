//! Pruning entries and collecting the blobs nothing references any more.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::JournalError;
use super::blobs::BlobStore;
use super::entry::JournalEntry;
use super::store::EntryStore;

/// How long a half-written blob must sit before it is assumed abandoned.
///
/// A temporary from a moment ago is very likely another Fractal process writing
/// right now, and removing it would corrupt that write. An hour is four orders
/// of magnitude past any real write.
pub const TEMPORARY_GRACE: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Entries to keep per object, newest first.
    pub keep_per_object: usize,
    /// Entries older than this are dropped, except the newest for an object.
    pub max_age: Duration,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            keep_per_object: 20,
            max_age: Duration::from_secs(60 * 60 * 24 * 30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PruneOutcome {
    pub entries_removed: usize,
    pub blobs_removed: usize,
    pub temporaries_removed: usize,
}

/// Runs the whole retention pass: prune, mark, sweep, then clear abandoned
/// temporaries.
///
/// The one place the mark phase is assembled, so the rule that every referencing
/// source must be unioned before sweeping lives here rather than at each call
/// site. When the read cache exists, its hashes join \`live\` in this function.
///
/// # Errors
///
/// Returns [`JournalError`] when the journal cannot be listed or a file cannot
/// be removed.
pub fn run_retention(
    journal_root: &Path,
    blobs: &BlobStore,
    policy: RetentionPolicy,
    now: SystemTime,
) -> Result<PruneOutcome, JournalError> {
    // Captured before anything is listed. Everything the sweep learns is a
    // snapshot from this instant, so a blob touched at or after it may be
    // referenced by an entry the snapshot could not see.
    let mark_started = SystemTime::now();
    let stores = entry_stores(journal_root)?;
    let mut outcome = PruneOutcome::default();
    for store in &stores {
        outcome.entries_removed += prune_entries(store, policy, now)?;
    }

    // Marked from what survived, across every system, because one blob store
    // serves them all.
    let mut live = HashSet::new();
    for store in &stores {
        live.extend(referenced_blobs(&store.list()?));
    }
    outcome.blobs_removed = sweep_blobs(blobs, &live, mark_started)?;
    outcome.temporaries_removed = sweep_temporaries(blobs, TEMPORARY_GRACE, now)?;
    Ok(outcome)
}

/// Removes the entries a policy no longer keeps.
///
/// The newest entry for an object always survives, whatever its age: it is the
/// only one `undo` can act on, and an object nobody has touched for a year is
/// exactly when its last known state is worth having.
///
/// Blobs are not touched here. They go by [`sweep_blobs`], from the entries
/// that survive — pruning one entry must never delete a blob another still
/// references.
///
/// # Errors
///
/// Returns [`JournalError::Read`] when the entries cannot be listed, or
/// [`JournalError::Write`] when one cannot be removed.
pub fn prune_entries(
    entries: &EntryStore,
    policy: RetentionPolicy,
    now: SystemTime,
) -> Result<usize, JournalError> {
    let mut removed = 0;
    for object_key in entries.object_keys()? {
        // Entries are filed per object, so "keep the last N for this object" is
        // a directory listing rather than a grouping pass over the system.
        let all = entries.entries_for(&object_key)?;
        for (index, entry) in all.iter().enumerate() {
            let newest = index + 1 == all.len();
            if newest {
                continue;
            }
            let too_many = all.len() - index > policy.keep_per_object;
            if too_many || is_older_than(entries, &object_key, entry, policy.max_age, now) {
                entries.remove(&object_key, &entry.id)?;
                removed += 1;
            }
        }
        entries.remove_if_empty(&object_key)?;
    }
    Ok(removed)
}

/// Deletes every blob no live reference claims and nothing has touched since
/// `mark_started`.
///
/// The set of live hashes is supplied rather than discovered, because one blob
/// store is shared by every system and will later be shared by the read cache.
/// A sweep that gathered its own references would need to know every source
/// that exists, and would quietly delete the content of any it did not.
///
/// `mark_started` must be read **before** the live set is gathered. It closes
/// the race the live set alone cannot: a process referencing existing content
/// writes nothing, so its reference is invisible until its entry lands, which
/// may be after the snapshot. Its confirming `put` touches the blob, and a
/// touch at or after `mark_started` means the snapshot cannot be trusted about
/// that blob.
///
/// # Errors
///
/// Returns [`JournalError::Read`] when the store cannot be listed, or
/// [`JournalError::Write`] when a blob cannot be removed.
pub fn sweep_blobs(
    blobs: &BlobStore,
    live: &HashSet<String>,
    mark_started: SystemTime,
) -> Result<usize, JournalError> {
    let mut removed = 0;
    for hash in blobs.hashes()? {
        if live.contains(&hash) {
            continue;
        }
        // Read the mtime here rather than up front, as late as the filesystem
        // allows before the unlink. A blob a concurrent write re-referenced
        // after marking began looks fresh and is left alone; the remaining
        // window is the gap between this stat and the unlink, and a write that
        // loses it rewrites the blob on its confirming `put`.
        let touched_since_marking = blobs
            .modified_at(&hash)
            .is_some_and(|modified| modified >= mark_started);
        if touched_since_marking {
            continue;
        }
        remove_file(&blobs.path_of(&hash))?;
        removed += 1;
    }
    Ok(removed)
}

/// Every blob the given entries reference, for the mark phase.
///
/// Takes entries rather than a store so a caller can union several systems, or
/// later a cache index, before sweeping.
pub fn referenced_blobs<'a>(
    entries: impl IntoIterator<Item = &'a JournalEntry>,
) -> HashSet<String> {
    entries
        .into_iter()
        .flat_map(JournalEntry::referenced_blobs)
        .map(str::to_owned)
        .collect()
}

/// Every system with a journal directory.
///
/// The mark phase needs all of them: entries are filed per host while blobs are
/// shared, so sweeping from one system's entries would delete blobs another
/// system still references.
///
/// # Errors
///
/// Returns [`JournalError::Read`] when the journal directory cannot be listed.
/// A journal that does not exist yet has no systems, which is not an error.
pub fn entry_stores(journal_root: &Path) -> Result<Vec<EntryStore>, JournalError> {
    if !journal_root.is_dir() {
        return Ok(Vec::new());
    }
    let read = std::fs::read_dir(journal_root).map_err(|source| JournalError::Read {
        path: journal_root.to_path_buf(),
        source,
    })?;
    let mut stores = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| JournalError::Read {
            path: journal_root.to_path_buf(),
            source,
        })?;
        if entry.path().is_dir() {
            stores.push(EntryStore::new(entry.path()));
        }
    }
    Ok(stores)
}

/// Removes half-written blobs old enough that no write can still be using them.
///
/// Separate from [`sweep_blobs`] on purpose: [`BlobStore::hashes`] skips
/// temporaries, so the mark and sweep never sees one and they would otherwise
/// survive forever.
///
/// # Errors
///
/// Returns [`JournalError::Read`] when the store cannot be listed, or
/// [`JournalError::Write`] when a temporary cannot be removed.
pub fn sweep_temporaries(
    blobs: &BlobStore,
    grace: Duration,
    now: SystemTime,
) -> Result<usize, JournalError> {
    let root = blobs.root();
    if !root.is_dir() {
        return Ok(0);
    }
    let read = std::fs::read_dir(root).map_err(|source| JournalError::Read {
        path: root.to_path_buf(),
        source,
    })?;

    let mut removed = 0;
    for entry in read {
        let entry = entry.map_err(|source| JournalError::Read {
            path: root.to_path_buf(),
            source,
        })?;
        if !entry.file_name().to_string_lossy().starts_with(".tmp-") {
            continue;
        }
        // A temporary younger than the grace period may be a live write. An
        // unreadable timestamp is treated as young, so the doubtful case leaves
        // the file alone.
        let old_enough = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age >= grace));
        if old_enough {
            remove_file(&entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn remove_file(path: &Path) -> Result<(), JournalError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(JournalError::Write {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn is_older_than(
    entries: &EntryStore,
    object_key: &str,
    entry: &JournalEntry,
    max_age: Duration,
    now: SystemTime,
) -> bool {
    // An entry whose age cannot be read is treated as young, so the doubtful
    // case keeps it.
    entries
        .modified_at(object_key, &entry.id)
        .is_some_and(|modified| now.duration_since(modified).is_ok_and(|age| age >= max_age))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::entry::{
        ContentRef, EntryObject, EntryStatus, EntrySystem, JournalOperation,
    };
    use crate::sap::object_family::AdtObjectFamily;

    fn entry(uri: &str, blob: &str) -> JournalEntry {
        JournalEntry {
            id: String::new(),
            recorded_at: String::new(),
            status: EntryStatus::Succeeded,
            system: EntrySystem {
                base_url: "https://sap.example:8001".to_owned(),
                profile: "dev".to_owned(),
                client: "100".to_owned(),
                user: "developer".to_owned(),
            },
            object: EntryObject {
                object_type: AdtObjectFamily::parse("PROG").unwrap(),
                name: "ZSAMPLE".to_owned(),
                uri: uri.to_owned(),
                source_part: None,
            },
            operation: JournalOperation::activate(),
            transport: None,
            active_before: ContentRef::Sha256(blob.to_owned()),
            inactive_before: None,
            active_after: None,
            etag_after: None,
        }
    }

    fn hash(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    #[test]
    fn keeps_the_newest_per_object_and_drops_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let store = EntryStore::new(dir.path().join("host"));
        for _ in 0..4 {
            store.create(entry("/a", &hash('a'))).unwrap();
        }
        store.create(entry("/b", &hash('b'))).unwrap();

        let policy = RetentionPolicy {
            keep_per_object: 2,
            ..RetentionPolicy::default()
        };
        let removed = prune_entries(&store, policy, SystemTime::now()).unwrap();

        assert_eq!(removed, 2);
        assert_eq!(store.list().unwrap().len(), 3);
    }

    #[test]
    fn the_newest_entry_survives_any_age() {
        let dir = tempfile::tempdir().unwrap();
        let store = EntryStore::new(dir.path().join("host"));
        store.create(entry("/a", &hash('a'))).unwrap();

        // Far in the future, so every entry is older than the limit.
        let much_later = SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 365);
        let removed = prune_entries(&store, RetentionPolicy::default(), much_later).unwrap();

        // An object nobody has touched in a year is exactly when its last known
        // state is worth keeping.
        assert_eq!(removed, 0);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn age_drops_older_entries_but_not_the_last_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = EntryStore::new(dir.path().join("host"));
        for _ in 0..3 {
            store.create(entry("/a", &hash('a'))).unwrap();
        }

        let much_later = SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 365);
        let removed = prune_entries(&store, RetentionPolicy::default(), much_later).unwrap();

        assert_eq!(removed, 2);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn a_blob_no_entry_claims_is_swept() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let kept = blobs.put("kept").unwrap();
        blobs.put("orphan").unwrap();

        let live = HashSet::from([kept.clone()]);
        assert_eq!(sweep_blobs(&blobs, &live, SystemTime::now()).unwrap(), 1);
        assert!(blobs.contains(&kept));
        assert_eq!(blobs.hashes().unwrap(), vec![kept]);
    }

    #[test]
    fn another_systems_blobs_survive_a_sweep() {
        // Entries are filed per host, blobs are shared. Marking from one
        // system's entries alone would delete another system's content — the
        // same failure a read cache would cause, which is why the sweep takes
        // the live set rather than finding it.
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal");
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let mine = blobs.put("mine").unwrap();
        let theirs = blobs.put("theirs").unwrap();

        let first = EntryStore::new(journal.join("de3"));
        first.create(entry("/a", &mine)).unwrap();
        let second = EntryStore::new(journal.join("qe2"));
        second.create(entry("/a", &theirs)).unwrap();

        let mut live = HashSet::new();
        for store in entry_stores(&journal).unwrap() {
            live.extend(referenced_blobs(&store.list().unwrap()));
        }

        assert_eq!(sweep_blobs(&blobs, &live, SystemTime::now()).unwrap(), 0);
        assert!(blobs.contains(&mine));
        assert!(blobs.contains(&theirs));
    }

    #[test]
    fn sweeping_with_no_live_references_empties_the_store() {
        // The caller decides what is live, so an empty set really does mean
        // nothing is. Union every source before calling this.
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        blobs.put("one").unwrap();
        blobs.put("two").unwrap();

        assert_eq!(
            sweep_blobs(&blobs, &HashSet::new(), SystemTime::now()).unwrap(),
            2
        );
        assert!(blobs.hashes().unwrap().is_empty());
    }

    #[test]
    fn a_blob_two_entries_share_survives_pruning_one() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal");
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let shared = blobs.put("shared").unwrap();

        let store = EntryStore::new(journal.join("de3"));
        store.create(entry("/a", &shared)).unwrap();
        store.create(entry("/b", &shared)).unwrap();

        let much_later = SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 365);
        prune_entries(&store, RetentionPolicy::default(), much_later).unwrap();
        let live = referenced_blobs(&store.list().unwrap());

        assert_eq!(sweep_blobs(&blobs, &live, SystemTime::now()).unwrap(), 0);
        assert!(blobs.contains(&shared));
    }

    #[test]
    fn temporaries_are_swept_only_once_they_are_old() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        blobs.put("real").unwrap();
        let temporary = blobs.path_of(&format!(".tmp-{}", hash('c')));
        std::fs::write(&temporary, "half written").unwrap();

        // Fresh: another process may be writing it right now.
        assert_eq!(
            sweep_temporaries(&blobs, TEMPORARY_GRACE, SystemTime::now()).unwrap(),
            0
        );
        assert!(temporary.is_file());

        let much_later = SystemTime::now() + TEMPORARY_GRACE + Duration::from_secs(60);
        assert_eq!(
            sweep_temporaries(&blobs, TEMPORARY_GRACE, much_later).unwrap(),
            1
        );
        assert!(!temporary.is_file());
    }

    #[test]
    fn sweeping_temporaries_leaves_real_blobs_alone() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let real = blobs.put("real").unwrap();

        let much_later = SystemTime::now() + TEMPORARY_GRACE * 2;
        assert_eq!(
            sweep_temporaries(&blobs, TEMPORARY_GRACE, much_later).unwrap(),
            0
        );
        assert!(blobs.contains(&real));
    }

    #[test]
    fn a_full_pass_prunes_marks_and_sweeps_together() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal");
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let old = blobs.put("superseded").unwrap();
        let current = blobs.put("current").unwrap();
        let orphan = blobs.put("never referenced").unwrap();
        std::fs::write(blobs.path_of(&format!(".tmp-{}", hash('d'))), "half").unwrap();

        let store = EntryStore::new(journal.join("de3"));
        store.create(entry("/a", &old)).unwrap();
        store.create(entry("/a", &current)).unwrap();

        let much_later = SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 365);
        let outcome =
            run_retention(&journal, &blobs, RetentionPolicy::default(), much_later).unwrap();

        assert_eq!(outcome.entries_removed, 1);
        // The superseded entry's blob goes with it; the newest entry's stays.
        assert_eq!(outcome.blobs_removed, 2);
        assert_eq!(outcome.temporaries_removed, 1);
        assert!(blobs.contains(&current));
        assert!(!blobs.contains(&old));
        assert!(!blobs.contains(&orphan));
    }

    #[test]
    fn a_full_pass_marks_from_every_system_not_just_one() {
        // The guarantee the whole signature exists for: one blob store, many
        // referencing sources. Marking from a subset deletes live content, and
        // the read cache will be another such source.
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal");
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let mine = blobs.put("mine").unwrap();
        let theirs = blobs.put("theirs").unwrap();

        EntryStore::new(journal.join("de3"))
            .create(entry("/a", &mine))
            .unwrap();
        EntryStore::new(journal.join("qe2"))
            .create(entry("/a", &theirs))
            .unwrap();

        let outcome = run_retention(
            &journal,
            &blobs,
            RetentionPolicy::default(),
            SystemTime::now(),
        )
        .unwrap();

        assert_eq!(outcome.blobs_removed, 0);
        assert!(blobs.contains(&mine));
        assert!(blobs.contains(&theirs));
    }

    #[test]
    fn a_blob_referenced_after_marking_began_survives_the_sweep() {
        // The race the mark timestamp exists for: a write references content
        // the mark phase already decided was dead. Its confirming `put` touches
        // the blob, and a touch at or after `mark_started` means the snapshot
        // cannot be trusted about it.
        use crate::journal::recorder::Journal;

        let dir = tempfile::tempdir().unwrap();
        let journal_root = dir.path().join("journal");
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let orphan = blobs.put("about to be referenced").unwrap();

        // A sweep starts and finds nothing referencing it.
        let mark_started = SystemTime::now();
        std::thread::sleep(Duration::from_millis(20));
        let live = HashSet::new();

        // Meanwhile another process records an operation over that content.
        let journal = Journal::with_roots(
            dir.path().join("blobs"),
            journal_root.join("qe2"),
            crate::journal::entry::EntrySystem {
                base_url: "https://sap.example:8001".to_owned(),
                profile: "dev".to_owned(),
                client: "100".to_owned(),
                user: "developer".to_owned(),
            },
        );
        journal
            .begin(
                EntryObject {
                    object_type: AdtObjectFamily::parse("PROG").unwrap(),
                    name: "ZSAMPLE".to_owned(),
                    uri: "/a".to_owned(),
                    source_part: None,
                },
                JournalOperation::activate(),
                None,
                Some("about to be referenced".to_owned()),
                None,
            )
            .unwrap();

        assert_eq!(sweep_blobs(&blobs, &live, mark_started).unwrap(), 0);
        assert!(blobs.contains(&orphan));
    }

    #[test]
    fn a_journal_with_no_systems_yet_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            entry_stores(&dir.path().join("journal"))
                .unwrap()
                .is_empty()
        );
        let blobs = BlobStore::new(dir.path().join("blobs"));
        assert_eq!(
            sweep_temporaries(&blobs, TEMPORARY_GRACE, SystemTime::now()).unwrap(),
            0
        );
    }
}
