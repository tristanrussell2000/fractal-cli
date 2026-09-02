//! DDIC objects that ADT edits as a form rather than as code.
//!
//! A data element has no `source/main` at all — asking for one is a 404 — so
//! the source-based edit workflow does not apply to this family. What it has
//! instead is a complete XML document, and SAP hands back the *whole* skeleton
//! on creation: every field present, blank where a decision is needed and
//! defaulted where SAP has an opinion. That is what makes a create-then-fill
//! workflow possible here without modelling DDIC semantics field by field.
//!
//! Creating and deleting are shared with every other family (see
//! [`super::object_creation`] and [`super::object_deletion`]); only the
//! identity and the creation payload live here.

use thiserror::Error;

use super::{
    adt_object_identity::AdtObjectIdentity,
    client::SapClient,
    editable_source::{
        AdtEditTargetValidationError, EditableAdtSourceTargetError, canonicalize_transport_request,
        validate_customer_namespace, validate_object_name,
    },
    object_creation::{
        AdtObjectCreationError, AdtObjectCreationResult, AdtObjectCreatePayload,
        create_validated_adt_object, validate_package_name,
    },
    object_deletion::{
        AdtObjectDeletionError, AdtObjectDeletionPreview, AdtObjectDeletionResult,
        delete_validated_adt_object, preview_validated_deletion,
    },
};
use crate::reportable_error::ReportableError;

/// A DDIC family whose objects are XML documents rather than source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataAdtObjectType {
    DataElement,
    Domain,
}

impl MetadataAdtObjectType {
    /// Parses a logical type such as `DTEL`.
    ///
    /// # Errors
    ///
    /// Returns [`MetadataObjectTypeError`] for any type outside this family.
    pub fn parse(value: &str) -> Result<Self, MetadataObjectTypeError> {
        match value.trim().to_ascii_uppercase().as_str() {
            "DTEL" => Ok(Self::DataElement),
            "DOMA" => Ok(Self::Domain),
            _ => Err(MetadataObjectTypeError(value.to_owned())),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DataElement => "DTEL",
            Self::Domain => "DOMA",
        }
    }

    #[must_use]
    pub const fn collection_path(self) -> &'static str {
        match self {
            Self::DataElement => "/sap/bc/adt/ddic/dataelements",
            Self::Domain => "/sap/bc/adt/ddic/domains",
        }
    }

    /// SAP's own type code, which the creation payload has to carry.
    #[must_use]
    pub const fn adtcore_type(self) -> &'static str {
        match self {
            Self::DataElement => "DTEL/DE",
            Self::Domain => "DOMA/DD",
        }
    }

    /// The media type from the backend's discovery document.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::DataElement => "application/vnd.sap.adt.dataelements.v2+xml",
            Self::Domain => "application/vnd.sap.adt.domains.v2+xml",
        }
    }

    /// The root element and its namespace, read off a real object of this type.
    ///
    /// The two families do not share an envelope: a data element is a generic
    /// `blue:wbobj` wrapper while a domain is its own `doma:domain` root, and
    /// the namespace URIs are unrelated. Neither is guessable from the type
    /// code.
    const fn root_element(self) -> (&'static str, &'static str) {
        match self {
            Self::DataElement => ("blue:wbobj", "http://www.sap.com/wbobj/dictionary/dtel"),
            Self::Domain => ("doma:domain", "http://www.sap.com/dictionary/domain"),
        }
    }

    /// The namespace prefix declared on the root element.
    const fn root_prefix(self) -> &'static str {
        match self {
            Self::DataElement => "blue",
            Self::Domain => "doma",
        }
    }
}

#[derive(Debug, Error)]
#[error("unsupported metadata object type '{0}'")]
pub struct MetadataObjectTypeError(pub String);

impl ReportableError for MetadataObjectTypeError {
    fn code(&self) -> &'static str {
        "unsupported_metadata_object_type"
    }

    fn hint(&self) -> Option<String> {
        Some(
            "Metadata objects are DTEL and DOMA. Source-based types use the same commands."
                .to_owned(),
        )
    }
}

/// One metadata object to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataObjectCreationRequest {
    pub object_type: MetadataAdtObjectType,
    pub name: String,
    pub package: String,
    pub description: String,
    pub transport: Option<String>,
}

/// Builds the identity of a metadata object, validating name and namespace.
///
/// # Errors
///
/// Returns [`AdtEditTargetValidationError`] when the name is malformed or falls
/// outside the configured customer namespaces.
pub fn metadata_object_identity(
    object_type: MetadataAdtObjectType,
    name: &str,
    customer_namespaces: &[String],
) -> Result<AdtObjectIdentity, AdtEditTargetValidationError> {
    let name = validate_object_name(name).map_err(|error: EditableAdtSourceTargetError| {
        AdtEditTargetValidationError::InvalidObject(error)
    })?;
    validate_customer_namespace(&name, customer_namespaces)?;
    let path_name = name.to_ascii_lowercase().replace('/', "%2f");

    Ok(AdtObjectIdentity {
        object_type: object_type.as_str().to_owned(),
        object_uri: format!("{}/{path_name}", object_type.collection_path()),
        name,
        // The whole point of this family: there is no source to point at.
        source_uri: None,
    })
}

/// Creates an empty metadata object and confirms that it exists.
///
/// The object is a shell: SAP accepts a data element with no type information
/// at all. Fill it by reading its XML, editing the blanks, and writing it back.
///
/// # Errors
///
/// Returns [`AdtObjectCreationError`] for validation, a refused creation, or a
/// new object that could not be read back.
pub async fn create_metadata_object(
    sap: &mut SapClient,
    customer_namespaces: &[String],
    request: &MetadataObjectCreationRequest,
) -> Result<AdtObjectCreationResult, AdtObjectCreationError> {
    let identity =
        metadata_object_identity(request.object_type, &request.name, customer_namespaces)?;
    let transport = canonicalize_transport_request(request.transport.as_deref())
        .map_err(AdtEditTargetValidationError::from)?;
    let package = validate_package_name(&request.package)?;
    let description = request.description.trim();
    if description.is_empty() {
        return Err(AdtObjectCreationError::BlankDescription);
    }

    let body = creation_payload(request.object_type, &identity.name, &package, description);
    create_validated_adt_object(
        sap,
        identity,
        AdtObjectCreatePayload {
            collection_path: request.object_type.collection_path(),
            media_type: request.object_type.media_type(),
            body: &body,
        },
        &package,
        description,
        transport,
    )
    .await
}

/// Deletes a metadata object, with the same guards as any other object.
///
/// The where-used guard matters more here rather than less: a data element is
/// typically referenced by every table field and structure component built on
/// it, so deleting one unguarded breaks things far from the object itself.
///
/// # Errors
///
/// Returns [`AdtObjectDeletionError`] for validation, a failed where-used
/// lookup, remaining references, lock failures, a rejected delete, or an object
/// that is still readable afterwards.
pub async fn delete_metadata_object(
    sap: &mut SapClient,
    customer_namespaces: &[String],
    object_type: MetadataAdtObjectType,
    name: &str,
    transport: Option<&str>,
    force: bool,
) -> Result<AdtObjectDeletionResult, AdtObjectDeletionError> {
    let identity = metadata_object_identity(object_type, name, customer_namespaces)?;
    let transport =
        canonicalize_transport_request(transport).map_err(AdtEditTargetValidationError::from)?;

    delete_validated_adt_object(sap, identity, transport, force).await
}

/// Reports what deleting a metadata object would do, without doing any of it.
///
/// # Errors
///
/// Returns [`AdtObjectDeletionError`] for validation failures or a where-used
/// lookup that could not be completed.
pub async fn preview_metadata_object_deletion(
    sap: &mut SapClient,
    customer_namespaces: &[String],
    object_type: MetadataAdtObjectType,
    name: &str,
    transport: Option<&str>,
    force: bool,
) -> Result<AdtObjectDeletionPreview, AdtObjectDeletionError> {
    let identity = metadata_object_identity(object_type, name, customer_namespaces)?;
    let transport =
        canonicalize_transport_request(transport).map_err(AdtEditTargetValidationError::from)?;

    preview_validated_deletion(sap, identity, transport, force).await
}

/// The creation body: a bare root element with the object's identity on it.
///
/// No type information is sent. SAP accepts that and answers with the full
/// skeleton, which is a better starting point than anything guessed here would
/// be — a data element created with a domain would still need every label
/// filling in afterwards.
fn creation_payload(
    object_type: MetadataAdtObjectType,
    name: &str,
    package: &str,
    description: &str,
) -> String {
    let (element, namespace) = object_type.root_element();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<{element} xmlns:{prefix}=\"{namespace}\" xmlns:adtcore=\"http://www.sap.com/adt/core\" \
adtcore:name=\"{name}\" adtcore:type=\"{object_code}\" adtcore:description=\"{description}\">\
<adtcore:packageRef adtcore:name=\"{package}\"/>\
</{element}>",
        prefix = object_type.root_prefix(),
        object_code = object_type.adtcore_type(),
        description = xml_escape(description),
        package = xml_escape(package),
        name = xml_escape(name),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_the_types_that_have_no_source() {
        assert_eq!(
            MetadataAdtObjectType::parse("dtel").unwrap(),
            MetadataAdtObjectType::DataElement
        );
        assert_eq!(
            MetadataAdtObjectType::parse(" DOMA ").unwrap(),
            MetadataAdtObjectType::Domain
        );
        // A source-based type must not be routed through this family.
        assert!(MetadataAdtObjectType::parse("CLAS").is_err());
        assert!(MetadataAdtObjectType::parse("TABL").is_err());
    }

    #[test]
    fn an_identity_has_no_source_uri() {
        let identity = metadata_object_identity(
            MetadataAdtObjectType::DataElement,
            "ZSAMPLE_DE",
            &["Z*".to_owned()],
        )
        .unwrap();

        assert_eq!(identity.object_type, "DTEL");
        assert_eq!(identity.name, "ZSAMPLE_DE");
        assert_eq!(
            identity.object_uri,
            "/sap/bc/adt/ddic/dataelements/zsample_de"
        );
        assert_eq!(identity.source_uri, None);
    }

    #[test]
    fn refuses_a_name_outside_the_customer_namespaces() {
        let error =
            metadata_object_identity(MetadataAdtObjectType::Domain, "SFLIGHT", &["Z*".to_owned()])
                .unwrap_err();

        assert_eq!(error.code(), "object_outside_customer_namespaces");
    }

    #[test]
    fn builds_the_data_element_payload_sap_accepts() {
        let body = creation_payload(
            MetadataAdtObjectType::DataElement,
            "ZSAMPLE_DE",
            "$TMP",
            "Sample element",
        );

        assert!(body.contains("<blue:wbobj"));
        assert!(body.contains("xmlns:blue=\"http://www.sap.com/wbobj/dictionary/dtel\""));
        assert!(body.contains("adtcore:type=\"DTEL/DE\""));
        assert!(body.contains("adtcore:name=\"ZSAMPLE_DE\""));
        assert!(body.contains("<adtcore:packageRef adtcore:name=\"$TMP\"/>"));
    }

    #[test]
    fn a_domain_uses_its_own_root_and_namespace() {
        // The two families share no envelope; using the data element's wrapper
        // for a domain is a 400 rather than anything descriptive.
        let body = creation_payload(
            MetadataAdtObjectType::Domain,
            "ZSAMPLE_DO",
            "$TMP",
            "Sample",
        );

        assert!(body.contains("<doma:domain"));
        assert!(body.contains("xmlns:doma=\"http://www.sap.com/dictionary/domain\""));
        assert!(body.contains("adtcore:type=\"DOMA/DD\""));
    }

    #[test]
    fn escapes_a_description_that_would_break_the_document() {
        let body = creation_payload(
            MetadataAdtObjectType::DataElement,
            "ZSAMPLE_DE",
            "$TMP",
            r#"Width & "height" <check>"#,
        );

        assert!(body.contains("Width &amp; &quot;height&quot; &lt;check&gt;"));
    }
}
