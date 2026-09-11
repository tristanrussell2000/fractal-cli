use std::fmt::Write as _;

use serde::Serialize;

use super::{connect, edit_object_identity::EditObjectIdentityOutput};
use crate::{
    cli::EditActivateArgs,
    output::{OutputFormat, print_result},
    reported::Reported,
};
use fractal::journal::recorder::Journal;
use fractal::sap::{
    activation_request::AdtActivationMessage,
    metadata_activation::{
        MetadataObjectActivationRequest, MetadataObjectActivationResult, activate_metadata_object,
    },
    object_family::AdtObjectFamily,
    source_activation::{
        AdtSourceActivationRequest, AdtSourceActivationResult, activate_adt_source,
    },
    source_check::AdtSourceCheckMessage,
};
use fractal::source_change::source_sha256;

#[derive(Debug, Serialize)]
pub struct EditActivationDiagnosticOutput {
    severity: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_description: Option<String>,
}

/// One activation, of either object family.
///
/// The fields only a source activation can report are grouped into
/// `source_details` and omitted entirely for a metadata object, rather than
/// being filled with plausible-looking defaults: a caller must never read
/// `precheck_errors: 0` and conclude a check passed when none was possible.
#[derive(Debug, Serialize)]
pub struct EditActivationOutput {
    ok: bool,
    profile: String,
    status: String,
    activated: bool,
    verified: bool,
    #[serde(flatten)]
    object: EditObjectIdentityOutput,
    transport: Option<String>,
    active_sha256_after: String,
    active_bytes_after: usize,
    /// The activation landed and this journal entry could not be completed, so
    /// it is stuck at `pending`. The object is activated either way; the entry
    /// still holds the previous version and `undo` will take it with `--force`.
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_entry_incomplete: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    source_details: Option<SourceActivationDetails>,
    sap_reported_activation_executed: Option<bool>,
    activation_response_parsed: bool,
    activation_messages: Vec<EditActivationDiagnosticOutput>,
}

/// What only a source activation can report: a metadata object has no source
/// to pre-check and no inactive snapshot to compare against.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Serialize)]
pub struct SourceActivationDetails {
    precheck_clean: bool,
    precheck_errors: usize,
    precheck_warnings: usize,
    precheck_infos: usize,
    precheck_messages: Vec<EditActivationDiagnosticOutput>,
    inactive_sha256_before: String,
    inactive_bytes_before: usize,
    active_matches_inactive: bool,
    inactive_version_exists_after: bool,
}

pub async fn edit_object_activate(
    explicit_profile: Option<&str>,
    args: &EditActivateArgs,
) -> Result<EditActivationOutput, Reported> {
    let object_type = AdtObjectFamily::parse(&args.object_type)?;
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    let policy = profile.edit_policy();

    let journal = if args.no_journal {
        None
    } else {
        Some(Journal::open(&profile_name, &profile)?)
    };

    match object_type {
        AdtObjectFamily::Source(object_type) => {
            let request = AdtSourceActivationRequest {
                object_type,
                name: args.name.clone(),
                transport: args.transport.clone(),
            };
            let result =
                activate_adt_source(&mut client, &policy, &request, journal.as_ref()).await?;
            Ok(map_source_activation_result(profile_name, result))
        }
        AdtObjectFamily::Metadata(object_type) => {
            let request = MetadataObjectActivationRequest {
                object_type,
                name: args.name.clone(),
                transport: args.transport.clone(),
            };
            let result =
                activate_metadata_object(&mut client, &policy, &request, journal.as_ref()).await?;
            Ok(map_metadata_activation_result(profile_name, result))
        }
    }
}

pub fn print_edit_object_activate(result: &EditActivationOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    print!("{}", render_activation_readable(result));
}

fn map_source_activation_result(
    profile: String,
    result: AdtSourceActivationResult,
) -> EditActivationOutput {
    EditActivationOutput {
        ok: true,
        profile,
        status: "activated_verified".to_owned(),
        activated: true,
        verified: true,
        object: result.identity.into(),
        transport: result.transport,
        active_sha256_after: result.active.sha256,
        active_bytes_after: result.active.bytes,
        journal_entry_incomplete: result.journal_entry_incomplete,
        source_details: Some(SourceActivationDetails {
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
            inactive_sha256_before: result.inactive.sha256,
            inactive_bytes_before: result.inactive.bytes,
            active_matches_inactive: true,
            inactive_version_exists_after: false,
        }),
        sap_reported_activation_executed: result.sap_reported_activation_executed,
        activation_response_parsed: result.activation_response_parsed,
        activation_messages: result
            .activation_messages
            .into_iter()
            .map(map_activation_message)
            .collect(),
    }
}

fn map_metadata_activation_result(
    profile: String,
    result: MetadataObjectActivationResult,
) -> EditActivationOutput {
    EditActivationOutput {
        ok: true,
        profile,
        status: "activated_verified".to_owned(),
        activated: true,
        verified: true,
        object: result.identity.into(),
        transport: result.transport,
        active_sha256_after: source_sha256(&result.active_xml),
        active_bytes_after: result.active_xml.len(),
        journal_entry_incomplete: result.journal_entry_incomplete,
        // No source to pre-check and no inactive snapshot to compare against.
        source_details: None,
        sap_reported_activation_executed: result.sap_reported_activation_executed,
        activation_response_parsed: result.activation_response_parsed,
        activation_messages: result
            .activation_messages
            .into_iter()
            .map(map_activation_message)
            .collect(),
    }
}

fn map_precheck_message(message: AdtSourceCheckMessage) -> EditActivationDiagnosticOutput {
    EditActivationDiagnosticOutput {
        severity: message.severity.as_str().to_owned(),
        text: message.text,
        line: message.line,
        object_description: None,
    }
}

fn map_activation_message(message: AdtActivationMessage) -> EditActivationDiagnosticOutput {
    EditActivationDiagnosticOutput {
        severity: message.severity.as_str().to_owned(),
        text: message.text,
        line: message.line,
        object_description: message.object_description,
    }
}

fn render_activation_readable(result: &EditActivationOutput) -> String {
    let mut output = String::new();
    if let Some(entry) = &result.journal_entry_incomplete {
        let _ = writeln!(
            output,
            "warning: the object is activated, but journal entry {entry} could not be completed"
        );
    }
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
    if let Some(details) = &result.source_details {
        let _ = writeln!(
            output,
            "precheck: {} error(s), {} warning(s), {} info message(s)",
            details.precheck_errors, details.precheck_warnings, details.precheck_infos
        );
        let _ = writeln!(
            output,
            "inactive SHA-256 before: {}",
            details.inactive_sha256_before
        );
    }
    let _ = writeln!(
        output,
        "active SHA-256 after: {}",
        result.active_sha256_after
    );
    if result.source_details.is_some() {
        let _ = writeln!(output, "active source matches inactive source: true");
        let _ = writeln!(output, "inactive version exists after: false");
    }
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
            "note: SAP reported activationExecuted=false, but the object was read back as active.\n",
        );
    }
    if let Some(details) = &result.source_details
        && !details.precheck_messages.is_empty()
    {
        output.push_str("precheck messages:\n");
        render_diagnostics(&mut output, &details.precheck_messages);
    }
    if !result.activation_messages.is_empty() {
        output.push_str("activation messages:\n");
        render_diagnostics(&mut output, &result.activation_messages);
    }
    output
}

fn render_diagnostics(output: &mut String, messages: &[EditActivationDiagnosticOutput]) {
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
        activation_request::AdtActivationMessage,
        adt_message_severity::AdtMessageSeverity,
        editable_source::{
            AdtSourceSnapshot, AdtSourceVersion, EditableAdtObjectType, EditableAdtSourceIdentity,
        },
        source_activation::AdtSourceActivationResult,
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
            "AB1K900575",
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
        assert_eq!(args.transport.as_deref(), Some("AB1K900575"));
    }

    #[test]
    fn maps_and_renders_verified_activation() {
        let output = map_source_activation_result(
            "development".to_owned(),
            AdtSourceActivationResult {
                journal_entry_incomplete: None,
                identity: EditableAdtSourceIdentity {
                    object_type: EditableAdtObjectType::Class,
                    name: "ZCL_SAMPLE".to_owned(),
                    object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                    source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
                },
                transport: Some("AB1K900575".to_owned()),
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
                inactive: AdtSourceSnapshot::from_parts(
                    "CLASS zcl_sample DEFINITION.\nENDCLASS.\n".to_owned(),
                    "same".to_owned(),
                    100,
                ),
                active: AdtSourceSnapshot::from_parts(
                    "CLASS zcl_sample DEFINITION.\nENDCLASS.\n".to_owned(),
                    "same".to_owned(),
                    100,
                ),
                sap_reported_activation_executed: Some(true),
                activation_response_parsed: true,
                activation_messages: vec![AdtActivationMessage {
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
        let readable = render_activation_readable(&output);
        assert!(readable.contains("status: activated and verified"));
        assert!(readable.contains("warning line 8: Obsolete statement"));
        assert!(readable.contains("info Class ZCL_SAMPLE: Activation completed"));
    }
}
