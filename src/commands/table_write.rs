use std::collections::BTreeMap;

use serde::Serialize;

use crate::{
    cli::TableSetArgs,
    commands::connect,
    output::{OutputFormat, print_json},
    reported::Reported,
};
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
) -> Result<TableSetOutput, Reported> {
    let keys = parse_all(&args.keys)?;
    let sets = parse_all(&args.sets)?;
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;

    let mode = if args.execute {
        WriteMode::Execute
    } else {
        WriteMode::DryRun
    };
    let request = TableWriteRequest {
        table: args.name.clone(),
        keys,
        sets,
        expected_before: None,
    };
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
            "fractal table set {} {} --execute",
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
