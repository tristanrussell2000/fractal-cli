use std::collections::BTreeMap;

use serde::Serialize;

use crate::{
    cli::TableSetArgs,
    commands::connect,
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::journal::entry::RowOperation;
use fractal::journal::recorder::Journal;
use fractal::reportable_error::ReportableError;
use fractal::sap::table_write::{FieldValue, TableWriteRequest, WriteMode, write_table_row};

/// A `--key` or `--set` argument that is not `FIELD=VALUE`.
///
/// Reading what the caller typed is the command layer's job; by the time a
/// request reaches `sap`, it is already a list of fields and values.
#[derive(Debug, thiserror::Error)]
#[error("{text} is not FIELD=VALUE")]
pub struct MalformedAssignment {
    text: String,
}

impl ReportableError for MalformedAssignment {
    fn code(&self) -> &'static str {
        "table_write_malformed_assignment"
    }

    fn hint(&self) -> Option<String> {
        Some("Write each field as --key FIELD=VALUE or --set FIELD=VALUE.".to_owned())
    }
}

#[derive(Debug, Serialize)]
pub struct TableSetOutput {
    ok: bool,
    profile: String,
    operation: &'static str,
    table: String,
    delivery_class: String,
    dry_run: bool,
    /// What the run reported: `applied`, `would_apply`, `conflict`,
    /// `row_not_found` or `exception`.
    status: String,
    rows_affected: i64,
    detail: String,
    keys: BTreeMap<String, String>,
    before: BTreeMap<String, String>,
    changes: Vec<FieldChangeOutput>,
    abap: String,
    next_step: Option<String>,
}

#[derive(Debug, Serialize)]
struct FieldChangeOutput {
    field: String,
    before: String,
    after: String,
}

pub async fn table_set(
    explicit_profile: Option<&str>,
    args: &TableSetArgs,
    operation: RowOperation,
) -> Result<TableSetOutput, Reported> {
    let keys = parse_all(&args.keys)?;
    let sets = parse_all(&args.sets)?;
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;

    let mode = if args.execute {
        WriteMode::Execute
    } else {
        WriteMode::DryRun
    };
    let request = build_request(
        operation,
        args.name.clone(),
        keys,
        sets,
        args.transport.clone(),
    )?;
    // Opened even for a dry run so a journal that cannot be written fails the
    // command rather than silently leaving a change unrecorded.
    let journal = Journal::open(&profile_name, &profile)?;
    let outcome = write_table_row(
        &mut client,
        &profile.edit_policy(),
        &profile.username,
        &request,
        mode,
        Some(&journal),
    )
    .await?;

    let next_step = (!args.execute && outcome.envelope.status == "would_apply").then(|| {
        format!(
            "fractal table {} {} {} --execute",
            match operation {
                RowOperation::Update => "set",
                RowOperation::Insert => "insert",
                RowOperation::Delete => "remove",
            },
            outcome.table,
            args.keys
                .iter()
                .map(|key| format!("--key {key}"))
                .chain(args.sets.iter().map(|set| format!("--set {set}")))
                .collect::<Vec<_>>()
                .join(" ")
        )
    });

    Ok(TableSetOutput {
        ok: true,
        profile: profile_name,
        operation: match operation {
            RowOperation::Update => "update",
            RowOperation::Insert => "insert",
            RowOperation::Delete => "delete",
        },
        table: outcome.table,
        delivery_class: outcome.delivery_class,
        dry_run: !args.execute,
        status: outcome.envelope.status,
        rows_affected: outcome.envelope.rows,
        detail: outcome.envelope.detail,
        keys: outcome
            .keys
            .into_iter()
            .map(|key| (key.field, key.value))
            .collect(),
        before: outcome.before,
        changes: outcome
            .changes
            .into_iter()
            .map(|change| FieldChangeOutput {
                field: change.field,
                before: change.before,
                after: change.after,
            })
            .collect(),
        abap: outcome.abap,
        next_step,
    })
}

/// Splits `FIELD=VALUE`, keeping any `=` in the value.
///
/// # Errors
///
/// Returns [`MalformedAssignment`] when there is no `=`, or nothing before it.
fn parse_assignment(text: &str) -> Result<FieldValue, MalformedAssignment> {
    let Some((field, value)) = text.split_once('=') else {
        return Err(MalformedAssignment {
            text: text.to_owned(),
        });
    };
    if field.trim().is_empty() {
        return Err(MalformedAssignment {
            text: text.to_owned(),
        });
    }
    Ok(FieldValue {
        field: field.trim().to_owned(),
        value: value.to_owned(),
    })
}

/// Maps what the caller typed onto the operation they asked for.
///
/// The three verbs share one set of arguments, so this is where a flag that
/// does not belong to the verb is caught: an insert names its key among the
/// fields it sets, and a delete sets nothing.
fn build_request(
    operation: RowOperation,
    table: String,
    keys: Vec<FieldValue>,
    sets: Vec<FieldValue>,
    transport: Option<String>,
) -> Result<TableWriteRequest, ArgumentNotAllowed> {
    match operation {
        RowOperation::Update => Ok(TableWriteRequest::Update {
            table,
            keys,
            sets,
            expected_before: None,
            transport,
        }),
        RowOperation::Insert => {
            if !keys.is_empty() {
                return Err(ArgumentNotAllowed {
                    verb: "insert",
                    flag: "--key",
                    instead: "give every field, key fields included, with --set",
                });
            }
            Ok(TableWriteRequest::Insert {
                table,
                fields: sets,
                transport,
            })
        }
        RowOperation::Delete => {
            if !sets.is_empty() {
                return Err(ArgumentNotAllowed {
                    verb: "remove",
                    flag: "--set",
                    instead: "a delete changes no fields, so give only --key",
                });
            }
            Ok(TableWriteRequest::Delete {
                table,
                keys,
                transport,
            })
        }
    }
}

/// A flag that belongs to a different verb.
#[derive(Debug, thiserror::Error)]
#[error("{flag} does not apply to `table {verb}`")]
pub struct ArgumentNotAllowed {
    verb: &'static str,
    flag: &'static str,
    instead: &'static str,
}

impl ReportableError for ArgumentNotAllowed {
    fn code(&self) -> &'static str {
        "table_write_argument_not_allowed"
    }

    fn hint(&self) -> Option<String> {
        let Self { instead, .. } = self;
        Some(format!("Instead, {instead}."))
    }
}

fn parse_all(values: &[String]) -> Result<Vec<FieldValue>, Reported> {
    values
        .iter()
        .map(|value| parse_assignment(value).map_err(Into::into))
        .collect()
}

pub fn print_table_set(result: &TableSetOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    let heading = if result.dry_run {
        format!("{} (dry run, nothing was committed)", result.table)
    } else {
        result.table.clone()
    };
    println!("{heading}");
    println!("status: {}  rows: {}", result.status, result.rows_affected);
    if !result.detail.is_empty() {
        println!("detail: {}", result.detail);
    }
    for change in &result.changes {
        println!("  {}: {} -> {}", change.field, change.before, change.after);
    }
    if let Some(next) = &result.next_step {
        println!("\nnext: {next}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_insert_will_not_take_a_key_flag() {
        // The key of an inserted row arrives with the other fields, so --key
        // would be a second, silent source of truth for it.
        let error = build_request(
            RowOperation::Insert,
            "ZSAMPLE".to_owned(),
            vec![FieldValue {
                field: "id".to_owned(),
                value: "R1".to_owned(),
            }],
            Vec::new(),
            None,
        )
        .unwrap_err();
        assert_eq!(error.code(), "table_write_argument_not_allowed");
    }

    #[test]
    fn a_delete_will_not_take_a_set_flag() {
        let error = build_request(
            RowOperation::Delete,
            "ZSAMPLE".to_owned(),
            Vec::new(),
            vec![FieldValue {
                field: "status".to_owned(),
                value: "X".to_owned(),
            }],
            None,
        )
        .unwrap_err();
        assert_eq!(error.code(), "table_write_argument_not_allowed");
    }

    #[test]
    fn splits_a_field_assignment_keeping_equals_in_the_value() {
        let parsed = parse_assignment("note=a=b").unwrap();
        assert_eq!(parsed.field, "note");
        assert_eq!(parsed.value, "a=b");
    }

    #[test]
    fn refuses_an_assignment_without_a_field() {
        assert_eq!(
            parse_assignment("=x").unwrap_err().code(),
            "table_write_malformed_assignment"
        );
        assert_eq!(
            parse_assignment("note").unwrap_err().code(),
            "table_write_malformed_assignment"
        );
    }
}
