use std::collections::BTreeMap;
use std::fmt::Write as _;

use thiserror::Error;

use super::{
    client::SapClient,
    exec_class::{ExecClassError, executor_class_name, run_generated_source},
    table::{
        QueryOptions, TableError, TableMetadata, TableMetadataOptions, get_table_metadata,
        run_query,
    },
};
use crate::config::EditPolicy;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableWriteRequest {
    pub table: String,
    pub keys: Vec<FieldValue>,
    pub sets: Vec<FieldValue>,
}

/// A request checked against the table's real fields, with the row it will change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedTableWrite {
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
    #[error("{table} has delivery class {delivery_class}, not A")]
    DeliveryClassRefused {
        table: String,
        delivery_class: String,
    },
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
    #[error("no row of {table} has that key")]
    RowNotFound { table: String },
    #[error("the run produced no {ENVELOPE} envelope")]
    NoEnvelope { output: String },
}

impl ReportableError for TableWriteError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotCustomerTable { .. } => "table_write_not_customer_table",
            Self::DeliveryClassRefused { .. } => "table_write_delivery_class_refused",
            Self::UnknownField { .. } => "table_write_unknown_field",
            Self::KeyFieldNotSettable { .. } => "table_write_key_not_settable",
            Self::IncompleteKey { .. } => "table_write_key_incomplete",
            Self::DuplicateField { .. } => "table_write_duplicate_field",
            Self::NothingToChange => "table_write_nothing_to_change",
            Self::UnusableValue { .. } => "table_write_value_unusable",
            Self::RowNotFound { .. } => "table_write_row_not_found",
            Self::NoEnvelope { .. } => "table_write_no_envelope",
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::NotCustomerTable { .. } => {
                "Only tables in a configured customer namespace can be written.".to_owned()
            }
            Self::DeliveryClassRefused { .. } => {
                "Only application tables (delivery class A) are writable. A customizing table needs \
                 a transport entry, which is not built yet."
                    .to_owned()
            }
            Self::UnknownField { table, .. } | Self::RowNotFound { table, .. } => {
                format!("`fractal table metadata {table}` lists the fields and keys.")
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

    if request.sets.is_empty() {
        return Err(TableWriteError::NothingToChange);
    }

    for set in &request.sets {
        if is_key(metadata, &set.field) {
            return Err(TableWriteError::KeyFieldNotSettable {
                table,
                field: set.field.to_ascii_uppercase(),
            });
        }
    }

    let mut seen = Vec::new();
    for given in request.keys.iter().chain(&request.sets) {
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
    let missing: Vec<_> = metadata
        .fields
        .iter()
        .filter(|f| f.is_key && !is_client_field(f.col_type.as_deref()))
        .filter(|f| {
            !request
                .keys
                .iter()
                .any(|k| k.field.eq_ignore_ascii_case(&f.name))
        })
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
    if row.is_empty() {
        return Err(TableWriteError::RowNotFound { table });
    }

    let changes = request
        .sets
        .iter()
        .map(|set| FieldChange {
            field: set.field.to_ascii_uppercase(),
            before: row
                .get(&set.field.to_ascii_uppercase())
                .cloned()
                .unwrap_or_default(),
            after: set.value.clone(),
        })
        .collect();

    Ok(ValidatedTableWrite {
        table,
        keys: request
            .keys
            .iter()
            .map(|k| FieldValue {
                field: k.field.to_ascii_uppercase(),
                value: k.value.clone(),
            })
            .collect(),
        changes,
        before: row.clone(),
    })
}

fn is_key(metadata: &TableMetadata, field: &str) -> bool {
    metadata
        .fields
        .iter()
        .any(|f| f.is_key && f.name.eq_ignore_ascii_case(field))
}

fn is_client_field(col_type: Option<&str>) -> bool {
    col_type.is_some_and(|t| t.eq_ignore_ascii_case("CLNT"))
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
pub fn generate_abap(class_name: &str, write: &ValidatedTableWrite, mode: WriteMode) -> String {
    let mut abap = declarations(class_name, write);
    abap.push_str(&literals(write));
    abap.push_str(&statement(write));
    abap.push('\n');
    abap.push_str(settle_clause(mode));
    abap.push_str(&envelope_clause());
    abap
}

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
      CATCH cx_root INTO DATA(lx_error).
        lv_status = 'exception'.
        lv_detail = lx_error->get_text( ).
    ENDTRY.
",
        key_only = key_conditions(write, "\n              AND "),
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
    #[error(transparent)]
    Exec(#[from] ExecClassError),
}

impl ReportableError for TableWriteRunError {
    fn code(&self) -> &'static str {
        match self {
            Self::Rejected(error) => error.code(),
            Self::Read { .. } => "table_write_read_failed",
            Self::Exec(error) => error.code(),
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::Rejected(error) => error.status(),
            Self::Read { source, .. } => source.status(),
            Self::Exec(error) => error.status(),
        }
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Rejected(error) => error.hint(),
            Self::Read { source, .. } => source.hint(),
            Self::Exec(error) => error.hint(),
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
) -> Result<TableWriteOutcome, TableWriteRunError> {
    let table = request.table.to_ascii_uppercase();

    if !policy
        .customer_namespaces
        .iter()
        .any(|pattern| glob_matches(pattern, &table))
    {
        return Err(TableWriteError::NotCustomerTable { table }.into());
    }

    let delivery_class = read_delivery_class(sap, &table).await?;
    if !delivery_class.eq_ignore_ascii_case("A") {
        return Err(TableWriteError::DeliveryClassRefused {
            table,
            delivery_class,
        }
        .into());
    }

    let metadata = get_table_metadata(sap, &table, &TableMetadataOptions::default())
        .await
        .map_err(|source| TableWriteRunError::Read {
            table: table.clone(),
            source,
        })?;

    validate_shape(&metadata, request)?;
    let row = read_row(sap, &table, &request.keys).await?;
    let validated = resolve_changes(&metadata, request, &row)?;

    let class = executor_class_name(username);
    let abap = generate_abap(&class, &validated, mode);
    let output = run_generated_source(sap, username, abap.clone()).await?;
    let envelope = parse_envelope(&output)?;

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
        TableWriteRequest {
            table: "ZSAMPLE_RECORD".to_owned(),
            keys: keys.iter().map(pair).collect(),
            sets: sets.iter().map(pair).collect(),
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
        let abap = generate_abap("ZCL_X", &write, WriteMode::Execute);

        assert!(abap.contains("UPDATE zsample_record SET status = @lv_new0"), "{abap}");
        assert!(abap.contains("note = @lv_new1"), "{abap}");
        assert!(abap.contains("id = @lv_k0"), "{abap}");
        assert!(abap.contains("status = @lv_old0"), "{abap}");
        assert!(abap.contains("note = @lv_old1"), "{abap}");
        assert!(abap.contains("COMMIT WORK."));
        assert!(abap.contains("INTO @ls_row"), "strict-mode SQL needs an escaped host variable");
    }

    #[test]
    fn a_dry_run_rolls_back_and_never_commits() {
        let write = validate(
            &metadata(),
            &request(&[("id", "R1")], &[("status", "DONE")]),
            &row(),
        )
        .unwrap();
        let abap = generate_abap("ZCL_X", &write, WriteMode::DryRun);

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
            generate_abap("ZCL_FRACTAL_EXEC_TRUSSELL", &write, WriteMode::Execute),
        )
        .unwrap();
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
