//! Creation of empty editable ADT objects.
//!
//! Creation deliberately produces a *shell* and nothing else: the object is
//! registered in its package, left inactive, and its source is filled in
//! afterwards by `fractal edit set`. That keeps this workflow small, keeps
//! source writing in the one place that already guards it with a lock and a
//! read-back, and preserves the save-only discipline — creating an object
//! never activates it.
//!
//! Each object family needs its own payload, media type, and collection, so a
//! new type is unverified until a real create has been observed for it:
//! pinning a payload in a test proves what Fractal sends, never what SAP
//! accepts. This codebase has been bitten by an assumption baked into its own
//! fixtures before — see the package `nodestructure` field encoding.

use reqwest::header::{HeaderMap, HeaderValue};
use thiserror::Error;

use super::{
    client::{SapClient, SapClientError},
    editable_source::{
        AdtEditTargetValidationError, EditableAdtObjectType, EditableAdtSourceIdentity,
        validate_adt_edit_target,
    },
};
use crate::{
    reportable_error::{ReportableError, sap_http_status},
    suggested_command,
};

/// One object to create, with the metadata SAP requires at creation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtObjectCreationRequest {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub package: String,
    pub description: String,
    pub transport: Option<String>,
}

/// The shell SAP created, confirmed to exist by a read-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtObjectCreationResult {
    pub identity: EditableAdtSourceIdentity,
    pub package: String,
    pub description: String,
    pub transport: Option<String>,
}

/// A failure while creating an editable ADT object.
#[derive(Debug, Error)]
pub enum AdtObjectCreationError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error("invalid package name '{0}'")]
    InvalidPackage(String),
    #[error("a new object needs a non-blank description")]
    BlankDescription,
    #[error("the ADT object-creation request failed: {source}")]
    CreateRequest {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: SapClientError,
    },
    /// The name is taken. Separated from a general failure because retrying
    /// cannot help and the remedy is a different command.
    #[error("{} {} already exists", identity.object_type.as_str(), identity.name)]
    AlreadyExists {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: SapClientError,
    },
    #[error(
        "SAP accepted the creation request, but the new object could not be read back: {source}"
    )]
    Verification {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: SapClientError,
    },
}

impl ReportableError for AdtObjectCreationError {
    fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::InvalidPackage(_) => "invalid_package_name",
            Self::BlankDescription => "blank_object_description",
            Self::CreateRequest { .. } => "edit_create_request_failed",
            Self::AlreadyExists { .. } => "edit_create_object_exists",
            Self::Verification { .. } => "edit_create_verification_failed",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(match self {
            Self::CreateRequest { source, .. }
            | Self::AlreadyExists { source, .. }
            | Self::Verification { source, .. } => Some(source),
            _ => None,
        })
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Validation(error) => return error.hint(),
            Self::InvalidPackage(_) => {
                "Use an existing package name containing letters, digits, underscore, or slash, or $TMP for local objects."
                    .to_owned()
            }
            Self::BlankDescription => {
                "Pass --description with a short summary; SAP stores it as the object's text."
                    .to_owned()
            }
            Self::CreateRequest { source, .. } => format!(
                "The object was not created. If it already exists, use `fractal edit set` instead. {}",
                source.hint().unwrap_or_default()
            ),
            // Deliberately no "retry" advice: the name is taken, and it will
            // still be taken next time.
            Self::AlreadyExists { .. } => {
                "The name is already in use, so creating it again cannot succeed. Use `fractal edit set` to change the existing object, or `fractal edit delete` to remove it first."
                    .to_owned()
            }
            Self::Verification { identity, .. } => format!(
                "SAP reported success but the object could not be read back; inspect it before retrying, because a second create would fail if the first one landed. Run `{}`.",
                suggested_command::object_xml(&identity.object_uri)
            ),
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            // A create that failed may mean the object already exists.
            Self::CreateRequest { identity, .. } => Some(suggested_command::object_search(
                identity.object_type.as_str(),
                &identity.name,
            )),
            // It definitely exists, so point at the object itself rather than
            // at a search for it.
            Self::AlreadyExists { identity, .. } => Some(suggested_command::edit_read(
                identity.object_type.as_str(),
                &identity.name,
                "active",
            )),
            Self::Verification { identity, .. } => {
                Some(suggested_command::object_xml(&identity.object_uri))
            }
            _ => None,
        }
    }
}

/// Whether SAP's refusal says the name is already taken.
///
/// There is no machine-readable marker for this, so the message is matched.
/// Neither the status nor any single phrase is usable — each object family's
/// handler answers differently. Observed on a live system:
///
/// | type | status | message |
/// |---|---|---|
/// | program | 500 | `A program or include already exists with the name X` |
/// | class | 400 | `Resource CLASS X does already exist.` |
/// | data definition | 400 | `Resource Data Definition X does already exist.` |
///
/// "already exist" is the substring common to all three, which also covers
/// both "already exists" and "does already exist". Matching more narrowly
/// would classify only the family it was written against.
fn reports_an_existing_object(error: &SapClientError) -> bool {
    matches!(
        error,
        SapClientError::Http { message, .. }
            if message.to_ascii_lowercase().contains("already exist")
    )
}

/// Creates an empty editable object and confirms that it exists.
///
/// The object is left inactive and without caller-supplied source: fill it with
/// `fractal edit set`, then activate it explicitly. HTTP success is not trusted
/// on its own — the object's metadata is read back before this reports success,
/// matching every other mutating workflow in this crate.
///
/// # Errors
///
/// Returns [`AdtObjectCreationError`] for identity, namespace, transport,
/// package, or description validation, an unsupported object type, a failed
/// creation request, or a failed read-back.
pub async fn create_adt_object(
    sap: &mut SapClient,
    customer_namespaces: &[String],
    request: &AdtObjectCreationRequest,
) -> Result<AdtObjectCreationResult, AdtObjectCreationError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        customer_namespaces,
        request.transport.as_deref(),
    )?;
    let identity = target.identity;
    let transport = target.transport;
    let package = validate_package_name(&request.package)?;
    let description = request.description.trim();
    if description.is_empty() {
        return Err(AdtObjectCreationError::BlankDescription);
    }

    let (media_type, body) = creation_payload(&identity, &package, description);
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", HeaderValue::from_static(media_type));

    let mut query = Vec::new();
    if let Some(transport) = &transport {
        query.push(("corrNr", transport.as_str()));
    }
    sap.post_text(
        identity.object_type.collection_path(),
        &query,
        Some(&body),
        headers,
    )
    .await
    .map_err(|source| {
        let identity = Box::new(identity.clone());
        if reports_an_existing_object(&source) {
            AdtObjectCreationError::AlreadyExists { identity, source }
        } else {
            AdtObjectCreationError::CreateRequest { identity, source }
        }
    })?;

    // HTTP success is not proof. Read the object's metadata back before
    // reporting that it exists.
    sap.get_text(&identity.object_uri).await.map_err(|source| {
        AdtObjectCreationError::Verification {
            identity: Box::new(identity.clone()),
            source,
        }
    })?;

    Ok(AdtObjectCreationResult {
        identity,
        package,
        description: description.to_owned(),
        transport,
    })
}

/// The media type and request body SAP expects for this object type.
///
/// Each object family has its own ADT collection, media type, and root
/// element; there is no generic create endpoint. The match is deliberately
/// exhaustive: a new editable type fails to compile here until someone decides
/// what its payload is.
fn creation_payload(
    identity: &EditableAdtSourceIdentity,
    package: &str,
    description: &str,
) -> (&'static str, String) {
    let description = xml_escape_attribute(description);
    match identity.object_type {
        EditableAdtObjectType::Program => (
            "application/vnd.sap.adt.programs.programs.v2+xml",
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><program:abapProgram xmlns:program=\"http://www.sap.com/adt/programs/programs\" xmlns:adtcore=\"http://www.sap.com/adt/core\" adtcore:description=\"{description}\" adtcore:name=\"{}\" adtcore:type=\"PROG/P\"><adtcore:packageRef adtcore:name=\"{package}\"/></program:abapProgram>",
                identity.name
            ),
        ),
        // A class shell is created public and final; ADT's own wizard defaults
        // the same way, and `edit set` replaces the whole source afterwards.
        EditableAdtObjectType::Class => (
            "application/vnd.sap.adt.oo.classes.v2+xml",
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><class:abapClass xmlns:class=\"http://www.sap.com/adt/oo/classes\" xmlns:adtcore=\"http://www.sap.com/adt/core\" adtcore:description=\"{description}\" adtcore:name=\"{}\" adtcore:type=\"CLAS/OC\" class:final=\"true\" class:visibility=\"public\"><adtcore:packageRef adtcore:name=\"{package}\"/></class:abapClass>",
                identity.name
            ),
        ),
        EditableAdtObjectType::Interface => (
            "application/vnd.sap.adt.oo.interfaces.v2+xml",
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><intf:abapInterface xmlns:intf=\"http://www.sap.com/adt/oo/interfaces\" xmlns:adtcore=\"http://www.sap.com/adt/core\" adtcore:description=\"{description}\" adtcore:name=\"{}\" adtcore:type=\"INTF/OI\"><adtcore:packageRef adtcore:name=\"{package}\"/></intf:abapInterface>",
                identity.name
            ),
        ),
        EditableAdtObjectType::DdlSource => (
            "application/vnd.sap.adt.ddlSource+xml",
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ddl:ddlSource xmlns:ddl=\"http://www.sap.com/adt/ddic/ddlsources\" xmlns:adtcore=\"http://www.sap.com/adt/core\" adtcore:description=\"{description}\" adtcore:name=\"{}\" adtcore:type=\"DDLS/DF\"><adtcore:packageRef adtcore:name=\"{package}\"/></ddl:ddlSource>",
                identity.name
            ),
        ),
        // A table is not an `adtcore` object like the others: its root element
        // is the generic workbench-object wrapper, which is what a live table's
        // own metadata returns.
        EditableAdtObjectType::Table => (
            "application/vnd.sap.adt.tables.v2+xml",
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><blue:blueSource xmlns:blue=\"http://www.sap.com/wbobj/blue\" xmlns:adtcore=\"http://www.sap.com/adt/core\" adtcore:description=\"{description}\" adtcore:name=\"{}\" adtcore:type=\"TABL/DT\"><adtcore:packageRef adtcore:name=\"{package}\"/></blue:blueSource>",
                identity.name
            ),
        ),
    }
}

/// Accepts an ABAP package name, including `$TMP` and slash namespaces.
fn validate_package_name(package: &str) -> Result<String, AdtObjectCreationError> {
    let trimmed = package.trim();
    let valid = !trimmed.is_empty()
        && trimmed.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character == '_'
                || character == '/'
                || character == '$'
        });
    if valid {
        Ok(trimmed.to_ascii_uppercase())
    } else {
        Err(AdtObjectCreationError::InvalidPackage(package.to_owned()))
    }
}

fn xml_escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sap::editable_source::editable_source_identity;

    fn identity(object_type: EditableAdtObjectType, name: &str) -> EditableAdtSourceIdentity {
        editable_source_identity(object_type, name).unwrap()
    }

    #[test]
    fn builds_the_program_creation_payload() {
        let (media_type, body) = creation_payload(
            &identity(EditableAdtObjectType::Program, "zsample"),
            "ZPKG",
            "Sample report",
        );

        assert_eq!(
            media_type,
            "application/vnd.sap.adt.programs.programs.v2+xml"
        );
        assert!(body.contains("adtcore:name=\"ZSAMPLE\""));
        assert!(body.contains("adtcore:type=\"PROG/P\""));
        assert!(body.contains("adtcore:description=\"Sample report\""));
        assert!(body.contains("<adtcore:packageRef adtcore:name=\"ZPKG\"/>"));
    }

    fn http_failure(status: u16, message: &str) -> SapClientError {
        SapClientError::Http {
            kind: crate::sap::client::SapHttpErrorKind::Other,
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            url: "https://example.test/sap/bc/adt/programs/programs".to_owned(),
            message: message.to_owned(),
        }
    }

    #[test]
    fn recognises_every_wording_a_live_system_uses_for_a_taken_name() {
        assert!(reports_an_existing_object(&http_failure(
            500,
            "A program or include already exists with the name ZSAMPLE"
        )));
        assert!(reports_an_existing_object(&http_failure(
            400,
            "Resource CLASS ZCL_SAMPLE does already exist."
        )));
        assert!(reports_an_existing_object(&http_failure(
            400,
            "Resource Data Definition ZSAMPLE_V does already exist."
        )));
    }

    #[test]
    fn does_not_claim_an_unrelated_refusal_means_the_name_is_taken() {
        // Misclassifying here tells the caller to stop and edit an object that
        // may not exist, and suppresses the retry advice that would have
        // helped.
        assert!(!reports_an_existing_object(&http_failure(
            403,
            "Not authorized to create objects in package ZPKG"
        )));
        assert!(!reports_an_existing_object(&http_failure(
            422,
            "Select a shorter name"
        )));
        assert!(!reports_an_existing_object(&SapClientError::Network {
            url: "https://example.test".to_owned(),
            message: "connection refused".to_owned(),
        }));
    }

    #[test]
    fn builds_the_class_creation_payload() {
        let (media_type, body) = creation_payload(
            &identity(EditableAdtObjectType::Class, "zcl_sample"),
            "ZPKG",
            "Sample class",
        );

        assert_eq!(media_type, "application/vnd.sap.adt.oo.classes.v2+xml");
        assert!(body.contains("adtcore:name=\"ZCL_SAMPLE\""));
        assert!(body.contains("adtcore:type=\"CLAS/OC\""));
        assert!(body.contains("class:visibility=\"public\""));
        assert!(body.contains("<adtcore:packageRef adtcore:name=\"ZPKG\"/>"));
    }

    #[test]
    fn builds_the_interface_creation_payload() {
        let (media_type, body) = creation_payload(
            &identity(EditableAdtObjectType::Interface, "zif_sample"),
            "ZPKG",
            "Sample interface",
        );

        assert_eq!(media_type, "application/vnd.sap.adt.oo.interfaces.v2+xml");
        assert!(body.contains("adtcore:name=\"ZIF_SAMPLE\""));
        assert!(body.contains("adtcore:type=\"INTF/OI\""));
        assert!(body.contains("<adtcore:packageRef adtcore:name=\"ZPKG\"/>"));
    }

    #[test]
    fn builds_the_ddl_source_creation_payload() {
        let (media_type, body) = creation_payload(
            &identity(EditableAdtObjectType::DdlSource, "zddl_sample"),
            "ZPKG",
            "Sample CDS view",
        );

        assert_eq!(media_type, "application/vnd.sap.adt.ddlSource+xml");
        assert!(body.contains("adtcore:name=\"ZDDL_SAMPLE\""));
        assert!(body.contains("adtcore:type=\"DDLS/DF\""));
    }

    #[test]
    fn escapes_a_description_that_would_break_the_payload() {
        let (_, body) = creation_payload(
            &identity(EditableAdtObjectType::Program, "zsample"),
            "ZPKG",
            r#"Reads "A" & <B>"#,
        );

        assert!(body.contains("adtcore:description=\"Reads &quot;A&quot; &amp; &lt;B&gt;\""));
    }

    #[test]
    fn builds_the_table_creation_payload() {
        let (media_type, body) = creation_payload(
            &identity(EditableAdtObjectType::Table, "ztable"),
            "ZPKG",
            "Sample table",
        );

        assert_eq!(media_type, "application/vnd.sap.adt.tables.v2+xml");
        assert!(body.contains("adtcore:name=\"ZTABLE\""));
        assert!(body.contains("adtcore:type=\"TABL/DT\""));
        assert!(body.contains("xmlns:blue=\"http://www.sap.com/wbobj/blue\""));
    }

    #[test]
    fn accepts_local_and_namespaced_packages() {
        assert_eq!(validate_package_name(" zpkg ").unwrap(), "ZPKG");
        assert_eq!(validate_package_name("$TMP").unwrap(), "$TMP");
        assert_eq!(validate_package_name("/acme/pkg").unwrap(), "/ACME/PKG");
    }

    #[test]
    fn rejects_a_package_name_that_could_alter_the_request() {
        for package in ["", "ZPKG ZOTHER", "ZPKG\"/>"] {
            let error = validate_package_name(package).unwrap_err();
            assert_eq!(error.code(), "invalid_package_name");
        }
    }
}
