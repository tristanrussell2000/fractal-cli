use std::fmt::Write as _;

use serde::Serialize;

use super::{connect, edit_read::map_source_version};
use crate::{
    cli::EditSourceCheckArgs,
    command_error::CommandError,
    output::{OutputFormat, print_result},
};
use fractal::sap::{
    editable_source::EditableAdtObjectType,
    source_check::{AdtSourceCheckMessage, AdtSourceCheckResult, check_adt_stored_source},
};

#[derive(Debug, Serialize)]
pub struct EditSourceCheckMessageOutput {
    severity: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct EditSourceCheckOutput {
    ok: bool,
    profile: String,
    object_type: String,
    name: String,
    object_uri: String,
    source_uri: String,
    requested_version: String,
    check_executed: bool,
    inactive_version_exists: Option<bool>,
    clean: bool,
    errors: usize,
    warnings: usize,
    infos: usize,
    messages: Vec<EditSourceCheckMessageOutput>,
}

pub async fn edit_source_check(
    explicit_profile: Option<&str>,
    args: &EditSourceCheckArgs,
) -> Result<EditSourceCheckOutput, CommandError> {
    let object_type = EditableAdtObjectType::parse(&args.object_type)?;
    let version = map_source_version(args.version);
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = check_adt_stored_source(&mut client, object_type, &args.name, version).await?;
    Ok(map_source_check_result(profile_name, result))
}

pub fn print_edit_source_check(result: &EditSourceCheckOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    print!("{}", render_source_check_readable(result));
}

fn map_source_check_result(profile: String, result: AdtSourceCheckResult) -> EditSourceCheckOutput {
    EditSourceCheckOutput {
        ok: true,
        profile,
        object_type: result.object_type.as_str().to_owned(),
        name: result.name,
        object_uri: result.object_uri,
        source_uri: result.source_uri,
        requested_version: result.requested_version.as_str().to_owned(),
        check_executed: result.check_executed,
        inactive_version_exists: result.inactive_version_exists,
        clean: result.clean,
        errors: result.errors,
        warnings: result.warnings,
        infos: result.infos,
        messages: result.messages.into_iter().map(map_check_message).collect(),
    }
}

fn map_check_message(message: AdtSourceCheckMessage) -> EditSourceCheckMessageOutput {
    EditSourceCheckMessageOutput {
        severity: message.severity.as_str().to_owned(),
        text: message.text,
        line: message.line,
    }
}

fn render_source_check_readable(result: &EditSourceCheckOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(output, "object: {} {}", result.object_type, result.name);
    let _ = writeln!(output, "version: {}", result.requested_version);
    let _ = writeln!(output, "check executed: {}", result.check_executed);
    if let Some(exists) = result.inactive_version_exists {
        let _ = writeln!(output, "inactive version exists: {exists}");
    }
    let _ = writeln!(output, "clean: {}", result.clean);
    let _ = writeln!(output, "errors: {}", result.errors);
    let _ = writeln!(output, "warnings: {}", result.warnings);
    let _ = writeln!(output, "infos: {}", result.infos);
    if !result.messages.is_empty() {
        output.push_str("messages:\n");
        for message in &result.messages {
            if let Some(line) = message.line {
                let _ = writeln!(
                    output,
                    "- {} line {}: {}",
                    message.severity, line, message.text
                );
            } else {
                let _ = writeln!(output, "- {}: {}", message.severity, message.text);
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, EditCommand, EditSourceVersionArg};
    use fractal::sap::{
        editable_source::{AdtSourceVersion, EditableAdtObjectType},
        source_check::{AdtSourceCheckMessage, AdtSourceCheckResult, AdtSourceCheckSeverity},
    };

    fn check_args(cli: Cli) -> EditSourceCheckArgs {
        let Command::Edit {
            command: EditCommand::Check(args),
        } = cli.command
        else {
            panic!("expected edit check command");
        };
        args
    }

    #[test]
    fn parses_inactive_as_the_default_check_version() {
        let args = check_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "check",
                "--type",
                "CLAS",
                "--name",
                "ZCL_SAMPLE",
            ])
            .unwrap(),
        );

        assert_eq!(args.version, EditSourceVersionArg::Inactive);
    }

    #[test]
    fn parses_an_explicit_active_check_version() {
        let args = check_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "check",
                "--type",
                "DDLS",
                "--name",
                "ZVIEW",
                "--version",
                "active",
            ])
            .unwrap(),
        );

        assert_eq!(args.version, EditSourceVersionArg::Active);
    }

    #[test]
    fn maps_and_renders_structured_check_messages() {
        let output = map_source_check_result(
            "development".to_owned(),
            AdtSourceCheckResult {
                object_type: EditableAdtObjectType::Class,
                name: "ZCL_SAMPLE".to_owned(),
                object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
                requested_version: AdtSourceVersion::Inactive,
                check_executed: true,
                inactive_version_exists: Some(true),
                clean: false,
                errors: 1,
                warnings: 0,
                infos: 0,
                messages: vec![AdtSourceCheckMessage {
                    severity: AdtSourceCheckSeverity::Error,
                    text: "Statement is not accessible".to_owned(),
                    line: Some(42),
                }],
            },
        );

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["requested_version"], "inactive");
        assert_eq!(json["check_executed"], true);
        assert_eq!(json["clean"], false);
        assert_eq!(json["messages"][0]["severity"], "error");
        assert_eq!(json["messages"][0]["line"], 42);
        let readable = render_source_check_readable(&output);
        assert!(readable.contains("error line 42: Statement is not accessible"));
    }
}
