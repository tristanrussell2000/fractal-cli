use std::fmt::Write as _;

use serde::Serialize;

use super::{
    ObjectSpecError, connect, edit_object_identity::EditObjectIdentityOutput, parse_object_spec,
};
use crate::{
    cli::EditActivateArgs,
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::journal::recorder::Journal;
use fractal::sap::group_activation::{
    GroupActivationRequest, GroupActivationResult, activate_adt_sources_together,
};
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
    uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_description: Option<String>,
}

/// One activation, of either object family.
///
/// The fields only a source activation can report are grouped into
/// `source_details` and omitted entirely for a metadata object, rather than
/// being filled with plausible-looking defaults: a caller must never read
/// `precheck_errors: 0` and conclude a check passed when none was possible.
// The flags are the JSON contract, as with the other edit outputs: each answers
// a question a caller must be able to ask without parsing prose.
#[allow(clippy::struct_excessive_bools)]
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
    /// For a class: each include this activation published, verified.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    published: Vec<PublishedIncludeOutput>,
    /// For a class: includes other users still have staged, which this
    /// activation left alone. Absent when it could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    staged_by_others: Option<Vec<StagedIncludeOutput>>,
}

#[derive(Debug, Serialize)]
pub struct PublishedIncludeOutput {
    include: &'static str,
    inactive_sha256_before: String,
    active_sha256_after: String,
}

#[derive(Debug, Serialize)]
pub struct StagedIncludeOutput {
    include: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

pub async fn edit_object_activate(
    explicit_profile: Option<&str>,
    args: &EditActivateArgs,
) -> Result<EditActivationOutput, Reported> {
    let (object_type, name) = single_object(args)?;
    let object_type = AdtObjectFamily::parse(&object_type)?;
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
                name: name.clone(),
                transport: args.transport.clone(),
            };
            let result =
                activate_adt_source(&mut client, &policy, &request, journal.as_ref()).await?;
            Ok(map_source_activation_result(profile_name, result))
        }
        AdtObjectFamily::Metadata(object_type) => {
            let request = MetadataObjectActivationRequest {
                object_type,
                name,
                transport: args.transport.clone(),
            };
            let result =
                activate_metadata_object(&mut client, &policy, &request, journal.as_ref()).await?;
            Ok(map_metadata_activation_result(profile_name, result))
        }
    }
}

/// The one object of a single activation: `--type` and `--name`, or a lone
/// `--object`.
fn single_object(args: &EditActivateArgs) -> Result<(String, String), ObjectSpecError> {
    if let [spec] = args.objects.as_slice() {
        return parse_object_spec(spec);
    }
    match (&args.object_type, &args.name) {
        (Some(object_type), Some(name)) => Ok((object_type.clone(), name.clone())),
        // clap requires both unless --object is given.
        _ => Err(ObjectSpecError(String::new())),
    }
}

/// A group of two or more names a type outside the source family.
#[derive(Debug, thiserror::Error)]
#[error("{0} cannot be activated in a group; group activation covers source-based types only")]
pub struct GroupTypeError(String);

impl fractal::reportable_error::ReportableError for GroupTypeError {
    fn code(&self) -> &'static str {
        "edit_activation_group_type_unsupported"
    }

    fn hint(&self) -> Option<String> {
        Some(format!(
            "Activate it on its own with `--type` and `--name`; the group can take CLAS, INTF, PROG, DDLS, TABL, STRU, BDEF, SRVD, DDLX and DCLS. ({})",
            self.0
        ))
    }
}

#[derive(Debug, Serialize)]
pub struct GroupMemberOutput {
    object_type: &'static str,
    name: String,
    object_uri: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_sha256_after: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    published: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_entry_incomplete: Option<String>,
    activation_messages: Vec<EditActivationDiagnosticOutput>,
}

#[derive(Debug, Serialize)]
pub struct GroupActivationOutput {
    ok: bool,
    profile: String,
    status: &'static str,
    transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_group: Option<String>,
    objects: Vec<GroupMemberOutput>,
    /// Named, but with nothing staged, so left out of the request.
    nothing_to_activate: Vec<String>,
    sap_reported_activation_executed: Option<bool>,
    activation_messages: Vec<EditActivationDiagnosticOutput>,
}

/// `edit activate` with two or more `--object`s.
pub async fn edit_group_activate(
    explicit_profile: Option<&str>,
    args: &EditActivateArgs,
) -> Result<GroupActivationOutput, Reported> {
    let mut objects = Vec::with_capacity(args.objects.len());
    for spec in &args.objects {
        let (object_type, name) = parse_object_spec(spec)?;
        match AdtObjectFamily::parse(&object_type)? {
            AdtObjectFamily::Source(object_type) => objects.push((object_type, name)),
            AdtObjectFamily::Metadata(_) => return Err(GroupTypeError(spec.clone()).into()),
        }
    }
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    let journal = if args.no_journal {
        None
    } else {
        Some(Journal::open(&profile_name, &profile)?)
    };
    let request = GroupActivationRequest {
        objects,
        transport: args.transport.clone(),
    };
    let result = activate_adt_sources_together(
        &mut client,
        &profile.edit_policy(),
        &request,
        journal.as_ref(),
    )
    .await?;
    Ok(map_group_activation_result(profile_name, result))
}

fn map_group_activation_result(
    profile: String,
    result: GroupActivationResult,
) -> GroupActivationOutput {
    GroupActivationOutput {
        ok: true,
        profile,
        status: "activated_verified",
        transport: result.transport,
        journal_group: result.group,
        objects: result
            .members
            .into_iter()
            .map(|member| GroupMemberOutput {
                object_type: member.identity.object_type.as_str(),
                name: member.identity.name,
                object_uri: member.identity.object_uri,
                status: member.outcome.as_str(),
                active_sha256_after: member.active_after.map(|active| active.sha256),
                published: member
                    .published
                    .iter()
                    .map(|include| include.include.as_str())
                    .collect(),
                journal_entry_incomplete: member.journal_entry_incomplete,
                activation_messages: member.messages.into_iter().map(diagnostic).collect(),
            })
            .collect(),
        nothing_to_activate: result
            .nothing_to_activate
            .into_iter()
            .map(|identity| identity.name)
            .collect(),
        sap_reported_activation_executed: result.sap_reported_activation_executed,
        activation_messages: result
            .general_messages
            .into_iter()
            .map(diagnostic)
            .collect(),
    }
}

fn diagnostic(message: AdtActivationMessage) -> EditActivationDiagnosticOutput {
    EditActivationDiagnosticOutput {
        severity: message.severity.as_str().to_owned(),
        text: message.text,
        uri: message.href,
        include: None,
        line: message.line,
        column: None,
        object_description: message.object_description,
    }
}

pub fn print_group_activate(result: &GroupActivationOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    let mut text = String::new();
    let _ = writeln!(
        text,
        "Activated {} object(s) together:",
        result.objects.len()
    );
    for object in &result.objects {
        let _ = writeln!(
            text,
            "  {} {}: {}",
            object.object_type, object.name, object.status
        );
    }
    if !result.nothing_to_activate.is_empty() {
        let _ = writeln!(
            text,
            "Nothing staged, left out: {}",
            result.nothing_to_activate.join(", ")
        );
    }
    for message in result
        .objects
        .iter()
        .flat_map(|object| &object.activation_messages)
        .chain(&result.activation_messages)
    {
        let _ = writeln!(text, "{}: {}", message.severity, message.text);
    }
    print!("{text}");
}

pub fn print_edit_object_activate(result: &EditActivationOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
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
            published: result
                .published
                .into_iter()
                .map(|published| PublishedIncludeOutput {
                    include: published.include.as_str(),
                    inactive_sha256_before: published.inactive.sha256,
                    active_sha256_after: published.active.sha256,
                })
                .collect(),
            staged_by_others: result.staged_by_others.map(|staged| {
                staged
                    .into_iter()
                    .map(|staged| StagedIncludeOutput {
                        include: staged.include.as_str(),
                        user: staged.author,
                    })
                    .collect()
            }),
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
        include: message
            .include
            .map(fractal::sap::class_include::ClassInclude::as_str),
        uri: message.uri,
        line: message.line,
        column: message.column,
        text: message.text,
        object_description: None,
    }
}

fn map_activation_message(message: AdtActivationMessage) -> EditActivationDiagnosticOutput {
    EditActivationDiagnosticOutput {
        severity: message.severity.as_str().to_owned(),
        text: message.text,
        uri: None,
        include: None,
        line: message.line,
        column: None,
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
    if let Some(details) = &result.source_details {
        let _ = writeln!(output, "active source matches inactive source: true");
        let _ = writeln!(output, "inactive version exists after: false");
        if !details.published.is_empty() {
            let names: Vec<&str> = details.published.iter().map(|p| p.include).collect();
            let _ = writeln!(output, "published: {}", names.join(", "));
        }
        if let Some(staged) = details.staged_by_others.as_ref().filter(|s| !s.is_empty()) {
            let names: Vec<String> = staged
                .iter()
                .map(|staged| {
                    staged.user.as_ref().map_or_else(
                        || staged.include.to_owned(),
                        |user| format!("{} ({user})", staged.include),
                    )
                })
                .collect();
            let _ = writeln!(
                output,
                "still staged by others, not activated: {}",
                names.join(", ")
            );
        }
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
        let position = match (message.include, message.line) {
            (Some(include), Some(line)) => Some(format!("{include} line {line}")),
            (Some(include), None) => Some(include.to_owned()),
            (None, Some(line)) => Some(format!("line {line}")),
            (None, None) => None,
        };
        let location = match (&message.object_description, position) {
            (Some(object), Some(position)) => format!(" {object}, {position}"),
            (Some(object), None) => format!(" {object}"),
            (None, Some(position)) => format!(" {position}"),
            (None, None) => String::new(),
        };
        let _ = writeln!(output, "- {}{location}: {}", message.severity, message.text);
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use fractal::sap::adt_version::AdtVersion;

    use super::*;
    use crate::cli::{Cli, Command, EditCommand};
    use fractal::sap::{
        activation_request::AdtActivationMessage,
        adt_message_severity::AdtMessageSeverity,
        editable_source::{AdtSourceSnapshot, EditableAdtObjectType, EditableAdtSourceIdentity},
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

        assert_eq!(args.object_type.as_deref(), Some("CLAS"));
        assert_eq!(args.name.as_deref(), Some("ZCL_SAMPLE"));
        assert_eq!(args.transport.as_deref(), Some("AB1K900575"));
    }

    fn activate_args(argv: &[&str]) -> Result<EditActivateArgs, clap::Error> {
        let mut full = vec!["fractal", "edit", "activate"];
        full.extend_from_slice(argv);
        let Command::Edit {
            command: EditCommand::Activate(args),
        } = Cli::try_parse_from(full)?.command
        else {
            panic!("expected edit activate command");
        };
        Ok(args)
    }

    #[test]
    fn several_objects_are_named_one_flag_each() {
        let args = activate_args(&[
            "--object",
            "DDLS:ZSAMPLE_PARENT",
            "--object",
            "DDLS:/ACME/CHILD",
        ])
        .unwrap();
        assert_eq!(args.objects, ["DDLS:ZSAMPLE_PARENT", "DDLS:/ACME/CHILD"]);
    }

    #[test]
    fn the_two_ways_of_naming_objects_do_not_mix() {
        assert!(activate_args(&["--type", "DDLS", "--name", "ZA", "--object", "DDLS:ZB"]).is_err());
        assert!(activate_args(&[]).is_err(), "something must be named");
        assert!(
            activate_args(&["--type", "DDLS"]).is_err(),
            "a type needs a name"
        );
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
                    include: Some(fractal::sap::class_include::ClassInclude::Main),
                },
                transport: Some("AB1K900575".to_owned()),
                precheck: AdtSourceCheckResult {
                    identity: EditableAdtSourceIdentity {
                        object_type: EditableAdtObjectType::Class,
                        name: "ZCL_SAMPLE".to_owned(),
                        object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                        source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
                        include: Some(fractal::sap::class_include::ClassInclude::Main),
                    },
                    requested_version: AdtVersion::Inactive,
                    check_executed: true,
                    inactive_version_exists: Some(true),
                    clean: true,
                    errors: 0,
                    warnings: 1,
                    infos: 0,
                    messages: vec![AdtSourceCheckMessage {
                        severity: AdtMessageSeverity::Warning,
                        text: "Obsolete statement".to_owned(),
                        uri: None,
                        include: None,
                        line: Some(8),
                        column: None,
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
                    href: None,
                }],
                published: vec![fractal::sap::source_activation::PublishedInclude {
                    include: fractal::sap::class_include::ClassInclude::Implementations,
                    inactive: AdtSourceSnapshot::from_parts("x".to_owned(), "e1a8".to_owned(), 1),
                    active: AdtSourceSnapshot::from_parts("x".to_owned(), "e1a8".to_owned(), 1),
                }],
                staged_by_others: Some(vec![fractal::sap::staged_work::StagedInclude {
                    include: fractal::sap::class_include::ClassInclude::Main,
                    author: Some("COLLEAGUE".to_owned()),
                }]),
            },
        );

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "activated_verified");
        assert_eq!(json["activated"], true);
        assert_eq!(json["verified"], true);
        assert_eq!(json["precheck_warnings"], 1);
        assert_eq!(json["sap_reported_activation_executed"], true);
        assert_eq!(json["published"][0]["include"], "implementations");
        assert_eq!(json["staged_by_others"][0]["user"], "COLLEAGUE");
        let readable = render_activation_readable(&output);
        assert!(readable.contains("status: activated and verified"));
        assert!(readable.contains("warning line 8: Obsolete statement"));
        assert!(readable.contains("info Class ZCL_SAMPLE: Activation completed"));
        assert!(readable.contains("published: implementations"));
        assert!(readable.contains("still staged by others, not activated: main (COLLEAGUE)"));
    }
}
