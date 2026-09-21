use std::collections::BTreeMap;
use std::fmt::Write as _;

use thiserror::Error;

use super::{
    client::SapClient,
    editable_source::EditableAdtObjectType,
    exec_class::{ExecClassError, executor_class_name, rerun_executor, run_generated_source},
    object_family::AdtObjectFamily,
    transport::{TransportShowError, show_transport_request},
    table::{
        QueryOptions, TableError, TableFieldMetadata, TableMetadata, TableMetadataOptions,
        get_table_metadata, run_query,
    },
};
use crate::config::EditPolicy;
use crate::journal::entry::{
    EntryObject, FieldChange as JournalFieldChange, JournalOperation, RowOperation, RowWrite,
};
use crate::journal::recorder::Journal;
use crate::journal::JournalError;
use crate::pattern::glob_matches;
use crate::reportable_error::ReportableError;

/// The envelope a generated class prints. Its absence means the run failed:
/// `classrun` answers 200 with an error string for a class it cannot run, so
/// the status code cannot carry success on its own.
pub const ENVELOPE: &str = "fractal.table.v1";

/// A field and the value the caller gave for it, as written on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldValue {
    pub field: String,
    pub value: String,
}

/// One requested change to one row.
///
/// An enum rather than a struct with an operation tag, because the three
/// operations do not share a shape: an insert has no key field of its own (the
/// key is among the fields it sets) and no value to guard against, and a delete
/// has nothing to set. As one struct those were fields that silently did
/// nothing for two operations out of three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableWriteRequest {
    /// Change named fields of an existing row.
    Update {
        table: String,
        keys: Vec<FieldValue>,
        sets: Vec<FieldValue>,
        /// What the changed fields must hold **now**, when the caller knows.
        ///
        /// Without it the guard is built from the row as just read, which is
        /// right for a write: change what is there. An undo needs the opposite
        /// — the row must still hold what the original write left, or somebody
        /// else has changed it and reversing would discard their work. Keyed by
        /// upper-case field name.
        expected_before: Option<BTreeMap<String, String>>,
        transport: Option<String>,
    },
    /// Add a row. `fields` carries the whole row, key fields included.
    Insert {
        table: String,
        fields: Vec<FieldValue>,
        transport: Option<String>,
    },
    /// Remove the row with this key.
    Delete {
        table: String,
        keys: Vec<FieldValue>,
        transport: Option<String>,
    },
}

impl TableWriteRequest {
    #[must_use]
    pub fn table(&self) -> &str {
        match self {
            Self::Update { table, .. } | Self::Insert { table, .. } | Self::Delete { table, .. } => {
                table
            }
        }
    }

    #[must_use]
    pub fn transport(&self) -> Option<&str> {
        match self {
            Self::Update { transport, .. }
            | Self::Insert { transport, .. }
            | Self::Delete { transport, .. } => transport.as_deref(),
        }
    }

    #[must_use]
    pub const fn operation(&self) -> RowOperation {
        match self {
            Self::Update { .. } => RowOperation::Update,
            Self::Insert { .. } => RowOperation::Insert,
            Self::Delete { .. } => RowOperation::Delete,
        }
    }

    /// Where the key comes from.
    ///
    /// An insert names it among the fields it sets; an update and a delete name
    /// it separately.
    fn key_source(&self) -> &[FieldValue] {
        match self {
            Self::Insert { fields, .. } => fields,
            Self::Update { keys, .. } | Self::Delete { keys, .. } => keys,
        }
    }

    /// Every field the caller named, for the checks that apply to all of them.
    fn named(&self) -> impl Iterator<Item = &FieldValue> {
        match self {
            Self::Update { keys, sets, .. } => keys.iter().chain(sets.iter()),
            Self::Insert { fields, .. } => fields.iter().chain([].iter()),
            Self::Delete { keys, .. } => keys.iter().chain([].iter()),
        }
    }
}

/// A request checked against the table's real fields, with the row it will change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedTableWrite {
    pub operation: RowOperation,
    pub table: String,
    pub keys: Vec<FieldValue>,
    /// Each changed field, with the value it holds now and the value to write.
    pub changes: Vec<FieldChange>,
    /// Every field of the row as it stands, for the caller to see and keep.
    pub before: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChange {
    pub field: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Run the statement, then roll back.
    DryRun,
    Execute,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEnvelope {
    pub status: String,
    pub subrc: i64,
    pub rows: i64,
    pub detail: String,
}

#[derive(Debug, Error)]
pub enum TableWriteError {
    #[error("{table} is not a customer table")]
    NotCustomerTable { table: String },
    #[error("{table} is a customizing table (delivery class {delivery_class}) and needs a transport")]
    TransportRequired {
        table: String,
        delivery_class: String,
    },
    #[error("{transport} has no modifiable task belonging to {user}")]
    NoTaskForUser { transport: String, user: String },
    #[error("{table} has no field {field}")]
    UnknownField { table: String, field: String },
    #[error("{field} is a key field of {table} and cannot be changed")]
    KeyFieldNotSettable { table: String, field: String },
    #[error("the key of {table} needs {missing}")]
    IncompleteKey { table: String, missing: String },
    #[error("{field} was given more than once")]
    DuplicateField { field: String },
    #[error("no field to change was given")]
    NothingToChange,
    #[error("the value for {field} contains a character that cannot go into ABAP source")]
    UnusableValue { field: String },
    #[error("no row of {table} has key {key}")]
    RowNotFound { table: String, key: String },
    #[error("{table} already has a row with key {key}")]
    RowExists { table: String, key: String },
    #[error("the run produced no {ENVELOPE} envelope")]
    NoEnvelope { output: String },
}

impl ReportableError for TableWriteError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotCustomerTable { .. } => "table_write_not_customer_table",
            Self::TransportRequired { .. } => "table_write_transport_required",
            Self::NoTaskForUser { .. } => "table_write_no_task",
            Self::UnknownField { .. } => "table_write_unknown_field",
            Self::KeyFieldNotSettable { .. } => "table_write_key_not_settable",
            Self::IncompleteKey { .. } => "table_write_key_incomplete",
            Self::DuplicateField { .. } => "table_write_duplicate_field",
            Self::NothingToChange => "table_write_nothing_to_change",
            Self::UnusableValue { .. } => "table_write_value_unusable",
            Self::RowNotFound { .. } => "table_write_row_not_found",
            Self::RowExists { .. } => "table_write_row_exists",
            Self::NoEnvelope { .. } => "table_write_no_envelope",
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::NotCustomerTable { .. } => {
                "Only tables in a configured customer namespace can be written.".to_owned()
            }
            Self::TransportRequired { .. } => {
                "Pass --transport. A customizing change with no transport entry stays in this \
                 client and never reaches QA or production."
                    .to_owned()
            }
            Self::NoTaskForUser { .. } => {
                "The entry goes in a task, not the request itself. Create one, or name a request \
                 you own a modifiable task in."
                    .to_owned()
            }
            Self::UnknownField { table, .. } | Self::RowNotFound { table, .. } => {
                format!("`fractal table metadata {table}` lists the fields and keys.")
            }
            Self::RowExists { table, .. } => {
                format!("Change it with `fractal table set {table}`, or delete it first.")
            }
            Self::KeyFieldNotSettable { .. } => {
                "Changing a key means deleting the row and inserting another.".to_owned()
            }
            Self::IncompleteKey { .. } => {
                "A write addresses exactly one row, so every key field is required.".to_owned()
            }
            Self::DuplicateField { .. } => "Give each field once.".to_owned(),
            Self::NothingToChange => "Pass --set FIELD=VALUE.".to_owned(),
            Self::UnusableValue { .. } => {
                "Control characters cannot be represented in an ABAP literal.".to_owned()
            }
            Self::NoEnvelope { .. } => {
                "The class ran but printed no result. Its output is in `output`.".to_owned()
            }
        })
    }
}

/// Checks everything that can be known before the row is read.
///
/// Separate because the key is what the row is read *by*: a partial key would
/// otherwise build a query with no `WHERE` and fail as a SQL error rather than
/// as a refusal.
///
/// # Errors
///
/// Returns [`TableWriteError`] when a field does not exist, is given twice, is
/// a key being changed, when the key is partial, or a value cannot be written.
pub fn validate_shape(
    metadata: &TableMetadata,
    request: &TableWriteRequest,
) -> Result<(), TableWriteError> {
    let table = metadata.entity.to_ascii_uppercase();

    match request {
        TableWriteRequest::Update { sets, .. } => {
            if sets.is_empty() {
                return Err(TableWriteError::NothingToChange);
            }
            // Moving a key means deleting the row and inserting another. An
            // insert is free to name key fields, because that is how it gives
            // them.
            for set in sets {
                if is_key(metadata, &set.field) {
                    return Err(TableWriteError::KeyFieldNotSettable {
                        table,
                        field: set.field.to_ascii_uppercase(),
                    });
                }
            }
        }
        TableWriteRequest::Insert { fields, .. } => {
            if fields.is_empty() {
                return Err(TableWriteError::NothingToChange);
            }
        }
        TableWriteRequest::Delete { .. } => {}
    }

    let mut seen = Vec::new();
    for given in request.named() {
        let field = given.field.to_ascii_uppercase();
        if seen.contains(&field) {
            return Err(TableWriteError::DuplicateField { field });
        }
        if !metadata
            .fields
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case(&field))
        {
            return Err(TableWriteError::UnknownField { table, field });
        }
        check_value(&field, &given.value)?;
        seen.push(field);
    }

    // The client field is the session's, never the caller's.
    let given = request.key_source();
    let missing: Vec<_> = metadata
        .fields
        .iter()
        .filter(|f| f.is_key && !is_client_field(f))
        .filter(|f| !given.iter().any(|k| k.field.eq_ignore_ascii_case(&f.name)))
        .map(|f| f.name.to_ascii_uppercase())
        .collect();
    if !missing.is_empty() {
        return Err(TableWriteError::IncompleteKey {
            table,
            missing: missing.join(", "),
        });
    }

    Ok(())
}

/// Pairs each requested change with the value the row holds now.
///
/// # Errors
///
/// Returns [`TableWriteError::RowNotFound`] when the key matches no row.
pub fn resolve_changes(
    metadata: &TableMetadata,
    request: &TableWriteRequest,
    row: &BTreeMap<String, String>,
) -> Result<ValidatedTableWrite, TableWriteError> {
    let table = metadata.entity.to_ascii_uppercase();
    match request {
        // An insert must find nothing; an update and a delete must find a row.
        TableWriteRequest::Insert { .. } if !row.is_empty() => {
            return Err(TableWriteError::RowExists {
                table,
                key: describe_key(&key_fields(metadata, request)),
            });
        }
        TableWriteRequest::Update { .. } | TableWriteRequest::Delete { .. } if row.is_empty() => {
            return Err(TableWriteError::RowNotFound {
                table,
                key: describe_key(&key_fields(metadata, request)),
            });
        }
        _ => {}
    }

    let changes = match request {
        TableWriteRequest::Update {
            sets,
            expected_before,
            ..
        } => sets
            .iter()
            .map(|set| {
                let field = set.field.to_ascii_uppercase();
                let before = expected_before
                    .as_ref()
                    .and_then(|expected| expected.get(&field))
                    .or_else(|| row.get(&field))
                    .cloned()
                    .unwrap_or_default();
                FieldChange {
                    field,
                    before,
                    after: set.value.clone(),
                }
            })
            .collect(),
        // An insert has no before; a delete changes no fields.
        TableWriteRequest::Insert { fields, .. } => fields
            .iter()
            .map(|field| FieldChange {
                field: field.field.to_ascii_uppercase(),
                before: String::new(),
                after: field.value.clone(),
            })
            .collect(),
        TableWriteRequest::Delete { .. } => Vec::new(),
    };

    Ok(ValidatedTableWrite {
        operation: request.operation(),
        table,
        keys: key_fields(metadata, request),
        changes,
        before: row.clone(),
    })
}

/// The key that addresses the row, wherever the caller put it.
///
/// An insert supplies its key through `--set` along with every other field; an
/// update and a delete name it with `--key`. Both the row read and the
/// generated statement need the same answer.
fn key_fields(metadata: &TableMetadata, request: &TableWriteRequest) -> Vec<FieldValue> {
    let insert = matches!(request, TableWriteRequest::Insert { .. });
    request
        .key_source()
        .iter()
        // An insert names the whole row, so the key has to be picked out of it.
        .filter(|given| !insert || is_key(metadata, &given.field))
        // Never the client. SQL refuses it in a WHERE — "client handling is
        // performed by the compiler" — and the transport key adds `sy-mandt`
        // itself, so including it here would both break the read and double it.
        .filter(|given| {
            !metadata
                .fields
                .iter()
                .any(|field| field.name.eq_ignore_ascii_case(&given.field) && is_client_field(field))
        })
        .map(|given| FieldValue {
            value: pad_numeric(metadata, &given.field, &given.value),
            field: given.field.to_ascii_uppercase(),
        })
        .collect()
}

/// The key as the caller would have typed it, for a message about it.
///
/// A refusal that says only "that key" leaves the reader to go and find out
/// which key was tried — and a value that was padded on the way in is not the
/// one they typed.
fn describe_key(keys: &[FieldValue]) -> String {
    keys.iter()
        .map(|key| format!("{}={}", key.field.to_ascii_lowercase(), key.value))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A `NUMC` value at its declared width.
///
/// `3` and `00000003` are the same value in a `NUMC(8)` field, but not to the
/// database: a query for the short form matches nothing and comes back as a row
/// that is not there, with nothing pointing at the padding. ABAP pads on
/// assignment, so only the SQL read needs this — but padding here keeps the key
/// identical everywhere it is used, the journal and the transport key included.
///
/// Either spelling of the type is accepted. `col_type` is the surer of the
/// two: it comes from the recorded field list, so every listed field has one,
/// while `sap_type` is only there when the preview also returned that column.
fn pad_numeric(metadata: &TableMetadata, field: &str, value: &str) -> String {
    let Some(declared) = metadata
        .fields
        .iter()
        .find(|candidate| candidate.name.eq_ignore_ascii_case(field))
    else {
        return value.to_owned();
    };
    let Some(width) = declared.length.map(|length| length as usize) else {
        return value.to_owned();
    };
    let numeric = declared
        .col_type
        .as_deref()
        .is_some_and(|col_type| col_type.eq_ignore_ascii_case("NUMC"))
        || declared
            .sap_type
            .as_deref()
            .is_some_and(|sap_type| sap_type.eq_ignore_ascii_case("N"));

    // Only a short run of digits. Anything else is left alone to fail as
    // itself rather than as a padded version of something the caller did not
    // write.
    if !numeric
        || value.is_empty()
        || value.len() >= width
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return value.to_owned();
    }
    format!("{value:0>width$}")
}

fn is_key(metadata: &TableMetadata, field: &str) -> bool {
    metadata
        .fields
        .iter()
        .any(|f| f.is_key && f.name.eq_ignore_ascii_case(field))
}

/// Whether a field is the table's client column, which the caller never gives.
///
/// Decided from type information only, and from two spellings of it.
/// `col_type` is the one that answers: it is the recorded DDIC type, present
/// on every release. The declared type stays as a fallback for a field whose
/// recorded type is blank.
///
/// Deliberately not a check on the name. A key field called `CLIENT` that is
/// not the client would then go unconstrained in the statement, and the write
/// would land on the wrong row. Failing to recognise a client column only
/// refuses the write, which is the safe direction.
fn is_client_field(field: &TableFieldMetadata) -> bool {
    field
        .col_type
        .as_deref()
        .is_some_and(|col_type| col_type.eq_ignore_ascii_case("CLNT"))
        || matches!(
            field.declared_type.trim().to_ascii_lowercase().as_str(),
            "abap.clnt" | "mandt"
        )
}

/// A value has to survive becoming a quoted ABAP literal.
fn check_value(field: &str, value: &str) -> Result<(), TableWriteError> {
    if value.chars().any(char::is_control) {
        return Err(TableWriteError::UnusableValue {
            field: field.to_owned(),
        });
    }
    Ok(())
}

/// Quotes a value as an ABAP character literal.
///
/// Doubling the quote is the whole escape ABAP has; control characters are
/// refused earlier because there is no escape for them.
fn abap_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Builds the class that performs the write.
///
/// The statement is a guarded column-targeted `UPDATE`: it names only the
/// fields being changed, and repeats their current values in the `WHERE`. That
/// makes it a compare-and-swap in one statement — it cannot revert a field
/// somebody else changed in the meantime, and re-running it after it succeeded
/// changes nothing.
#[must_use]
pub fn generate_abap(
    class_name: &str,
    write: &ValidatedTableWrite,
    mode: WriteMode,
    transport_entry: Option<&TransportEntry>,
) -> String {
    let client_dependent = transport_entry.is_some_and(|entry| entry.client_dependent);
    let mut abap = declarations(class_name, write);
    abap.push_str(&literals(write));
    abap.push_str(&lock_block(write, client_dependent));

    abap.push_str("\n        IF lv_status IS INITIAL.\n");
    if let Some(entry) = transport_entry {
        abap.push_str(&transport_block(write, entry));
        // The row is only touched if the transport entry will be accepted.
        abap.push_str("\n        IF lv_status IS INITIAL.\n");
        abap.push_str(&statement(write));
        abap.push_str("        ENDIF.\n");
    } else {
        abap.push_str(&statement(write));
    }
    abap.push_str("        ENDIF.\n");

    abap.push_str(TRY_END);
    abap.push('\n');
    abap.push_str(settle_clause(mode));
    if let (Some(entry), WriteMode::Execute) = (transport_entry, mode) {
        abap.push_str(&transport_commit_block(entry));
    }
    abap.push_str(&unlock_block(write));
    abap.push_str(&envelope_clause());
    abap
}

/// Closes the `TRY` the literals opened. Shared, because the write may or may
/// not be wrapped in a transport check.
const TRY_END: &str = "      CATCH cx_root INTO DATA(lx_error).
        lv_status = 'exception'.
        lv_detail = lx_error->get_text( ).
    ENDTRY.
";

fn declarations(class_name: &str, write: &ValidatedTableWrite) -> String {
    let class = class_name.to_ascii_lowercase();
    let table = write.table.to_ascii_lowercase();
    let mut abap = format!(
        "CLASS {class} DEFINITION PUBLIC FINAL CREATE PUBLIC.
  PUBLIC SECTION.
    INTERFACES if_oo_adt_classrun.
ENDCLASS.

CLASS {class} IMPLEMENTATION.
  METHOD if_oo_adt_classrun~main.
    DATA ls_row TYPE {table}.
    DATA lv_status TYPE string.
    DATA lv_detail TYPE string.
    DATA lv_subrc TYPE i.
    DATA lv_rows TYPE i.
"
    );

    abap.push_str(
        "    DATA lt_e071 TYPE STANDARD TABLE OF e071.
    DATA lt_e071k TYPE STANDARD TABLE OF e071k.
    DATA ls_e071 TYPE e071.
    DATA ls_e071k TYPE e071k.
    DATA lv_tabkey TYPE e071k-tabkey.
    DATA lv_varkey TYPE rstable-varkey.
    DATA lv_off TYPE i VALUE 0.
    DATA lv_len TYPE i.
    DATA lv_key_too_long TYPE abap_bool VALUE abap_false.
",
    );
    for (index, key) in write.keys.iter().enumerate() {
        let field = key.field.to_ascii_lowercase();
        let _ = writeln!(abap, "    DATA lv_k{index} LIKE ls_row-{field}.");
    }
    for (index, change) in write.changes.iter().enumerate() {
        let field = change.field.to_ascii_lowercase();
        let _ = writeln!(
            abap,
            "    DATA lv_old{index} LIKE ls_row-{field}.
    DATA lv_new{index} LIKE ls_row-{field}."
        );
    }
    abap
}

/// Every caller value assigned to a field-typed variable.
///
/// Values never reach the statement as literals: assigning to a variable
/// declared `LIKE` the field is what converts a command-line string into the
/// field's real type, whatever that is.
fn literals(write: &ValidatedTableWrite) -> String {
    let mut abap = String::from("\n    TRY.\n");
    for (index, key) in write.keys.iter().enumerate() {
        let _ = writeln!(abap, "        lv_k{index} = {}.", abap_literal(&key.value));
    }
    for (index, change) in write.changes.iter().enumerate() {
        let _ = writeln!(
            abap,
            "        lv_old{index} = {}.
        lv_new{index} = {}.",
            abap_literal(&change.before),
            abap_literal(&change.after)
        );
    }
    abap
}

fn key_conditions(write: &ValidatedTableWrite, separator: &str) -> String {
    write
        .keys
        .iter()
        .enumerate()
        .map(|(index, key)| format!("{} = @lv_k{index}", key.field.to_ascii_lowercase()))
        .collect::<Vec<_>>()
        .join(separator)
}

/// The write itself: a column-targeted `UPDATE` that names only the changed
/// fields and repeats their current values in the `WHERE`.
///
/// That makes it a compare-and-swap in one statement. It cannot revert a field
/// somebody else changed meanwhile, and running it again after it succeeded
/// matches nothing.
fn statement(write: &ValidatedTableWrite) -> String {
    match write.operation {
        RowOperation::Insert => return insert_statement(write),
        RowOperation::Delete => return delete_statement(write),
        RowOperation::Update => {}
    }
    update_statement(write)
}

fn update_statement(write: &ValidatedTableWrite) -> String {
    let table = write.table.to_ascii_lowercase();
    let assignments = write
        .changes
        .iter()
        .enumerate()
        .map(|(index, change)| format!("{} = @lv_new{index}", change.field.to_ascii_lowercase()))
        .collect::<Vec<_>>()
        .join(",\n                   ");

    let mut conditions = vec![key_conditions(write, "\n                 AND ")];
    conditions.extend(
        write.changes.iter().enumerate().map(|(index, change)| {
            format!("{} = @lv_old{index}", change.field.to_ascii_lowercase())
        }),
    );
    let conditions = conditions.join("\n                 AND ");

    format!(
        "
        UPDATE {table} SET {assignments}
               WHERE {conditions}.
        lv_subrc = sy-subrc.
        lv_rows  = sy-dbcnt.

        IF lv_subrc = 0.
          lv_status = 'applied'.
        ELSE.
          SELECT SINGLE * FROM {table} INTO @ls_row
            WHERE {key_only}.
          IF sy-subrc <> 0.
            lv_status = 'row_not_found'.
          ELSE.
            lv_status = 'conflict'.
          ENDIF.
        ENDIF.
",
        key_only = key_conditions(write, "\n              AND "),
    )
}

/// The `TABU` entry a customizing change needs, and the task it goes in.
///
/// "Entry" throughout, never "recording": Fractal *records* in its journal, and
/// CTS takes an *entry*. A change to a delivery-class `C` or `G` table with no
/// entry exists in one client and never reaches QA or production. Silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportEntry {
    /// The **task**, not the parent request. `TR_APPEND_TO_COMM_OBJS_KEYS`
    /// refuses a request with `TK 127`, "changes to objects are only allowed in
    /// correction/repair".
    pub task: String,
    /// Whether the table has a client column, which leads the `TABU` key.
    pub client_dependent: bool,
}

/// The ABAP that puts one row's key into the task as a `TABU` entry.
///
/// Built the way SE16N builds it: `E071` naming the table, one `E071K` whose
/// `TABKEY` is the key fields concatenated at fixed character offsets, client
/// first. Lengths come from `DESCRIBE FIELD ... IN CHARACTER MODE` rather than
/// from the metadata, so a key type this code has not seen still lands at the
/// right offset.
///
/// `TABKEY` caps at 120 characters; SE16N writes `*` at the overflow and
/// refuses, and so does this.
fn transport_block(write: &ValidatedTableWrite, entry: &TransportEntry) -> String {
    let fill = key_string(write, "lv_tabkey", entry.client_dependent);

    format!(
        "
        ls_e071-pgmid    = 'R3TR'.
        ls_e071-object   = 'TABU'.
        ls_e071-obj_name = '{table_upper}'.
        ls_e071-objfunc  = 'K'.
        APPEND ls_e071 TO lt_e071.

{fill}
        IF lv_key_too_long = abap_true.
          lv_status = 'transport_key_too_long'.
        ELSE.
          ls_e071k-pgmid      = 'R3TR'.
          ls_e071k-object     = 'TABU'.
          ls_e071k-mastertype = 'TABU'.
          ls_e071k-mastername = '{table_upper}'.
          ls_e071k-objname    = '{table_upper}'.
          ls_e071k-tabkey     = lv_tabkey.
          APPEND ls_e071k TO lt_e071k.

*         Checked before the row is touched: a task that will not take the
*         entry must refuse the whole operation, not leave the data changed
*         and unrecorded.
          CALL FUNCTION 'TR_APPEND_TO_COMM_OBJS_KEYS'
            EXPORTING
              wi_simulation         = 'X'
              wi_suppress_key_check = ' '
              wi_trkorr             = '{task}'
            TABLES
              wt_e071               = lt_e071
              wt_e071k              = lt_e071k
            EXCEPTIONS
              OTHERS                = 68.
          IF sy-subrc <> 0.
            lv_status = 'transport_refused'.
            lv_detail = |TK{{ sy-msgno }} {{ sy-msgv1 }}|.
          ENDIF.
        ENDIF.
",
        table_upper = write.table.to_ascii_uppercase(),
        task = entry.task,
    )
}

/// The real append, after the row is committed.
fn transport_commit_block(entry: &TransportEntry) -> String {
    format!(
        "
      IF lv_status = 'applied'.
        CALL FUNCTION 'TR_APPEND_TO_COMM_OBJS_KEYS'
          EXPORTING
            wi_simulation         = ' '
            wi_suppress_key_check = ' '
            wi_trkorr             = '{task}'
          TABLES
            wt_e071               = lt_e071
            wt_e071k              = lt_e071k
          EXCEPTIONS
            OTHERS                = 68.
        IF sy-subrc <> 0.
*         The row is already committed. Saying so is the whole point: the
*         change is live in this client and will not travel.
          lv_status = 'applied_without_transport_entry'.
          lv_detail = |TK{{ sy-msgno }} {{ sy-msgv1 }}|.
        ELSE.
          lv_status = 'applied_with_transport_entry'.
        ENDIF.
      ENDIF.
",
        task = entry.task,
    )
}

/// The key as a character string, client first.
///
/// Both the enqueue `VARKEY` and the transport entry's `TABKEY` are the same
/// shape: key fields concatenated at running character offsets. Lengths come
/// from `DESCRIBE FIELD ... IN CHARACTER MODE`, so a key type this code has not
/// seen still lands at the right offset.
fn key_string(write: &ValidatedTableWrite, target: &str, client_dependent: bool) -> String {
    let mut fill = String::new();
    fill.push_str("        CLEAR lv_off.\n");
    if client_dependent {
        let _ = write!(
            fill,
            "        lv_len = 3.
        {target}+lv_off(lv_len) = sy-mandt.
        ADD lv_len TO lv_off.\n"
        );
    }
    for index in 0..write.keys.len() {
        let _ = write!(
            fill,
            "        DESCRIBE FIELD lv_k{index} LENGTH lv_len IN CHARACTER MODE.
        IF lv_off + lv_len > 120.
          lv_key_too_long = abap_true.
        ELSE.
          {target}+lv_off(lv_len) = lv_k{index}.
          ADD lv_len TO lv_off.
        ENDIF.\n"
        );
    }
    fill
}

/// Takes the same lock SE16N takes.
///
/// Not needed for the guarded `UPDATE`, which is already atomic, but an insert
/// and a delete have no value guard to lean on — SE16N's own delete is by key
/// alone — and SM30 and SE16N expect to find this lock held.
fn lock_block(write: &ValidatedTableWrite, client_dependent: bool) -> String {
    let fill = key_string(write, "lv_varkey", client_dependent);
    format!(
        "
{fill}
        CALL FUNCTION 'ENQUEUE_E_TABLEE'
          EXPORTING
            tabname      = '{table}'
            varkey       = lv_varkey
          EXCEPTIONS
            foreign_lock = 1
            system_failure = 2
            OTHERS       = 3.
        IF sy-subrc <> 0.
          lv_status = 'locked_by_another'.
          lv_detail = |{{ sy-msgv1 }}|.
        ENDIF.
",
        table = write.table.to_ascii_uppercase(),
    )
}

fn unlock_block(write: &ValidatedTableWrite) -> String {
    format!(
        "
    CALL FUNCTION 'DEQUEUE_E_TABLEE'
      EXPORTING
        tabname = '{table}'
        varkey  = lv_varkey.
",
        table = write.table.to_ascii_uppercase(),
    )
}

/// `INSERT`, which fails on a duplicate key rather than overwriting.
fn insert_statement(write: &ValidatedTableWrite) -> String {
    let table = write.table.to_ascii_lowercase();
    let mut fill = String::new();
    for (index, change) in write.changes.iter().enumerate() {
        let _ = writeln!(
            fill,
            "        ls_row-{} = lv_new{index}.",
            change.field.to_ascii_lowercase()
        );
    }
    format!(
        "
{fill}
        INSERT {table} FROM @ls_row.
        lv_subrc = sy-subrc.
        lv_rows  = sy-dbcnt.
        IF lv_subrc = 0.
          lv_status = 'applied'.
        ELSE.
          lv_status = 'row_exists'.
        ENDIF.
"
    )
}

/// `DELETE` by key, under the lock.
///
/// By key alone, as SE16N deletes: there is no value guard, because
/// reconstructing a whole row as typed literals is not something every column
/// type survives. The lock is the protection, and the journal holds the row.
fn delete_statement(write: &ValidatedTableWrite) -> String {
    format!(
        "
        DELETE FROM {table} WHERE {conditions}.
        lv_subrc = sy-subrc.
        lv_rows  = sy-dbcnt.
        IF lv_subrc = 0.
          lv_status = 'applied'.
        ELSE.
          lv_status = 'row_not_found'.
        ENDIF.
",
        table = write.table.to_ascii_lowercase(),
        conditions = key_conditions(write, "\n                 AND "),
    )
}

/// A dry run performs the real statement and then discards it; a run that only
/// printed what it would do would not prove the statement works.
const fn settle_clause(mode: WriteMode) -> &'static str {
    match mode {
        WriteMode::Execute => {
            "    IF lv_status = 'applied'.
      COMMIT WORK.
    ELSE.
      ROLLBACK WORK.
    ENDIF."
        }
        WriteMode::DryRun => {
            "    ROLLBACK WORK.
    IF lv_status = 'applied'.
      lv_status = 'would_apply'.
    ENDIF."
        }
    }
}

/// The one line the client reads. A run that dies prints nothing at all — a
/// short dump answers 500 with an empty body — so the envelope's absence is
/// the failure signal.
fn envelope_clause() -> String {
    format!(
        "

    REPLACE ALL OCCURRENCES OF '\"' IN lv_detail WITH ''''.
    REPLACE ALL OCCURRENCES OF '\\' IN lv_detail WITH '/'.
    out->write( |\\{{\"envelope\":\"{ENVELOPE}\",\"status\":\"{{ lv_status }}\",\"subrc\":{{ lv_subrc }},\"rows\":{{ lv_rows }},\"detail\":\"{{ lv_detail }}\"\\}}| ).
  ENDMETHOD.
ENDCLASS.
"
    )
}

/// Reads the envelope out of what the class printed.
///
/// # Errors
///
/// Returns [`TableWriteError::NoEnvelope`] when no line is the envelope, which
/// is how a run that never reached the write is detected.
pub fn parse_envelope(output: &str) -> Result<RunEnvelope, TableWriteError> {
    for line in output.lines() {
        let line = line.trim();
        if !line.starts_with('{') || !line.contains(ENVELOPE) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("envelope").and_then(serde_json::Value::as_str) != Some(ENVELOPE) {
            continue;
        }
        return Ok(RunEnvelope {
            status: value
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            subrc: value
                .get("subrc")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default(),
            rows: value
                .get("rows")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default(),
            detail: value
                .get("detail")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Err(TableWriteError::NoEnvelope {
        output: output.to_owned(),
    })
}

/// Everything one `table set` produced, whether or not it changed anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableWriteOutcome {
    pub table: String,
    pub keys: Vec<FieldValue>,
    pub changes: Vec<FieldChange>,
    pub before: BTreeMap<String, String>,
    pub delivery_class: String,
    pub mode: WriteMode,
    pub abap: String,
    pub envelope: RunEnvelope,
}

#[derive(Debug, Error)]
pub enum TableWriteRunError {
    #[error(transparent)]
    Rejected(#[from] TableWriteError),
    #[error("could not read {table}: {source}")]
    Read {
        table: String,
        #[source]
        source: TableError,
    },
    #[error("could not read {transport}: {source}")]
    Transport {
        transport: String,
        #[source]
        source: Box<TransportShowError>,
    },
    #[error(transparent)]
    Exec(#[from] ExecClassError),
    #[error("could not record the change in the journal: {0}")]
    Journal(#[from] JournalError),
}

impl ReportableError for TableWriteRunError {
    fn code(&self) -> &'static str {
        match self {
            Self::Rejected(error) => error.code(),
            Self::Read { .. } => "table_write_read_failed",
            Self::Transport { source, .. } => source.code(),
            Self::Exec(error) => error.code(),
            Self::Journal(error) => error.code(),
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::Rejected(error) => error.status(),
            Self::Read { source, .. } => source.status(),
            Self::Transport { source, .. } => source.status(),
            Self::Exec(error) => error.status(),
            Self::Journal(error) => error.status(),
        }
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Rejected(error) => error.hint(),
            Self::Read { source, .. } => source.hint(),
            Self::Transport { source, .. } => source.hint(),
            Self::Exec(error) => error.hint(),
            Self::Journal(error) => error.hint(),
        }
    }
}

/// Changes one row of one table.
///
/// Reads the table's fields, its delivery class and the row itself, refuses
/// anything outside what phase 1 covers, then generates ABAP and runs it
/// through this user's executor class.
///
/// # Errors
///
/// Returns [`TableWriteRunError`] when a read fails, the request is refused, or
/// the generated class does not activate or run.
pub async fn write_table_row(
    sap: &mut SapClient,
    policy: &EditPolicy,
    username: &str,
    request: &TableWriteRequest,
    mode: WriteMode,
    journal: Option<&Journal>,
) -> Result<TableWriteOutcome, TableWriteRunError> {
    let table = request.table().to_ascii_uppercase();

    if !policy
        .customer_namespaces
        .iter()
        .any(|pattern| glob_matches(pattern, &table))
    {
        return Err(TableWriteError::NotCustomerTable { table }.into());
    }

    let delivery_class = read_delivery_class(sap, &table).await?;
    let customizing = matches!(delivery_class.to_ascii_uppercase().as_str(), "C" | "G");

    let metadata = get_table_metadata(sap, &table, &TableMetadataOptions::default())
        .await
        .map_err(|source| TableWriteRunError::Read {
            table: table.clone(),
            source,
        })?;

    // A customizing change that records no `TABU` entry lives in one client and
    // never travels, and nothing says so afterwards. Refusing is the only option.
    let transport_entry = if customizing {
        let Some(transport) = request.transport() else {
            return Err(TableWriteError::TransportRequired {
                table,
                delivery_class,
            }
            .into());
        };
        Some(TransportEntry {
            task: resolve_task(sap, transport, username).await?,
            client_dependent: metadata.fields.iter().any(is_client_field),
        })
    } else {
        None
    };

    validate_shape(&metadata, request)?;
    let row = read_row(sap, &table, &key_fields(&metadata, request)).await?;
    let validated = resolve_changes(&metadata, request, &row)?;

    let class = executor_class_name(username);
    let abap = generate_abap(&class, &validated, mode, transport_entry.as_ref());

    // Recorded before the run, not after: an operation whose before-image was
    // never written has no recovery at all. A dry run changes nothing and so
    // records nothing.
    let entry = match (journal, mode) {
        (Some(journal), WriteMode::Execute) => Some(journal.begin(
            entry_object(&validated.table),
            JournalOperation::table_write(vec![RowWrite {
                operation: validated.operation,
                key: validated
                    .keys
                    .iter()
                    .map(|key| (key.field.clone(), key.value.clone()))
                    .collect(),
                changes: validated
                    .changes
                    .iter()
                    .map(|change| JournalFieldChange {
                        field: change.field.clone(),
                        before: change.before.clone(),
                        after: change.after.clone(),
                    })
                    .collect(),
            }]),
            request.transport().map(str::to_owned),
            Some(&row_json(&validated.before)),
            None,
        )?),
        _ => None,
    };

    let output = run_generated_source(sap, username, abap.clone()).await?;
    // No envelope on a 200 means the class did not execute at all — a dump
    // would have been a 500 with an empty body. The one cause seen is an
    // executor created moments ago that SAP has not registered as runnable, so
    // the run alone is repeated. Nothing is written twice.
    let envelope = if let Ok(envelope) = parse_envelope(&output) {
        envelope
    } else {
        let retried = rerun_executor(sap, username).await?;
        parse_envelope(&retried)?
    };

    // The after-image is read back rather than assembled from what was sent.
    // Constructing it would assert the write landed; reading it is what every
    // other Fractal mutation does, and it is what an undo is later gated on.
    if let (Some(journal), Some(entry)) = (journal, entry) {
        if row_changed(&envelope.status) {
            let after = read_row(sap, &validated.table, &validated.keys).await?;
            journal.succeeded(entry, Some(&row_json(&after)), None)?;
        } else {
            journal.failed(entry)?;
        }
    }

    Ok(TableWriteOutcome {
        table: validated.table,
        keys: validated.keys,
        changes: validated.changes,
        before: validated.before,
        delivery_class,
        mode,
        abap,
        envelope,
    })
}


/// The task a `TABU` entry goes in.
///
/// `TR_APPEND_TO_COMM_OBJS_KEYS` refuses a request with `TK 127`, "changes to
/// objects are only allowed in correction/repair", so a request has to be
/// resolved to the caller's own modifiable task within it. A task number given
/// directly is used as it stands.
async fn resolve_task(
    sap: &SapClient,
    transport: &str,
    username: &str,
) -> Result<String, TableWriteRunError> {
    let detail = show_transport_request(sap, transport)
        .await
        .map_err(|source| TableWriteRunError::Transport {
            transport: transport.to_owned(),
            source: Box::new(source),
        })?;

    if detail.tasks.is_empty() {
        // Already a task: a request always reports its own.
        return Ok(detail.number);
    }

    detail
        .tasks
        .iter()
        .find(|task| {
            task.owner
                .as_deref()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(username))
                && task
                    .status_text
                    .as_deref()
                    .is_some_and(|status| status.eq_ignore_ascii_case("Modifiable"))
        })
        .map(|task| task.number.clone())
        .ok_or_else(|| {
            TableWriteError::NoTaskForUser {
                transport: transport.to_owned(),
                user: username.to_owned(),
            }
            .into()
        })
}

/// Reads `DD02L-CONTFLAG`, the table's delivery class.
///
/// Phase 1 writes application tables only. A customizing table needs a `TABU`
/// entry in a customizing request, and a change made without one never leaves
/// the client it was made in.
async fn read_delivery_class(
    sap: &mut SapClient,
    table: &str,
) -> Result<String, TableWriteRunError> {
    let query = format!(
        "SELECT contflag FROM dd02l WHERE tabname = '{}' AND as4local = 'A'",
        sql_literal(table)
    );
    let result = run_query(sap, &query, &QueryOptions::default())
        .await
        .map_err(|source| TableWriteRunError::Read {
            table: table.to_owned(),
            source,
        })?;

    Ok(result
        .rows
        .first()
        .and_then(|row| row.first())
        .map(|value| value.trim().to_owned())
        .unwrap_or_default())
}

/// Reads the row the write addresses, as the before-image the caller is shown.
async fn read_row(
    sap: &mut SapClient,
    table: &str,
    keys: &[FieldValue],
) -> Result<BTreeMap<String, String>, TableWriteRunError> {
    let conditions = keys
        .iter()
        .map(|key| {
            format!(
                "{} = '{}'",
                key.field.to_ascii_lowercase(),
                sql_literal(&key.value)
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let query = format!("SELECT * FROM {} WHERE {conditions}", table.to_ascii_lowercase());

    let result = run_query(sap, &query, &QueryOptions::default())
        .await
        .map_err(|source| TableWriteRunError::Read {
            table: table.to_owned(),
            source,
        })?;

    let Some(row) = result.rows.first() else {
        return Ok(BTreeMap::new());
    };
    Ok(result
        .columns
        .iter()
        .zip(row)
        .map(|(column, value)| (column.name.to_ascii_uppercase(), value.clone()))
        .collect())
}


/// The repository object a row write is recorded against.
///
/// A table is a real ADT object with a real URI, so the entry needs no new kind
/// of target: what is new is the operation, and the row's key lives in it.
fn entry_object(table: &str) -> EntryObject {
    EntryObject {
        object_type: AdtObjectFamily::Source(EditableAdtObjectType::Table),
        name: table.to_owned(),
        uri: format!(
            "{}/{}",
            EditableAdtObjectType::Table.collection_path(),
            table.to_ascii_lowercase()
        ),
        source_part: None,
    }
}

/// Whether the run actually changed the row.
///
/// Not an equality check against `applied`: a customizing write reports
/// `applied_with_transport_entry`, or `applied_without_transport_entry` when
/// the row was committed and the `TABU` entry was not accepted. The row changed
/// in all three, and the journal has to hold it as a change or there is nothing
/// to undo.
#[must_use]
pub fn row_changed(status: &str) -> bool {
    status.starts_with("applied")
}

/// One row as canonical JSON: fields sorted, values as read.
///
/// This is what the journal stores as a row's before- and after-image. Sorted
/// so the same row always hashes to the same blob.
#[must_use]
pub fn row_json(row: &BTreeMap<String, String>) -> String {
    serde_json::to_string_pretty(row).unwrap_or_else(|_| "{}".to_owned())
}

/// Re-reads the row and says which of the recorded fields no longer match.
///
/// The generated class reports only that the guard failed. Naming the fields
/// that moved is done here, where the values are real strings rather than
/// something assembled inside an ABAP string template.
///
/// # Errors
///
/// Returns [`TableWriteRunError`] when the row cannot be read.
pub async fn diagnose_divergence(
    sap: &mut SapClient,
    table: &str,
    keys: &[FieldValue],
    expected: &[(String, String)],
) -> Result<Vec<FieldDivergence>, TableWriteRunError> {
    let row = read_row(sap, table, keys).await?;
    Ok(expected
        .iter()
        .map(|(field, want)| {
            let found = row.get(&field.to_ascii_uppercase()).cloned();
            FieldDivergence {
                diverged: found.as_ref() != Some(want),
                field: field.clone(),
                expected: want.clone(),
                found,
            }
        })
        .collect())
}

/// One guarded field, and whether it still holds what was expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDivergence {
    pub field: String,
    pub expected: String,
    /// `None` when the row itself is gone.
    pub found: Option<String>,
    pub diverged: bool,
}

/// Doubling the quote is the escape SQL and ABAP share; control characters are
/// refused before they reach either.
fn sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sap::table::TableFieldMetadata;

    fn field(name: &str, is_key: bool, col_type: &str) -> TableFieldMetadata {
        TableFieldMetadata {
            name: name.to_owned(),
            declared_type: "abap.char(10)".to_owned(),
            is_key,
            sap_type: Some("C".to_owned()),
            col_type: Some(col_type.to_owned()),
            length: Some(10),
            description: None,
        }
    }

    /// The same table with no recorded DDIC type on any field, leaving the
    /// declared type as the only thing that identifies the client column.
    fn metadata_without_col_types() -> TableMetadata {
        let mut metadata = metadata();
        for field in &mut metadata.fields {
            field.col_type = None;
        }
        metadata.fields[0].declared_type = "mandt".to_owned();
        metadata
    }

    fn metadata() -> TableMetadata {
        TableMetadata {
            entity: "ZSAMPLE_RECORD".to_owned(),
            total_rows: Some(1),
            fields: vec![
                field("MANDT", true, "CLNT"),
                field("ID", true, "CHAR"),
                field("STATUS", false, "CHAR"),
                field("NOTE", false, "CHAR"),
            ],
        }
    }

    fn row() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("MANDT".to_owned(), "100".to_owned()),
            ("ID".to_owned(), "R1".to_owned()),
            ("STATUS".to_owned(), "OPEN".to_owned()),
            ("NOTE".to_owned(), "first".to_owned()),
        ])
    }

    /// Both halves in sequence, which is what a caller does either side of
    /// reading the row. Production runs them separately: the key has to be
    /// checked before the read it addresses.
    fn validate(
        metadata: &TableMetadata,
        request: &TableWriteRequest,
        row: &BTreeMap<String, String>,
    ) -> Result<ValidatedTableWrite, TableWriteError> {
        validate_shape(metadata, request)?;
        resolve_changes(metadata, request, row)
    }

    fn request(keys: &[(&str, &str)], sets: &[(&str, &str)]) -> TableWriteRequest {
        let pair = |(f, v): &(&str, &str)| FieldValue {
            field: (*f).to_owned(),
            value: (*v).to_owned(),
        };
        TableWriteRequest::Update {
            table: "ZSAMPLE_RECORD".to_owned(),
            keys: keys.iter().map(pair).collect(),
            sets: sets.iter().map(pair).collect(),
            expected_before: None,
            transport: None,
        }
    }

    #[test]
    fn accepts_a_full_key_and_records_the_current_value() {
        let write = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("status", "DONE")]),
            &row(),
        )
        .unwrap();

        assert_eq!(write.changes[0].before, "OPEN");
        assert_eq!(write.changes[0].after, "DONE");
        assert_eq!(write.keys[0].field, "ID");
    }

    #[test]
    fn does_not_ask_the_caller_for_the_client_field() {
        assert!(
            validate(
                &metadata(),
                &request(&[("id", "R1")], &[("status", "DONE")]),
                &row()
            )
            .is_ok()
        );
    }

    #[test]
    fn finds_the_client_field_without_a_recorded_ddic_type() {
        // The client is never the caller's to give. With no `col_type`, the
        // declared type is the only thing that says which column it is.
        assert!(
            validate(
                &metadata_without_col_types(),
                &request(&[("id", "R1")], &[("status", "DONE")]),
                &row()
            )
            .is_ok()
        );
    }

    #[test]
    fn pads_a_short_numc_key_to_its_width() {
        let mut metadata = metadata();
        metadata.fields[1].sap_type = Some("N".to_owned());
        metadata.fields[1].length = Some(8);

        let write = validate(
            &metadata,
            &request(&[("id", "3")], &[("status", "DONE")]),
            &row(),
        )
        .unwrap();

        // 3 and 00000003 are the same value in a NUMC(8); the database does not
        // agree, and an unpadded key comes back as a row that is not there.
        assert_eq!(write.keys[0].value, "00000003");
    }

    #[test]
    fn leaves_alone_what_is_not_a_short_run_of_digits() {
        let mut metadata = metadata();
        metadata.fields[1].sap_type = Some("N".to_owned());
        metadata.fields[1].length = Some(8);

        for value in ["R1", "", "00000003", "0000000000000"] {
            let write = validate(
                &metadata,
                &request(&[("id", value)], &[("status", "DONE")]),
                &row(),
            );
            if let Ok(write) = write {
                assert_eq!(write.keys[0].value, value, "{value:?} was rewritten");
            }
        }
    }

    #[test]
    fn a_char_key_is_never_padded() {
        // Only NUMC. A CHAR(10) holding "R1" means "R1", not "00000000R1".
        let write = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("status", "DONE")]),
            &row(),
        )
        .unwrap();
        assert_eq!(write.keys[0].value, "R1");
    }

    #[test]
    fn a_missing_row_names_the_key_it_looked_for() {
        let error = validate(
            &metadata(),
            &request(&[("id", "NOPE")], &[("status", "DONE")]),
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "table_write_row_not_found");
        assert!(error.to_string().contains("id=NOPE"), "{error}");
    }

    #[test]
    fn refuses_a_partial_key() {
        let error = validate(&metadata(), &request(&[], &[("status", "DONE")]), &row()).unwrap_err();
        assert_eq!(error.code(), "table_write_key_incomplete");
    }

    #[test]
    fn refuses_changing_a_key_field() {
        let error = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("id", "R2")]),
            &row(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "table_write_key_not_settable");
    }

    #[test]
    fn checks_the_key_before_anything_reads_a_row() {
        let error = validate_shape(&metadata(), &request(&[], &[("status", "DONE")])).unwrap_err();
        assert_eq!(error.code(), "table_write_key_incomplete");
    }

    #[test]
    fn refuses_setting_a_key_field() {
        let error = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("mandt", "200")]),
            &row(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "table_write_key_not_settable");
    }

    #[test]
    fn refuses_a_field_the_table_does_not_have() {
        let error = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("nope", "X")]),
            &row(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "table_write_unknown_field");
    }

    #[test]
    fn refuses_a_value_that_cannot_become_a_literal() {
        let error = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("status", "a\nb")]),
            &row(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "table_write_value_unusable");
    }

    #[test]
    fn doubles_a_quote_in_a_value() {
        assert_eq!(abap_literal("it's"), "'it''s'");
    }

    #[test]
    fn guards_the_update_with_every_current_value() {
        let write = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("status", "DONE"), ("note", "second")]),
            &row(),
        )
        .unwrap();
        let abap = generate_abap("ZCL_X", &write, WriteMode::Execute, None);

        assert!(abap.contains("UPDATE zsample_record SET status = @lv_new0"), "{abap}");
        assert!(abap.contains("note = @lv_new1"), "{abap}");
        assert!(abap.contains("id = @lv_k0"), "{abap}");
        assert!(abap.contains("status = @lv_old0"), "{abap}");
        assert!(abap.contains("note = @lv_old1"), "{abap}");
        assert!(abap.contains("COMMIT WORK."));
        assert!(abap.contains("INTO @ls_row"), "strict-mode SQL needs an escaped host variable");
    }

    #[test]
    fn an_asserted_before_value_becomes_the_guard() {
        let request = TableWriteRequest::Update {
            table: "ZSAMPLE_RECORD".to_owned(),
            keys: vec![FieldValue {
                field: "id".to_owned(),
                value: "R1".to_owned(),
            }],
            sets: vec![FieldValue {
                field: "status".to_owned(),
                value: "OPEN".to_owned(),
            }],
            expected_before: Some(BTreeMap::from([(
                "STATUS".to_owned(),
                "DONE".to_owned(),
            )])),
            transport: None,
        };

        let write = validate(&metadata(), &request, &row()).unwrap();

        // The row holds OPEN, but the caller asserted DONE, so DONE is what the
        // statement guards on. This is what stops an undo reverting a change
        // somebody else made after the write it reverses.
        assert_eq!(write.changes[0].before, "DONE");
        let abap = generate_abap("ZCL_X", &write, WriteMode::Execute, None);
        assert!(abap.contains("lv_old0 = 'DONE'"), "{abap}");
    }

    #[test]
    fn a_customizing_write_records_a_tabu_entry_before_it_touches_the_row() {
        let write = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("status", "DONE")]),
            &row(),
        )
        .unwrap();
        let entry = TransportEntry {
            task: "DE3K900671".to_owned(),
            client_dependent: true,
        };
        let abap = generate_abap("ZCL_X", &write, WriteMode::Execute, Some(&entry));

        // The simulated append comes first: a task that will not take the entry
        // must refuse the whole operation rather than leave the row changed and
        // unrecorded.
        let simulated = abap.find("wi_simulation         = 'X'").unwrap();
        let update = abap.find("UPDATE zsample_record").unwrap();
        let real = abap.find("wi_simulation         = ' '").unwrap();
        assert!(simulated < update, "the check must precede the write");
        assert!(update < real, "the entry is recorded after the commit");

        assert!(abap.contains("ls_e071-obj_name = 'ZSAMPLE_RECORD'"));
        assert!(abap.contains("wi_trkorr             = 'DE3K900671'"));
        // The client leads the key of a client-dependent table.
        assert!(abap.contains("lv_tabkey+lv_off(lv_len) = sy-mandt"), "{abap}");
        assert!(abap.contains("IF lv_off + lv_len > 120."), "the 120-char cap");
    }

    #[test]
    fn a_dry_run_never_records_the_entry_for_real() {
        let write = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("status", "DONE")]),
            &row(),
        )
        .unwrap();
        let entry = TransportEntry {
            task: "DE3K900671".to_owned(),
            client_dependent: false,
        };
        let abap = generate_abap("ZCL_X", &write, WriteMode::DryRun, Some(&entry));

        assert!(abap.contains("wi_simulation         = 'X'"));
        assert!(!abap.contains("wi_simulation         = ' '"), "{abap}");
        assert!(!abap.contains("lv_tabkey+lv_off(lv_len) = sy-mandt"));
    }

    #[test]
    fn a_dry_run_rolls_back_and_never_commits() {
        let write = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("status", "DONE")]),
            &row(),
        )
        .unwrap();
        let abap = generate_abap("ZCL_X", &write, WriteMode::DryRun, None);

        assert!(abap.contains("ROLLBACK WORK."));
        assert!(!abap.contains("COMMIT WORK."), "{abap}");
        assert!(abap.contains("would_apply"));
    }

    #[test]
    fn writes_the_generated_abap_for_inspection() {
        if std::env::var("FRACTAL_DUMP_ABAP").is_err() {
            return;
        }
        let write = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("status", "DONE")]),
            &row(),
        )
        .unwrap();
        std::fs::write(
            std::env::var("FRACTAL_DUMP_ABAP").unwrap(),
            generate_abap("ZCL_FRACTAL_EXEC_TRUSSELL", &write, WriteMode::Execute, None),
        )
        .unwrap();
    }

    #[test]
    fn every_applied_status_counts_as_a_change() {
        // Phase 2 added two more. An equality check against "applied" recorded
        // a successful customizing write as failed, which left it with no undo.
        assert!(row_changed("applied"));
        assert!(row_changed("applied_with_transport_entry"));
        assert!(row_changed("applied_without_transport_entry"));
        assert!(!row_changed("would_apply"));
        assert!(!row_changed("conflict"));
        assert!(!row_changed("row_not_found"));
        assert!(!row_changed("transport_refused"));
    }

    fn insert_request(fields: &[(&str, &str)]) -> TableWriteRequest {
        TableWriteRequest::Insert {
            table: "ZSAMPLE_RECORD".to_owned(),
            fields: fields
                .iter()
                .map(|(field, value)| FieldValue {
                    field: (*field).to_owned(),
                    value: (*value).to_owned(),
                })
                .collect(),
            transport: None,
        }
    }

    fn delete_request(keys: &[(&str, &str)]) -> TableWriteRequest {
        TableWriteRequest::Delete {
            table: "ZSAMPLE_RECORD".to_owned(),
            keys: keys
                .iter()
                .map(|(field, value)| FieldValue {
                    field: (*field).to_owned(),
                    value: (*value).to_owned(),
                })
                .collect(),
            transport: None,
        }
    }

    #[test]
    fn an_insert_takes_its_key_from_the_fields_it_sets() {
        let write = validate(
            &metadata(),
            &insert_request(&[("id", "R9"), ("status", "OPEN")]),
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(write.keys.len(), 1);
        assert_eq!(write.keys[0].field, "ID");
        let abap = generate_abap("ZCL_X", &write, WriteMode::Execute, None);
        assert!(abap.contains("INSERT zsample_record FROM @ls_row"), "{abap}");
        assert!(abap.contains("lv_status = 'row_exists'"));
    }

    #[test]
    fn the_client_never_becomes_part_of_the_key() {
        // Restoring a deleted row replays every recorded field, MANDT with
        // them. SQL refuses the client field in a WHERE.
        let write = validate(
            &metadata(),
            &insert_request(&[("mandt", "100"), ("id", "R9"), ("status", "OPEN")]),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(
            write.keys.iter().all(|key| key.field != "MANDT"),
            "{:?}",
            write.keys
        );
    }

    #[test]
    fn an_insert_refuses_a_key_that_is_already_there() {
        let error = validate(
            &metadata(),
            &insert_request(&[("id", "R1"), ("status", "OPEN")]),
            &row(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "table_write_row_exists");
    }

    #[test]
    fn a_delete_is_by_key_under_the_lock() {
        let write = validate(&metadata(), &delete_request(&[("id", "R1")]), &row()).unwrap();
        let abap = generate_abap("ZCL_X", &write, WriteMode::Execute, None);

        assert!(abap.contains("DELETE FROM zsample_record WHERE id = @lv_k0"), "{abap}");
        // SE16N deletes by key too, and the lock is what protects it.
        let lock = abap.find("ENQUEUE_E_TABLEE").unwrap();
        let delete = abap.find("DELETE FROM").unwrap();
        let unlock = abap.find("DEQUEUE_E_TABLEE").unwrap();
        assert!(lock < delete && delete < unlock, "{abap}");
    }

    #[test]
    fn every_operation_takes_the_lock() {
        for operation in [RowOperation::Update, RowOperation::Insert, RowOperation::Delete] {
            let request = match operation {
                RowOperation::Insert => insert_request(&[("id", "R9"), ("status", "OPEN")]),
                RowOperation::Delete => delete_request(&[("id", "R1")]),
                RowOperation::Update => request(&[("id", "R1")], &[("status", "DONE")]),
            };
            let existing = if operation == RowOperation::Insert {
                BTreeMap::new()
            } else {
                row()
            };
            let write = validate(&metadata(), &request, &existing).unwrap();
            let abap = generate_abap("ZCL_X", &write, WriteMode::Execute, None);
            assert!(abap.contains("ENQUEUE_E_TABLEE"), "{operation:?}");
            assert!(abap.contains("DEQUEUE_E_TABLEE"), "{operation:?}");
        }
    }

    #[test]
    fn reads_the_envelope_out_of_the_output() {
        let envelope = parse_envelope(
            "noise\n{\"envelope\":\"fractal.table.v1\",\"status\":\"applied\",\"subrc\":0,\"rows\":1,\"detail\":\"\"}\n",
        )
        .unwrap();
        assert_eq!(envelope.status, "applied");
        assert_eq!(envelope.rows, 1);
    }

    #[test]
    fn treats_a_missing_envelope_as_failure() {
        let error =
            parse_envelope("Error: Class does not implement if_oo_adt_classrun~main method!")
                .unwrap_err();
        assert_eq!(error.code(), "table_write_no_envelope");
    }
}
