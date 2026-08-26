use std::fmt::Write as _;

use serde::Serialize;

use super::edit_object_identity::EditObjectIdentityOutput;
use crate::{
    cli::EditSourcePatchArgs,
    command_error::CommandError,
    commands::connect,
    output::{OutputFormat, print_result},
};
use fractal::sap::{
    editable_source::EditableAdtObjectType,
    source_patch::{
        AdtSourcePatchPreview, AdtSourcePatchRequest, AdtSourcePatchWriteResult,
        patch_adt_source_atomically, preview_adt_source_patch,
    },
};

#[derive(Debug, Serialize)]
pub struct EditSourcePatchOutput {
    ok: bool,
    profile: String,
    status: String,
    dry_run: bool,
    wrote_source: bool,
    activated: bool,
    #[serde(flatten)]
    object: EditObjectIdentityOutput,
    transport: Option<String>,
    replacements: usize,
    original_bytes: usize,
    proposed_bytes: usize,
    stored_bytes: Option<usize>,
    original_sha256: String,
    proposed_sha256: String,
    stored_sha256: Option<String>,
    sap_changed_submitted_source: Option<bool>,
    find: String,
    replace: String,
    proposed_source: String,
    stored_source: Option<String>,
}

pub async fn edit_source_patch(
    explicit_profile: Option<&str>,
    args: &EditSourcePatchArgs,
) -> Result<EditSourcePatchOutput, CommandError> {
    let object_type = EditableAdtObjectType::parse(&args.object_type)?;
    let request = AdtSourcePatchRequest {
        object_type,
        name: args.name.clone(),
        find: args.find.clone(),
        replace: args.replace.clone(),
        expected_sha256: args.expected_sha256.clone(),
        transport: args.transport.clone(),
    };
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;

    if args.dry_run {
        let preview =
            preview_adt_source_patch(&client, &profile.customer_namespaces, &request).await?;
        return Ok(map_patch_preview(profile_name, &request, preview));
    }

    let result =
        patch_adt_source_atomically(&mut client, &profile.customer_namespaces, &request).await?;
    Ok(map_applied_patch(profile_name, &request, result))
}

pub fn print_edit_source_patch(result: &EditSourcePatchOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    print!("{}", render_edit_source_patch_readable(result));
}

fn map_patch_preview(
    profile: String,
    request: &AdtSourcePatchRequest,
    preview: AdtSourcePatchPreview,
) -> EditSourcePatchOutput {
    EditSourcePatchOutput {
        ok: true,
        profile,
        status: "dry_run".to_owned(),
        dry_run: true,
        wrote_source: false,
        activated: false,
        object: preview.identity.into(),
        transport: preview.transport,
        replacements: preview.replacements,
        original_bytes: preview.original_bytes,
        proposed_bytes: preview.proposed_bytes,
        stored_bytes: None,
        original_sha256: preview.original_sha256,
        proposed_sha256: preview.proposed_sha256,
        stored_sha256: None,
        sap_changed_submitted_source: None,
        find: request.find.clone(),
        replace: request.replace.clone(),
        proposed_source: preview.proposed_source,
        stored_source: None,
    }
}

fn map_applied_patch(
    profile: String,
    request: &AdtSourcePatchRequest,
    result: AdtSourcePatchWriteResult,
) -> EditSourcePatchOutput {
    let sap_changed_submitted_source = result.proposed_sha256 != result.stored_sha256;
    EditSourcePatchOutput {
        ok: true,
        profile,
        status: "stored_inactive".to_owned(),
        dry_run: false,
        wrote_source: true,
        activated: false,
        object: result.identity.into(),
        transport: result.transport,
        replacements: result.replacements,
        original_bytes: result.original_bytes,
        proposed_bytes: result.proposed_bytes,
        stored_bytes: Some(result.stored_bytes),
        original_sha256: result.original_sha256,
        proposed_sha256: result.proposed_sha256,
        stored_sha256: Some(result.stored_sha256),
        sap_changed_submitted_source: Some(sap_changed_submitted_source),
        find: request.find.clone(),
        replace: request.replace.clone(),
        proposed_source: result.proposed_source,
        stored_source: Some(result.stored_source),
    }
}

fn render_edit_source_patch_readable(result: &EditSourcePatchOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(
        output,
        "object: {} {}",
        result.object.object_type, result.object.name
    );
    let _ = writeln!(
        output,
        "transport: {}",
        result.transport.as_deref().unwrap_or("none")
    );
    if result.dry_run {
        let _ = writeln!(
            output,
            "status: dry run; no lock acquired and no source written"
        );
    } else {
        let _ = writeln!(output, "status: stored as inactive source; not activated");
    }
    let _ = writeln!(output, "replacements: {}", result.replacements);
    let _ = writeln!(output, "original bytes: {}", result.original_bytes);
    let _ = writeln!(output, "proposed bytes: {}", result.proposed_bytes);
    if let Some(stored_bytes) = result.stored_bytes {
        let _ = writeln!(output, "stored bytes: {stored_bytes}");
    }
    let _ = writeln!(output, "original sha256: {}", result.original_sha256);
    let _ = writeln!(output, "proposed sha256: {}", result.proposed_sha256);
    if let Some(stored_sha256) = &result.stored_sha256 {
        let _ = writeln!(output, "stored sha256: {stored_sha256}");
    }
    if let Some(changed) = result.sap_changed_submitted_source {
        let _ = writeln!(
            output,
            "SAP changed submitted source: {}",
            if changed { "yes" } else { "no" }
        );
    }
    let _ = writeln!(
        output,
        "find: {}",
        serde_json::to_string(&result.find).expect("a string is always JSON-serializable")
    );
    let _ = writeln!(
        output,
        "replace: {}",
        serde_json::to_string(&result.replace).expect("a string is always JSON-serializable")
    );
    if result.dry_run {
        output.push_str(
            "note: a real patch re-reads and validates the source again while holding an ADT lock.\n",
        );
    }
    output
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::cli::{Cli, Command, EditCommand};
    use fractal::sap::editable_source::EditableAdtSourceIdentity;
    use fractal::source_change::source_sha256;

    fn patch_args(cli: Cli) -> EditSourcePatchArgs {
        let Command::Edit {
            command: EditCommand::Patch(args),
        } = cli.command
        else {
            panic!("expected edit patch command");
        };
        args
    }

    fn request() -> AdtSourcePatchRequest {
        AdtSourcePatchRequest {
            object_type: EditableAdtObjectType::Program,
            name: "ZSAMPLE".to_owned(),
            find: "WRITE 'before'.".to_owned(),
            replace: "WRITE 'after'.".to_owned(),
            expected_sha256: None,
            transport: None,
        }
    }

    #[test]
    fn parses_patch_arguments_with_an_optional_hash_and_dry_run() {
        let args = patch_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "patch",
                "--type",
                "prog",
                "--name",
                "ZSAMPLE",
                "--find",
                "WRITE 'before'.",
                "--replace",
                "WRITE 'after'.",
                "--expected-sha256",
                &"a".repeat(64),
                "--transport",
                "de3k900575",
                "--dry-run",
            ])
            .unwrap(),
        );

        assert_eq!(args.object_type, "prog");
        assert_eq!(args.name, "ZSAMPLE");
        assert_eq!(args.find, "WRITE 'before'.");
        assert_eq!(args.replace, "WRITE 'after'.");
        assert_eq!(args.expected_sha256, Some("a".repeat(64)));
        assert_eq!(args.transport.as_deref(), Some("de3k900575"));
        assert!(args.dry_run);
    }

    #[test]
    fn patch_defaults_to_a_real_write_without_an_expected_hash() {
        let args = patch_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "patch",
                "--type",
                "PROG",
                "--name",
                "ZSAMPLE",
                "--find",
                "before",
                "--replace",
                "after",
            ])
            .unwrap(),
        );

        assert_eq!(args.expected_sha256, None);
        assert_eq!(args.transport, None);
        assert!(!args.dry_run);
    }

    #[test]
    fn patch_help_explicitly_says_that_saving_does_not_activate() {
        let mut command = Cli::command();
        let edit = command
            .find_subcommand_mut("edit")
            .expect("edit subcommand exists");
        let patch = edit
            .find_subcommand_mut("patch")
            .expect("patch subcommand exists");
        let help = patch.render_long_help().to_string();

        assert!(help.contains("Does not activate"));
    }

    #[test]
    fn maps_a_dry_run_without_claiming_that_source_was_stored() {
        let request = request();
        let original = "REPORT zsample.\nWRITE 'before'.\n";
        let proposed = "REPORT zsample.\nWRITE 'after'.\n";
        let output = map_patch_preview(
            "development".to_owned(),
            &request,
            AdtSourcePatchPreview {
                identity: EditableAdtSourceIdentity {
                    object_type: EditableAdtObjectType::Program,
                    name: "ZSAMPLE".to_owned(),
                    object_uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
                    source_uri: "/sap/bc/adt/programs/programs/zsample/source/main".to_owned(),
                },
                transport: Some("DE3K900575".to_owned()),
                original_sha256: source_sha256(original),
                proposed_sha256: source_sha256(proposed),
                original_bytes: original.len(),
                proposed_bytes: proposed.len(),
                replacements: 1,
                proposed_source: proposed.to_owned(),
            },
        );

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "dry_run");
        assert_eq!(json["wrote_source"], false);
        assert_eq!(json["activated"], false);
        assert_eq!(json["transport"], "DE3K900575");
        assert_eq!(json["stored_sha256"], serde_json::Value::Null);
        assert_eq!(
            json["sap_changed_submitted_source"],
            serde_json::Value::Null
        );
        assert_eq!(json["proposed_source"], proposed);
        assert!(render_edit_source_patch_readable(&output).contains("no source written"));
    }

    #[test]
    fn applied_output_detects_only_that_sap_changed_the_submitted_source() {
        let request = request();
        let original = "REPORT zsample.\nWRITE 'before'.\n";
        let proposed = "REPORT zsample.\nWRITE 'after'.\n";
        let stored = "REPORT zsample.\r\nWRITE 'after'.\r\n";
        let output = map_applied_patch(
            "development".to_owned(),
            &request,
            AdtSourcePatchWriteResult {
                identity: EditableAdtSourceIdentity {
                    object_type: EditableAdtObjectType::Program,
                    name: "ZSAMPLE".to_owned(),
                    object_uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
                    source_uri: "/sap/bc/adt/programs/programs/zsample/source/main".to_owned(),
                },
                transport: Some("DE3K900575".to_owned()),
                original_sha256: source_sha256(original),
                proposed_sha256: source_sha256(proposed),
                stored_sha256: source_sha256(stored),
                original_bytes: original.len(),
                proposed_bytes: proposed.len(),
                stored_bytes: stored.len(),
                replacements: 1,
                proposed_source: proposed.to_owned(),
                stored_source: stored.to_owned(),
            },
        );

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "stored_inactive");
        assert_eq!(json["wrote_source"], true);
        assert_eq!(json["activated"], false);
        assert_eq!(json["transport"], "DE3K900575");
        assert_eq!(json["sap_changed_submitted_source"], true);
        assert_eq!(json["stored_source"], stored);
        let readable = render_edit_source_patch_readable(&output);
        assert!(readable.contains("stored as inactive source; not activated"));
        assert!(readable.contains("SAP changed submitted source: yes"));
    }
}
