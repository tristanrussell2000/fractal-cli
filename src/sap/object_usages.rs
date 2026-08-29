//! Where-used retrieval.

use reqwest::header::{HeaderMap, HeaderValue};
use thiserror::Error;

use super::{
    adt_object_uri::{AdtObjectUriError, SOURCE_SUFFIX, validate_adt_object_uri},
    adt_response::{AdtResponseParseError, parse_adt_document},
    client::{SapClient, SapClientError},
    find_child, non_empty_attribute,
    repository_kind::AdtObjectType,
};

const USAGES_PATH: &str = "/sap/bc/adt/repository/informationsystem/usageReferences";
const USAGES_CONTENT_TYPE: &str =
    "application/vnd.sap.adt.repository.usagereferences.request.v1+xml";

/// A failure while reading where-used references.
#[derive(Debug, Error)]
pub enum ObjectUsagesError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error(transparent)]
    Uri(#[from] AdtObjectUriError),
    #[error(transparent)]
    Parse(#[from] AdtResponseParseError),
}

impl ObjectUsagesError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Uri(error) => error.code(),
            Self::Parse(error) => error.code(),
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Uri(error) => error.hint(),
            Self::Parse(error) => error.hint(),
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) => Some(error),
            _ => None,
        }
    }

    /// A read-only command that diagnoses this failure, if one exists.
    #[must_use]
    pub fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.suggested_command(),
            Self::Uri(_) | Self::Parse(_) => None,
        }
    }
}

/// One row from an ADT where-used ("usage references") response.
///
/// SAP returns a flat hierarchy: `direct_result` rows are genuine direct usages;
/// the rest are breadcrumb context (the containing object, method, or
/// package) that Eclipse renders as a tree. `name`/`object_type`/`package`
/// are `None` when SAP omits them for a row (observed for method-level
/// nodes, which carry only a name).
#[derive(Debug, Clone)]
pub struct UsageReference {
    pub uri: String,
    pub parent_uri: Option<String>,
    pub name: Option<String>,
    pub object_type: Option<AdtObjectType>,
    pub package: Option<String>,
    pub direct_result: bool,
}

/// Fetches where-used ("usage references") for an ADT object URI.
///
/// Returns every referencing row SAP reports, including non-result hierarchy
/// context (containing object, method, package); see [`UsageReference`] for
/// how to tell those apart from direct hits. Self-references are stripped.
///
/// # Errors
///
/// Returns [`ObjectUsagesError::InvalidUri`] for a non-ADT URI, the underlying SAP
/// error when the request fails, or [`ObjectUsagesError::Parse`] when the response is
/// not valid XML.
pub async fn get_object_usages(
    sap: &mut SapClient,
    uri: &str,
) -> Result<Vec<UsageReference>, ObjectUsagesError> {
    validate_adt_object_uri(uri)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Type",
        HeaderValue::from_static(USAGES_CONTENT_TYPE),
    );
    let body = usage_request_body(uri);
    let xml = sap
        .post_text(USAGES_PATH, &[("uri", uri)], Some(&body), headers)
        .await?;
    parse_usage_references(&xml, uri)
}

/// Builds the `usageReferenceRequest` XML body. SAP requires the target URI in
/// both the `?uri=` query parameter (a plain GET-style param, 400s without it)
/// and this body; the query param alone takes a much slower code path on SAP's
/// side for some object kinds.
fn usage_request_body(uri: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<usagereferences:usageReferenceRequest xmlns:usagereferences=\"http://www.sap.com/adt/ris/usageReferences\" xmlns:adtcore=\"http://www.sap.com/adt/core\">\n\
  <usagereferences:affectedObjects>\n\
    <adtcore:objectReference adtcore:uri=\"{}\"/>\n\
  </usagereferences:affectedObjects>\n\
</usagereferences:usageReferenceRequest>",
        xml_escape_attribute(uri)
    )
}

fn xml_escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

fn parse_usage_references(
    xml: &str,
    target_uri: &str,
) -> Result<Vec<UsageReference>, ObjectUsagesError> {
    let document = parse_adt_document(xml)?;
    let target = normalize_reference_uri(target_uri);

    Ok(document
        .descendants()
        .filter(|node| node.tag_name().name() == "referencedObject")
        .filter_map(|node| {
            let uri = node.attribute("uri")?.to_owned();
            if normalize_reference_uri(&uri) == target {
                return None;
            }

            let adt_object = find_child(node, "adtObject");
            let name = adt_object.and_then(|n| non_empty_attribute(n.attribute("name")));
            let object_type = adt_object
                .and_then(|n| n.attribute("type"))
                .map(AdtObjectType::parse);
            let package = adt_object
                .and_then(|n| find_child(n, "packageRef"))
                .and_then(|n| non_empty_attribute(n.attribute("name")));

            Some(UsageReference {
                uri,
                parent_uri: non_empty_attribute(node.attribute("parentUri")),
                name,
                object_type,
                package,
                direct_result: node.attribute("isResult") == Some("true"),
            })
        })
        .collect())
}

/// Strips a `#fragment` and a trailing `/source/main`/`/` so a reference URI
/// can be compared against the target URI regardless of which form SAP used.
fn normalize_reference_uri(uri: &str) -> &str {
    let without_fragment = uri.split('#').next().unwrap_or(uri);
    without_fragment
        .strip_suffix(SOURCE_SUFFIX)
        .unwrap_or(without_fragment)
        .trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures below are trimmed from real `usageReferences` responses captured
    // against a live DE3 system (empty-result and multi-row cases), not invented.

    #[test]
    fn parses_a_response_with_no_referenced_objects() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><usageReferences:usageReferenceResult numberOfResults="0" resultDescription="[DE3] Where-Used List: ZDLTS_PB_IS_REPLY (Data Element)" referencedObjectIdentifier="" xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences"><usageReferences:referencedObjects/></usageReferences:usageReferenceResult>"#;
        let refs =
            parse_usage_references(xml, "/sap/bc/adt/ddic/dataelements/zdlts_pb_is_reply").unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn parses_referenced_objects_and_flags_direct_results() {
        let xml = r#"<usageReferences:usageReferenceResult xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences" numberOfResults="1" resultDescription="[DE3] Where-Used List: ZDTLS_CHECK_IN (Database Table)" referencedObjectIdentifier="">
            <usageReferences:referencedObjects>
                <usageReferences:referencedObject uri="/sap/bc/adt/ddic/structures/zdtls_check_in_s" parentUri="/sap/bc/adt/packages/zdtls" isResult="true" canHaveChildren="false">
                    <usageReferences:adtObject xmlns:adtcore="http://www.sap.com/adt/core" adtcore:responsible="TKIRKLAND" adtcore:name="ZDTLS_CHECK_IN_S" adtcore:type="TABL/DS">
                        <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zdtls" adtcore:type="DEVC/K" adtcore:name="ZDTLS"/>
                    </usageReferences:adtObject>
                </usageReferences:referencedObject>
                <usageReferences:referencedObject uri="/sap/bc/adt/oo/classes/zcl_zdtls_pb_gw_dpc_ext/source/main#type=CLAS%2FOM;name=CHECKINSET_CREATE_ENTITY;start=1" parentUri="/sap/bc/adt/oo/classes/zcl_zdtls_pb_gw_dpc_ext" isResult="false" canHaveChildren="true">
                    <usageReferences:adtObject xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="CHECKINSET_CREATE_ENTITY">
                        <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zdtls_pb" adtcore:type="DEVC/K" adtcore:name="ZDTLS_PB"/>
                    </usageReferences:adtObject>
                </usageReferences:referencedObject>
                <usageReferences:referencedObject uri="/sap/bc/adt/packages/zdtls" isResult="false" canHaveChildren="true">
                    <usageReferences:adtObject xmlns:adtcore="http://www.sap.com/adt/core" adtcore:responsible="TKIRKLAND" adtcore:name="ZDTLS" adtcore:type="DEVC/K">
                        <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zdtls" adtcore:type="DEVC/K" adtcore:name="ZDTLS"/>
                    </usageReferences:adtObject>
                </usageReferences:referencedObject>
                <usageReferences:referencedObject uri="/sap/bc/adt/ddic/tables/zdtls_check_in" isResult="false" canHaveChildren="false"/>
            </usageReferences:referencedObjects>
        </usageReferences:usageReferenceResult>"#;

        let refs = parse_usage_references(xml, "/sap/bc/adt/ddic/tables/zdtls_check_in").unwrap();

        // The fourth row is a self-reference and must be stripped.
        assert_eq!(refs.len(), 3);

        let direct: Vec<_> = refs.iter().filter(|r| r.direct_result).collect();
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].name.as_deref(), Some("ZDTLS_CHECK_IN_S"));
        assert_eq!(
            direct[0].object_type.as_ref().map(AdtObjectType::as_str),
            Some("TABL/DS")
        );
        assert_eq!(direct[0].package.as_deref(), Some("ZDTLS"));
        assert_eq!(
            direct[0].parent_uri.as_deref(),
            Some("/sap/bc/adt/packages/zdtls")
        );

        let method_row = refs
            .iter()
            .find(|r| r.uri.contains("CHECKINSET_CREATE_ENTITY"))
            .unwrap();
        assert!(!method_row.direct_result);
        assert_eq!(method_row.name.as_deref(), Some("CHECKINSET_CREATE_ENTITY"));
        assert!(method_row.object_type.is_none());

        let package_row = refs
            .iter()
            .find(|r| r.uri == "/sap/bc/adt/packages/zdtls")
            .unwrap();
        assert!(package_row.parent_uri.is_none());
    }

    #[test]
    fn normalizes_reference_uris_for_self_match_comparison() {
        assert_eq!(
            normalize_reference_uri("/sap/bc/adt/oo/classes/zcl_test/source/main"),
            "/sap/bc/adt/oo/classes/zcl_test"
        );
        assert_eq!(
            normalize_reference_uri("/sap/bc/adt/oo/classes/zcl_test#start=1"),
            "/sap/bc/adt/oo/classes/zcl_test"
        );
        assert_eq!(
            normalize_reference_uri("/sap/bc/adt/oo/classes/zcl_test/"),
            "/sap/bc/adt/oo/classes/zcl_test"
        );
    }

    #[test]
    fn returns_a_parse_error_for_malformed_usage_xml() {
        let error = parse_usage_references("<not-closed", "/uri").unwrap_err();
        assert_eq!(error.code(), "adt_response_parse_error");
    }

    #[test]
    fn escapes_special_characters_in_the_request_body_uri() {
        let body = usage_request_body(r#"/sap/bc/adt/uri?a=1&b="x"<y"#);
        assert!(body.contains(r#"adtcore:uri="/sap/bc/adt/uri?a=1&amp;b=&quot;x&quot;&lt;y""#));
    }
}
