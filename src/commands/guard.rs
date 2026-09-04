//! Writes agent-harness permission rules for Fractal's mutating commands.
//!
//! Nothing this CLI checks about its own invocation can stop the agent that
//! invoked it: the caller supplies argv, stdin, the environment and the config
//! file, so any in-band confirmation is a flag the caller simply passes. The
//! layer that *can* stop it is the harness's permission system, which decides
//! before the command runs and which the agent does not control.
//!
//! So this command does not guard anything itself. It writes the rules that
//! make the harness guard it, because a rule nobody writes protects nobody.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::{
    cli::{GuardHarnessArg, GuardInstallArgs},
    output::{OutputFormat, print_result},
    reported::Reported,
};
use fractal::reportable_error::ReportableError;

/// The one verb that destroys committed work, and the only one refused
/// outright. Everything else here can be redone; a deleted object cannot.
pub(super) const DENIED: &[&str] = &["fractal delete"];

/// Real changes with a bounded blast radius: they write source, create
/// objects, or move transports. Worth a prompt, not a refusal.
///
/// `edit discard` sits here rather than with the refusals: it throws away
/// inactive changes, which is somebody's work in progress, but the active
/// version is untouched and the blast radius stops there.
pub(super) const ASKED: &[&str] = &[
    "fractal edit create",
    "fractal edit set",
    "fractal edit set-xml",
    "fractal edit patch",
    "fractal edit activate",
    "fractal edit discard",
    "fractal transport create",
    "fractal auth",
];

/// What the Codex hook runs. Fractal is its own hook program, so the rule lists
/// above stay the single source of truth instead of being copied into a
/// generated script that then drifts.
const CODEX_HOOK_COMMAND: &str = "fractal guard hook";

#[derive(Debug, Error)]
pub enum GuardError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not valid JSON: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{path} does not contain a JSON object at its top level")]
    NotAnObject { path: PathBuf },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl ReportableError for GuardError {
    fn code(&self) -> &'static str {
        match self {
            Self::Read { .. } => "guard_settings_read_error",
            Self::Parse { .. } | Self::NotAnObject { .. } => "guard_settings_invalid",
            Self::Write { .. } => "guard_settings_write_error",
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Read { .. } | Self::Write { .. } => {
                "Check that the directory exists and is writable.".to_owned()
            }
            // Never overwrite a settings file we could not understand: it is
            // the user's, and it may hold rules that matter more than ours.
            Self::Parse { .. } | Self::NotAnObject { .. } => {
                "Fix or move the settings file first. Fractal will not overwrite a settings file it cannot parse."
                    .to_owned()
            }
        })
    }
}

#[derive(Debug, Serialize)]
pub struct GuardInstallResult {
    ok: bool,
    harness: &'static str,
    settings_path: String,
    written: bool,
    dry_run: bool,
    added_deny: Vec<String>,
    added_ask: Vec<String>,
    already_present: usize,
    /// Anything the caller needs to know that the rule lists do not say.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    notes: Vec<String>,
}

pub fn guard_install(args: &GuardInstallArgs) -> Result<GuardInstallResult, Reported> {
    let harness = args.harness.unwrap_or(GuardHarnessArg::Claude);
    let directory = args
        .dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if matches!(harness, GuardHarnessArg::Codex) {
        return codex_install(&directory, args);
    }
    let settings_path = claude_settings_path(&directory, args.local);

    let mut settings = read_settings(&settings_path)?;
    // Everything mutating can be made to prompt instead, for a workflow where a
    // human is watching every step anyway.
    let everything: Vec<&str>;
    let (deny, ask): (&[&str], &[&str]) = if args.ask_only {
        everything = DENIED.iter().chain(ASKED).copied().collect();
        (&[], &everything)
    } else {
        (DENIED, ASKED)
    };

    let added_deny = merge_rules(&mut settings, "deny", deny);
    let added_ask = merge_rules(&mut settings, "ask", ask);
    let already_present = deny.len() + ask.len() - added_deny.len() - added_ask.len();
    let changed = !added_deny.is_empty() || !added_ask.is_empty();

    if changed && !args.dry_run {
        write_settings(&settings_path, &settings)?;
    }

    Ok(GuardInstallResult {
        ok: true,
        harness: "claude-code",
        settings_path: settings_path.display().to_string(),
        written: changed && !args.dry_run,
        dry_run: args.dry_run,
        added_deny,
        added_ask,
        already_present,
        notes: Vec::new(),
    })
}

pub fn print_guard_install(result: &GuardInstallResult, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    let mut rendered = String::new();
    let _ = writeln!(rendered, "harness: {}", result.harness);
    let _ = writeln!(rendered, "settings: {}", result.settings_path);
    for rule in &result.added_deny {
        let _ = writeln!(rendered, "  deny  {rule}");
    }
    for rule in &result.added_ask {
        let _ = writeln!(rendered, "  ask   {rule}");
    }
    if result.already_present > 0 {
        let _ = writeln!(
            rendered,
            "  ({} rule(s) already present, left alone)",
            result.already_present
        );
    }
    for note in &result.notes {
        let _ = writeln!(rendered, "note: {note}");
    }
    let _ = writeln!(
        rendered,
        "{}",
        if result.dry_run {
            "dry run: nothing written"
        } else if result.written {
            "written"
        } else {
            "no change needed"
        }
    );
    print!("{rendered}");
}

/// Codex has no declarative allow/deny list. Its permission layer is a hook
/// program that is handed each tool call, so what gets installed is a pointer
/// back at this binary rather than a set of rules.
///
/// Codex parses the `ask` decision but does not yet act on it, so only the
/// refusals are enforceable there. The result says so rather than implying a
/// coverage the harness will not deliver.
fn codex_install(
    directory: &Path,
    args: &GuardInstallArgs,
) -> Result<GuardInstallResult, Reported> {
    let hooks_path = directory.join(".codex").join("hooks.json");
    let mut settings = read_settings(&hooks_path)?;
    let added = merge_codex_hook(&mut settings);

    if added && !args.dry_run {
        write_settings(&hooks_path, &settings)?;
    }

    Ok(GuardInstallResult {
        ok: true,
        harness: "codex",
        settings_path: hooks_path.display().to_string(),
        written: added && !args.dry_run,
        dry_run: args.dry_run,
        added_deny: if added {
            DENIED.iter().map(|rule| (*rule).to_owned()).collect()
        } else {
            Vec::new()
        },
        // Codex parses `ask` and does not act on it. Reporting these as
        // installed would claim protection that is not there.
        added_ask: Vec::new(),
        already_present: usize::from(!added),
        notes: vec![
            "Codex parses the `ask` decision but does not act on it yet, so only the refusals are enforced. The commands that would merely prompt are unguarded here."
                .to_owned(),
            "The hook runs `fractal guard hook`, so `fractal` has to be on the PATH Codex runs commands with."
                .to_owned(),
        ],
    })
}

/// Adds a `PreToolUse` hook for Bash tool calls, if one pointing at Fractal is
/// not already there. Returns whether anything changed.
fn merge_codex_hook(settings: &mut Map<String, Value>) -> bool {
    let hooks = settings
        .entry("hooks".to_owned())
        .or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        return false;
    };
    let events = hooks
        .entry("PreToolUse".to_owned())
        .or_insert_with(|| json!([]));
    let Some(events) = events.as_array_mut() else {
        return false;
    };

    if events.iter().any(is_fractal_hook) {
        return false;
    }
    events.push(json!({
        "matcher": "Bash",
        "hooks": [{
            "type": "command",
            "command": format!("{CODEX_HOOK_COMMAND} --no-ask"),
            "statusMessage": "Checking Fractal command",
            "timeout": 30
        }]
    }));
    true
}

/// Whether a matcher group already runs Fractal's hook, so that installing
/// twice does not stack duplicates.
fn is_fractal_hook(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.contains(CODEX_HOOK_COMMAND))
            })
        })
}

/// Project settings by default; `--local` targets the personal, gitignored
/// file instead, for a rule the rest of the team should not inherit.
fn claude_settings_path(directory: &Path, local: bool) -> PathBuf {
    let file = if local {
        "settings.local.json"
    } else {
        "settings.json"
    };
    directory.join(".claude").join(file)
}

fn read_settings(path: &Path) -> Result<Map<String, Value>, GuardError> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let text = std::fs::read_to_string(path).map_err(|source| GuardError::Read {
        path: path.to_owned(),
        source,
    })?;
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(&text).map_err(|source| GuardError::Parse {
        path: path.to_owned(),
        source,
    })?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| GuardError::NotAnObject {
            path: path.to_owned(),
        })
}

fn write_settings(path: &Path, settings: &Map<String, Value>) -> Result<(), GuardError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| GuardError::Write {
            path: parent.to_owned(),
            source,
        })?;
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(settings.clone()))
        .unwrap_or_else(|_| "{}".to_owned());
    text.push('\n');
    std::fs::write(path, text).map_err(|source| GuardError::Write {
        path: path.to_owned(),
        source,
    })
}

/// Adds the rules that are not already there, and returns those.
///
/// Merges rather than replaces, and never removes: the file belongs to the
/// user, and the rules already in it may matter more than these.
fn merge_rules(settings: &mut Map<String, Value>, bucket: &str, commands: &[&str]) -> Vec<String> {
    let permissions = settings
        .entry("permissions".to_owned())
        .or_insert_with(|| json!({}));
    let Some(permissions) = permissions.as_object_mut() else {
        return Vec::new();
    };
    let entries = permissions
        .entry(bucket.to_owned())
        .or_insert_with(|| json!([]));
    let Some(entries) = entries.as_array_mut() else {
        return Vec::new();
    };

    let mut added = Vec::new();
    for command in commands {
        let rule = rule_for(command);
        if entries
            .iter()
            .any(|existing| existing.as_str() == Some(&rule))
        {
            continue;
        }
        entries.push(Value::String(rule.clone()));
        added.push(rule);
    }
    added
}

/// Claude Code matches a Bash rule by command prefix, so the trailing `:*`
/// covers every invocation of the command whatever its arguments.
fn rule_for(command: &str) -> String {
    format!("Bash({command}:*)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(bucket: &str, rules: &[&str]) -> Map<String, Value> {
        let mut settings = Map::new();
        settings.insert(
            "permissions".to_owned(),
            json!({ bucket: rules.iter().map(|r| Value::String((*r).to_owned())).collect::<Vec<_>>() }),
        );
        settings
    }

    #[test]
    fn builds_prefix_rules_that_cover_every_argument_list() {
        assert_eq!(rule_for("fractal delete"), "Bash(fractal delete:*)");
    }

    #[test]
    fn adds_the_rules_that_are_missing() {
        let mut settings = Map::new();
        let added = merge_rules(&mut settings, "deny", &["fractal delete"]);

        assert_eq!(added, vec!["Bash(fractal delete:*)".to_owned()]);
        assert_eq!(
            settings["permissions"]["deny"][0],
            json!("Bash(fractal delete:*)")
        );
    }

    #[test]
    fn installing_twice_changes_nothing_the_second_time() {
        let mut settings = settings_with("deny", &["Bash(fractal delete:*)"]);
        let added = merge_rules(&mut settings, "deny", &["fractal delete"]);

        assert!(added.is_empty());
        assert_eq!(settings["permissions"]["deny"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn rules_the_user_already_had_are_kept() {
        let mut settings = settings_with("deny", &["Bash(rm:*)"]);
        merge_rules(&mut settings, "deny", &["fractal delete"]);

        let deny = settings["permissions"]["deny"].as_array().unwrap();
        // The file is the user's. Their rules may matter more than ours, so
        // this only ever appends.
        assert!(deny.contains(&json!("Bash(rm:*)")));
        assert!(deny.contains(&json!("Bash(fractal delete:*)")));
    }

    #[test]
    fn other_settings_in_the_file_survive() {
        let mut settings = Map::new();
        settings.insert("model".to_owned(), json!("opus"));
        settings.insert("permissions".to_owned(), json!({ "allow": ["Bash(ls:*)"] }));
        merge_rules(&mut settings, "deny", &["fractal delete"]);

        assert_eq!(settings["model"], json!("opus"));
        assert_eq!(settings["permissions"]["allow"][0], json!("Bash(ls:*)"));
    }

    #[test]
    fn the_destructive_verb_is_denied_and_not_merely_asked_about() {
        assert_eq!(DENIED, &["fractal delete"]);
        assert!(!ASKED.contains(&"fractal delete"));
        // Discarding loses work in progress but leaves the active version
        // intact, so it prompts rather than being refused outright.
        assert!(ASKED.contains(&"fractal edit discard"));
        // `edit read` and every other read-only command must stay unlisted, or
        // the rules would make ordinary exploration prompt for approval.
        for rule in DENIED.iter().chain(ASKED) {
            assert!(!rule.contains("read"), "{rule}");
            assert!(!rule.contains("search"), "{rule}");
        }
    }

    #[test]
    fn the_codex_hook_points_back_at_this_binary() {
        let mut settings = Map::new();
        assert!(merge_codex_hook(&mut settings));

        let group = &settings["hooks"]["PreToolUse"][0];
        assert_eq!(group["matcher"], json!("Bash"));
        let hook = &group["hooks"][0];
        assert_eq!(hook["type"], json!("command"));
        // `--no-ask` because Codex parses that decision but does not act on it.
        assert_eq!(hook["command"], json!("fractal guard hook --no-ask"));
    }

    #[test]
    fn installing_the_codex_hook_twice_does_not_stack_it() {
        let mut settings = Map::new();
        assert!(merge_codex_hook(&mut settings));
        assert!(!merge_codex_hook(&mut settings));
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_codex_hook_someone_else_wrote_is_left_alone() {
        let mut settings = Map::new();
        settings.insert(
            "hooks".to_owned(),
            json!({ "PreToolUse": [
                { "matcher": "Bash", "hooks": [{ "type": "command", "command": "/usr/local/bin/house-rules" }] }
            ]}),
        );
        assert!(merge_codex_hook(&mut settings));

        let groups = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0]["hooks"][0]["command"],
            json!("/usr/local/bin/house-rules")
        );
    }

    #[test]
    fn project_and_personal_settings_are_different_files() {
        let directory = Path::new("/tmp/project");
        assert!(claude_settings_path(directory, false).ends_with(".claude/settings.json"));
        assert!(claude_settings_path(directory, true).ends_with(".claude/settings.local.json"));
    }
}
