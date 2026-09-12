//! Reading the journal, and applying its retention policy.
//!
//! These are the recoverable-by-hand half of the feature: `show` prints the
//! path of the content it holds rather than the content itself, so `diff` and
//! an editor do the work they are already good at.

use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::{
    cli::{JournalClearArgs, JournalListArgs, JournalShowArgs},
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::config;
use fractal::journal::blobs::BlobStore;
use fractal::journal::entry::{ContentKind, ContentRef, JournalEntry};
use fractal::journal::paths;
use fractal::journal::retention::{PruneOutcome, RetentionPolicy, entry_stores, run_retention};
use fractal::journal::store::EntryStore;

#[derive(Debug, Serialize)]
pub struct JournalListOutput {
    ok: bool,
    profile: String,
    scope: &'static str,
    total: usize,
    returned: usize,
    entries: Vec<JournalEntrySummary>,
}

#[derive(Debug, Serialize)]
struct JournalEntrySummary {
    id: String,
    recorded_at: String,
    status: String,
    operation: String,
    object_type: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<String>,
    /// Whether the content this entry holds is still on disk. An entry whose
    /// blobs were swept is a record of what happened, not something to restore.
    content_available: bool,
}

#[derive(Debug, Serialize)]
pub struct JournalShowOutput {
    ok: bool,
    profile: String,
    #[serde(flatten)]
    entry: JournalEntry,
    /// Where the content lives, for `diff` and an editor.
    content: Vec<JournalContentPath>,
    /// How to put a deleted object back, by hand. Absent for an activation,
    /// which `fractal undo` reverses on its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    restore: Option<RestoreRecipe>,
}

/// The steps that recreate a deleted object.
///
/// Not automated: restoring is create, write and activate, it needs a package
/// and a transport, and any step can fail halfway. The journal makes recovery
/// possible by hand, which is the part that matters.
#[derive(Debug, Serialize)]
struct RestoreRecipe {
    steps: Vec<String>,
    /// Things that will bite, learned the hard way.
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct JournalContentPath {
    field: &'static str,
    sha256: String,
    path: String,
    available: bool,
}

#[derive(Debug, Serialize)]
pub struct JournalClearOutput {
    ok: bool,
    profile: String,
    dry_run: bool,
    entries_removed: usize,
    blobs_removed: usize,
    temporaries_removed: usize,
}

pub fn journal_list(
    explicit_profile: Option<&str>,
    args: &JournalListArgs,
) -> Result<JournalListOutput, Reported> {
    let (profile_name, profile) = selected_profile(explicit_profile)?;
    let blobs = BlobStore::new(paths::blob_root()?);

    let mut entries = Vec::new();
    for store in stores(&profile, args.all_systems)? {
        entries.extend(store.list()?);
    }
    entries.retain(|entry| matches(entry, args));
    // Newest first: a journal is read to find what just happened.
    entries.reverse();

    let total = entries.len();
    entries.truncate(args.limit);
    Ok(JournalListOutput {
        ok: true,
        profile: profile_name,
        scope: if args.all_systems {
            "all-systems"
        } else {
            "profile"
        },
        total,
        returned: entries.len(),
        entries: entries
            .iter()
            .map(|entry| summarize(entry, &blobs))
            .collect(),
    })
}

pub fn journal_show(
    explicit_profile: Option<&str>,
    args: &JournalShowArgs,
) -> Result<JournalShowOutput, Reported> {
    let (profile_name, profile) = selected_profile(explicit_profile)?;
    let blobs = BlobStore::new(paths::blob_root()?);

    // A bare id does not say which object it belongs to, so this searches.
    for store in stores(&profile, args.all_systems)? {
        if let Ok(entry) = store.find(&args.id) {
            let content = content_paths(&entry, &blobs);
            let restore = restore_recipe(&entry, &blobs);
            return Ok(JournalShowOutput {
                ok: true,
                profile: profile_name,
                entry,
                content,
                restore,
            });
        }
    }
    Err(fractal::journal::JournalError::EntryMissing {
        id: args.id.clone(),
    }
    .into())
}

pub fn journal_clear(
    explicit_profile: Option<&str>,
    args: &JournalClearArgs,
) -> Result<JournalClearOutput, Reported> {
    let (profile_name, _profile) = selected_profile(explicit_profile)?;
    let policy = policy_from(args);

    // Retention is a whole-journal operation: one blob store serves every
    // system, so pruning one system's entries without marking from the others
    // would delete content they still reference.
    let outcome = if args.dry_run {
        preview_retention(policy)?
    } else {
        run_retention(
            &paths::journal_root()?,
            &BlobStore::new(paths::blob_root()?),
            policy,
            SystemTime::now(),
        )?
    };

    Ok(JournalClearOutput {
        ok: true,
        profile: profile_name,
        dry_run: args.dry_run,
        entries_removed: outcome.entries_removed,
        blobs_removed: outcome.blobs_removed,
        temporaries_removed: outcome.temporaries_removed,
    })
}

/// Counts what a real run would remove, without removing anything.
///
/// Deliberately not `run_retention` with a flag: a dry run must not be one
/// forgotten branch away from deleting.
fn preview_retention(policy: RetentionPolicy) -> Result<PruneOutcome, Reported> {
    let now = SystemTime::now();
    let mut outcome = PruneOutcome::default();
    let mut live = std::collections::HashSet::new();

    for store in entry_stores(&paths::journal_root()?)? {
        for object_key in store.object_keys()? {
            let all = store.entries_for(&object_key)?;
            for (index, entry) in all.iter().enumerate() {
                let newest = index + 1 == all.len();
                let too_many = all.len() - index > policy.keep_per_object;
                let old = store
                    .modified_at(&object_key, &entry.id)
                    .and_then(|modified| now.duration_since(modified).ok())
                    .is_some_and(|age| age >= policy.max_age);
                if !newest && (too_many || old) {
                    outcome.entries_removed += 1;
                } else {
                    live.extend(entry.referenced_blobs().map(str::to_owned));
                }
            }
        }
    }

    let blobs = BlobStore::new(paths::blob_root()?);
    outcome.blobs_removed = blobs
        .hashes()?
        .into_iter()
        .filter(|hash| !live.contains(hash))
        .count();
    Ok(outcome)
}

fn policy_from(args: &JournalClearArgs) -> RetentionPolicy {
    let default = RetentionPolicy::default();
    RetentionPolicy {
        keep_per_object: args.keep.unwrap_or(default.keep_per_object),
        max_age: args
            .older_than
            .map_or(default.max_age, |days| Duration::from_secs(days * 86_400)),
    }
}

fn selected_profile(explicit_profile: Option<&str>) -> Result<(String, config::Profile), Reported> {
    let loaded = config::load()?;
    let (name, profile) = config::resolve_profile(&loaded.config, explicit_profile)?;
    Ok((name.to_owned(), profile.clone()))
}

/// The stores to read: this profile's system, or every system on the machine.
fn stores(profile: &config::Profile, all_systems: bool) -> Result<Vec<EntryStore>, Reported> {
    if all_systems {
        Ok(entry_stores(&paths::journal_root()?)?)
    } else {
        Ok(vec![EntryStore::new(paths::entry_root(&profile.base_url)?)])
    }
}

fn matches(entry: &JournalEntry, args: &JournalListArgs) -> bool {
    let type_matches = args.object_type.as_ref().is_none_or(|wanted| {
        entry
            .object
            .object_type
            .as_str()
            .eq_ignore_ascii_case(wanted)
    });
    let name_matches = args
        .name
        .as_ref()
        .is_none_or(|wanted| entry.object.name.eq_ignore_ascii_case(wanted));
    type_matches && name_matches
}

fn summarize(entry: &JournalEntry, blobs: &BlobStore) -> JournalEntrySummary {
    JournalEntrySummary {
        id: entry.id.clone(),
        recorded_at: entry.recorded_at.clone(),
        status: entry.status.as_str().to_owned(),
        operation: entry.operation.as_str().to_owned(),
        object_type: entry.object.object_type.as_str().to_owned(),
        name: entry.object.name.clone(),
        transport: entry.transport.clone(),
        content_available: entry.referenced_blobs().all(|hash| blobs.contains(hash)),
    }
}

/// The commands that recreate a deleted object, filled in from what the entry
/// recorded.
fn restore_recipe(entry: &JournalEntry, blobs: &BlobStore) -> Option<RestoreRecipe> {
    let (package, description) = entry.operation.deletion_recipe()?;
    let object_type = entry.object.object_type.as_str();
    let name = &entry.object.name;
    let content = entry
        .active_before
        .sha256()
        .map(|sha256| blobs.path_of(sha256));

    let mut steps = vec![format!(
        "fractal edit create --type {object_type} --name {name} --package {} --description '{}'{}",
        // Named as unknown rather than guessed: a create against the wrong
        // package is a new object in the wrong place.
        package.map_or("<package>", String::as_str),
        description.map_or("<description>", String::as_str),
        if entry.transport.is_some() {
            " --transport <request>"
        } else {
            ""
        }
    )];
    let (write, file) = match entry.content_kind() {
        ContentKind::Source => ("edit set", "--source-file"),
        ContentKind::Xml => ("edit set-xml", "--xml-file"),
    };
    steps.push(match &content {
        // The blob path lives under the OS data directory, which has spaces in
        // it on macOS and Windows, so it is quoted to stay copy-pasteable.
        Some(path) => format!(
            "fractal {write} --type {object_type} --name {name} {file} '{}'",
            path.display()
        ),
        None => {
            "the content is no longer in the journal, so there is nothing to write back".to_owned()
        }
    });
    steps.push(format!(
        "fractal edit activate --type {object_type} --name {name}"
    ));

    let mut warnings = Vec::new();
    if entry.transport.is_some() {
        warnings.push(format!(
            "Recorded in {}, which may since have been released. Name a current request rather than reusing it.",
            entry.transport.as_deref().unwrap_or_default()
        ));
    }
    // Not specific to a family, though it was first seen on a service binding:
    // observed again on a PROG deleted and recreated seconds later.
    warnings.push(format!(
        "Step 1 may be refused with `403 ... User <you> is currently editing {name}`. The editing lock outlives the object it was taken on, so a restore under the same name can need a wait."
    ));
    if content.is_none() {
        warnings.push(
            "The entry survives but its content was swept, so only the shell can be recreated."
                .to_owned(),
        );
    }
    Some(RestoreRecipe { steps, warnings })
}

fn content_paths(entry: &JournalEntry, blobs: &BlobStore) -> Vec<JournalContentPath> {
    [
        ("active_before", Some(&entry.active_before)),
        ("inactive_before", entry.inactive_before.as_ref()),
        ("active_after", entry.active_after.as_ref()),
    ]
    .into_iter()
    .filter_map(|(field, reference)| {
        let sha256 = reference?.sha256()?;
        Some(JournalContentPath {
            field,
            sha256: sha256.to_owned(),
            path: blobs.path_of(sha256).display().to_string(),
            available: blobs.contains(sha256),
        })
    })
    .collect()
}

pub fn print_journal_list(result: &JournalListOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    let mut rendered = String::new();
    let _ = writeln!(rendered, "entries: {} of {}", result.returned, result.total);
    for entry in &result.entries {
        let _ = writeln!(
            rendered,
            "{}  {:<10} {:<9} {} {}{}",
            entry.id,
            entry.operation,
            entry.status,
            entry.object_type,
            entry.name,
            if entry.content_available {
                ""
            } else {
                "  (content gone)"
            }
        );
    }
    print!("{rendered}");
}

pub fn print_journal_show(result: &JournalShowOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    let entry = &result.entry;
    let mut rendered = String::new();
    let _ = writeln!(rendered, "entry: {}", entry.id);
    let _ = writeln!(rendered, "recorded: {}", entry.recorded_at);
    let _ = writeln!(
        rendered,
        "object: {} {}",
        entry.object.object_type.as_str(),
        entry.object.name
    );
    let _ = writeln!(
        rendered,
        "operation: {} ({})",
        entry.operation.as_str(),
        entry.status.as_str()
    );
    if let Some(transport) = &entry.transport {
        let _ = writeln!(rendered, "transport: {transport}");
    }
    if entry.active_before == ContentRef::Absent {
        let _ = writeln!(rendered, "active before: none, the object was not active");
    }
    for content in &result.content {
        let _ = writeln!(
            rendered,
            "{}: {}{}",
            content.field,
            content.path,
            if content.available { "" } else { "  (gone)" }
        );
    }
    if let Some(restore) = &result.restore {
        let _ = writeln!(rendered, "\nto restore it, by hand:");
        for (index, step) in restore.steps.iter().enumerate() {
            let _ = writeln!(rendered, "  {}. {step}", index + 1);
        }
        for warning in &restore.warnings {
            let _ = writeln!(rendered, "  ! {warning}");
        }
    }
    print!("{rendered}");
}

pub fn print_journal_clear(result: &JournalClearOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    let mut rendered = String::new();
    let _ = writeln!(
        rendered,
        "{} {} entr{}, {} blob{}, {} temporar{}",
        if result.dry_run {
            "would remove"
        } else {
            "removed"
        },
        result.entries_removed,
        if result.entries_removed == 1 {
            "y"
        } else {
            "ies"
        },
        result.blobs_removed,
        if result.blobs_removed == 1 { "" } else { "s" },
        result.temporaries_removed,
        if result.temporaries_removed == 1 {
            "y"
        } else {
            "ies"
        }
    );
    print!("{rendered}");
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, JournalCommand};
    use fractal::journal::entry::JournalOperation;

    fn list_args(cli: Cli) -> JournalListArgs {
        let Command::Journal {
            command: JournalCommand::List(args),
        } = cli.command
        else {
            panic!("expected journal list");
        };
        args
    }

    fn clear_args(cli: Cli) -> JournalClearArgs {
        let Command::Journal {
            command: JournalCommand::Clear(args),
        } = cli.command
        else {
            panic!("expected journal clear");
        };
        args
    }

    fn entry(name: &str, object_type: &str) -> JournalEntry {
        use fractal::journal::entry::{EntryObject, EntryStatus, EntrySystem, JournalOperation};
        use fractal::sap::object_family::AdtObjectFamily;

        JournalEntry {
            id: "20260909T000000.000Z".to_owned(),
            recorded_at: "2026-09-09T00:00:00.000Z".to_owned(),
            status: EntryStatus::Succeeded,
            system: EntrySystem {
                base_url: "https://sap.example:8001".to_owned(),
                profile: "dev".to_owned(),
                client: "100".to_owned(),
                user: "developer".to_owned(),
            },
            object: EntryObject {
                object_type: AdtObjectFamily::parse(object_type).unwrap(),
                name: name.to_owned(),
                uri: format!("/sap/bc/adt/programs/programs/{}", name.to_lowercase()),
                source_part: None,
            },
            operation: JournalOperation::activate(),
            transport: None,
            active_before: ContentRef::Sha256("a".repeat(64)),
            inactive_before: None,
            active_after: Some(ContentRef::Sha256("b".repeat(64))),
            etag_after: None,
        }
    }

    #[test]
    fn filters_are_case_insensitive_and_independent() {
        let all = list_args(Cli::try_parse_from(["fractal", "journal", "list"]).unwrap());
        assert!(matches(&entry("ZSAMPLE", "PROG"), &all));

        let by_type = list_args(
            Cli::try_parse_from(["fractal", "journal", "list", "--type", "prog"]).unwrap(),
        );
        assert!(matches(&entry("ZSAMPLE", "PROG"), &by_type));
        assert!(!matches(&entry("ZCL_X", "CLAS"), &by_type));

        let by_name = list_args(
            Cli::try_parse_from(["fractal", "journal", "list", "--name", "zsample"]).unwrap(),
        );
        assert!(matches(&entry("ZSAMPLE", "PROG"), &by_name));
        assert!(!matches(&entry("ZOTHER", "PROG"), &by_name));
    }

    #[test]
    fn clear_defaults_to_the_retention_policy_and_flags_override_it() {
        let default = policy_from(&clear_args(
            Cli::try_parse_from(["fractal", "journal", "clear"]).unwrap(),
        ));
        assert_eq!(default, RetentionPolicy::default());

        let overridden = policy_from(&clear_args(
            Cli::try_parse_from([
                "fractal",
                "journal",
                "clear",
                "--keep",
                "3",
                "--older-than",
                "7",
            ])
            .unwrap(),
        ));
        assert_eq!(overridden.keep_per_object, 3);
        assert_eq!(overridden.max_age, Duration::from_secs(7 * 86_400));
    }

    #[test]
    fn a_dry_run_is_a_separate_path_from_the_one_that_deletes() {
        // The flag reaches the command, and nothing in `journal_clear` passes
        // it into `run_retention`: a dry run must not be one forgotten branch
        // away from deleting.
        let args =
            clear_args(Cli::try_parse_from(["fractal", "journal", "clear", "--dry-run"]).unwrap());
        assert!(args.dry_run);
    }

    #[test]
    fn statuses_render_as_their_json_spelling() {
        // `list` and `show` must agree, and both must match what a caller
        // filtering the JSON sees.
        let entry = entry("ZSAMPLE", "PROG");
        assert_eq!(entry.status.as_str(), "succeeded");
        assert_eq!(entry.operation.as_str(), "activate");
        // Both spellings are written out, not derived from `Debug`: the
        // operation gained a payload and a derived one would have silently
        // started emitting `activate { undo_progress: none }`.
        assert_eq!(
            serde_json::to_value(entry.status).unwrap(),
            serde_json::json!("succeeded")
        );
    }

    #[test]
    fn a_deletion_recipe_names_the_package_the_description_and_the_content() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let sha256 = blobs.put("REPORT zsample.").unwrap();
        let mut entry = entry("ZSAMPLE", "PROG");
        entry.operation =
            JournalOperation::delete(Some("ZPKG".to_owned()), Some("Sample".to_owned()));
        entry.active_before = ContentRef::Sha256(sha256.clone());

        let recipe = restore_recipe(&entry, &blobs).expect("a delete has a recipe");

        assert!(
            recipe.steps[0].contains("--package ZPKG"),
            "{:?}",
            recipe.steps
        );
        assert!(recipe.steps[0].contains("--description 'Sample'"));
        assert!(recipe.steps[1].contains(&sha256));
        // A copy-pasteable line: the flag keeps its value, and the path is
        // quoted because the OS data directory has spaces in it.
        assert!(
            recipe.steps[1].contains("--source-file '"),
            "{}",
            recipe.steps[1]
        );
        assert!(recipe.steps[2].starts_with("fractal edit activate"));
        // The trap that actually bites, on every family: the editing lock
        // outlives the object, so step 1 can be refused straight away.
        assert!(
            recipe
                .warnings
                .iter()
                .any(|warning| warning.contains("currently editing")),
            "{:?}",
            recipe.warnings
        );
    }

    #[test]
    fn an_activation_has_no_restore_recipe() {
        // `fractal undo` reverses it, so printing a by-hand recipe would offer
        // a worse route than the one that exists.
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));

        assert!(restore_recipe(&entry("ZSAMPLE", "PROG"), &blobs).is_none());
    }

    #[test]
    fn a_recipe_says_when_the_content_is_gone_rather_than_naming_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let mut entry = entry("ZSAMPLE", "PROG");
        entry.operation = JournalOperation::delete(None, None);
        entry.active_before = ContentRef::Absent;

        let recipe = restore_recipe(&entry, &blobs).unwrap();

        assert!(recipe.steps[1].contains("no longer in the journal"));
        assert!(
            recipe
                .warnings
                .iter()
                .any(|warning| warning.contains("swept"))
        );
        // Unknown rather than guessed: a create against the wrong package puts
        // the object back in the wrong place.
        assert!(recipe.steps[0].contains("<package>"));
    }

    #[test]
    fn content_paths_skip_absences_and_report_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let stored = blobs.put("kept").unwrap();

        let mut entry = entry("ZSAMPLE", "PROG");
        entry.active_before = ContentRef::Absent;
        entry.active_after = Some(ContentRef::Sha256(stored.clone()));
        entry.inactive_before = Some(ContentRef::Sha256("c".repeat(64)));

        let paths = content_paths(&entry, &blobs);
        // The absence is not a path, and the swept blob is reported as gone
        // rather than silently offered.
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|path| path.field != "active_before"));
        assert!(
            paths
                .iter()
                .any(|path| path.sha256 == stored && path.available)
        );
        assert!(paths.iter().any(|path| !path.available));
    }
}
