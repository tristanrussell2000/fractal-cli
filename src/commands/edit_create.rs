use std::fmt::Write as _;

use serde::Serialize;

use super::{connect, edit_object_identity::EditObjectIdentityOutput};
use crate::{
    cli::EditObjectCreateArgs,
    output::{OutputFormat, print_result},
    reported::Reported,
};
use fractal::reportable_error::ReportableError;
use fractal::sap::{
    metadata_object::{
        MetadataObjectCreationRequest, ServiceBindingSpec, ServiceBindingType,
        create_metadata_object,
    },
    object_creation::{AdtObjectCreationRequest, AdtObjectCreationResult, create_adt_object},
    object_family::AdtObjectFamily,
};

// Same reasoning as the other edit outputs: `created`, `activated`, and
// `wrote_source` are separately meaningful in the emitted JSON, and saying
// "created but neither written nor activated" is the point of this command.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Serialize)]
pub struct EditObjectCreateOutput {
    ok: bool,
    profile: String,
    status: String,
    created: bool,
    /// Always false: creation registers an inactive shell and never activates.
    activated: bool,
    /// Always false: source is written separately by `fractal edit set`.
    wrote_source: bool,
    #[serde(flatten)]
    object: EditObjectIdentityOutput,
    package: String,
    description: String,
    transport: Option<String>,
    next_step: String,
}

/// # Errors
///
/// Returns [`Reported`] when the type is unknown, validation fails, or SAP
/// rejects the creation request.
pub async fn edit_object_create(
    explicit_profile: Option<&str>,
    args: &EditObjectCreateArgs,
) -> Result<EditObjectCreateOutput, Reported> {
    // Resolved before connecting: an unknown type is not worth a round trip,
    // and the two families validate their names differently.
    let object_type = AdtObjectFamily::parse(&args.object_type)?;
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;

    let result = match object_type {
        AdtObjectFamily::Source(object_type) => {
            create_adt_object(
                &mut client,
                &profile.edit_policy(),
                &AdtObjectCreationRequest {
                    object_type,
                    name: args.name.clone(),
                    package: args.package.clone(),
                    description: args.description.clone(),
                    transport: args.transport.clone(),
                },
            )
            .await?
        }
        AdtObjectFamily::Metadata(object_type) => {
            create_metadata_object(
                &mut client,
                &profile.edit_policy(),
                &MetadataObjectCreationRequest {
                    object_type,
                    name: args.name.clone(),
                    package: args.package.clone(),
                    description: args.description.clone(),
                    transport: args.transport.clone(),
                    binding: binding_spec(args)?,
                },
            )
            .await?
        }
    };
    Ok(map_object_creation_result(profile_name, result))
}

/// Reads the two arguments only a service binding uses.
///
/// Both or neither: a binding needs the pair, and any other type accepts
/// neither, so a half-supplied pair is refused here rather than producing a
/// payload SAP would reject with an unexplained 500.
fn binding_spec(args: &EditObjectCreateArgs) -> Result<Option<ServiceBindingSpec>, Reported> {
    match (
        args.service_definition.as_deref(),
        args.binding_type.as_deref(),
    ) {
        (None, None) => Ok(None),
        (Some(service_definition), Some(binding_type)) => Ok(Some(ServiceBindingSpec {
            service_definition: service_definition.to_owned(),
            binding_type: ServiceBindingType::parse(binding_type)?,
        })),
        (Some(_), None) => Err(IncompleteBindingError::MissingBindingType.into()),
        (None, Some(_)) => Err(IncompleteBindingError::MissingServiceDefinition.into()),
    }
}

/// One of the binding arguments was given without the other.
#[derive(Debug, thiserror::Error)]
enum IncompleteBindingError {
    #[error("--service-definition needs --binding-type as well")]
    MissingBindingType,
    #[error("--binding-type needs --service-definition as well")]
    MissingServiceDefinition,
}

impl ReportableError for IncompleteBindingError {
    fn code(&self) -> &'static str {
        "incomplete_binding_arguments"
    }

    fn hint(&self) -> Option<String> {
        Some(
            "A service binding needs both: which service definition to expose, and over which protocol."
                .to_owned(),
        )
    }
}

pub fn print_edit_object_create(result: &EditObjectCreateOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    print!("{}", render_object_create_readable(result));
}

fn map_object_creation_result(
    profile: String,
    result: AdtObjectCreationResult,
) -> EditObjectCreateOutput {
    // An object with no source cannot be filled by `edit set`; its XML is the
    // object, so the next step is to read that XML and see what needs filling.
    let next_step = result.identity.source_uri.as_ref().map_or_else(
        || format!("fractal object xml {}", result.identity.object_uri),
        |_| {
            format!(
                "fractal edit set --type {} --name {} --source-file <path>",
                result.identity.object_type, result.identity.name
            )
        },
    );
    EditObjectCreateOutput {
        ok: true,
        profile,
        status: "created_inactive".to_owned(),
        created: true,
        activated: false,
        wrote_source: false,
        object: result.identity.into(),
        package: result.package,
        description: result.description,
        transport: result.transport,
        next_step,
    }
}

fn render_object_create_readable(result: &EditObjectCreateOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(
        output,
        "object: {} {}",
        result.object.object_type, result.object.name
    );
    let _ = writeln!(output, "package: {}", result.package);
    let _ = writeln!(output, "description: {}", result.description);
    if let Some(transport) = &result.transport {
        let _ = writeln!(output, "transport: {transport}");
    }
    let _ = writeln!(output, "status: created, inactive, no source written");
    let _ = writeln!(output, "next: {}", result.next_step);
    output
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, EditCommand};
    use fractal::sap::editable_source::{EditableAdtObjectType, EditableAdtSourceIdentity};

    fn create_args(cli: Cli) -> EditObjectCreateArgs {
        let Command::Edit {
            command: EditCommand::Create(args),
        } = cli.command
        else {
            panic!("expected edit create command");
        };
        args
    }

    #[test]
    fn parses_the_create_arguments() {
        let args = create_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "create",
                "--type",
                "PROG",
                "--name",
                "ZSAMPLE",
                "--package",
                "ZPKG",
                "--description",
                "Sample report",
                "--transport",
                "AB1K900575",
            ])
            .unwrap(),
        );

        assert_eq!(args.object_type, "PROG");
        assert_eq!(args.name, "ZSAMPLE");
        assert_eq!(args.package, "ZPKG");
        assert_eq!(args.description, "Sample report");
        assert_eq!(args.transport.as_deref(), Some("AB1K900575"));
    }

    #[test]
    fn requires_a_package_and_a_description() {
        for missing in [
            vec![
                "fractal",
                "edit",
                "create",
                "--type",
                "PROG",
                "--name",
                "ZSAMPLE",
                "--package",
                "ZPKG",
            ],
            vec![
                "fractal",
                "edit",
                "create",
                "--type",
                "PROG",
                "--name",
                "ZSAMPLE",
                "--description",
                "Sample report",
            ],
        ] {
            assert!(Cli::try_parse_from(missing).is_err());
        }
    }

    #[test]
    fn transport_is_optional_for_a_local_object() {
        let args = create_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "create",
                "--type",
                "PROG",
                "--name",
                "ZSAMPLE",
                "--package",
                "$TMP",
                "--description",
                "Scratch report",
            ])
            .unwrap(),
        );

        assert_eq!(args.package, "$TMP");
        assert_eq!(args.transport, None);
    }

    #[test]
    fn output_states_that_nothing_was_written_or_activated() {
        let output = map_object_creation_result(
            "development".to_owned(),
            AdtObjectCreationResult {
                identity: EditableAdtSourceIdentity {
                    object_type: EditableAdtObjectType::Program,
                    name: "ZSAMPLE".to_owned(),
                    object_uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
                    source_uri: "/sap/bc/adt/programs/programs/zsample/source/main".to_owned(),
                }
                .into(),
                package: "ZPKG".to_owned(),
                description: "Sample report".to_owned(),
                transport: Some("AB1K900575".to_owned()),
            },
        );

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "created_inactive");
        assert_eq!(json["created"], true);
        assert_eq!(json["activated"], false);
        assert_eq!(json["wrote_source"], false);
        assert_eq!(json["object_type"], "PROG");
        assert_eq!(json["package"], "ZPKG");
        assert_eq!(
            json["next_step"],
            "fractal edit set --type PROG --name ZSAMPLE --source-file <path>"
        );

        let readable = render_object_create_readable(&output);
        assert!(readable.contains("status: created, inactive, no source written"));
        assert!(readable.contains("next: fractal edit set"));
    }
}
