use std::fmt::Write as _;

use serde::Serialize;

use crate::{
    cli::{EditSourceReadArgs, EditSourceVersionArg},
    command_error::CommandError,
    commands::connect,
    output::{OutputFormat, print_result},
};
use fractal::sap::editable_source::{
    AdtSourceReadResult, AdtSourceVersion, EditableAdtObjectType, read_adt_source_for_edit,
};

#[derive(Debug, Serialize)]
pub struct EditSourceReadOutput {
    ok: bool,
    profile: String,
    object_type: String,
    name: String,
    object_uri: String,
    source_uri: String,
    requested_version: String,
    bytes: usize,
    sha256: String,
    source: String,
}

pub async fn edit_source_read(
    explicit_profile: Option<&str>,
    args: &EditSourceReadArgs,
) -> Result<EditSourceReadOutput, CommandError> {
    let object_type = EditableAdtObjectType::parse(&args.object_type)?;
    let version = map_source_version(args.version);
    let (profile_name, _profile, client) = connect(explicit_profile).await?;
    let result = read_adt_source_for_edit(&client, object_type, &args.name, version).await?;

    Ok(map_edit_source_read_result(profile_name, result))
}

pub fn print_edit_source_read(result: &EditSourceReadOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    print!("{}", render_edit_source_readable(result));
}

pub(super) const fn map_source_version(version: EditSourceVersionArg) -> AdtSourceVersion {
    match version {
        EditSourceVersionArg::Active => AdtSourceVersion::Active,
        EditSourceVersionArg::Inactive => AdtSourceVersion::Inactive,
    }
}

fn map_edit_source_read_result(
    profile: String,
    result: AdtSourceReadResult,
) -> EditSourceReadOutput {
    EditSourceReadOutput {
        ok: true,
        profile,
        object_type: result.object_type.as_str().to_owned(),
        name: result.name,
        object_uri: result.object_uri,
        source_uri: result.source_uri,
        requested_version: result.requested_version.as_str().to_owned(),
        bytes: result.bytes,
        sha256: result.sha256,
        source: result.source,
    }
}

fn render_edit_source_readable(result: &EditSourceReadOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(output, "object: {} {}", result.object_type, result.name);
    let _ = writeln!(output, "requested version: {}", result.requested_version);
    let _ = writeln!(output, "object uri: {}", result.object_uri);
    let _ = writeln!(output, "source uri: {}", result.source_uri);
    let _ = writeln!(output, "bytes: {}", result.bytes);
    let _ = writeln!(output, "sha256: {}", result.sha256);
    output.push_str("\nsource:\n");
    output.push_str(&result.source);
    output
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, EditCommand};

    fn read_args(cli: Cli) -> EditSourceReadArgs {
        let Command::Edit {
            command: EditCommand::Read(args),
        } = cli.command
        else {
            panic!("expected edit read command");
        };
        args
    }

    #[test]
    fn parses_edit_read_with_active_as_the_default_version() {
        let args = read_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "read",
                "--type",
                "clas",
                "--name",
                "ZCL_EXAMPLE",
            ])
            .unwrap(),
        );

        assert_eq!(args.object_type, "clas");
        assert_eq!(args.name, "ZCL_EXAMPLE");
        assert_eq!(args.version, EditSourceVersionArg::Active);
    }

    #[test]
    fn parses_an_explicit_inactive_version() {
        let args = read_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "read",
                "--type",
                "DDLS",
                "--name",
                "/ACME/EXAMPLE",
                "--version",
                "inactive",
            ])
            .unwrap(),
        );

        assert_eq!(args.version, EditSourceVersionArg::Inactive);
    }

    #[test]
    fn rejects_an_unknown_source_version_during_cli_parsing() {
        assert!(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "read",
                "--type",
                "CLAS",
                "--name",
                "ZCL_EXAMPLE",
                "--version",
                "latest",
            ])
            .is_err()
        );
    }

    #[test]
    fn maps_json_fields_and_preserves_source_in_readable_output() {
        let source = "CLASS zcl_example DEFINITION.\r\nENDCLASS.";
        let result = map_edit_source_read_result(
            "development".to_owned(),
            AdtSourceReadResult {
                object_type: EditableAdtObjectType::Class,
                name: "ZCL_EXAMPLE".to_owned(),
                object_uri: "/sap/bc/adt/oo/classes/zcl_example".to_owned(),
                source_uri: "/sap/bc/adt/oo/classes/zcl_example/source/main".to_owned(),
                requested_version: AdtSourceVersion::Active,
                source: source.to_owned(),
                sha256: "a".repeat(64),
                bytes: source.len(),
            },
        );

        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["object_type"], "CLAS");
        assert_eq!(json["requested_version"], "active");
        assert_eq!(json["bytes"], source.len());
        assert_eq!(json["sha256"], "a".repeat(64));
        assert_eq!(json["source"], source);

        let rendered = render_edit_source_readable(&result);
        assert!(rendered.contains("object: CLAS ZCL_EXAMPLE"));
        assert!(rendered.contains("requested version: active"));
        assert!(rendered.contains("sha256: "));
        assert!(rendered.ends_with(source));
    }
}
