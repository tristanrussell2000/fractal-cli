use std::{fmt::Write as _, io::Read};

use serde::Serialize;

use crate::{
    cli::EditSourceSetArgs,
    command_error::CommandError,
    commands::connect,
    output::{OutputFormat, print_result},
};
use fractal::sap::{
    editable_source::EditableAdtObjectType,
    source_replace::{
        AdtSourceReplacementPreview, AdtSourceReplacementRequest, AdtSourceReplacementWriteResult,
        preview_adt_source_replacement, replace_adt_source_atomically,
    },
};

#[derive(Debug, Serialize)]
pub struct EditSourceSetOutput {
    ok: bool,
    profile: String,
    status: String,
    source_input: String,
    dry_run: bool,
    wrote_source: bool,
    activated: bool,
    object_type: String,
    name: String,
    object_uri: String,
    source_uri: String,
    transport: Option<String>,
    original_bytes: usize,
    replacement_bytes: usize,
    stored_bytes: Option<usize>,
    original_sha256: String,
    replacement_sha256: String,
    stored_sha256: Option<String>,
    sap_normalized_source: Option<bool>,
    replacement_source: String,
    stored_source: Option<String>,
}

pub async fn edit_source_set(
    explicit_profile: Option<&str>,
    args: &EditSourceSetArgs,
) -> Result<EditSourceSetOutput, CommandError> {
    let object_type = EditableAdtObjectType::parse(&args.object_type)?;
    let replacement_source = {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        resolve_replacement_source(&args.source_file, &mut stdin)?
    };
    let request = AdtSourceReplacementRequest {
        object_type,
        name: args.name.clone(),
        replacement_source,
        expected_sha256: args.expected_sha256.clone(),
        transport: args.transport.clone(),
    };
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;

    if args.dry_run {
        let preview =
            preview_adt_source_replacement(&client, &profile.customer_namespaces, &request).await?;
        return Ok(map_set_preview(
            profile_name,
            args.source_file.clone(),
            preview,
        ));
    }

    let result =
        replace_adt_source_atomically(&mut client, &profile.customer_namespaces, &request).await?;
    Ok(map_applied_set(
        profile_name,
        args.source_file.clone(),
        result,
    ))
}

pub fn print_edit_source_set(result: &EditSourceSetOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    print!("{}", render_edit_source_set_readable(result));
}

fn resolve_replacement_source<R: Read>(
    source_file: &str,
    stdin: &mut R,
) -> Result<String, CommandError> {
    if source_file == "-" {
        let mut source = String::new();
        stdin.read_to_string(&mut source).map_err(|error| {
            CommandError::with_hint(
                "source_stdin_read_error",
                format!("could not read complete replacement source from stdin: {error}"),
                "Pipe complete UTF-8 source into the command, or pass --source-file <path>.",
            )
        })?;
        return Ok(source);
    }

    std::fs::read_to_string(source_file).map_err(|error| {
        CommandError::with_hint(
            "source_file_read_error",
            format!("could not read complete replacement source file '{source_file}': {error}"),
            "Pass the path to a readable UTF-8 source file, or use --source-file - for stdin.",
        )
    })
}

fn map_set_preview(
    profile: String,
    source_input: String,
    preview: AdtSourceReplacementPreview,
) -> EditSourceSetOutput {
    EditSourceSetOutput {
        ok: true,
        profile,
        status: "dry_run".to_owned(),
        source_input,
        dry_run: true,
        wrote_source: false,
        activated: false,
        object_type: preview.object_type.as_str().to_owned(),
        name: preview.name,
        object_uri: preview.object_uri,
        source_uri: preview.source_uri,
        transport: preview.transport,
        original_bytes: preview.original_bytes,
        replacement_bytes: preview.replacement_bytes,
        stored_bytes: None,
        original_sha256: preview.original_sha256,
        replacement_sha256: preview.replacement_sha256,
        stored_sha256: None,
        sap_normalized_source: None,
        replacement_source: preview.replacement_source,
        stored_source: None,
    }
}

fn map_applied_set(
    profile: String,
    source_input: String,
    result: AdtSourceReplacementWriteResult,
) -> EditSourceSetOutput {
    EditSourceSetOutput {
        ok: true,
        profile,
        status: "stored_inactive".to_owned(),
        source_input,
        dry_run: false,
        wrote_source: true,
        activated: false,
        object_type: result.object_type.as_str().to_owned(),
        name: result.name,
        object_uri: result.object_uri,
        source_uri: result.source_uri,
        transport: result.transport,
        original_bytes: result.original_bytes,
        replacement_bytes: result.replacement_bytes,
        stored_bytes: Some(result.stored_bytes),
        original_sha256: result.original_sha256,
        replacement_sha256: result.replacement_sha256,
        stored_sha256: Some(result.stored_sha256),
        sap_normalized_source: Some(result.sap_normalized_source),
        replacement_source: result.replacement_source,
        stored_source: Some(result.stored_source),
    }
}

fn render_edit_source_set_readable(result: &EditSourceSetOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(output, "object: {} {}", result.object_type, result.name);
    let _ = writeln!(output, "source input: {}", result.source_input);
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
        let _ = writeln!(
            output,
            "status: complete source stored as inactive; not activated"
        );
    }
    let _ = writeln!(output, "original bytes: {}", result.original_bytes);
    let _ = writeln!(output, "replacement bytes: {}", result.replacement_bytes);
    if let Some(stored_bytes) = result.stored_bytes {
        let _ = writeln!(output, "stored bytes: {stored_bytes}");
    }
    let _ = writeln!(output, "original sha256: {}", result.original_sha256);
    let _ = writeln!(output, "replacement sha256: {}", result.replacement_sha256);
    if let Some(stored_sha256) = &result.stored_sha256 {
        let _ = writeln!(output, "stored sha256: {stored_sha256}");
    }
    if let Some(normalized) = result.sap_normalized_source {
        let _ = writeln!(
            output,
            "SAP normalized submitted source: {}",
            if normalized { "yes" } else { "no" }
        );
    }
    if result.dry_run {
        output.push_str(
            "note: a real set re-reads and validates source while holding an ADT lock; it still does not activate.\n",
        );
    }
    output
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use clap::{CommandFactory, Parser};
    use fractal::{sap::editable_source::EditableAdtObjectType, source_change::source_sha256};

    use super::*;
    use crate::cli::{Cli, Command, EditCommand};

    fn set_args(cli: Cli) -> EditSourceSetArgs {
        let Command::Edit {
            command: EditCommand::Set(args),
        } = cli.command
        else {
            panic!("expected edit set command");
        };
        args
    }

    #[test]
    fn parses_file_hash_transport_and_dry_run_arguments() {
        let args = set_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "set",
                "--type",
                "prog",
                "--name",
                "ZSAMPLE",
                "--source-file",
                "replacement.abap",
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
        assert_eq!(args.source_file, "replacement.abap");
        assert_eq!(args.expected_sha256, Some("a".repeat(64)));
        assert_eq!(args.transport.as_deref(), Some("de3k900575"));
        assert!(args.dry_run);
    }

    #[test]
    fn parses_dash_and_reads_complete_source_from_stdin() {
        let args = set_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "set",
                "--type",
                "PROG",
                "--name",
                "ZSAMPLE",
                "--source-file",
                "-",
            ])
            .unwrap(),
        );
        let source =
            resolve_replacement_source(&args.source_file, &mut Cursor::new("REPORT zsample.\n"))
                .unwrap();

        assert_eq!(source, "REPORT zsample.\n");
    }

    #[test]
    fn missing_source_file_has_a_specific_error_and_hint() {
        let error = resolve_replacement_source(
            "/path/that/does/not/exist/fractal-source.abap",
            &mut Cursor::new(Vec::<u8>::new()),
        )
        .unwrap_err();

        assert_eq!(error.code(), "source_file_read_error");
        assert!(error.message().contains("fractal-source.abap"));
        assert!(error.hint().unwrap().contains("--source-file -"));
    }

    #[test]
    fn help_explains_stdin_dry_run_and_no_activation() {
        let mut command = Cli::command();
        let edit = command
            .find_subcommand_mut("edit")
            .expect("edit subcommand exists");
        let set = edit
            .find_subcommand_mut("set")
            .expect("set subcommand exists");
        let help = set.render_long_help().to_string();

        assert!(help.contains("Does not activate"));
        assert!(help.contains("or - for stdin"));
        assert!(help.contains("--dry-run"));
    }

    #[test]
    fn maps_preview_and_applied_results_without_claiming_activation() {
        let original = "REPORT zsample.\nWRITE 'before'.\n";
        let replacement = "REPORT zsample.\nWRITE 'after'.\n";
        let preview = map_set_preview(
            "development".to_owned(),
            "-".to_owned(),
            AdtSourceReplacementPreview {
                object_type: EditableAdtObjectType::Program,
                name: "ZSAMPLE".to_owned(),
                object_uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
                source_uri: "/sap/bc/adt/programs/programs/zsample/source/main".to_owned(),
                transport: Some("DE3K900575".to_owned()),
                original_sha256: source_sha256(original),
                replacement_sha256: source_sha256(replacement),
                original_bytes: original.len(),
                replacement_bytes: replacement.len(),
                replacement_source: replacement.to_owned(),
            },
        );
        let preview_json = serde_json::to_value(&preview).unwrap();
        assert_eq!(preview_json["status"], "dry_run");
        assert_eq!(preview_json["wrote_source"], false);
        assert_eq!(preview_json["activated"], false);
        assert_eq!(preview_json["stored_source"], serde_json::Value::Null);

        let stored = "REPORT zsample.\r\nWRITE 'after'.\r\n";
        let applied = map_applied_set(
            "development".to_owned(),
            "replacement.abap".to_owned(),
            AdtSourceReplacementWriteResult {
                object_type: EditableAdtObjectType::Program,
                name: "ZSAMPLE".to_owned(),
                object_uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
                source_uri: "/sap/bc/adt/programs/programs/zsample/source/main".to_owned(),
                transport: Some("DE3K900575".to_owned()),
                original_sha256: source_sha256(original),
                replacement_sha256: source_sha256(replacement),
                stored_sha256: source_sha256(stored),
                original_bytes: original.len(),
                replacement_bytes: replacement.len(),
                stored_bytes: stored.len(),
                replacement_source: replacement.to_owned(),
                stored_source: stored.to_owned(),
                sap_normalized_source: true,
            },
        );
        let applied_json = serde_json::to_value(&applied).unwrap();
        assert_eq!(applied_json["status"], "stored_inactive");
        assert_eq!(applied_json["wrote_source"], true);
        assert_eq!(applied_json["activated"], false);
        assert_eq!(applied_json["sap_normalized_source"], true);
        assert!(
            render_edit_source_set_readable(&applied)
                .contains("complete source stored as inactive; not activated")
        );
    }
}
