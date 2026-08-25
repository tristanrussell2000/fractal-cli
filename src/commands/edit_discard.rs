use std::fmt::Write as _;

use serde::Serialize;

use super::connect;
use crate::{
    cli::EditSourceDiscardArgs,
    command_error::CommandError,
    output::{OutputFormat, print_result},
};
use fractal::sap::{
    editable_source::EditableAdtObjectType,
    source_discard::{
        AdtInactiveSourceDiscardRequest, AdtInactiveSourceDiscardResult,
        discard_inactive_adt_source,
    },
};

#[derive(Debug, Serialize)]
pub struct EditSourceDiscardOutput {
    ok: bool,
    profile: String,
    status: String,
    implementation: String,
    discarded: bool,
    verified: bool,
    object_type: String,
    name: String,
    object_uri: String,
    source_uri: String,
    transport: Option<String>,
    discarded_sha256: String,
    discarded_bytes: usize,
    active_sha256_before: String,
    active_bytes_before: usize,
    restored_inactive_sha256: String,
    restored_inactive_bytes: usize,
    active_sha256_after: String,
    active_bytes_after: usize,
    active_source_unchanged: bool,
    inactive_version_exists_after: bool,
    activation_response_parsed: bool,
    sap_reported_activation_executed: Option<bool>,
}

pub async fn edit_source_discard(
    explicit_profile: Option<&str>,
    args: &EditSourceDiscardArgs,
) -> Result<EditSourceDiscardOutput, CommandError> {
    let object_type = EditableAdtObjectType::parse(&args.object_type)?;
    let request = AdtInactiveSourceDiscardRequest {
        object_type,
        name: args.name.clone(),
        transport: args.transport.clone(),
    };
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    let result =
        discard_inactive_adt_source(&mut client, &profile.customer_namespaces, &request).await?;
    Ok(map_source_discard_result(profile_name, result))
}

pub fn print_edit_source_discard(result: &EditSourceDiscardOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    print!("{}", render_source_discard_readable(result));
}

fn map_source_discard_result(
    profile: String,
    result: AdtInactiveSourceDiscardResult,
) -> EditSourceDiscardOutput {
    EditSourceDiscardOutput {
        ok: true,
        profile,
        status: "inactive_source_discarded_verified".to_owned(),
        implementation: "restore_active_then_activate".to_owned(),
        discarded: true,
        verified: true,
        object_type: result.object_type.as_str().to_owned(),
        name: result.name,
        object_uri: result.object_uri,
        source_uri: result.source_uri,
        transport: result.transport,
        discarded_sha256: result.discarded_sha256,
        discarded_bytes: result.discarded_bytes,
        active_sha256_before: result.active_sha256_before,
        active_bytes_before: result.active_bytes_before,
        restored_inactive_sha256: result.restored_inactive_sha256,
        restored_inactive_bytes: result.restored_inactive_bytes,
        active_sha256_after: result.active_sha256_after,
        active_bytes_after: result.active_bytes_after,
        active_source_unchanged: true,
        inactive_version_exists_after: false,
        activation_response_parsed: result.activation_response_parsed,
        sap_reported_activation_executed: result.sap_reported_activation_executed,
    }
}

fn render_source_discard_readable(result: &EditSourceDiscardOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(output, "object: {} {}", result.object_type, result.name);
    let _ = writeln!(output, "status: inactive source discarded and verified");
    let _ = writeln!(
        output,
        "implementation: restore active source, then activate"
    );
    if let Some(transport) = &result.transport {
        let _ = writeln!(output, "transport: {transport}");
    }
    let _ = writeln!(
        output,
        "discarded inactive SHA-256: {}",
        result.discarded_sha256
    );
    let _ = writeln!(
        output,
        "discarded inactive bytes: {}",
        result.discarded_bytes
    );
    let _ = writeln!(
        output,
        "active SHA-256 before: {}",
        result.active_sha256_before
    );
    let _ = writeln!(
        output,
        "restored inactive SHA-256: {}",
        result.restored_inactive_sha256
    );
    let _ = writeln!(
        output,
        "active SHA-256 after: {}",
        result.active_sha256_after
    );
    let _ = writeln!(output, "active source unchanged: true");
    let _ = writeln!(output, "inactive version exists after: false");
    output
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, EditCommand};

    #[test]
    fn parses_discard_arguments() {
        let cli = Cli::try_parse_from([
            "fractal",
            "edit",
            "discard",
            "--type",
            "clas",
            "--name",
            "zcl_sample",
            "--transport",
            "DE3K900575",
        ])
        .unwrap();
        let Command::Edit {
            command: EditCommand::Discard(args),
        } = cli.command
        else {
            panic!("expected edit discard command");
        };

        assert_eq!(args.object_type, "clas");
        assert_eq!(args.name, "zcl_sample");
        assert_eq!(args.transport.as_deref(), Some("DE3K900575"));
    }

    #[test]
    fn maps_and_renders_a_verified_discard() {
        let result = map_source_discard_result(
            "DE3".to_owned(),
            AdtInactiveSourceDiscardResult {
                object_type: EditableAdtObjectType::Class,
                name: "ZCL_SAMPLE".to_owned(),
                object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
                transport: Some("DE3K900575".to_owned()),
                discarded_sha256: "inactive-hash".to_owned(),
                discarded_bytes: 45,
                active_sha256_before: "active-hash".to_owned(),
                active_bytes_before: 40,
                restored_inactive_sha256: "active-hash".to_owned(),
                restored_inactive_bytes: 40,
                active_sha256_after: "active-hash".to_owned(),
                active_bytes_after: 40,
                activation_response_parsed: true,
                sap_reported_activation_executed: Some(true),
            },
        );

        assert_eq!(result.status, "inactive_source_discarded_verified");
        assert_eq!(result.implementation, "restore_active_then_activate");
        assert!(result.discarded);
        assert!(result.verified);
        assert!(result.active_source_unchanged);
        assert!(!result.inactive_version_exists_after);
        let readable = render_source_discard_readable(&result);
        assert!(readable.contains("inactive source discarded and verified"));
        assert!(readable.contains("restore active source, then activate"));
        assert!(readable.contains("active source unchanged: true"));
        assert!(readable.contains("inactive version exists after: false"));
    }
}
