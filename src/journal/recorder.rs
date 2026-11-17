//! Recording one operation: the only supported way to write to the journal.
//!
//! Every write here is **put, write the entry, put again**, and the two puts do
//! different jobs.
//!
//! The **second** put is what makes the sweep safe. Storing content that
//! already exists writes nothing, so a concurrent sweep cannot see that a new
//! entry is about to reference it; the put after the entry lands touches the
//! blob, which a sweep started earlier respects via `mark_started`, or rewrites
//! it if that sweep already took it.
//!
//! The **first** put is not needed for that — the hash can be computed locally
//! and the entry written before any blob exists. It is there for the crash
//! window: with it, dying between the entry write and the confirm leaves an
//! orphan blob, which the sweep later collects. Without it, the same crash
//! leaves an entry pointing at content that was never written, which is a
//! recovery someone believes they have. It costs an open and a `set_times`,
//! not a second write of the content.
//!
//! Doing either by hand at each call site would work until someone forgot, so
//! callers get [`Journal`] and never the two stores directly.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::JournalError;
use super::blobs::BlobStore;
use super::entry::{
    ActivationImages, ContentRef, EntryRecord, EntryStatus, EntrySystem, JournalEntry,
    RecordedObject, RecordedRow, RowWrite, Stream, TableRef,
};
use super::paths;
use super::store::EntryStore;

/// The journal for one SAP system.
///
/// Its entry store *is* that system's directory, so the identity it stamps on
/// entries describes the same system its location does.
pub struct Journal {
    blobs: BlobStore,
    entries: EntryStore,
    system: EntrySystem,
}

/// What one stream held before an activation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivationBefore {
    /// `None` when the stream had no active version.
    pub active: Option<String>,
    /// The pending work about to be published, or `None` when there was none.
    pub inactive: Option<String>,
}

/// What each stream holds after an operation, as read back.
///
/// A stream missing from the map was not read and gets no after-image; a
/// stream mapped to `None` was read and is absent.
pub type AfterImages = BTreeMap<Stream, Option<String>>;

impl Journal {
    /// Opens the journal for a profile's system.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when the platform has no data directory, or
    /// when the profile's URL has no host to key on.
    pub fn open(
        profile_name: &str,
        profile: &crate::config::Profile,
    ) -> Result<Self, JournalError> {
        Ok(Self::with_roots(
            paths::blob_root()?,
            paths::entry_root(&profile.base_url)?,
            EntrySystem {
                base_url: profile.base_url.clone(),
                profile: profile_name.to_owned(),
                client: profile.client.clone(),
                user: profile.username.clone(),
            },
        ))
    }

    /// A journal over roots the caller placed, for tests and for any caller
    /// that does not read its location from a profile.
    #[must_use]
    pub const fn with_roots(blob_root: PathBuf, entry_root: PathBuf, system: EntrySystem) -> Self {
        Self {
            blobs: BlobStore::new(blob_root),
            entries: EntryStore::new(entry_root),
            system,
        }
    }

    #[must_use]
    pub const fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    #[must_use]
    pub const fn entries(&self) -> &EntryStore {
        &self.entries
    }

    /// Records an activation about to run, as `pending`, with every stream's
    /// before-images.
    ///
    /// Called before the operation runs: the before-image can only be had
    /// beforehand, and a crash after this leaves a `pending` entry holding it.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when a blob or the entry cannot be written. The
    /// caller must treat that as fatal: an operation with no before-image has
    /// no recovery at all.
    pub fn begin_activation(
        &self,
        object: RecordedObject<ActivationBefore>,
        transport: Option<String>,
    ) -> Result<JournalEntry, JournalError> {
        self.begin_activation_in_group(object, transport, None)
    }

    /// [`Self::begin_activation`] for one object of a group activation, tagged
    /// with the id every entry of the group shares.
    ///
    /// # Errors
    ///
    /// As [`Self::begin_activation`].
    pub fn begin_activation_in_group(
        &self,
        object: RecordedObject<ActivationBefore>,
        transport: Option<String>,
        group: Option<String>,
    ) -> Result<JournalEntry, JournalError> {
        let contents = object
            .streams()
            .into_iter()
            .flat_map(|(_, before)| [before.active.clone(), before.inactive.clone()])
            .flatten()
            .collect::<Vec<_>>();
        let mut stored = Ok(());
        let object = object.map(|_, before| {
            let active_before = self.store_or(&mut stored, before.active.as_deref());
            let inactive_before = before
                .inactive
                .as_deref()
                .map(|content| self.store_or(&mut stored, Some(content)));
            ActivationImages {
                active_before,
                inactive_before,
                active_after: None,
            }
        });
        stored?;
        self.create(
            transport,
            group,
            EntryRecord::Activate {
                object,
                undo_progress: None,
            },
            &contents,
        )
    }

    /// Records a delete about to run, with what it is about to destroy.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when a blob or the entry cannot be written.
    pub fn begin_delete(
        &self,
        object: RecordedObject<Option<String>>,
        package: Option<String>,
        description: Option<String>,
        transport: Option<String>,
    ) -> Result<JournalEntry, JournalError> {
        let contents = object
            .streams()
            .into_iter()
            .filter_map(|(_, content)| content.clone())
            .collect::<Vec<_>>();
        let mut stored = Ok(());
        let object = object.map(|_, content| self.store_or(&mut stored, content.as_deref()));
        stored?;
        self.create(
            transport,
            None,
            EntryRecord::Delete {
                object,
                package,
                description,
            },
            &contents,
        )
    }

    /// Records row writes about to run, each with the row as read beforehand.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when a blob or the entry cannot be written.
    pub fn begin_table_write(
        &self,
        table: TableRef,
        rows: Vec<(RowWrite, Option<String>)>,
        transport: Option<String>,
    ) -> Result<JournalEntry, JournalError> {
        let contents = rows
            .iter()
            .filter_map(|(_, before)| before.clone())
            .collect::<Vec<_>>();
        let mut recorded = Vec::with_capacity(rows.len());
        for (write, before) in rows {
            recorded.push(RecordedRow {
                write,
                before: self.store(before.as_deref())?,
                after: None,
            });
        }
        self.create(
            transport,
            None,
            EntryRecord::TableWrite {
                table,
                rows: recorded,
            },
            &contents,
        )
    }

    /// Records that a table write landed, with each row re-read after it.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when a blob or the entry cannot be written.
    pub fn table_write_succeeded(
        &self,
        mut entry: JournalEntry,
        after: &[Option<String>],
    ) -> Result<JournalEntry, JournalError> {
        if let EntryRecord::TableWrite { rows, .. } = &mut entry.record {
            for (row, after) in rows.iter_mut().zip(after) {
                row.after = Some(self.store(after.as_deref())?);
            }
        }
        entry.status = EntryStatus::Succeeded;
        let contents = after.iter().flatten().cloned().collect::<Vec<_>>();
        self.resolve(entry, &contents)
    }

    /// Records that the operation worked, with what SAP holds now.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when a blob or the entry cannot be written.
    pub fn succeeded(
        &self,
        entry: JournalEntry,
        after: &AfterImages,
    ) -> Result<JournalEntry, JournalError> {
        self.with_after(entry, EntryStatus::Succeeded, after)
    }

    /// Records that SAP refused the operation. Nothing changed, so there is no
    /// after-image.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when the entry cannot be written.
    pub fn failed(&self, mut entry: JournalEntry) -> Result<JournalEntry, JournalError> {
        entry.failed();
        self.resolve(entry, &[])
    }

    /// Records that SAP accepted the operation and the read-back could not
    /// confirm it — the case where the before-image matters most.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when a blob or the entry cannot be written.
    pub fn unverified(
        &self,
        entry: JournalEntry,
        after: &AfterImages,
    ) -> Result<JournalEntry, JournalError> {
        self.with_after(entry, EntryStatus::Unverified, after)
    }

    /// Sets the status and, for an activation, each stream's after-image.
    /// A delete has no after-image to set: the object is gone.
    fn with_after(
        &self,
        mut entry: JournalEntry,
        status: EntryStatus,
        after: &AfterImages,
    ) -> Result<JournalEntry, JournalError> {
        if let EntryRecord::Activate { object, .. } = &mut entry.record {
            for (stream, images) in object.streams_mut() {
                if let Some(content) = after.get(&stream) {
                    images.active_after = Some(self.store(content.as_deref())?);
                }
            }
        }
        entry.status = status;
        let contents = after.values().flatten().cloned().collect::<Vec<_>>();
        self.resolve(entry, &contents)
    }

    /// Writes a new entry, then confirms the content it references.
    fn create(
        &self,
        transport: Option<String>,
        group: Option<String>,
        record: EntryRecord,
        contents: &[String],
    ) -> Result<JournalEntry, JournalError> {
        // Written once, complete: an entry is never on disk without its group.
        let entry = self.entries.create(JournalEntry {
            id: String::new(),
            recorded_at: String::new(),
            status: EntryStatus::Pending,
            system: self.system.clone(),
            transport,
            group,
            record,
        })?;
        self.confirm(contents)?;
        Ok(entry)
    }

    /// Writes the updated entry, then confirms the content it now references.
    fn resolve(
        &self,
        entry: JournalEntry,
        new_contents: &[String],
    ) -> Result<JournalEntry, JournalError> {
        self.entries.update(&entry)?;
        // Only the newly referenced content needs confirming: everything else
        // has been referenced by an entry on disk since it began.
        self.confirm(new_contents)?;
        Ok(entry)
    }

    /// `None` content is an absence, not empty content.
    fn store(&self, content: Option<&str>) -> Result<ContentRef, JournalError> {
        match content {
            Some(content) => Ok(ContentRef::Sha256(self.blobs.put(content)?)),
            None => Ok(ContentRef::Absent),
        }
    }

    /// [`Self::store`] inside a closure that cannot return early: the first
    /// failure is kept and the placeholder it returns is never written.
    fn store_or(
        &self,
        failure: &mut Result<(), JournalError>,
        content: Option<&str>,
    ) -> ContentRef {
        if failure.is_err() {
            return ContentRef::Absent;
        }
        self.store(content).unwrap_or_else(|error| {
            *failure = Err(error);
            ContentRef::Absent
        })
    }

    /// Re-stores content now that an entry references it.
    ///
    /// This is the put that matters for concurrency: it touches the blob so a
    /// sweep leaves it alone, and rewrites it if a sweep already took it.
    fn confirm(&self, contents: &[String]) -> Result<(), JournalError> {
        for content in contents {
            self.blobs.put(content)?;
        }
        Ok(())
    }
}

/// How an operation ended, from the journal's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution<'a> {
    /// The operation landed, with what each stream holds now.
    Succeeded(&'a AfterImages),
    /// SAP refused it, so nothing was published and the before-image is still
    /// true.
    Failed,
    /// SAP accepted it and the read-back could not confirm the result — the
    /// case where the before-image matters most. Holds whatever was read.
    Unverified(&'a AfterImages),
}

/// Resolves an entry, naming one that could not be completed.
///
/// **Never fails the operation.** By the time this runs the mutation has landed
/// or been refused, so returning an error would report failure for work that is
/// already done and invite a retry that cannot succeed. What survives is a
/// `pending` entry holding a real before-image: degraded, not useless. The
/// caller reports success and says which entry is stuck.
///
/// A free function rather than a method because both arguments are optional at
/// every call site — `--no-journal` gives no journal, and an operation that
/// failed before recording anything has no entry — and pushing that back to the
/// callers is what put three copies of this in the tree.
#[must_use]
pub fn resolve(
    journal: Option<&Journal>,
    entry: Option<JournalEntry>,
    resolution: Resolution<'_>,
) -> Option<String> {
    let (Some(journal), Some(entry)) = (journal, entry) else {
        return None;
    };
    let id = entry.id.clone();
    match resolution {
        Resolution::Succeeded(after) => journal.succeeded(entry, after),
        Resolution::Failed => journal.failed(entry),
        Resolution::Unverified(after) => journal.unverified(entry, after),
    }
    .err()
    .map(|_| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sap::class_include::ClassInclude;
    use crate::sap::object_family::AdtObjectFamily;
    use crate::source_change::source_sha256;

    fn journal() -> (Journal, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let journal = Journal::with_roots(
            dir.path().join("blobs"),
            dir.path().join("journal").join("qas"),
            EntrySystem {
                base_url: "https://sap.example:8001".to_owned(),
                profile: "dev".to_owned(),
                client: "100".to_owned(),
                user: "developer".to_owned(),
            },
        );
        (journal, dir)
    }

    const BEFORE: &str = "REPORT zsample. \" before";
    const PENDING: &str = "REPORT zsample. \" pending";
    const AFTER: &str = "REPORT zsample. \" after";

    fn program(before: ActivationBefore) -> RecordedObject<ActivationBefore> {
        RecordedObject::for_family(
            AdtObjectFamily::parse("PROG").unwrap(),
            "ZSAMPLE".to_owned(),
            "/sap/bc/adt/programs/programs/zsample".to_owned(),
            |_| before.clone(),
        )
    }

    fn before(active: Option<&str>, inactive: Option<&str>) -> ActivationBefore {
        ActivationBefore {
            active: active.map(str::to_owned),
            inactive: inactive.map(str::to_owned),
        }
    }

    fn whole(after: Option<&str>) -> AfterImages {
        AfterImages::from([(Stream::Whole, after.map(str::to_owned))])
    }

    fn images(entry: &JournalEntry, stream: Stream) -> ActivationImages {
        let EntryRecord::Activate { object, .. } = &entry.record else {
            panic!("not an activation");
        };
        object.stream(stream).unwrap().clone()
    }

    #[test]
    fn begin_stores_the_before_images_and_a_pending_entry() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin_activation(program(before(Some(BEFORE), Some(PENDING))), None)
            .unwrap();

        assert_eq!(entry.status, EntryStatus::Pending);
        let images = images(&entry, Stream::Whole);
        assert_eq!(
            images.active_before,
            ContentRef::Sha256(source_sha256(BEFORE))
        );
        assert!(images.active_after.is_none());
        for hash in entry.referenced_blobs() {
            assert!(journal.blobs().contains(hash), "blob {hash} missing");
        }
    }

    #[test]
    fn a_class_stores_every_include() {
        let (journal, _dir) = journal();
        let object = RecordedObject::for_family(
            AdtObjectFamily::parse("CLAS").unwrap(),
            "ZCL_SAMPLE".to_owned(),
            "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
            |stream| match stream {
                Stream::Include(ClassInclude::Testclasses) => before(None, None),
                Stream::Include(include) => before(Some(include.as_str()), None),
                Stream::Whole => unreachable!(),
            },
        );
        let entry = journal.begin_activation(object, None).unwrap();

        for include in ClassInclude::ALL {
            let images = images(&entry, Stream::Include(include));
            if include == ClassInclude::Testclasses {
                assert_eq!(images.active_before, ContentRef::Absent);
            } else {
                assert!(
                    journal
                        .blobs()
                        .contains(images.active_before.sha256().unwrap())
                );
            }
        }
    }

    #[test]
    fn no_pending_work_is_recorded_as_no_field_at_all() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin_activation(program(before(Some(BEFORE), None)), None)
            .unwrap();

        assert_eq!(images(&entry, Stream::Whole).inactive_before, None);
    }

    #[test]
    fn an_object_with_no_active_version_records_an_absence() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin_activation(program(before(None, Some(PENDING))), None)
            .unwrap();

        assert_eq!(
            images(&entry, Stream::Whole).active_before,
            ContentRef::Absent
        );
        assert!(!journal.blobs().contains(&source_sha256("")));
    }

    #[test]
    fn succeeding_stores_the_after_image_and_resolves() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin_activation(program(before(Some(BEFORE), Some(PENDING))), None)
            .unwrap();
        let resolved = journal.succeeded(entry, &whole(Some(AFTER))).unwrap();

        assert_eq!(resolved.status, EntryStatus::Succeeded);
        assert_eq!(
            images(&resolved, Stream::Whole).active_after,
            Some(ContentRef::Sha256(source_sha256(AFTER)))
        );
        assert_eq!(
            journal.entries().find(&resolved.id).unwrap().status,
            EntryStatus::Succeeded
        );
    }

    #[test]
    fn a_refused_operation_keeps_its_before_image_and_gains_no_after() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin_activation(program(before(Some(BEFORE), Some(PENDING))), None)
            .unwrap();
        let before = images(&entry, Stream::Whole);
        let resolved = journal.failed(entry).unwrap();

        assert_eq!(resolved.status, EntryStatus::Failed);
        assert_eq!(images(&resolved, Stream::Whole), before);
    }

    #[test]
    fn a_delete_records_what_it_destroyed_and_no_after() {
        let (journal, _dir) = journal();
        let object = RecordedObject::for_family(
            AdtObjectFamily::parse("PROG").unwrap(),
            "ZSAMPLE".to_owned(),
            "/sap/bc/adt/programs/programs/zsample".to_owned(),
            |_| Some(BEFORE.to_owned()),
        );
        let entry = journal.begin_delete(object, None, None, None).unwrap();
        let resolved = journal.succeeded(entry, &AfterImages::new()).unwrap();

        assert_eq!(resolved.status, EntryStatus::Succeeded);
        assert_eq!(resolved.referenced_blobs(), vec![source_sha256(BEFORE)]);
    }

    #[test]
    fn a_table_write_records_each_row_on_both_sides() {
        let (journal, _dir) = journal();
        let row = |key: &str| RowWrite {
            operation: crate::journal::entry::RowOperation::Update,
            key: BTreeMap::from([("ID".to_owned(), key.to_owned())]),
            changes: vec![],
        };
        let entry = journal
            .begin_table_write(
                TableRef {
                    name: "ZSAMPLE_T".to_owned(),
                    uri: "/sap/bc/adt/ddic/tables/zsample_t".to_owned(),
                },
                vec![
                    (row("1"), Some("{\"ID\":\"1\"}".to_owned())),
                    (row("2"), None),
                ],
                None,
            )
            .unwrap();
        let resolved = journal
            .table_write_succeeded(
                entry,
                &[Some("{\"ID\":\"1\",\"V\":\"x\"}".to_owned()), None],
            )
            .unwrap();

        let EntryRecord::TableWrite { rows, .. } = &resolved.record else {
            panic!("not a table write");
        };
        assert!(
            rows[0]
                .after
                .as_ref()
                .is_some_and(|after| after.sha256().is_some())
        );
        assert_eq!(rows[1].before, ContentRef::Absent);
        assert_eq!(rows[1].after, Some(ContentRef::Absent));
    }

    #[test]
    fn a_blob_is_touched_after_the_entry_write_that_first_references_it() {
        // The ordering the double `put` exists for, as an invariant a test can
        // check. Touching only *before* the entry write is unsound: a sweep can
        // start in between, see an untouched blob and no entry, and delete it.
        let (journal, _dir) = journal();

        let entry = journal
            .begin_activation(program(before(Some(BEFORE), Some(PENDING))), None)
            .unwrap();
        let began_at = entry_written_at(&journal, &entry);
        for hash in entry.referenced_blobs() {
            assert!(
                journal.blobs().modified_at(hash).unwrap() >= began_at,
                "blob {hash} was last touched before the entry that references it"
            );
        }

        let entry = journal.succeeded(entry, &whole(Some(AFTER))).unwrap();
        let resolved_at = entry_written_at(&journal, &entry);
        let after = source_sha256(AFTER);
        assert!(
            journal.blobs().modified_at(&after).unwrap() >= resolved_at,
            "the after-image was touched before the entry that references it"
        );
    }

    fn entry_written_at(journal: &Journal, entry: &JournalEntry) -> std::time::SystemTime {
        journal
            .entries()
            .modified_at(&paths::object_key(entry.object_uri()), &entry.id)
            .expect("entry exists")
    }

    #[test]
    fn resolving_names_an_entry_it_could_not_complete_rather_than_failing() {
        let (journal, dir) = journal();
        let entry = journal
            .begin_activation(program(before(Some(BEFORE), None)), None)
            .unwrap();
        let id = entry.id.clone();
        std::fs::remove_dir_all(dir.path().join("journal")).unwrap();

        let stuck = resolve(
            Some(&journal),
            Some(entry),
            Resolution::Succeeded(&whole(Some("after"))),
        );

        assert_eq!(stuck, Some(id));
    }

    #[test]
    fn resolving_reports_nothing_when_it_worked_or_when_there_is_no_journal() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin_activation(program(before(Some(BEFORE), None)), None)
            .unwrap();

        assert_eq!(
            resolve(
                Some(&journal),
                Some(entry),
                Resolution::Succeeded(&whole(None))
            ),
            None
        );
        assert_eq!(resolve(None, None, Resolution::Failed), None);
    }

    #[test]
    fn confirming_rewrites_content_a_sweep_already_took() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin_activation(program(before(Some(BEFORE), Some(PENDING))), None)
            .unwrap();
        std::fs::remove_file(journal.blobs().path_of(&source_sha256(AFTER))).ok();

        journal.succeeded(entry, &whole(Some(AFTER))).unwrap();
        assert!(journal.blobs().contains(&source_sha256(AFTER)));
    }
}
