//! One journal entry: what an object looked like before an operation, and what
//! became of it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::sap::{
    class_include::ClassInclude, editable_source::EditableAdtObjectType,
    metadata_object::MetadataAdtObjectType, object_family::AdtObjectFamily,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// Written before the operation. Also what a crash between the two writes
    /// leaves behind.
    Pending,
    Succeeded,
    /// The operation was refused. The before-image is still true.
    Failed,
    /// SAP accepted it and the read-back could not confirm the result.
    Unverified,
    /// An undo put the object back the way this entry found it.
    ///
    /// The undo writes no entry of its own: this one records both that the
    /// operation happened and that it was reversed, so the journal holds one
    /// entry per logical change rather than a chain of undos.
    Undone,
}

impl EntryStatus {
    /// The JSON spelling, which is what a caller filtering the output sees.
    ///
    /// Written out rather than derived from `Debug`, which drifts the moment a
    /// variant gains a payload and takes the output with it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unverified => "unverified",
            Self::Undone => "undone",
        }
    }

    /// Whether `undo` may act on an entry in this state without `--force`.
    ///
    /// `Pending` is excluded because it has no after-image, which is what the
    /// staleness gate compares against. `Undone` is excluded because it has
    /// already been reversed.
    #[must_use]
    pub const fn is_undoable(self) -> bool {
        matches!(self, Self::Succeeded | Self::Unverified)
    }
}

/// A reference to stored content, or the fact that there was none.
///
/// `Absent` is not `sha256("")`: an object holding an empty document is not the
/// same as an object that is not there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentRef {
    Sha256(String),
    Absent,
}

impl ContentRef {
    #[must_use]
    pub fn sha256(&self) -> Option<&str> {
        match self {
            Self::Sha256(sha256) => Some(sha256),
            Self::Absent => None,
        }
    }
}

/// Which system the entry belongs to. Only the host part of `base_url` keys the
/// directory; the rest is information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrySystem {
    pub base_url: String,
    pub profile: String,
    pub client: String,
    pub user: String,
}

/// One row a table write changed, and what it changed about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowWrite {
    /// What was done to the row.
    pub operation: RowOperation,
    /// The complete primary key that addressed the row. The client is not here:
    /// it is the session's, and `EntrySystem` already records it.
    pub key: BTreeMap<String, String>,
    /// Only the fields this write changed.
    pub changes: Vec<FieldChange>,
}

/// What a table write did to one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowOperation {
    #[default]
    Update,
    Insert,
    Delete,
}

impl RowOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Insert => "insert",
            Self::Delete => "delete",
        }
    }

    /// What undoing it does.
    #[must_use]
    pub const fn inverse(self) -> Self {
        match self {
            Self::Update => Self::Update,
            Self::Insert => Self::Delete,
            Self::Delete => Self::Insert,
        }
    }
}

/// One field, before and after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldChange {
    pub field: String,
    pub before: String,
    pub after: String,
}

/// The three steps of undoing an activation, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationUndoStep {
    WroteInactive,
    Activated,
    RestoredInactive,
}

/// One journal entry: the system it belongs to, and what was recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// The file name, without its extension.
    pub id: String,
    pub recorded_at: String,
    pub status: EntryStatus,
    pub system: EntrySystem,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// Shared by the entries of one group activation: the objects SAP was
    /// asked to activate together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub record: EntryRecord,
}

/// The operation, the object it acted on, and the content it recorded.
///
/// One type because the three decide each other: the object decides how many
/// text streams there are, and the operation decides what each stream records.
/// A delete has no after-image, a table write records rows rather than source,
/// and a class records every include — none of which can be stated wrongly
/// here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum EntryRecord {
    Activate {
        object: RecordedObject<ActivationImages>,
        /// How far an interrupted undo got, so a rerun resumes rather than
        /// restarts. Absent until an undo starts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        undo_progress: Option<ActivationUndoStep>,
    },
    /// Restoring one is create, write and activate, and is not automated. See
    /// `journal show` for the recipe, which needs these two beyond the content.
    Delete {
        /// What the delete destroyed, per stream.
        object: RecordedObject<ContentRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// One command's worth of row changes, against one table.
    ///
    /// The unit of undo is the entry, so a command that changes several rows
    /// records them together — an entry covering part of a change would report
    /// a successful undo having reversed only some of it.
    TableWrite {
        table: TableRef,
        rows: Vec<RecordedRow>,
    },
}

/// A repository object, and one `T` per text stream it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedObject<T> {
    /// An object with one source stream.
    Source {
        object_type: SingleSourceType,
        name: String,
        uri: String,
        content: T,
    },
    /// A class: one stream per include, each recorded.
    Class {
        name: String,
        uri: String,
        content: ClassStreams<T>,
    },
    /// An object whose XML document is the object.
    Metadata {
        object_type: MetadataAdtObjectType,
        name: String,
        uri: String,
        content: T,
    },
}

/// Which stream of a recorded object something is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stream {
    /// The only stream of a single-source or metadata object.
    Whole,
    Include(ClassInclude),
}

impl Stream {
    /// `None` for an object with one stream.
    #[must_use]
    pub const fn include(self) -> Option<ClassInclude> {
        match self {
            Self::Whole => None,
            Self::Include(include) => Some(include),
        }
    }

    /// A class include's name, or `content` for a single stream.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Whole => "content",
            Self::Include(include) => include.as_str(),
        }
    }
}

impl<T> RecordedObject<T> {
    /// Builds the record for an object from its edit identity, one `T` per
    /// stream. The one place that decides class or not, so a class can only
    /// ever be recorded with every include.
    pub fn for_family(
        family: AdtObjectFamily,
        name: String,
        uri: String,
        mut content: impl FnMut(Stream) -> T,
    ) -> Self {
        match family {
            AdtObjectFamily::Source(EditableAdtObjectType::Class) => Self::Class {
                name,
                uri,
                content: ClassStreams::from_fn(|include| content(Stream::Include(include))),
            },
            AdtObjectFamily::Source(object_type) => Self::Source {
                object_type: SingleSourceType(object_type),
                name,
                uri,
                content: content(Stream::Whole),
            },
            AdtObjectFamily::Metadata(object_type) => Self::Metadata {
                object_type,
                name,
                uri,
                content: content(Stream::Whole),
            },
        }
    }

    #[must_use]
    pub const fn family(&self) -> AdtObjectFamily {
        match self {
            Self::Source { object_type, .. } => AdtObjectFamily::Source(object_type.0),
            Self::Class { .. } => AdtObjectFamily::Source(EditableAdtObjectType::Class),
            Self::Metadata { object_type, .. } => AdtObjectFamily::Metadata(*object_type),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Source { name, .. } | Self::Class { name, .. } | Self::Metadata { name, .. } => {
                name
            }
        }
    }

    #[must_use]
    pub fn uri(&self) -> &str {
        match self {
            Self::Source { uri, .. } | Self::Class { uri, .. } | Self::Metadata { uri, .. } => uri,
        }
    }

    /// Every stream and what was recorded for it, `main` first for a class.
    #[must_use]
    pub fn streams(&self) -> Vec<(Stream, &T)> {
        match self {
            Self::Source { content, .. } | Self::Metadata { content, .. } => {
                vec![(Stream::Whole, content)]
            }
            Self::Class { content, .. } => content
                .iter()
                .map(|(include, value)| (Stream::Include(include), value))
                .collect(),
        }
    }

    pub fn streams_mut(&mut self) -> Vec<(Stream, &mut T)> {
        match self {
            Self::Source { content, .. } | Self::Metadata { content, .. } => {
                vec![(Stream::Whole, content)]
            }
            Self::Class { content, .. } => content
                .iter_mut()
                .map(|(include, value)| (Stream::Include(include), value))
                .collect(),
        }
    }

    /// The same object with each stream's value replaced.
    pub fn map<U>(self, mut f: impl FnMut(Stream, T) -> U) -> RecordedObject<U> {
        match self {
            Self::Source {
                object_type,
                name,
                uri,
                content,
            } => RecordedObject::Source {
                object_type,
                name,
                uri,
                content: f(Stream::Whole, content),
            },
            Self::Class { name, uri, content } => RecordedObject::Class {
                name,
                uri,
                content: content.map(|include, value| f(Stream::Include(include), value)),
            },
            Self::Metadata {
                object_type,
                name,
                uri,
                content,
            } => RecordedObject::Metadata {
                object_type,
                name,
                uri,
                content: f(Stream::Whole, content),
            },
        }
    }

    /// The value for one stream, if the object has it.
    #[must_use]
    pub const fn stream(&self, stream: Stream) -> Option<&T> {
        match (self, stream) {
            (Self::Source { content, .. } | Self::Metadata { content, .. }, Stream::Whole) => {
                Some(content)
            }
            (Self::Class { content, .. }, Stream::Include(include)) => Some(content.get(include)),
            _ => None,
        }
    }
}

/// One value per class include. A struct rather than a map, so recording a
/// class without one of them does not compile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassStreams<T> {
    pub main: T,
    pub definitions: T,
    pub implementations: T,
    pub macros: T,
    pub testclasses: T,
}

impl<T> ClassStreams<T> {
    pub fn from_fn(mut f: impl FnMut(ClassInclude) -> T) -> Self {
        Self {
            main: f(ClassInclude::Main),
            definitions: f(ClassInclude::Definitions),
            implementations: f(ClassInclude::Implementations),
            macros: f(ClassInclude::Macros),
            testclasses: f(ClassInclude::Testclasses),
        }
    }

    #[must_use]
    pub const fn get(&self, include: ClassInclude) -> &T {
        match include {
            ClassInclude::Main => &self.main,
            ClassInclude::Definitions => &self.definitions,
            ClassInclude::Implementations => &self.implementations,
            ClassInclude::Macros => &self.macros,
            ClassInclude::Testclasses => &self.testclasses,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (ClassInclude, &T)> {
        [
            (ClassInclude::Main, &self.main),
            (ClassInclude::Definitions, &self.definitions),
            (ClassInclude::Implementations, &self.implementations),
            (ClassInclude::Macros, &self.macros),
            (ClassInclude::Testclasses, &self.testclasses),
        ]
        .into_iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (ClassInclude, &mut T)> {
        [
            (ClassInclude::Main, &mut self.main),
            (ClassInclude::Definitions, &mut self.definitions),
            (ClassInclude::Implementations, &mut self.implementations),
            (ClassInclude::Macros, &mut self.macros),
            (ClassInclude::Testclasses, &mut self.testclasses),
        ]
        .into_iter()
    }

    pub fn map<U>(self, mut f: impl FnMut(ClassInclude, T) -> U) -> ClassStreams<U> {
        ClassStreams {
            main: f(ClassInclude::Main, self.main),
            definitions: f(ClassInclude::Definitions, self.definitions),
            implementations: f(ClassInclude::Implementations, self.implementations),
            macros: f(ClassInclude::Macros, self.macros),
            testclasses: f(ClassInclude::Testclasses, self.testclasses),
        }
    }
}

/// What one stream looked like around an activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationImages {
    pub active_before: ContentRef,
    /// The pending work the activation published. Absent when there was none
    /// in this stream, which is different from pending work that was empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inactive_before: Option<ContentRef>,
    /// Unset until the operation resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_after: Option<ContentRef>,
}

/// The table a row write addressed. Not a [`RecordedObject`]: its content is
/// rows, not the table's definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableRef {
    pub name: String,
    pub uri: String,
}

/// One row write, and the row on either side of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedRow {
    #[serde(flatten)]
    pub write: RowWrite,
    /// The row as read before the write, as canonical JSON. Absent for an
    /// insert.
    pub before: ContentRef,
    /// The row re-read after the commit. Unset until resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<ContentRef>,
}

/// An editable source type that has exactly one stream: anything but a class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SingleSourceType(EditableAdtObjectType);

impl SingleSourceType {
    #[must_use]
    pub const fn get(self) -> EditableAdtObjectType {
        self.0
    }

    /// `None` for a class, which is recorded per include instead.
    #[must_use]
    pub fn new(object_type: EditableAdtObjectType) -> Option<Self> {
        (object_type != EditableAdtObjectType::Class).then_some(Self(object_type))
    }
}

impl Serialize for SingleSourceType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for SingleSourceType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let object_type = EditableAdtObjectType::parse(&value).map_err(serde::de::Error::custom)?;
        Self::new(object_type)
            .ok_or_else(|| serde::de::Error::custom("a class is recorded per include"))
    }
}

impl EntryRecord {
    /// The stable spelling for output.
    #[must_use]
    pub const fn operation(&self) -> &'static str {
        match self {
            Self::Activate { .. } => "activate",
            Self::Delete { .. } => "delete",
            Self::TableWrite { .. } => "table_write",
        }
    }

    /// The object's ADT URI, which keys the entry's directory.
    #[must_use]
    pub fn object_uri(&self) -> &str {
        match self {
            Self::Activate { object, .. } => object.uri(),
            Self::Delete { object, .. } => object.uri(),
            Self::TableWrite { table, .. } => &table.uri,
        }
    }

    #[must_use]
    pub fn object_name(&self) -> &str {
        match self {
            Self::Activate { object, .. } => object.name(),
            Self::Delete { object, .. } => object.name(),
            Self::TableWrite { table, .. } => &table.name,
        }
    }

    /// The logical type name, as `journal list --type` matches it.
    #[must_use]
    pub const fn object_type(&self) -> &'static str {
        match self {
            Self::Activate { object, .. } => object.family().as_str(),
            Self::Delete { object, .. } => object.family().as_str(),
            Self::TableWrite { .. } => "TABL",
        }
    }

    /// Every blob this record references, for the mark-and-sweep.
    #[must_use]
    pub fn referenced_blobs(&self) -> Vec<&str> {
        let refs: Vec<&ContentRef> = match self {
            Self::Activate { object, .. } => object
                .streams()
                .into_iter()
                .flat_map(|(_, images)| {
                    [
                        Some(&images.active_before),
                        images.inactive_before.as_ref(),
                        images.active_after.as_ref(),
                    ]
                })
                .flatten()
                .collect(),
            Self::Delete { object, .. } => object
                .streams()
                .into_iter()
                .map(|(_, content)| content)
                .collect(),
            Self::TableWrite { rows, .. } => rows
                .iter()
                .flat_map(|row| [Some(&row.before), row.after.as_ref()])
                .flatten()
                .collect(),
        };
        refs.into_iter().filter_map(ContentRef::sha256).collect()
    }
}

impl JournalEntry {
    #[must_use]
    pub fn object_uri(&self) -> &str {
        self.record.object_uri()
    }

    /// Every blob this entry references, for the mark-and-sweep.
    #[must_use]
    pub fn referenced_blobs(&self) -> Vec<&str> {
        self.record.referenced_blobs()
    }

    /// One stream's images, if this records an activation.
    #[must_use]
    pub const fn activation_images(&self, stream: Stream) -> Option<&ActivationImages> {
        match &self.record {
            EntryRecord::Activate { object, .. } => object.stream(stream),
            EntryRecord::Delete { .. } | EntryRecord::TableWrite { .. } => None,
        }
    }

    /// What a delete destroyed in one stream, if this records a delete.
    #[must_use]
    pub const fn deleted_content(&self, stream: Stream) -> Option<&ContentRef> {
        match &self.record {
            EntryRecord::Delete { object, .. } => object.stream(stream),
            EntryRecord::Activate { .. } | EntryRecord::TableWrite { .. } => None,
        }
    }

    pub const fn failed(&mut self) {
        self.status = EntryStatus::Failed;
    }

    /// An undo reversed this operation.
    pub const fn undone(&mut self) {
        self.status = EntryStatus::Undone;
    }

    /// Records how far an undo of this activation has got.
    ///
    /// Does nothing for an operation with no automated undo, so a step from one
    /// sequence can never be recorded against another.
    pub const fn record_undo_step(&mut self, step: ActivationUndoStep) {
        if let EntryRecord::Activate { undo_progress, .. } = &mut self.record {
            *undo_progress = Some(step);
        }
    }

    /// How far an undo of this entry has got.
    #[must_use]
    pub const fn undo_progress(&self) -> Option<ActivationUndoStep> {
        match &self.record {
            EntryRecord::Activate { undo_progress, .. } => *undo_progress,
            EntryRecord::Delete { .. } | EntryRecord::TableWrite { .. } => None,
        }
    }
}

/// A single-source activation entry over one blob, for tests elsewhere in
/// the crate that care about the journal's mechanics and not its shapes.
#[cfg(test)]
#[must_use]
pub fn sample_activation(uri: &str, active_before: ContentRef) -> JournalEntry {
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
        transport: None,
        group: None,
        record: EntryRecord::Activate {
            object: RecordedObject::Source {
                object_type: SingleSourceType(EditableAdtObjectType::Program),
                name: "ZSAMPLE".to_owned(),
                uri: uri.to_owned(),
                content: ActivationImages {
                    active_before,
                    inactive_before: None,
                    active_after: None,
                },
            },
            undo_progress: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> EntrySystem {
        EntrySystem {
            base_url: "https://sap.example:8001".to_owned(),
            profile: "dev".to_owned(),
            client: "100".to_owned(),
            user: "developer".to_owned(),
        }
    }

    fn sha(byte: char) -> ContentRef {
        ContentRef::Sha256(byte.to_string().repeat(64))
    }

    fn images(before: char) -> ActivationImages {
        ActivationImages {
            active_before: sha(before),
            inactive_before: None,
            active_after: None,
        }
    }

    fn entry(record: EntryRecord) -> JournalEntry {
        JournalEntry {
            id: "20260905T221503.412Z".to_owned(),
            recorded_at: "2026-09-05T22:15:03.412Z".to_owned(),
            status: EntryStatus::Pending,
            system: system(),
            transport: None,
            group: None,
            record,
        }
    }

    fn class_activation() -> JournalEntry {
        entry(EntryRecord::Activate {
            object: RecordedObject::for_family(
                AdtObjectFamily::parse("CLAS").unwrap(),
                "ZCL_SAMPLE".to_owned(),
                "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                |stream| match stream {
                    Stream::Include(ClassInclude::Testclasses) => ActivationImages {
                        active_before: ContentRef::Absent,
                        inactive_before: None,
                        active_after: None,
                    },
                    Stream::Include(ClassInclude::Implementations) => ActivationImages {
                        active_before: sha('b'),
                        inactive_before: Some(sha('c')),
                        active_after: Some(sha('d')),
                    },
                    _ => images('a'),
                },
            ),
            undo_progress: None,
        })
    }

    fn program_activation() -> JournalEntry {
        entry(EntryRecord::Activate {
            object: RecordedObject::for_family(
                AdtObjectFamily::parse("PROG").unwrap(),
                "ZSAMPLE".to_owned(),
                "/sap/bc/adt/programs/programs/zsample".to_owned(),
                |_| images('a'),
            ),
            undo_progress: None,
        })
    }

    fn json(entry: &JournalEntry) -> serde_json::Value {
        serde_json::to_value(entry).expect("serializes")
    }

    #[test]
    fn every_shape_round_trips() {
        let delete = entry(EntryRecord::Delete {
            object: RecordedObject::for_family(
                AdtObjectFamily::parse("DTEL").unwrap(),
                "ZSAMPLE_DE".to_owned(),
                "/sap/bc/adt/ddic/dataelements/zsample_de".to_owned(),
                |_| sha('e'),
            ),
            package: Some("ZPKG".to_owned()),
            description: None,
        });
        let rows = entry(EntryRecord::TableWrite {
            table: TableRef {
                name: "ZSAMPLE_T".to_owned(),
                uri: "/sap/bc/adt/ddic/tables/zsample_t".to_owned(),
            },
            rows: vec![RecordedRow {
                write: RowWrite {
                    operation: RowOperation::Update,
                    key: BTreeMap::from([("ID".to_owned(), "1".to_owned())]),
                    changes: vec![],
                },
                before: sha('f'),
                after: Some(sha('0')),
            }],
        });
        for original in [class_activation(), program_activation(), delete, rows] {
            let text = serde_json::to_string(&original).unwrap();
            assert_eq!(
                serde_json::from_str::<JournalEntry>(&text).unwrap(),
                original
            );
        }
    }

    #[test]
    fn a_class_is_recorded_with_every_include() {
        let value = json(&class_activation());
        let content = &value["record"]["object"]["content"];
        for include in ClassInclude::ALL {
            assert!(
                content.get(include.as_str()).is_some(),
                "{} missing",
                include.as_str()
            );
        }
        assert_eq!(value["record"]["object"]["kind"], "class");
        assert_eq!(value["record"]["operation"], "activate");
    }

    #[test]
    fn a_class_cannot_be_read_back_as_a_single_source() {
        let mut value = json(&program_activation());
        value["record"]["object"]["object_type"] = serde_json::json!("CLAS");
        assert!(serde_json::from_value::<JournalEntry>(value).is_err());
    }

    #[test]
    fn referenced_blobs_walk_every_include_and_skip_absences() {
        let entry = class_activation();
        let mut blobs = entry.referenced_blobs();
        blobs.sort_unstable();
        blobs.dedup();
        let expected: Vec<String> = ['a', 'b', 'c', 'd']
            .into_iter()
            .map(|byte| byte.to_string().repeat(64))
            .collect();
        assert_eq!(blobs, expected);
    }

    #[test]
    fn referenced_blobs_walk_every_row() {
        let row = |before: char, after: Option<char>| RecordedRow {
            write: RowWrite {
                operation: RowOperation::Update,
                key: BTreeMap::new(),
                changes: vec![],
            },
            before: sha(before),
            after: after.map(sha),
        };
        let entry = entry(EntryRecord::TableWrite {
            table: TableRef {
                name: "ZSAMPLE_T".to_owned(),
                uri: "/sap/bc/adt/ddic/tables/zsample_t".to_owned(),
            },
            rows: vec![row('1', Some('2')), row('3', None)],
        });
        assert_eq!(entry.referenced_blobs().len(), 3);
    }

    #[test]
    fn absent_is_not_an_empty_hash() {
        let empty = ContentRef::Sha256(crate::source_change::source_sha256(""));
        assert_ne!(ContentRef::Absent, empty);
        assert_eq!(ContentRef::Absent.sha256(), None);
        assert_ne!(
            serde_json::to_value(&ContentRef::Absent).unwrap(),
            serde_json::to_value(&empty).unwrap()
        );
    }

    #[test]
    fn an_activation_carries_its_own_undo_progress() {
        let mut entry = program_activation();
        assert_eq!(entry.undo_progress(), None);

        entry.record_undo_step(ActivationUndoStep::Activated);
        assert_eq!(entry.undo_progress(), Some(ActivationUndoStep::Activated));
        assert_eq!(json(&entry)["record"]["undo_progress"], "activated");
    }

    #[test]
    fn a_delete_cannot_record_a_step_from_the_activation_sequence() {
        let mut entry = entry(EntryRecord::Delete {
            object: RecordedObject::for_family(
                AdtObjectFamily::parse("PROG").unwrap(),
                "ZSAMPLE".to_owned(),
                "/sap/bc/adt/programs/programs/zsample".to_owned(),
                |_| sha('a'),
            ),
            package: None,
            description: None,
        });

        entry.record_undo_step(ActivationUndoStep::Activated);

        assert_eq!(entry.undo_progress(), None);
        assert!(json(&entry)["record"].get("undo_progress").is_none());
    }

    #[test]
    fn unset_fields_are_omitted_rather_than_null() {
        let value = json(&program_activation());
        assert!(value.get("transport").is_none());
        let content = &value["record"]["object"]["content"];
        for absent in ["inactive_before", "active_after"] {
            assert!(content.get(absent).is_none(), "{absent} should be omitted");
        }
    }

    #[test]
    fn only_resolved_entries_are_undoable() {
        assert!(!EntryStatus::Pending.is_undoable());
        assert!(!EntryStatus::Failed.is_undoable());
        assert!(EntryStatus::Succeeded.is_undoable());
        assert!(EntryStatus::Unverified.is_undoable());
        assert!(!EntryStatus::Undone.is_undoable());
    }

    #[test]
    fn the_object_type_reads_as_the_type_every_command_takes() {
        assert_eq!(class_activation().record.object_type(), "CLAS");
        assert_eq!(program_activation().record.object_type(), "PROG");
    }
}
