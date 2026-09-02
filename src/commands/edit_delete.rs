use std::fmt::Write as _;

use serde::Serialize;

use super::{connect, edit_object_identity::EditObjectIdentityOutput};
use crate::{
    cli::EditObjectDeleteArgs,
    output::{OutputFormat, print_result},
    reported::Reported,
};
use fractal::sap::{
    metadata_object::{delete_metadata_object, preview_metadata_object_deletion},
    object_deletion::{
        AdtObjectDeletionPreview, AdtObjectDeletionRequest, AdtObjectDeletionResult,
        delete_adt_object, preview_adt_object_deletion,
    },
    object_family::AdtObjectFamily,
};

// As with the other edit outputs, the flags are the JSON contract: `dry_run`,
// `deleted`, and `forced` each answer a different question a caller must be
// able to ask without parsing prose.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Serialize)]
pub struct EditObjectDeleteOutput {
    ok: bool,
    profile: String,
    status: String,
    dry_run: bool,
    deleted: bool,
    forced: bool,
    #[serde(flatten)]
    object: EditObjectIdentityOutput,
    transport: Option<String>,
    /// Objects SAP reports as genuine references. Non-empty on a completed
    /// delete only when `--force` overrode the refusal.
    direct_usages: Vec<String>,
}

/// # Errors
///
/// Returns [`Reported`] when the type is unknown, validation fails, the object
/// is still referenced, or SAP rejects or fails to complete the delete.
pub async fn edit_object_delete(
    explicit_profile: Option<&str>,
    args: &EditObjectDeleteArgs,
) -> Result<EditObjectDeleteOutput, Reported> {
    let object_type = AdtObjectFamily::parse(&args.object_type)?;
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    let namespaces = &profile.customer_namespaces;

    match object_type {
        AdtObjectFamily::Source(object_type) => {
            let request = AdtObjectDeletionRequest {
                object_type,
                name: args.name.clone(),
                transport: args.transport.clone(),
                force: args.force,
            };
            if args.dry_run {
                let preview =
                    preview_adt_object_deletion(&mut client, namespaces, &request).await?;
                return Ok(map_deletion_preview(profile_name, preview));
            }
            let result = delete_adt_object(&mut client, namespaces, &request).await?;
            Ok(map_deletion_result(profile_name, result))
        }
        AdtObjectFamily::Metadata(object_type) => {
            if args.dry_run {
                let preview = preview_metadata_object_deletion(
                    &mut client,
                    namespaces,
                    object_type,
                    &args.name,
                    args.transport.as_deref(),
                    args.force,
                )
                .await?;
                return Ok(map_deletion_preview(profile_name, preview));
            }
            let result = delete_metadata_object(
                &mut client,
                namespaces,
                object_type,
                &args.name,
                args.transport.as_deref(),
                args.force,
            )
            .await?;
            Ok(map_deletion_result(profile_name, result))
        }
    }
}

pub fn print_edit_object_delete(result: &EditObjectDeleteOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    print!("{}", render_object_delete_readable(result));
}

fn map_deletion_preview(
    profile: String,
    preview: AdtObjectDeletionPreview,
) -> EditObjectDeleteOutput {
    EditObjectDeleteOutput {
        ok: true,
        profile,
        status: if preview.would_delete {
            "dry_run_would_delete".to_owned()
        } else {
            "dry_run_would_refuse".to_owned()
        },
        dry_run: true,
        deleted: false,
        forced: false,
        object: preview.identity.into(),
        transport: preview.transport,
        direct_usages: preview.direct_usages,
    }
}

fn map_deletion_result(profile: String, result: AdtObjectDeletionResult) -> EditObjectDeleteOutput {
    EditObjectDeleteOutput {
        ok: true,
        profile,
        status: "deleted_verified".to_owned(),
        dry_run: false,
        deleted: true,
        forced: result.forced,
        object: result.identity.into(),
        transport: result.transport,
        direct_usages: result.direct_usages,
    }
}

fn render_object_delete_readable(result: &EditObjectDeleteOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(
        output,
        "object: {} {}",
        result.object.object_type, result.object.name
    );
    if let Some(transport) = &result.transport {
        let _ = writeln!(output, "transport: {transport}");
    }
    if result.dry_run {
        let _ = writeln!(
            output,
            "status: dry run, nothing deleted; a real delete would {}",
            if result.status == "dry_run_would_delete" {
                "proceed"
            } else {
                "be refused"
            }
        );
    } else {
        let _ = writeln!(output, "status: deleted and verified gone");
        if result.forced {
            let _ = writeln!(output, "forced: references were overridden");
        }
    }
    if result.direct_usages.is_empty() {
        let _ = writeln!(output, "referenced by: nothing");
    } else {
        let _ = writeln!(output, "referenced by:");
        for usage in &result.direct_usages {
            let _ = writeln!(output, "- {usage}");
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, EditCommand};
    use fractal::sap::{
        adt_object_identity::AdtObjectIdentity,
        editable_source::{EditableAdtObjectType, EditableAdtSourceIdentity},
    };

    fn delete_args(cli: Cli) -> EditObjectDeleteArgs {
        let Command::Edit {
            command: EditCommand::Delete(args),
        } = cli.command
        else {
            panic!("expected edit delete command");
        };
        args
    }

    fn identity() -> AdtObjectIdentity {
        EditableAdtSourceIdentity {
            object_type: EditableAdtObjectType::Program,
            name: "ZSAMPLE".to_owned(),
            object_uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
            source_uri: "/sap/bc/adt/programs/programs/zsample/source/main".to_owned(),
        }
        .into()
    }

    #[test]
    fn defaults_to_a_guarded_non_dry_delete() {
        let args = delete_args(
            Cli::try_parse_from([
                "fractal", "edit", "delete", "--type", "PROG", "--name", "ZSAMPLE",
            ])
            .unwrap(),
        );

        assert!(!args.force);
        assert!(!args.dry_run);
        assert_eq!(args.transport, None);
    }

    #[test]
    fn parses_force_and_dry_run() {
        let args = delete_args(
            Cli::try_parse_from([
                "fractal",
                "edit",
                "delete",
                "--type",
                "CLAS",
                "--name",
                "ZCL_X",
                "--force",
                "--dry-run",
                "--transport",
                "AB1K900575",
            ])
            .unwrap(),
        );

        assert!(args.force);
        assert!(args.dry_run);
        assert_eq!(args.transport.as_deref(), Some("AB1K900575"));
    }

    #[test]
    fn a_dry_run_never_claims_the_object_was_deleted() {
        let output = map_deletion_preview(
            "development".to_owned(),
            AdtObjectDeletionPreview {
                identity: identity(),
                transport: None,
                direct_usages: vec!["ZCL_CALLER".to_owned()],
                would_delete: false,
            },
        );

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "dry_run_would_refuse");
        assert_eq!(json["dry_run"], true);
        assert_eq!(json["deleted"], false);
        assert_eq!(json["direct_usages"][0], "ZCL_CALLER");

        let readable = render_object_delete_readable(&output);
        assert!(readable.contains("a real delete would be refused"));
        assert!(readable.contains("- ZCL_CALLER"));
    }

    #[test]
    fn a_completed_delete_reports_verified_and_whether_it_was_forced() {
        let output = map_deletion_result(
            "development".to_owned(),
            AdtObjectDeletionResult {
                identity: identity(),
                transport: Some("AB1K900575".to_owned()),
                direct_usages: vec!["ZCL_CALLER".to_owned()],
                forced: true,
            },
        );

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "deleted_verified");
        assert_eq!(json["deleted"], true);
        assert_eq!(json["forced"], true);

        let readable = render_object_delete_readable(&output);
        assert!(readable.contains("status: deleted and verified gone"));
        assert!(readable.contains("forced: references were overridden"));
    }
}
