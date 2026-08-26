use std::fmt::Write as _;

use serde::Serialize;

use super::{connect, edit_object_identity::EditObjectIdentityOutput};
use crate::{
    cli::EditSourceActivateArgs,
    command_error::CommandError,
    output::{OutputFormat, print_result},
};
use fractal::sap::{
    editable_source::EditableAdtObjectType,
    source_activation::{
        AdtSourceActivationMessage, AdtSourceActivationRequest, AdtSourceActivationResult,
        activate_adt_source,
    },
    source_check::AdtSourceCheckMessage,
};

#[derive(Debug, Serialize)]
pub struct EditSourceActivationDiagnosticOutput {
    severity: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_description: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EditSourceActivationOutput {
    ok: bool,
    profile: String,
    status: String,
    activated: bool,
    verified: bool,
    #[serde(flatten)]
    object: EditObjectIdentityOutput,
    transport: Option<String>,
    precheck_clean: bool,
    precheck_errors: usize,
    precheck_warnings: usize,
    precheck_infos: usize,
    precheck_messages: Vec<EditSourceActivationDiagnosticOutput>,
    inactive_sha256_before: String,
    inactive_bytes_before: usize,
    active_sha256_after: String,
    active_bytes_after: usize,
    active_matches_inactive: bool,
    inactive_version_exists_after: bool,
    sap_reported_activation_executed: Option<bool>,
    activation_response_parsed: bool,
    activation_messages: Vec<EditSourceActivationDiagnosticOutput>,
}

pub async fn edit_source_activate(
    explicit_profile: Option<&str>,
    args: &EditSourceActivateArgs,
) -> Result<EditSourceActivationOutput, CommandError> {
    let object_type = EditableAdtObjectType::parse(&args.object_type)?;
    let request = AdtSourceActivationRequest {
        object_type,
        name: args.name.clone(),
        transport: args.transport.clone(),
    };
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    let result = activate_adt_source(&mut client, &profile.customer_namespaces, &request).await?;
    Ok(map_source_activation_result(profile_name, result))
}

pub fn print_edit_source_activate(result: &EditSourceActivationOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    print!("{}", render_source_activation_readable(result));
}

fn map_source_activation_result(
    profile: String,
    result: AdtSourceActivationResult,
) -> EditSourceActivationOutput {
    EditSourceActivationOutput {
        ok: true,
        profile,
        status: "activated_verified".to_owned(),
        activated: true,
        verified: true,
        object: result.identity.into(),
        transport: result.transport,
        precheck_clean: result.precheck.clean,
        precheck_errors: result.precheck.errors,
        precheck_warnings: result.precheck.warnings,
        precheck_infos: result.precheck.infos,
        precheck_messages: result
            .precheck
            .messages
            .into_iter()
            .map(map_precheck_message)
            .collect(),
        inactive_sha256_before: result.inactive_sha256,
        inactive_bytes_before: result.inactive_bytes,
        active_sha256_after: result.active_sha256,
        active_bytes_after: result.active_bytes,
        active_matches_inactive: true,
        inactive_version_exists_after: false,
        sap_reported_activation_executed: result.sap_reported_activation_executed,
        activation_response_parsed: result.activation_response_parsed,
        activation_messages: result
            .activation_messages
            .into_iter()
            .map(map_activation_message)
            .collect(),
    }
}

fn map_precheck_message(message: AdtSourceCheckMessage) -> EditSourceActivationDiagnosticOutput {
    EditSourceActivationDiagnosticOutput {
        severity: message.severity.as_str().to_owned(),
        text: message.text,
        line: message.line,
        object_description: None,
    }
}

fn map_activation_message(
    message: AdtSourceActivationMessage,
) -> EditSourceActivationDiagnosticOutput {
    EditSourceActivationDiagnosticOutput {
        severity: message.severity.as_str().to_owned(),
        text: message.text,
        line: message.line,
        object_description: message.object_description,
    }
}

fn render_source_activation_readable(result: &EditSourceActivationOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(
        output,
        "object: {} {}",
        result.object.object_type, result.object.name
    );
    let _ = writeln!(output, "status: activated and verified");
    if let Some(transport) = &result.transport {
        let _ = writeln!(output, "transport: {transport}");
    }
    let _ = writeln!(
        output,
        "precheck: {} error(s), {} warning(s), {} info message(s)",
        result.precheck_errors, result.precheck_warnings, result.precheck_infos
    );
    let _ = writeln!(
        output,
        "inactive SHA-256 before: {}",
        result.inactive_sha256_before
    );
    let _ = writeln!(
        output,
        "active SHA-256 after: {}",
        result.active_sha256_after
    );
    let _ = writeln!(output, "active source matches inactive source: true");
    let _ = writeln!(output, "inactive version exists after: false");
    let reported = result
        .sap_reported_activation_executed
        .map_or(
            "not reported",
            |executed| if executed { "true" } else { "false" },
        );
    let _ = writeln!(output, "SAP reported activation executed: {reported}");
    let _ = writeln!(
        output,
        "activation response parsed: {}",
        result.activation_response_parsed
    );
    if result.sap_reported_activation_executed == Some(false) {
        output.push_str(
            "note: SAP reported activationExecuted=false, but the inactive version disappeared and the verified active source matches.\n",
        );
    }
    if !result.precheck_messages.is_empty() {
        output.push_str("precheck messages:\n");
        render_diagnostics(&mut output, &result.precheck_messages);
    }
    if !result.activation_messages.is_empty() {
        output.push_str("activation messages:\n");
        render_diagnostics(&mut output, &result.activation_messages);
    }
    output
}

fn render_diagnostics(output: &mut String, messages: &[EditSourceActivationDiagnosticOutput]) {
    for message in messages {
        let location = match (&message.object_description, message.line) {
            (Some(object), Some(line)) => format!(" {object}, line {line}"),
            (Some(object), None) => format!(" {object}"),
            (None, Some(line)) => format!(" line {line}"),
            (None, None) => String::new(),
        };
        let _ = writeln!(output, "- {}{location}: {}", message.severity, message.text);
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, EditCommand};
    use fractal::sap::{
        adt_message_severity::AdtMessageSeverity,
        editable_source::{AdtSourceVersion, EditableAdtObjectType, EditableAdtSourceIdentity},
        source_activation::{AdtSourceActivationMessage, AdtSourceActivationResult},
        source_check::AdtSourceCheckResult,
    };

    #[test]
    fn parses_activation_arguments() {
        let cli = Cli::try_parse_from([
            "fractal",
            "edit",
            "activate",
            "--type",
            "CLAS",
            "--name",
            "ZCL_SAMPLE",
            "--transport",
            "DE3K900575",
        ])
        .unwrap();
        let Command::Edit {
            command: EditCommand::Activate(args),
        } = cli.command
        else {
            panic!("expected edit activate command");
        };

        assert_eq!(args.object_type, "CLAS");
        assert_eq!(args.name, "ZCL_SAMPLE");
        assert_eq!(args.transport.as_deref(), Some("DE3K900575"));
    }

    #[test]
    fn maps_and_renders_verified_activation() {
        let output = map_source_activation_result(
            "development".to_owned(),
            AdtSourceActivationResult {
                identity: EditableAdtSourceIdentity {
                    object_type: EditableAdtObjectType::Class,
                    name: "ZCL_SAMPLE".to_owned(),
                    object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                    source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
                },
                transport: Some("DE3K900575".to_owned()),
                precheck: AdtSourceCheckResult {
                    identity: EditableAdtSourceIdentity {
                        object_type: EditableAdtObjectType::Class,
                        name: "ZCL_SAMPLE".to_owned(),
                        object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                        source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
                    },
                    requested_version: AdtSourceVersion::Inactive,
                    check_executed: true,
                    inactive_version_exists: Some(true),
                    clean: true,
                    errors: 0,
                    warnings: 1,
                    infos: 0,
                    messages: vec![AdtSourceCheckMessage {
                        severity: AdtMessageSeverity::Warning,
                        text: "Obsolete statement".to_owned(),
                        line: Some(8),
                    }],
                },
                inactive_sha256: "same".to_owned(),
                inactive_bytes: 100,
                active_sha256: "same".to_owned(),
                active_bytes: 100,
                sap_reported_activation_executed: Some(true),
                activation_response_parsed: true,
                activation_messages: vec![AdtSourceActivationMessage {
                    severity: AdtMessageSeverity::Info,
                    text: "Activation completed".to_owned(),
                    line: None,
                    object_description: Some("Class ZCL_SAMPLE".to_owned()),
                }],
            },
        );

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "activated_verified");
        assert_eq!(json["activated"], true);
        assert_eq!(json["verified"], true);
        assert_eq!(json["precheck_warnings"], 1);
        assert_eq!(json["sap_reported_activation_executed"], true);
        let readable = render_source_activation_readable(&output);
        assert!(readable.contains("status: activated and verified"));
        assert!(readable.contains("warning line 8: Obsolete statement"));
        assert!(readable.contains("info Class ZCL_SAMPLE: Activation completed"));
    }
}
