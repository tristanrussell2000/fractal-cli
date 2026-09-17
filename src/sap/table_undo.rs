use thiserror::Error;

use super::{
    client::SapClient,
    table_write::{
        FieldDivergence, FieldValue, TableWriteRequest, TableWriteRunError, WriteMode,
        diagnose_divergence, row_changed, write_table_row,
    },
};
use crate::config::EditPolicy;
use crate::journal::JournalError;
use crate::journal::entry::{JournalEntry, RowWrite};
use crate::journal::recorder::Journal;
use crate::reportable_error::ReportableError;

impl From<TableWriteRunError> for TableUndoError {
    fn from(error: TableWriteRunError) -> Self {
        Self::Write(Box::new(error))
    }
}

/// What undoing one recorded table write will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableUndoPlan {
    pub entry_id: String,
    pub table: String,
    /// The one row this undo reverses.
    ///
    /// The journal stores a list, so that a batched write can be recorded as
    /// one entry without a format change. Undo handles exactly one and refuses
    /// the rest: reversing the first row of several and reporting success is
    /// the silent partial undo that roadmap item 11 exists to prevent. When a
    /// write can produce several rows, the undo grows with it, in the same
    /// change.
    pub row: TableUndoRow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableUndoRow {
    pub key: Vec<FieldValue>,
    /// Field, the value the write left, and the value to restore.
    pub restore: Vec<(String, String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableUndoOutcome {
    pub plan: TableUndoPlan,
    pub status: String,
    pub rows_affected: i64,
    /// Filled in only when the undo was refused because the row moved.
    pub divergence: Vec<FieldDivergence>,
}

/// Why an undo was refused, and exactly which fields moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub id: String,
    pub table: String,
    pub divergence: Vec<FieldDivergence>,
}

impl std::fmt::Display for Divergence {
    /// Names the fields that moved, in the message.
    ///
    /// The error envelope carries five strings and nothing structured, so a
    /// message that only said "diverged" would leave the caller to go and find
    /// out what did.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} no longer holds what journal entry {} recorded writing",
            self.table, self.id
        )?;
        for field in self.divergence.iter().filter(|field| field.diverged) {
            write!(
                formatter,
                "; {} expected '{}', found {}",
                field.field,
                field.expected,
                field
                    .found
                    .as_ref()
                    .map_or_else(|| "no such row".to_owned(), |found| format!("'{found}'"))
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum TableUndoError {
    #[error("journal entry {id} records no table write")]
    NotATableWrite { id: String },
    #[error("journal entry {id} records {rows} rows, and undo reverses one")]
    NotOneRow { id: String, rows: usize },
    #[error("journal entry {id} is {status} and has no result to undo")]
    Unresolved { id: String, status: String },
    #[error("journal entry {id} has already been undone")]
    AlreadyUndone { id: String },
    #[error("{0}")]
    Diverged(Box<Divergence>),
    #[error(transparent)]
    Write(Box<TableWriteRunError>),
    #[error(transparent)]
    Journal(#[from] JournalError),
}

impl ReportableError for TableUndoError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotATableWrite { .. } => "undo_not_a_table_write",
            Self::NotOneRow { .. } => "undo_row_count_unsupported",
            Self::Unresolved { .. } => "undo_entry_unresolved",
            Self::AlreadyUndone { .. } => "undo_already_undone",
            Self::Diverged(_) => "undo_row_diverged",
            Self::Write(error) => error.code(),
            Self::Journal(error) => error.code(),
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::Write(error) => error.status(),
            Self::Journal(error) => error.status(),
            _ => None,
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::NotATableWrite { .. } => {
                "`fractal undo` reverses a table write only for an entry that records one."
                    .to_owned()
            }
            Self::NotOneRow { .. } => {
                "Reversing part of a recorded change and reporting success would be worse than \
                 refusing. `fractal journal show` prints every row the entry holds."
                    .to_owned()
            }
            Self::Unresolved { .. } => {
                "Only a write that is recorded as having succeeded can be reversed.".to_owned()
            }
            Self::AlreadyUndone { .. } => "The row is already back the way it was.".to_owned(),
            Self::Diverged(_) => {
                "Somebody changed the row after this write, so reversing it now would discard \
                 their change. Read the row, decide what it should hold, and set it explicitly."
                    .to_owned()
            }
            Self::Write(error) => error.hint()?,
            Self::Journal(error) => error.hint()?,
        })
    }
}

/// Reads the reversal out of a journal entry.
///
/// # Errors
///
/// Returns [`TableUndoError`] when the entry is not a resolved table write.
pub fn plan_table_undo(entry: &JournalEntry) -> Result<TableUndoPlan, TableUndoError> {
    if !entry.status.is_undoable() {
        return Err(match entry.status {
            crate::journal::entry::EntryStatus::Undone => TableUndoError::AlreadyUndone {
                id: entry.id.clone(),
            },
            status => TableUndoError::Unresolved {
                id: entry.id.clone(),
                status: status.as_str().to_owned(),
            },
        });
    }
    let Some(rows) = entry.operation.table_write_rows() else {
        return Err(TableUndoError::NotATableWrite {
            id: entry.id.clone(),
        });
    };

    let [row] = rows else {
        return Err(TableUndoError::NotOneRow {
            id: entry.id.clone(),
            rows: rows.len(),
        });
    };

    Ok(TableUndoPlan {
        entry_id: entry.id.clone(),
        table: entry.object.name.clone(),
        row: reverse_row(row),
    })
}

/// One recorded row write, read backwards.
fn reverse_row(row: &RowWrite) -> TableUndoRow {
    TableUndoRow {
        key: row
            .key
            .iter()
            .map(|(field, value)| FieldValue {
                field: field.clone(),
                value: value.clone(),
            })
            .collect(),
        restore: row
            .changes
            .iter()
            .map(|change| {
                (
                    change.field.clone(),
                    change.after.clone(),
                    change.before.clone(),
                )
            })
            .collect(),
    }
}

/// Puts the recorded row back.
///
/// The reversal is the write's own statement with the two values swapped, so it
/// is one guarded `UPDATE` per row: every field the write changed reverts
/// together, or none does and the row is reported as diverged.
///
/// # Errors
///
/// Returns [`TableUndoError`] when the row has moved since the write, or when
/// the reversal cannot be run.
pub async fn undo_table_write(
    sap: &mut SapClient,
    policy: &EditPolicy,
    username: &str,
    entry: &JournalEntry,
    journal: &Journal,
) -> Result<TableUndoOutcome, TableUndoError> {
    let plan = plan_table_undo(entry)?;
    let row = &plan.row;

    let request = TableWriteRequest {
        table: plan.table.clone(),
        keys: row.key.clone(),
        sets: row
            .restore
            .iter()
            .map(|(field, _left, restore)| FieldValue {
                field: field.clone(),
                value: restore.clone(),
            })
            .collect(),
        // The guard is what the original write left behind, not what the row
        // holds now. Guarding on the current value would revert a colleague's
        // change instead of refusing to.
        // An undo of a customizing change records into the same request the
        // write used; the journal kept it.
        transport: entry.transport.clone(),
        expected_before: Some(
            row.restore
                .iter()
                .map(|(field, left, _restore)| (field.to_ascii_uppercase(), left.clone()))
                .collect(),
        ),
    };

    // Not journaled: an undo is the reversal of an entry that already exists,
    // and recording it would offer an undo of the undo that the status
    // transition already expresses.
    let outcome = write_table_row(sap, policy, username, &request, WriteMode::Execute, None).await?;

    if row_changed(&outcome.envelope.status) {
        let mut entry = entry.clone();
        entry.undone();
        journal.entries().update(&entry)?;
        return Ok(TableUndoOutcome {
            plan,
            status: outcome.envelope.status,
            rows_affected: outcome.envelope.rows,
            divergence: Vec::new(),
        });
    }

    // The write refused because the row no longer holds what we left. Name the
    // fields rather than reporting only that the guard failed.
    let expected: Vec<(String, String)> = row
        .restore
        .iter()
        .map(|(field, left, _restore)| (field.clone(), left.clone()))
        .collect();
    let divergence = diagnose_divergence(sap, &plan.table, &row.key, &expected).await?;
    Err(TableUndoError::Diverged(Box::new(Divergence {
        id: plan.entry_id,
        table: plan.table,
        divergence,
    })))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::journal::entry::{
        EntryObject, EntryStatus, EntrySystem, FieldChange, JournalOperation,
    };
    use crate::sap::editable_source::EditableAdtObjectType;
    use crate::sap::object_family::AdtObjectFamily;

    fn entry(status: EntryStatus, operation: JournalOperation) -> JournalEntry {
        JournalEntry {
            id: "0001".to_owned(),
            recorded_at: "2026-01-01T00:00:00Z".to_owned(),
            status,
            system: EntrySystem {
                base_url: "https://sap.example".to_owned(),
                profile: "dev".to_owned(),
                client: "100".to_owned(),
                user: "developer".to_owned(),
            },
            object: EntryObject {
                object_type: AdtObjectFamily::Source(EditableAdtObjectType::Table),
                name: "ZSAMPLE_RECORD".to_owned(),
                uri: "/sap/bc/adt/ddic/tables/zsample_record".to_owned(),
                source_part: None,
            },
            operation,
            transport: None,
            active_before: crate::journal::entry::ContentRef::Absent,
            inactive_before: None,
            active_after: None,
            etag_after: None,
        }
    }

    fn write() -> JournalOperation {
        JournalOperation::table_write(vec![RowWrite {
            key: BTreeMap::from([("ID".to_owned(), "R1".to_owned())]),
            changes: vec![
                FieldChange {
                    field: "STATUS".to_owned(),
                    before: "OPEN".to_owned(),
                    after: "DONE".to_owned(),
                },
                FieldChange {
                    field: "NOTE".to_owned(),
                    before: "first".to_owned(),
                    after: "second".to_owned(),
                },
            ],
        }])
    }

    #[test]
    fn reads_the_write_backwards() {
        let plan = plan_table_undo(&entry(EntryStatus::Succeeded, write())).unwrap();

        assert_eq!(plan.table, "ZSAMPLE_RECORD");
        assert_eq!(plan.row.key[0].field, "ID");
        assert_eq!(
            plan.row.restore,
            vec![
                ("STATUS".to_owned(), "DONE".to_owned(), "OPEN".to_owned()),
                ("NOTE".to_owned(), "second".to_owned(), "first".to_owned()),
            ]
        );
    }

    #[test]
    fn refuses_an_entry_holding_more_than_one_row() {
        let many = JournalOperation::table_write(vec![
            RowWrite {
                key: BTreeMap::from([("ID".to_owned(), "R1".to_owned())]),
                changes: vec![FieldChange {
                    field: "STATUS".to_owned(),
                    before: "OPEN".to_owned(),
                    after: "DONE".to_owned(),
                }],
            },
            RowWrite {
                key: BTreeMap::from([("ID".to_owned(), "R2".to_owned())]),
                changes: vec![FieldChange {
                    field: "STATUS".to_owned(),
                    before: "OPEN".to_owned(),
                    after: "DONE".to_owned(),
                }],
            },
        ]);
        let error = plan_table_undo(&entry(EntryStatus::Succeeded, many)).unwrap_err();
        assert_eq!(error.code(), "undo_row_count_unsupported");
    }

    #[test]
    fn refuses_an_entry_holding_no_rows() {
        let none = JournalOperation::table_write(Vec::new());
        let error = plan_table_undo(&entry(EntryStatus::Succeeded, none)).unwrap_err();
        assert_eq!(error.code(), "undo_row_count_unsupported");
    }

    #[test]
    fn refuses_an_entry_that_is_not_a_table_write() {
        let error =
            plan_table_undo(&entry(EntryStatus::Succeeded, JournalOperation::activate())).unwrap_err();
        assert_eq!(error.code(), "undo_not_a_table_write");
    }

    #[test]
    fn refuses_an_entry_that_was_already_undone() {
        let error = plan_table_undo(&entry(EntryStatus::Undone, write())).unwrap_err();
        assert_eq!(error.code(), "undo_already_undone");
    }

    #[test]
    fn refuses_a_write_that_never_resolved() {
        let error = plan_table_undo(&entry(EntryStatus::Pending, write())).unwrap_err();
        assert_eq!(error.code(), "undo_entry_unresolved");
    }

    #[test]
    fn a_row_write_stores_rows_not_source() {
        assert_eq!(
            entry(EntryStatus::Succeeded, write()).content_kind(),
            crate::journal::entry::ContentKind::Row
        );
    }
}
