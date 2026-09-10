//! `fractal undo`: restoring the active version an activation replaced.
//!
//! This is the assessment half. It picks the entry, checks whether it can be
//! acted on, reads what the object holds now, and reports the verdict. The
//! three write steps are not built yet, so a run without `--dry-run` is
//! refused rather than half-performed.

use std::fmt::Write as _;

use serde::Serialize;

use super::connect;
use crate::{
    cli::UndoArgs,
    output::{OutputFormat, print_result},
    reported::Reported,
};
use fractal::journal::blobs::BlobStore;
use fractal::journal::entry::{ActivationUndoStep, JournalEntry};
use fractal::journal::paths;
use fractal::journal::recorder::Journal;
use fractal::journal::store::EntryStore;
use fractal::reportable_error::ReportableError;
use fractal::sap::undo::{UndoOutcome, UndoPlan, plan_activation_undo, undo_activation};

#[derive(Debug, Serialize)]
pub struct UndoOutput {
    ok: bool,
    profile: String,
    dry_run: bool,
    entry_id: String,
    object_type: String,
    name: String,
    object_uri: String,
    /// What step 1 would write back as the inactive version.
    restore_sha256: String,
    restore_bytes: usize,
    /// What step 3 would put back afterwards, without activating it. Absent
    /// when the caller had no pending work at the time.
    #[serde(skip_serializing_if = "Option::is_none")]
    restore_inactive_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_active_sha256: Option<String>,
    /// What the object holds now. Absent when it has no active version.
    #[serde(skip_serializing_if = "Option::is_none")]
    current_active_sha256: Option<String>,
    matches_recorded_state: bool,
    forced: bool,
    /// Which refusals `--force` proceeded past, so an override is never silent.
    overridden: Vec<&'static str>,
    /// Pending work that undoing would replace with something else. Not merely
    /// "there is pending work": a redo's pending work is what it would restore.
    pending_work_at_risk: bool,
    /// Which of the three steps this run performed. Fewer than three when an
    /// earlier run got part way, or when there was no pending work to restore.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    steps_run: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resumed_from: Option<&'static str>,
    /// A write landed and its lock could not be released. Somebody has to clear
    /// it: it blocks the next edit of this object.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    still_locked: bool,
    /// What a real run would do, in order.
    steps: Vec<String>,
    /// Where the content lives, for `diff` and an editor.
    content: Vec<UndoContentPath>,
}

#[derive(Debug, Serialize)]
struct UndoContentPath {
    field: &'static str,
    sha256: String,
    path: String,
}

/// Undoes one activation, or reports what undoing it would do.
///
/// # Errors
///
/// Returns [`Reported`] when no entry matches, when the entry cannot be undone,
/// when the object has moved since it was recorded, or when a step fails.
pub async fn object_undo(
    explicit_profile: Option<&str>,
    args: &UndoArgs,
) -> Result<UndoOutput, Reported> {
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    let journal = Journal::open(&profile_name, &profile)?;
    let blobs = BlobStore::new(paths::blob_root()?);
    let entry = select_entry(journal.entries(), args)?;
    let policy = profile.edit_policy();

    let plan = plan_activation_undo(&mut client, &policy, entry, &blobs, args.force).await?;
    if args.dry_run {
        return Ok(report(profile_name, args, &plan, None, &blobs));
    }

    let outcome = undo_activation(
        &mut client,
        &policy,
        &plan,
        args.transport.as_deref(),
        &journal,
    )
    .await?;
    Ok(report(profile_name, args, &plan, Some(&outcome), &blobs))
}

/// The entry to act on: the one named, or the object's most recent.
fn select_entry(entries: &EntryStore, args: &UndoArgs) -> Result<JournalEntry, Reported> {
    if let Some(id) = &args.entry {
        return Ok(entries.find(id)?);
    }
    let (Some(object_type), Some(name)) = (&args.object_type, &args.name) else {
        return Err(NothingSelected.into());
    };

    // Matched the way `journal list --type --name` matches, rather than by
    // resolving a URI: the entry already holds the canonical name, and the two
    // must agree about what "this object" means.
    let mut matching = entries.list()?.into_iter().filter(|entry| {
        entry
            .object
            .object_type
            .as_str()
            .eq_ignore_ascii_case(object_type)
            && entry.object.name.eq_ignore_ascii_case(name)
    });
    // The list is oldest first, so the last match is the most recent.
    matching.next_back().ok_or_else(|| {
        NoEntryForObject {
            object_type: object_type.clone(),
            name: name.clone(),
        }
        .into()
    })
}

#[derive(Debug)]
struct NothingSelected;

impl std::fmt::Display for NothingSelected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "no entry or object was named")
    }
}

impl std::error::Error for NothingSelected {}

impl ReportableError for NothingSelected {
    fn code(&self) -> &'static str {
        "undo_no_target"
    }

    fn hint(&self) -> Option<String> {
        Some(
            "Name what to undo: --entry <id>, or --type and --name for that object's most recent recorded activation."
                .to_owned(),
        )
    }

    fn suggested_command(&self) -> Option<String> {
        Some("fractal journal list".to_owned())
    }
}

#[derive(Debug)]
struct NoEntryForObject {
    object_type: String,
    name: String,
}

impl std::fmt::Display for NoEntryForObject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "nothing recorded for {} {}",
            self.object_type, self.name
        )
    }
}

impl std::error::Error for NoEntryForObject {}

impl ReportableError for NoEntryForObject {
    fn code(&self) -> &'static str {
        "undo_no_entry_for_object"
    }

    fn hint(&self) -> Option<String> {
        Some(
            "The journal holds nothing for this object on the selected profile's system. Only operations Fractal performed are recorded."
                .to_owned(),
        )
    }

    fn suggested_command(&self) -> Option<String> {
        Some(format!(
            "fractal journal list --type {} --name {}",
            self.object_type, self.name
        ))
    }
}

fn report(
    profile: String,
    args: &UndoArgs,
    plan: &UndoPlan,
    outcome: Option<&UndoOutcome>,
    blobs: &BlobStore,
) -> UndoOutput {
    UndoOutput {
        ok: true,
        profile,
        dry_run: args.dry_run,
        entry_id: plan.entry.id.clone(),
        object_type: plan.entry.object.object_type.as_str().to_owned(),
        name: plan.entry.object.name.clone(),
        object_uri: plan.object_uri.clone(),
        restore_sha256: plan.restore_sha256.clone(),
        restore_bytes: plan.restore.len(),
        restore_inactive_sha256: plan.restore_inactive_sha256.clone(),
        expected_active_sha256: plan.expected_active_sha256.clone(),
        current_active_sha256: plan.current_active_sha256.clone(),
        matches_recorded_state: plan.matches_recorded_state(),
        forced: args.force,
        overridden: plan
            .overridden
            .iter()
            .map(|override_| override_.as_str())
            .collect(),
        pending_work_at_risk: plan.pending_work_at_risk,
        steps_run: outcome
            .map(|outcome| outcome.steps_run.iter().map(step_name).collect())
            .unwrap_or_default(),
        resumed_from: outcome.and_then(|outcome| outcome.resumed_from.as_ref().map(step_name)),
        still_locked: outcome.is_some_and(|outcome| outcome.still_locked),
        steps: steps(plan),
        content: content_paths(plan, blobs),
    }
}

/// The three steps, spelled out. Step 3 is the one worth naming explicitly:
/// without it an undo silently discards whatever pending work the activation
/// consumed.
fn steps(plan: &UndoPlan) -> Vec<String> {
    let write = match plan.entry.content_kind() {
        fractal::journal::entry::ContentKind::Source => "source",
        fractal::journal::entry::ContentKind::Xml => "document",
    };
    vec![
        format!(
            "write the previous active {write} ({}) as the inactive version",
            &plan.restore_sha256[..12]
        ),
        "activate it".to_owned(),
        plan.restore_inactive_sha256.as_ref().map_or_else(
            || "leave the inactive layer empty, as it was before the activation".to_owned(),
            |sha256| {
                format!(
                    "restore the pending {write} ({}) without activating it",
                    &sha256[..12]
                )
            },
        ),
    ]
}

/// The JSON spelling of a step, which is what a caller reading the output sees.
const fn step_name(step: &ActivationUndoStep) -> &'static str {
    match step {
        ActivationUndoStep::WroteInactive => "wrote_inactive",
        ActivationUndoStep::Activated => "activated",
        ActivationUndoStep::RestoredInactive => "restored_inactive",
    }
}

fn content_paths(plan: &UndoPlan, blobs: &BlobStore) -> Vec<UndoContentPath> {
    [
        ("restore", Some(&plan.restore_sha256)),
        ("restore_inactive", plan.restore_inactive_sha256.as_ref()),
    ]
    .into_iter()
    .filter_map(|(field, sha256)| {
        let sha256 = sha256?;
        Some(UndoContentPath {
            field,
            sha256: sha256.clone(),
            path: blobs.path_of(sha256).display().to_string(),
        })
    })
    .collect()
}

pub fn print_object_undo(result: &UndoOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    let mut rendered = String::new();
    let _ = writeln!(
        rendered,
        "{} {} on {} {}",
        if result.dry_run {
            "would undo"
        } else {
            "undid"
        },
        result.entry_id,
        result.object_type,
        result.name
    );
    if !result.overridden.is_empty() {
        let _ = writeln!(rendered, "forced past: {}", result.overridden.join(", "));
    }
    if let Some(resumed) = result.resumed_from {
        let _ = writeln!(rendered, "resumed after: {resumed}");
    }
    for (index, step) in result.steps.iter().enumerate() {
        let _ = writeln!(rendered, "  {}. {step}", index + 1);
    }
    if result.still_locked {
        let _ = writeln!(
            rendered,
            "the object is still locked: clear it before the next edit"
        );
    }
    for content in &result.content {
        let _ = writeln!(rendered, "{}: {}", content.field, content.path);
    }
    print!("{rendered}");
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{Cli, Command};

    fn parse(args: &[&str]) -> Result<Command, clap::Error> {
        Cli::try_parse_from(args).map(|cli| cli.command)
    }

    #[test]
    fn an_entry_id_and_an_object_are_two_ways_to_say_the_same_thing() {
        // Naming both would leave which one wins up to argument order.
        assert!(
            parse(&[
                "fractal",
                "undo",
                "--entry",
                "20260909T201150.262Z",
                "--type",
                "CLAS",
                "--name",
                "ZCL_SAMPLE",
            ])
            .is_err()
        );
    }

    #[test]
    fn naming_an_object_takes_both_halves() {
        assert!(parse(&["fractal", "undo", "--type", "CLAS"]).is_err());
        assert!(parse(&["fractal", "undo", "--name", "ZCL_SAMPLE"]).is_err());
        assert!(parse(&["fractal", "undo", "--type", "CLAS", "--name", "ZCL_SAMPLE"]).is_ok());
    }

    #[test]
    fn nothing_is_forced_or_dry_by_default() {
        let Ok(Command::Undo(args)) =
            parse(&["fractal", "undo", "--entry", "20260909T201150.262Z"])
        else {
            panic!("expected undo");
        };
        assert!(!args.force);
        assert!(!args.dry_run);
    }
}
