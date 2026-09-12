//! Fractal as a `PreToolUse` hook: decides whether a shell command it is
//! shown should be allowed.
//!
//! Codex has no declarative allow/deny list — its permission layer is a hook
//! program that is handed the tool call and answers. Rather than generating a
//! script and leaving it to drift, Fractal *is* that program, so the rule list
//! has exactly one home and `guard install` only has to write a pointer to it.
//!
//! The decision is made on a command string the harness supplies, which means
//! it recognises the ordinary spellings of an invocation and not deliberate
//! obfuscation. That is the right level: this catches an agent doing the wrong
//! thing, not an agent hiding what it is doing. Nothing here is a security
//! boundary.

use serde::Serialize;
use serde_json::Value;

use super::guard::{ASKED, DENIED, NO_JOURNAL_FLAG};
use crate::cli::GuardHookArgs;

/// What the harness is told. `ask` is omitted entirely when the harness cannot
/// act on it, rather than being downgraded to a silent `allow` that looks like
/// a decision and is not one.
#[derive(Debug, PartialEq, Eq)]
pub enum HookDecision {
    Deny(String),
    Ask(String),
    NoOpinion,
}

#[derive(Debug, Serialize)]
struct HookSpecificOutput {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'static str,
    #[serde(rename = "permissionDecision")]
    permission_decision: &'static str,
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: String,
}

#[derive(Debug, Serialize)]
struct HookOutput {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookSpecificOutput,
}

/// Reads one hook payload from stdin and prints the decision.
///
/// Always exits successfully: a hook that fails noisily on an unexpected
/// payload would break every tool call in the session, which is a far worse
/// outcome than failing to guard one command. Silence means "no opinion".
pub fn guard_hook(args: &GuardHookArgs) {
    let mut payload = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut payload).is_err() {
        return;
    }

    let command = command_from_payload(&payload);
    let decision = command.map_or(HookDecision::NoOpinion, |command| {
        decide(&command, args.no_ask)
    });

    if let Some(rendered) = render_decision(&decision) {
        println!("{rendered}");
    }
}

/// Extracts the shell command from a `PreToolUse` payload.
///
/// Only Bash tool calls carry one. Anything else — a file edit, an MCP call —
/// is not this guard's business and yields `None`.
fn command_from_payload(payload: &str) -> Option<String> {
    let value: Value = serde_json::from_str(payload).ok()?;
    let tool_name = value.get("tool_name")?.as_str()?;
    if !tool_name.eq_ignore_ascii_case("bash") {
        return None;
    }
    Some(
        value
            .get("tool_input")?
            .get("command")?
            .as_str()?
            .to_owned(),
    )
}

/// Matches a command string against the rules.
///
/// `no_ask` is for harnesses that parse the `ask` decision but cannot act on
/// it: those rules become no opinion, because a decision the harness will
/// silently drop is worse than an honest gap.
#[must_use]
pub fn decide(command: &str, no_ask: bool) -> HookDecision {
    for segment in shell_segments(command) {
        let Some(invocation) = fractal_invocation(segment) else {
            continue;
        };
        if let Some(rule) = matching_rule(&invocation, DENIED) {
            return HookDecision::Deny(format!(
                "`{rule}` is refused by this project's Fractal guard. It deletes a repository object, which cannot be undone.{} A person can run it directly if it is genuinely wanted.",
                unjournalled_note(&invocation)
            ));
        }
        if !no_ask && let Some(rule) = matching_rule(&invocation, ASKED) {
            return HookDecision::Ask(format!(
                "`{rule}` {}.{} This project's Fractal guard asks before it runs.",
                what_it_changes(rule),
                unjournalled_note(&invocation)
            ));
        }
    }
    HookDecision::NoOpinion
}

/// What the person approving is actually agreeing to.
///
/// Not every rule on the ask list touches SAP, and saying so of all of them
/// would be false for two — which is worse than vague, because the reason is
/// the only thing a person reads before deciding.
fn what_it_changes(rule: &str) -> &'static str {
    match rule {
        "fractal journal clear" => {
            "deletes the local record of what Fractal changed, which is the only way back from an operation SAP will not reverse"
        }
        "fractal auth" => "changes stored SAP credentials",
        _ => "changes a SAP system",
    }
}

/// What to add to a decision's reason when the caller has turned the journal
/// off.
///
/// This does not change the verdict — every command that accepts the flag is
/// already denied or asked about — but it changes what the person approving is
/// agreeing to, which is the whole point of asking them. A declarative rule
/// cannot see a flag at all; the hook is handed the command string, so this is
/// the only place the distinction can be made.
fn unjournalled_note(invocation: &[String]) -> &'static str {
    if invocation.iter().any(|word| word == NO_JOURNAL_FLAG) {
        " It also passes `--no-journal`, so nothing is recorded and there is no way back."
    } else {
        ""
    }
}

/// Splits a shell command on the separators that start a new command, so that
/// `cd /x && fractal delete ...` is seen.
fn shell_segments(command: &str) -> impl Iterator<Item = &str> {
    command
        .split(['\n', ';', '|', '&'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
}

/// The argument words of a Fractal invocation, if this segment is one.
///
/// Leading `VAR=value` assignments are skipped, and the program is matched on
/// its file name so that `./target/debug/fractal` and an absolute path both
/// count.
fn fractal_invocation(segment: &str) -> Option<Vec<String>> {
    let mut words = segment
        .split_whitespace()
        .skip_while(|word| word.contains('=') && !word.starts_with('-'));
    let program = words.next()?;
    let program = program
        .trim_matches(['"', '\''])
        .rsplit(['/', '\\'])
        .next()?;
    if program != "fractal" && program != "fractal.exe" {
        return None;
    }
    Some(words.map(str::to_owned).collect())
}

/// Global flags that take a separate value word, which has to be skipped along
/// with the flag or it would be mistaken for the subcommand.
const VALUE_FLAGS: &[&str] = &["--profile", "--output"];

/// Whether the invocation's leading words are one of the listed commands.
///
/// Compares whole words, so `fractal delete` does not match a rule for
/// `fractal deletex`, and global flags before the subcommand are skipped —
/// including their values, so `fractal --profile de3 delete` still reads as a
/// delete.
fn matching_rule<'a>(invocation: &[String], rules: &[&'a str]) -> Option<&'a str> {
    let mut words: Vec<&str> = Vec::new();
    let mut remaining = invocation.iter().map(String::as_str).peekable();
    while let Some(word) = remaining.peek() {
        if !word.starts_with('-') {
            break;
        }
        let flag = remaining.next().unwrap_or_default();
        if VALUE_FLAGS.contains(&flag) {
            remaining.next();
        }
    }
    words.extend(remaining);
    rules.iter().copied().find(|rule| {
        let expected: Vec<&str> = rule.split_whitespace().skip(1).collect();
        words.len() >= expected.len() && words[..expected.len()] == expected[..]
    })
}

fn render_decision(decision: &HookDecision) -> Option<String> {
    let (verdict, reason) = match decision {
        HookDecision::Deny(reason) => ("deny", reason.clone()),
        HookDecision::Ask(reason) => ("ask", reason.clone()),
        HookDecision::NoOpinion => return None,
    };
    serde_json::to_string(&HookOutput {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "PreToolUse",
            permission_decision: verdict,
            permission_decision_reason: reason,
        },
    })
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(command: &str) -> String {
        serde_json::json!({
            "session_id": "thr_1",
            "cwd": "/workspace",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command }
        })
        .to_string()
    }

    #[test]
    fn denies_the_delete_verb() {
        let decision = decide("fractal delete --type CLAS --name ZCL_SAMPLE", false);
        assert!(matches!(decision, HookDecision::Deny(_)));
    }

    #[test]
    fn asks_about_a_write_unless_the_harness_cannot_ask() {
        let command = "fractal edit set --type PROG --name ZSAMPLE --source-file x.abap";
        assert!(matches!(decide(command, false), HookDecision::Ask(_)));
        // Codex parses `ask` but does not act on it, so emitting one would be a
        // decision the harness silently drops. An honest gap beats that.
        assert_eq!(decide(command, true), HookDecision::NoOpinion);
    }

    #[test]
    fn read_only_commands_get_no_opinion() {
        for command in [
            "fractal edit read --type PROG --name ZSAMPLE",
            "fractal object search 'Z*'",
            "fractal ddic show ZSAMPLE_STATUS",
            "fractal table data ZSAMPLE",
            // Reading the journal is not changing it.
            "fractal journal list",
            "fractal journal show 20260909T201150.262Z",
        ] {
            assert_eq!(decide(command, false), HookDecision::NoOpinion, "{command}");
        }
    }

    #[test]
    fn a_reason_says_what_the_command_actually_changes() {
        // The reason is the only thing a person reads before deciding, so a
        // rule that touches no SAP system must not claim to.
        let HookDecision::Ask(reason) = decide("fractal journal clear", false) else {
            panic!("expected an ask");
        };
        assert!(!reason.contains("changes a SAP system"), "{reason}");
        assert!(reason.contains("local record"), "{reason}");

        let HookDecision::Ask(credentials) = decide("fractal auth login", false) else {
            panic!("expected an ask");
        };
        assert!(credentials.contains("credentials"), "{credentials}");
    }

    #[test]
    fn undoing_and_clearing_the_journal_are_asked_about() {
        // `undo` publishes a previous version, which is a change to the active
        // version like any other. `journal clear` changes nothing in SAP but
        // destroys the before-images every other rule here relies on.
        for command in [
            "fractal undo --type PROG --name ZSAMPLE",
            "fractal journal clear --older-than 0",
        ] {
            assert!(
                matches!(decide(command, false), HookDecision::Ask(_)),
                "{command}"
            );
        }
    }

    #[test]
    fn turning_the_journal_off_is_named_in_the_reason() {
        // The flag cannot change the verdict — every command that takes it is
        // already asked about or denied — but it changes what the person
        // approving is agreeing to, and only the hook can see it at all.
        let HookDecision::Ask(reason) = decide(
            "fractal edit activate --type PROG --name ZSAMPLE --no-journal",
            false,
        ) else {
            panic!("expected an ask");
        };
        assert!(reason.contains("--no-journal"), "{reason}");
        assert!(reason.contains("no way back"), "{reason}");

        let HookDecision::Ask(plain) =
            decide("fractal edit activate --type PROG --name ZSAMPLE", false)
        else {
            panic!("expected an ask");
        };
        assert!(!plain.contains("--no-journal"), "{plain}");
    }

    #[test]
    fn a_dry_run_is_still_guarded() {
        // It changes nothing, and the hook could see that. It does not, on
        // purpose: a declarative rule matches a prefix and cannot tell a dry run
        // apart, so exempting it here would make the same command answer
        // differently depending on the harness.
        assert!(matches!(
            decide(
                "fractal delete --type CLAS --name ZCL_SAMPLE --dry-run",
                false
            ),
            HookDecision::Deny(_)
        ));
        assert!(matches!(
            decide("fractal undo --type PROG --name ZSAMPLE --dry-run", false),
            HookDecision::Ask(_)
        ));
    }

    #[test]
    fn sees_through_the_ordinary_spellings_of_an_invocation() {
        for command in [
            "cd /repo && fractal delete --type CLAS --name ZCL_SAMPLE",
            "./target/debug/fractal delete --type CLAS --name ZCL_SAMPLE",
            "/usr/local/bin/fractal delete --type CLAS --name ZCL_SAMPLE",
            "FRACTAL_PROFILE=de3 fractal delete --type CLAS --name ZCL_SAMPLE",
            "fractal --profile de3 delete --type CLAS --name ZCL_SAMPLE",
            "echo hi; fractal delete --type CLAS --name ZCL_SAMPLE",
        ] {
            assert!(
                matches!(decide(command, false), HookDecision::Deny(_)),
                "{command}"
            );
        }
    }

    #[test]
    fn does_not_match_a_different_program_or_a_longer_verb() {
        // `notfractal` is not us, and a rule for `delete` must not catch a verb
        // that merely starts with it.
        assert_eq!(
            decide("notfractal delete --type CLAS", false),
            HookDecision::NoOpinion
        );
        assert_eq!(
            decide("fractal deletex --type CLAS", false),
            HookDecision::NoOpinion
        );
        // A word that contains the verb elsewhere is not an invocation of it.
        assert_eq!(
            decide("fractal object search 'delete'", false),
            HookDecision::NoOpinion
        );
        // A flag's *value* must not be mistaken for the subcommand either way:
        // this one is a search, not a delete.
        assert_eq!(
            decide("fractal object search 'Z*' --kind delete", false),
            HookDecision::NoOpinion
        );
    }

    #[test]
    fn global_flags_before_the_subcommand_do_not_hide_it() {
        for command in [
            "fractal --profile de3 delete --name X",
            "fractal --output json delete --name X",
            "fractal --profile=de3 delete --name X",
            "fractal --profile de3 --output json delete --name X",
        ] {
            assert!(
                matches!(decide(command, false), HookDecision::Deny(_)),
                "{command}"
            );
        }
    }

    #[test]
    fn renders_the_documented_decision_shape() {
        let rendered = render_decision(&decide("fractal delete --name X", false)).expect("denies");
        let value: Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(
            value["hookSpecificOutput"]["permissionDecision"],
            serde_json::json!("deny")
        );
        assert_eq!(
            value["hookSpecificOutput"]["hookEventName"],
            serde_json::json!("PreToolUse")
        );
        assert!(
            value["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("cannot be undone")
        );
    }

    #[test]
    fn no_opinion_prints_nothing_at_all() {
        assert_eq!(render_decision(&HookDecision::NoOpinion), None);
    }

    #[test]
    fn reads_the_command_out_of_a_bash_payload() {
        assert_eq!(
            command_from_payload(&payload("fractal delete --name X")).as_deref(),
            Some("fractal delete --name X")
        );
    }

    #[test]
    fn ignores_tool_calls_that_are_not_shell_commands() {
        let edit = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/x", "old_string": "a", "new_string": "b" }
        })
        .to_string();
        assert_eq!(command_from_payload(&edit), None);
    }

    #[test]
    fn a_payload_this_hook_cannot_read_yields_no_opinion() {
        // A hook that failed loudly here would break every tool call in the
        // session, which is far worse than missing one command.
        assert_eq!(command_from_payload("not json"), None);
        assert_eq!(command_from_payload("{}"), None);
        assert_eq!(
            command_from_payload(r#"{"tool_name":"Bash","tool_input":{}}"#),
            None
        );
    }
}
