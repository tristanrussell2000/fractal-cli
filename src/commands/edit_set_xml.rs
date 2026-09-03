use std::fmt::Write as _;

use serde::Serialize;

use super::{
    connect, edit_lock_warning::still_locked_warning,
    edit_object_identity::EditObjectIdentityOutput, edit_set::resolve_replacement_source,
};
use crate::{
    cli::EditXmlSetArgs,
    output::{OutputFormat, print_result},
    reported::Reported,
};
use fractal::sap::metadata_object::{MetadataAdtObjectType, write_metadata_object};

#[derive(Debug, Serialize)]
pub struct EditXmlSetOutput {
    ok: bool,
    profile: String,
    status: String,
    #[serde(flatten)]
    object: EditObjectIdentityOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<String>,
    /// Whether the stored document actually differs from the one before.
    changed: bool,
    stored_bytes: usize,
    /// What SAP holds now, read back rather than assumed.
    stored_xml: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

/// # Errors
///
/// Returns [`Reported`] when the type is not a metadata type, the document
/// cannot be read, or SAP rejects the write or the read-back.
pub async fn edit_xml_set(
    explicit_profile: Option<&str>,
    args: &EditXmlSetArgs,
) -> Result<EditXmlSetOutput, Reported> {
    // Only this family is XML-edited; a source-based type would silently
    // replace an object's metadata rather than its code.
    let object_type = MetadataAdtObjectType::parse(&args.object_type)?;
    let xml = {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        resolve_replacement_source(&args.xml_file, &mut stdin)?
    };
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;

    let result = write_metadata_object(
        &mut client,
        &profile.edit_policy(),
        object_type,
        &args.name,
        &xml,
        args.transport.as_deref(),
    )
    .await?;

    // A write SAP accepted that changed nothing is reported, not hidden: it is
    // the shape a silent no-op takes, and the caller would otherwise believe
    // their edit landed.
    // Both caveats can apply at once, so they are joined rather than one
    // silently hiding the other.
    let warning = [
        still_locked_warning(result.still_locked),
        (!result.changed).then(|| {
            "SAP accepted the write but the stored document is unchanged. Check that the XML you sent differs from what was there, and that the fields you set are ones SAP stores."
                .to_owned()
        }),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let warning = (!warning.is_empty()).then(|| warning.join(" "));

    Ok(EditXmlSetOutput {
        ok: true,
        profile: profile_name,
        status: if result.changed {
            "written".to_owned()
        } else {
            "unchanged".to_owned()
        },
        object: result.identity.into(),
        transport: result.transport,
        changed: result.changed,
        stored_bytes: result.stored_xml.len(),
        stored_xml: result.stored_xml,
        warning,
    })
}

pub fn print_edit_xml_set(result: &EditXmlSetOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    let mut readable = String::new();
    let _ = writeln!(readable, "profile: {}", result.profile);
    let _ = writeln!(
        readable,
        "object: {} {}",
        result.object.object_type, result.object.name
    );
    if let Some(transport) = &result.transport {
        let _ = writeln!(readable, "transport: {transport}");
    }
    let _ = writeln!(readable, "status: {}", result.status);
    let _ = writeln!(readable, "stored bytes: {}", result.stored_bytes);
    if let Some(warning) = &result.warning {
        let _ = writeln!(readable, "warning: {warning}");
    }
    print!("{readable}");
}
