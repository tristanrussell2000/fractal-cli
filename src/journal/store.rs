//! Reading and writing entries for one system.

use std::path::{Path, PathBuf};

use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

use super::JournalError;
use super::blobs::{create_private_dir, write_private_file};
use super::entry::JournalEntry;

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
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Writes a new entry, allocating its id.
    ///
    /// The caller supplies everything but `id` and `recorded_at`; both are set
    /// here so an id always matches the file it names.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Write`] when the entry cannot be written, or
    /// [`JournalError::IdExhausted`] when too many entries claim one
    /// millisecond.
    pub fn create(&self, mut entry: JournalEntry) -> Result<JournalEntry, JournalError> {
        create_private_dir(&self.root)?;
        let now = OffsetDateTime::now_utc();
        entry.recorded_at = format(now, RECORDED_AT_FORMAT);
        let stamp = format(now, ID_FORMAT);

        for attempt in 0..MAX_ID_ATTEMPTS {
            let id = if attempt == 0 {
                stamp.clone()
            } else {
                format!("{stamp}-{attempt:02}")
            };
            let path = self.path_of(&id);
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
        let path = self.path_of(&entry.id);
        if !path.is_file() {
            return Err(JournalError::EntryMissing {
                id: entry.id.clone(),
            });
        }
        let temporary = self.root.join(format!(".tmp-{}.{EXTENSION}", entry.id));
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
    pub fn read(&self, id: &str) -> Result<JournalEntry, JournalError> {
        let path = self.path_of(id);
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

    /// Every entry for this system, oldest first.
    ///
    /// An entry that cannot be parsed is skipped rather than failing the whole
    /// listing: one damaged file must not hide every other recovery option.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Read`] when the directory cannot be listed. A
    /// system with no entries yet is empty, not an error.
    pub fn list(&self) -> Result<Vec<JournalEntry>, JournalError> {
        let mut ids = self.ids()?;
        ids.sort();
        Ok(ids
            .into_iter()
            .filter_map(|id| self.read(&id).ok())
            .collect())
    }

    /// The most recent entry for one object, by ADT URI.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Read`] when the directory cannot be listed.
    pub fn latest_for(&self, object_uri: &str) -> Result<Option<JournalEntry>, JournalError> {
        Ok(self
            .list()?
            .into_iter()
            .rfind(|entry| entry.object.uri.eq_ignore_ascii_case(object_uri)))
    }

    /// Entry ids present on disk, unsorted.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Read`] when the directory cannot be listed.
    pub fn ids(&self) -> Result<Vec<String>, JournalError> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let entries = std::fs::read_dir(&self.root).map_err(|source| JournalError::Read {
            path: self.root.clone(),
            source,
        })?;
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| JournalError::Read {
                path: self.root.clone(),
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
    pub fn remove(&self, id: &str) -> Result<(), JournalError> {
        let path = self.path_of(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(JournalError::Write { path, source }),
        }
    }

    /// When an entry was last written, for age-based pruning.
    ///
    /// The file's own timestamp rather than the id parsed back into a date:
    /// same answer, and it cannot drift from the file it describes.
    #[must_use]
    pub fn modified_at(&self, id: &str) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.path_of(id))
            .and_then(|metadata| metadata.modified())
            .ok()
    }

    fn path_of(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.{EXTENSION}"))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
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
                host: "sap.example".to_owned(),
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

    #[test]
    fn writes_an_entry_and_reads_it_back() {
        let (store, _dir) = store();
        let written = store
            .create(entry("/sap/bc/adt/programs/programs/zsample"))
            .unwrap();

        assert!(!written.id.is_empty());
        assert!(!written.recorded_at.is_empty());
        assert_eq!(store.read(&written.id).unwrap(), written);
    }

    #[test]
    fn the_id_is_the_file_name_and_sorts_chronologically() {
        let (store, _dir) = store();
        let first = store.create(entry("/a")).unwrap();
        let second = store.create(entry("/b")).unwrap();

        assert!(store.root().join(format!("{}.json", first.id)).is_file());
        // Fixed-width and most-significant-first, so a plain string sort is a
        // chronological sort.
        let mut ids = [second.id.clone(), first.id.clone()];
        ids.sort();
        assert_eq!(ids[0], first.id);
    }

    #[test]
    fn two_entries_in_one_millisecond_both_survive() {
        let (store, _dir) = store();
        let first = store.create(entry("/a")).unwrap();
        // Force the collision the suffix exists for.
        std::fs::write(store.root().join(format!("{}.json", first.id)), "{}").unwrap();
        let second = store.create(entry("/b")).unwrap();

        assert_ne!(first.id, second.id);
        assert_eq!(store.ids().unwrap().len(), 2);
    }

    #[test]
    fn a_new_entry_never_overwrites_an_existing_one() {
        let (store, _dir) = store();
        let first = store.create(entry("/a")).unwrap();
        let before =
            std::fs::read_to_string(store.root().join(format!("{}.json", first.id))).unwrap();

        let second = store.create(entry("/b")).unwrap();
        let after =
            std::fs::read_to_string(store.root().join(format!("{}.json", first.id))).unwrap();

        assert_ne!(first.id, second.id);
        assert_eq!(before, after);
    }

    #[test]
    fn resolving_an_entry_replaces_it_in_place() {
        let (store, _dir) = store();
        let mut written = store.create(entry("/a")).unwrap();
        written.succeeded(ContentRef::Sha256("b".repeat(64)), Some("etag".to_owned()));
        store.update(&written).unwrap();

        let read = store.read(&written.id).unwrap();
        assert_eq!(read.status, EntryStatus::Succeeded);
        assert_eq!(read.etag_after.as_deref(), Some("etag"));
        assert_eq!(store.ids().unwrap().len(), 1);
    }

    #[test]
    fn resolving_an_entry_that_is_not_there_is_an_error() {
        let (store, _dir) = store();
        let mut missing = entry("/a");
        missing.id = "20260905T221503.412Z".to_owned();

        assert_eq!(
            store.update(&missing).unwrap_err().code(),
            "journal_entry_missing"
        );
    }

    #[test]
    fn listing_is_oldest_first_and_skips_damaged_entries() {
        let (store, _dir) = store();
        let first = store.create(entry("/a")).unwrap();
        let second = store.create(entry("/b")).unwrap();
        std::fs::write(store.root().join("20260101T000000.000Z.json"), "not json").unwrap();

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
        store
            .create(entry("/sap/bc/adt/programs/programs/zsample"))
            .unwrap();
        store
            .create(entry("/sap/bc/adt/oo/classes/zcl_other"))
            .unwrap();
        let newest = store
            .create(entry("/sap/bc/adt/programs/programs/zsample"))
            .unwrap();

        let found = store
            .latest_for("/sap/bc/adt/programs/programs/ZSAMPLE")
            .unwrap()
            .expect("finds one");
        assert_eq!(found.id, newest.id);
    }

    #[test]
    fn an_object_with_no_history_is_none_rather_than_an_error() {
        let (store, _dir) = store();
        assert!(
            store
                .latest_for("/sap/bc/adt/programs/programs/zsample")
                .unwrap()
                .is_none()
        );
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn temporary_files_are_not_entries() {
        let (store, _dir) = store();
        store.create(entry("/a")).unwrap();
        std::fs::write(store.root().join(".tmp-20260905T221503.412Z.json"), "{}").unwrap();

        assert_eq!(store.ids().unwrap().len(), 1);
    }

    #[test]
    fn a_damaged_entry_read_directly_says_which_one() {
        let (store, _dir) = store();
        std::fs::create_dir_all(store.root()).unwrap();
        std::fs::write(store.root().join("bad.json"), "not json").unwrap();

        assert_eq!(
            store.read("bad").unwrap_err().code(),
            "journal_entry_invalid"
        );
    }

    #[cfg(unix)]
    #[test]
    fn entries_are_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt as _;

        let (store, _dir) = store();
        let written = store.create(entry("/a")).unwrap();
        let mode = std::fs::metadata(store.root().join(format!("{}.json", written.id)))
            .unwrap()
            .permissions()
            .mode();

        assert_eq!(mode & 0o777, 0o600, "entry mode {mode:o}");
    }
}
