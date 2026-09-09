//! Reading and writing entries for one system.

use std::path::{Path, PathBuf};

use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

use super::JournalError;
use super::blobs::{create_private_dir, write_private_file};
use super::entry::JournalEntry;
use super::paths;

const EXTENSION: &str = "json";

/// Sorts as it reads: fixed width, most-significant first.
const ID_FORMAT: &[BorrowedFormatItem<'_>] =
    format_description!("[year][month][day]T[hour][minute][second].[subsecond digits:3]Z");
const RECORDED_AT_FORMAT: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

/// How many suffixes to try before giving up on one millisecond.
///
/// Only reached when that many entries land in the same millisecond, which
/// takes concurrent processes; far past that, something is wrong.
const MAX_ID_ATTEMPTS: u32 = 100;

pub struct EntryStore {
    root: PathBuf,
}

impl EntryStore {
    /// `root` is one system's journal directory; entries live one level deeper,
    /// in a directory per object.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes a new entry, allocating its id.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Write`] when the entry cannot be written, or
    /// [`JournalError::IdExhausted`] when too many entries claim one
    /// millisecond.
    pub fn create(&self, mut entry: JournalEntry) -> Result<JournalEntry, JournalError> {
        let directory = self.object_dir(&paths::object_key(&entry.object.uri));
        create_private_dir(&directory)?;
        let now = OffsetDateTime::now_utc();
        entry.recorded_at = format(now, RECORDED_AT_FORMAT);
        let stamp = format(now, ID_FORMAT);

        for attempt in 0..MAX_ID_ATTEMPTS {
            let id = if attempt == 0 {
                stamp.clone()
            } else {
                format!("{stamp}-{attempt:02}")
            };
            let path = directory.join(format!("{id}.{EXTENSION}"));
            // `create_new` is the whole collision mechanism: the filesystem
            // decides, so two processes cannot both win the same name. The mode
            // goes on here rather than on the write that follows: it only
            // applies when a file is created, and by then this one exists.
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(mut file) => {
                    use std::io::Write as _;

                    entry.id = id;
                    file.write_all(serialize(&entry)?.as_bytes())
                        .map_err(|source| JournalError::Write {
                            path: path.clone(),
                            source,
                        })?;
                    return Ok(entry);
                }
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(JournalError::Write { path, source }),
            }
        }
        Err(JournalError::IdExhausted { stamp })
    }

    /// Replaces an entry that already exists, for resolving a `pending` one.
    ///
    /// Written to a temporary file and renamed, so a reader never sees a
    /// half-written entry.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::EntryMissing`] when there is no such entry, or
    /// [`JournalError::Write`] when it cannot be replaced.
    pub fn update(&self, entry: &JournalEntry) -> Result<(), JournalError> {
        let directory = self.object_dir(&paths::object_key(&entry.object.uri));
        let path = directory.join(format!("{}.{EXTENSION}", entry.id));
        if !path.is_file() {
            return Err(JournalError::EntryMissing {
                id: entry.id.clone(),
            });
        }
        let temporary = directory.join(format!(".tmp-{}.{EXTENSION}", entry.id));
        write_private_file(&temporary, &serialize(entry)?)?;
        std::fs::rename(&temporary, &path).map_err(|source| {
            let _ = std::fs::remove_file(&temporary);
            JournalError::Write { path, source }
        })
    }

    /// # Errors
    ///
    /// Returns [`JournalError::EntryMissing`] when there is no such entry,
    /// [`JournalError::Read`] when it cannot be read, or
    /// [`JournalError::EntryInvalid`] when its JSON does not parse.
    pub fn read(&self, object_key: &str, id: &str) -> Result<JournalEntry, JournalError> {
        let path = self
            .object_dir(object_key)
            .join(format!("{id}.{EXTENSION}"));
        if !path.is_file() {
            return Err(JournalError::EntryMissing { id: id.to_owned() });
        }
        let text = std::fs::read_to_string(&path).map_err(|source| JournalError::Read {
            path: path.clone(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|source| JournalError::EntryInvalid {
            id: id.to_owned(),
            source,
        })
    }

    /// Finds an entry by id alone, across every object.
    ///
    /// A bare id does not say which object it belongs to, so this scans. It is
    /// for `journal show`, which runs once against an id a person copied from a
    /// listing; everything on a hot path takes the object key.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::EntryMissing`] when no object holds that id.
    pub fn find(&self, id: &str) -> Result<JournalEntry, JournalError> {
        for object_key in self.object_keys()? {
            if let Ok(entry) = self.read(&object_key, id) {
                return Ok(entry);
            }
        }
        Err(JournalError::EntryMissing { id: id.to_owned() })
    }

    /// One object's entries, oldest first.
    ///
    /// A directory listing rather than a scan, which is the reason entries are
    /// filed per object at all.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Read`] when the directory cannot be listed.
    pub fn entries_for(&self, object_key: &str) -> Result<Vec<JournalEntry>, JournalError> {
        let mut ids = self.ids_for(object_key)?;
        ids.sort();
        Ok(ids
            .into_iter()
            .filter_map(|id| self.read(object_key, &id).ok())
            .collect())
    }

    /// Every entry for this system, oldest first.
    ///
    /// An entry that cannot be parsed is skipped rather than failing the whole
    /// listing: one damaged file must not hide every other recovery option.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Read`] when a directory cannot be listed.
    pub fn list(&self) -> Result<Vec<JournalEntry>, JournalError> {
        let mut all = Vec::new();
        for object_key in self.object_keys()? {
            all.extend(self.entries_for(&object_key)?);
        }
        all.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(all)
    }

    /// The most recent entry for one object.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Read`] when the directory cannot be listed.
    pub fn latest_for(&self, object_uri: &str) -> Result<Option<JournalEntry>, JournalError> {
        Ok(self.entries_for(&paths::object_key(object_uri))?.pop())
    }

    /// Every object with entries in this system.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Read`] when the system directory cannot be
    /// listed. A system with no entries yet has no objects, not an error.
    pub fn object_keys(&self) -> Result<Vec<String>, JournalError> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let read = std::fs::read_dir(&self.root).map_err(|source| JournalError::Read {
            path: self.root.clone(),
            source,
        })?;
        let mut keys = Vec::new();
        for entry in read {
            let entry = entry.map_err(|source| JournalError::Read {
                path: self.root.clone(),
                source,
            })?;
            if entry.path().is_dir() {
                keys.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        Ok(keys)
    }

    /// Entry ids for one object, unsorted.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Read`] when the directory cannot be listed.
    pub fn ids_for(&self, object_key: &str) -> Result<Vec<String>, JournalError> {
        let directory = self.object_dir(object_key);
        if !directory.is_dir() {
            return Ok(Vec::new());
        }
        let read = std::fs::read_dir(&directory).map_err(|source| JournalError::Read {
            path: directory.clone(),
            source,
        })?;
        let mut ids = Vec::new();
        for entry in read {
            let entry = entry.map_err(|source| JournalError::Read {
                path: directory.clone(),
                source,
            })?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(&format!(".{EXTENSION}"))
                && !name.starts_with(".tmp-")
            {
                ids.push(id.to_owned());
            }
        }
        Ok(ids)
    }

    /// # Errors
    ///
    /// Returns [`JournalError::Write`] when the entry cannot be removed.
    pub fn remove(&self, object_key: &str, id: &str) -> Result<(), JournalError> {
        let path = self
            .object_dir(object_key)
            .join(format!("{id}.{EXTENSION}"));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(JournalError::Write { path, source }),
        }
    }

    /// Removes an object's directory once nothing is left in it, so pruning
    /// does not leave empty directories behind.
    ///
    /// # Errors
    ///
    /// Never returns an error: a directory that is not empty, or not there, is
    /// simply left alone.
    pub fn remove_if_empty(&self, object_key: &str) -> Result<(), JournalError> {
        let _ = std::fs::remove_dir(self.object_dir(object_key));
        Ok(())
    }

    /// When an entry was last written, for age-based pruning.
    ///
    /// The file's own timestamp rather than the id parsed back into a date:
    /// same answer, and it cannot drift from the file it describes.
    #[must_use]
    pub fn modified_at(&self, object_key: &str, id: &str) -> Option<std::time::SystemTime> {
        std::fs::metadata(
            self.object_dir(object_key)
                .join(format!("{id}.{EXTENSION}")),
        )
        .and_then(|metadata| metadata.modified())
        .ok()
    }

    fn object_dir(&self, object_key: &str) -> PathBuf {
        self.root.join(object_key)
    }
}

fn serialize(entry: &JournalEntry) -> Result<String, JournalError> {
    serde_json::to_string_pretty(entry).map_err(|source| JournalError::EntryInvalid {
        id: entry.id.clone(),
        source,
    })
}

fn format(at: OffsetDateTime, description: &[BorrowedFormatItem<'_>]) -> String {
    at.format(description)
        // The format is a compile-time constant over an in-range timestamp, so
        // this cannot fail; a clock that made it fail should not stop an edit.
        .unwrap_or_else(|_| at.unix_timestamp().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::entry::{ContentRef, EntryStatus};
    use crate::reportable_error::ReportableError;

    const PROGRAM: &str = "/sap/bc/adt/programs/programs/zsample";
    const CLASS: &str = "/sap/bc/adt/oo/classes/zcl_other";

    fn store() -> (EntryStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        (EntryStore::new(dir.path().join("de3")), dir)
    }

    fn entry(uri: &str) -> JournalEntry {
        use crate::journal::entry::{EntryObject, EntrySystem, JournalOperation};
        use crate::sap::object_family::AdtObjectFamily;

        JournalEntry {
            id: String::new(),
            recorded_at: String::new(),
            status: EntryStatus::Pending,
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
            operation: JournalOperation::Activate,
            transport: None,
            active_before: ContentRef::Sha256("a".repeat(64)),
            inactive_before: None,
            active_after: None,
            etag_after: None,
            undo_progress: None,
        }
    }

    fn key(uri: &str) -> String {
        paths::object_key(uri)
    }

    #[test]
    fn writes_an_entry_and_reads_it_back() {
        let (store, _dir) = store();
        let written = store.create(entry(PROGRAM)).unwrap();

        assert!(!written.id.is_empty());
        assert!(!written.recorded_at.is_empty());
        assert_eq!(store.read(&key(PROGRAM), &written.id).unwrap(), written);
    }

    #[test]
    fn each_object_gets_its_own_directory() {
        let (store, _dir) = store();
        store.create(entry(PROGRAM)).unwrap();
        store.create(entry(CLASS)).unwrap();

        let mut keys = store.object_keys().unwrap();
        keys.sort();
        let mut expected = [key(PROGRAM), key(CLASS)];
        expected.sort();
        assert_eq!(keys, expected);
    }

    #[test]
    fn one_objects_entries_are_a_directory_listing_not_a_scan() {
        let (store, _dir) = store();
        store.create(entry(PROGRAM)).unwrap();
        store.create(entry(CLASS)).unwrap();
        let newest = store.create(entry(PROGRAM)).unwrap();

        let found = store.entries_for(&key(PROGRAM)).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found.last().unwrap().id, newest.id);
    }

    #[test]
    fn ids_sort_chronologically_within_an_object() {
        let (store, _dir) = store();
        let first = store.create(entry(PROGRAM)).unwrap();
        let second = store.create(entry(PROGRAM)).unwrap();

        let mut ids = [second.id.clone(), first.id.clone()];
        ids.sort();
        assert_eq!(ids[0], first.id);
    }

    #[test]
    fn two_entries_in_one_millisecond_both_survive() {
        let (store, _dir) = store();
        let first = store.create(entry(PROGRAM)).unwrap();
        // Force the collision the suffix exists for.
        let path = store
            .root()
            .join(key(PROGRAM))
            .join(format!("{}.json", first.id));
        std::fs::write(&path, "{}").unwrap();
        let second = store.create(entry(PROGRAM)).unwrap();

        assert_ne!(first.id, second.id);
        assert_eq!(store.ids_for(&key(PROGRAM)).unwrap().len(), 2);
    }

    #[test]
    fn resolving_an_entry_replaces_it_in_place() {
        let (store, _dir) = store();
        let mut written = store.create(entry(PROGRAM)).unwrap();
        written.succeeded(ContentRef::Sha256("b".repeat(64)), Some("etag".to_owned()));
        store.update(&written).unwrap();

        let read = store.read(&key(PROGRAM), &written.id).unwrap();
        assert_eq!(read.status, EntryStatus::Succeeded);
        assert_eq!(read.etag_after.as_deref(), Some("etag"));
        assert_eq!(store.ids_for(&key(PROGRAM)).unwrap().len(), 1);
    }

    #[test]
    fn resolving_an_entry_that_is_not_there_is_an_error() {
        let (store, _dir) = store();
        let mut missing = entry(PROGRAM);
        missing.id = "20260905T221503.412Z".to_owned();

        assert_eq!(
            store.update(&missing).unwrap_err().code(),
            "journal_entry_missing"
        );
    }

    #[test]
    fn listing_is_oldest_first_across_objects_and_skips_damaged_entries() {
        let (store, _dir) = store();
        let first = store.create(entry(PROGRAM)).unwrap();
        let second = store.create(entry(CLASS)).unwrap();
        let directory = store.root().join(key(PROGRAM));
        std::fs::write(directory.join("20260101T000000.000Z.json"), "not json").unwrap();

        let listed = store.list().unwrap();
        // One damaged file must not hide every other recovery option.
        assert_eq!(
            listed.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            vec![first.id, second.id]
        );
    }

    #[test]
    fn the_latest_entry_for_an_object_ignores_other_objects() {
        let (store, _dir) = store();
        store.create(entry(PROGRAM)).unwrap();
        store.create(entry(CLASS)).unwrap();
        let newest = store.create(entry(PROGRAM)).unwrap();

        // Case-insensitively, since the key is built from a lowercased URI.
        let found = store
            .latest_for("/sap/bc/adt/programs/programs/ZSAMPLE")
            .unwrap()
            .expect("finds one");
        assert_eq!(found.id, newest.id);
    }

    #[test]
    fn an_entry_can_be_found_by_id_alone() {
        // What `journal show` does with an id copied from a listing.
        let (store, _dir) = store();
        store.create(entry(PROGRAM)).unwrap();
        let wanted = store.create(entry(CLASS)).unwrap();

        assert_eq!(store.find(&wanted.id).unwrap().id, wanted.id);
        assert_eq!(
            store.find("20260101T000000.000Z").unwrap_err().code(),
            "journal_entry_missing"
        );
    }

    #[test]
    fn an_object_with_no_history_is_none_rather_than_an_error() {
        let (store, _dir) = store();
        assert!(store.latest_for(PROGRAM).unwrap().is_none());
        assert!(store.list().unwrap().is_empty());
        assert!(store.object_keys().unwrap().is_empty());
    }

    #[test]
    fn temporary_files_are_not_entries() {
        let (store, _dir) = store();
        store.create(entry(PROGRAM)).unwrap();
        let directory = store.root().join(key(PROGRAM));
        std::fs::write(directory.join(".tmp-20260905T221503.412Z.json"), "{}").unwrap();

        assert_eq!(store.ids_for(&key(PROGRAM)).unwrap().len(), 1);
    }

    #[test]
    fn an_empty_object_directory_is_removed_but_a_used_one_is_kept() {
        let (store, _dir) = store();
        let written = store.create(entry(PROGRAM)).unwrap();
        store.create(entry(CLASS)).unwrap();

        store.remove_if_empty(&key(CLASS)).unwrap();
        assert!(store.root().join(key(CLASS)).is_dir(), "still has an entry");

        store.remove(&key(PROGRAM), &written.id).unwrap();
        store.remove_if_empty(&key(PROGRAM)).unwrap();
        assert!(!store.root().join(key(PROGRAM)).exists());
    }

    #[test]
    fn a_damaged_entry_read_directly_says_which_one() {
        let (store, _dir) = store();
        let directory = store.root().join(key(PROGRAM));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("bad.json"), "not json").unwrap();

        assert_eq!(
            store.read(&key(PROGRAM), "bad").unwrap_err().code(),
            "journal_entry_invalid"
        );
    }

    #[cfg(unix)]
    #[test]
    fn entries_are_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt as _;

        let (store, _dir) = store();
        let written = store.create(entry(PROGRAM)).unwrap();
        let path = store
            .root()
            .join(key(PROGRAM))
            .join(format!("{}.json", written.id));
        let mode = std::fs::metadata(path).unwrap().permissions().mode();

        assert_eq!(mode & 0o777, 0o600, "entry mode {mode:o}");
    }
}
