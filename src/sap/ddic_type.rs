//! DDIC data-element and domain inspection.
//!
//! A field's declared type names a data element, which either carries a
//! predefined ABAP type directly or delegates to a domain. Everything a caller
//! usually wants — fixed values, a value table, a conversion exit — lives on
//! the domain, so this module follows that link and reports one merged view.
//!
//! Reads only, and deliberately not restricted to the customer namespaces: a
//! customer data element almost always points at an SAP-standard domain, and
//! refusing to read it would make the resolution useless.

use serde::Serialize;
use thiserror::Error;

use super::{
    adt_response::{AdtResponseParseError, parse_adt_document},
    client::{SapClient, SapClientError},
    editable_source::validate_object_name,
    find_child, find_non_empty_attribute,
    metadata_object::MetadataAdtObjectType,
};
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::suggested_command;

/// What a data element's `typeKind` says it is typed by.
///
/// SAP spells these in camel case inside the element; the open variant keeps
/// an unrecognized kind readable instead of silently reporting it as one of
/// the two known ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "name")]
pub enum DataElementTypeSource {
    Domain(String),
    PredefinedAbapType,
    Other(String),
}

impl DataElementTypeSource {
    fn parse(type_kind: Option<&str>, type_name: Option<&str>) -> Self {
        match (type_kind, type_name) {
            (Some("domain"), Some(name)) => Self::Domain(name.to_owned()),
            (Some("predefinedAbapType"), _) => Self::PredefinedAbapType,
            (Some(kind), _) => Self::Other(kind.to_owned()),
            (None, _) => Self::Other(String::new()),
        }
    }

    /// The domain this data element delegates to, if it delegates at all.
    #[must_use]
    pub fn domain_name(&self) -> Option<&str> {
        match self {
            Self::Domain(name) => Some(name),
            Self::PredefinedAbapType | Self::Other(_) => None,
        }
    }
}

/// The ABAP type a caller ends up with, wherever it was declared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EffectiveType {
    pub data_type: Option<String>,
    pub length: Option<u32>,
    pub decimals: Option<u32>,
}

/// A named object the DDIC metadata points at, with its ADT URI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DdicObjectRef {
    pub name: String,
    pub uri: Option<String>,
}

/// One entry of a domain's fixed-value list. `high` is set only for an
/// interval; a single value leaves it empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DomainFixedValue {
    pub position: Option<u32>,
    pub low: String,
    pub high: Option<String>,
    pub text: Option<String>,
}

/// The parts of a data element that are not on its domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataElementInfo {
    pub type_source: DataElementTypeSource,
    pub short_label: Option<String>,
    pub medium_label: Option<String>,
    pub long_label: Option<String>,
    pub heading_label: Option<String>,
    pub search_help: Option<String>,
    pub search_help_parameter: Option<String>,
    pub set_get_parameter: Option<String>,
    pub change_document: bool,
}

/// A domain: the type itself, its output formatting, and its value range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DomainInfo {
    pub name: String,
    pub uri: String,
    pub description: Option<String>,
    pub package: Option<String>,
    pub data_type: Option<String>,
    pub length: Option<u32>,
    pub decimals: Option<u32>,
    pub output_length: Option<u32>,
    pub conversion_exit: Option<String>,
    pub lowercase: bool,
    pub sign_exists: bool,
    pub value_table: Option<DdicObjectRef>,
    pub fixed_values: Vec<DomainFixedValue>,
}

/// One inspected DDIC type. `data_element` is absent when a domain was asked
/// for directly; `domain` is absent when the data element carries a predefined
/// type, or when resolution was turned off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DdicTypeInfo {
    pub name: String,
    pub kind: &'static str,
    pub uri: String,
    pub description: Option<String>,
    pub package: Option<String>,
    pub effective_type: EffectiveType,
    pub data_element: Option<DataElementInfo>,
    pub domain: Option<DomainInfo>,
}

/// How to look a name up.
#[derive(Debug, Clone, Default)]
pub struct DdicTypeOptions {
    /// The type to request. `None` tries a data element first, then a domain.
    pub object_type: Option<MetadataAdtObjectType>,
    /// Whether to follow a data element's domain reference.
    pub resolve_domain: bool,
}

/// A failure while inspecting a DDIC type.
#[derive(Debug, Error)]
pub enum DdicTypeError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error(transparent)]
    Parse(#[from] AdtResponseParseError),
    #[error("invalid DDIC object name '{0}'")]
    InvalidName(String),
    #[error("'{0}' is neither a data element nor a domain")]
    NotFound(String),
    #[error("unsupported DDIC type '{0}' for inspection")]
    UnsupportedType(String),
    #[error("data element '{data_element}' names domain '{domain}', which does not exist")]
    DomainMissing {
        data_element: String,
        domain: String,
    },
}

impl DdicTypeError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) => Some(error),
            _ => None,
        }
    }
}

impl ReportableError for DdicTypeError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(error) => error.code(),
            Self::InvalidName(_) => "invalid_object_name",
            Self::NotFound(_) => "ddic_type_not_found",
            Self::UnsupportedType(_) => "unsupported_ddic_type",
            Self::DomainMissing { .. } => "ddic_domain_missing",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Sap(error) => error.hint()?,
            Self::Parse(error) => error.hint()?,
            Self::InvalidName(_) => {
                "Use a DDIC name containing letters, digits, or underscores, optionally in the form /NAMESPACE/NAME."
                    .to_owned()
            }
            Self::NotFound(_) => {
                "This command inspects data elements and domains. Search for the name to see what it actually is, or pass --type to skip detection."
                    .to_owned()
            }
            Self::UnsupportedType(_) => {
                "Pass --type DTEL for a data element or --type DOMA for a domain.".to_owned()
            }
            Self::DomainMissing { .. } => {
                "The data element itself was read. Re-run with --no-resolve to see it without its domain, then check why the domain is missing."
                    .to_owned()
            }
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::NotFound(name) => Some(suggested_command::object_search("DTEL", name)),
            Self::DomainMissing { domain, .. } => {
                Some(suggested_command::object_search("DOMA", domain))
            }
            Self::Sap(error) => error.suggested_command(),
            Self::Parse(_) | Self::InvalidName(_) | Self::UnsupportedType(_) => None,
        }
    }
}

/// Reads a data element or domain and, for a domain-typed data element,
/// the domain behind it.
///
/// With no explicit type, a data element is tried first and a domain second.
/// That order matters for cost, not correctness: a field's declared type is a
/// data element far more often than a domain, and the two namespaces are
/// separate, so a name can legitimately exist in both.
///
/// # Errors
///
/// Returns [`DdicTypeError::InvalidName`] before any request for a malformed
/// name, [`DdicTypeError::NotFound`] when detection finds neither, the
/// underlying SAP error for any other failed request,
/// [`DdicTypeError::DomainMissing`] when a referenced domain cannot be read,
/// or [`DdicTypeError::Parse`] for a response that is not valid XML.
pub async fn get_ddic_type(
    sap: &mut SapClient,
    name: &str,
    options: &DdicTypeOptions,
) -> Result<DdicTypeInfo, DdicTypeError> {
    let name =
        validate_object_name(name).map_err(|_| DdicTypeError::InvalidName(name.to_owned()))?;

    let (object_type, xml) = match options.object_type {
        Some(object_type) => {
            supported_type(object_type)?;
            let xml = sap.get_text(&ddic_object_uri(object_type, &name)).await?;
            (object_type, xml)
        }
        None => detect_and_read(sap, &name).await?,
    };

    match object_type {
        MetadataAdtObjectType::DataElement => {
            let mut info = parse_data_element(&xml, &name)?;
            if options.resolve_domain
                && let Some(domain) = info
                    .data_element
                    .as_ref()
                    .and_then(|element| element.type_source.domain_name())
                    .map(str::to_owned)
            {
                info.domain = Some(read_domain(sap, &domain, &name).await?);
            }
            Ok(info)
        }
        MetadataAdtObjectType::Domain => parse_domain_object(&xml, &name),
        unsupported => Err(DdicTypeError::UnsupportedType(
            unsupported.as_str().to_owned(),
        )),
    }
}

fn supported_type(object_type: MetadataAdtObjectType) -> Result<(), DdicTypeError> {
    match object_type {
        MetadataAdtObjectType::DataElement | MetadataAdtObjectType::Domain => Ok(()),
        unsupported => Err(DdicTypeError::UnsupportedType(
            unsupported.as_str().to_owned(),
        )),
    }
}

/// Tries a data element, then a domain. Only a 404 moves on to the next
/// candidate: any other failure is the real answer and must not be reported as
/// "neither".
async fn detect_and_read(
    sap: &mut SapClient,
    name: &str,
) -> Result<(MetadataAdtObjectType, String), DdicTypeError> {
    let element_uri = ddic_object_uri(MetadataAdtObjectType::DataElement, name);
    match sap.get_text(&element_uri).await {
        Ok(xml) => return Ok((MetadataAdtObjectType::DataElement, xml)),
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }

    let domain_uri = ddic_object_uri(MetadataAdtObjectType::Domain, name);
    match sap.get_text(&domain_uri).await {
        Ok(xml) => Ok((MetadataAdtObjectType::Domain, xml)),
        Err(error) if error.is_not_found() => Err(DdicTypeError::NotFound(name.to_owned())),
        Err(error) => Err(error.into()),
    }
}

async fn read_domain(
    sap: &mut SapClient,
    domain: &str,
    data_element: &str,
) -> Result<DomainInfo, DdicTypeError> {
    let uri = ddic_object_uri(MetadataAdtObjectType::Domain, domain);
    let xml = match sap.get_text(&uri).await {
        Ok(xml) => xml,
        Err(error) if error.is_not_found() => {
            return Err(DdicTypeError::DomainMissing {
                data_element: data_element.to_owned(),
                domain: domain.to_owned(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    parse_domain(&xml, domain)
}

/// Builds the ADT URI for a DDIC name. Registered namespaces are percent-
/// encoded so `/ACME/SAMPLE_TEXT` stays one path segment.
fn ddic_object_uri(object_type: MetadataAdtObjectType, name: &str) -> String {
    let path_name = name.to_ascii_lowercase().replace('/', "%2f");
    format!("{}/{path_name}", object_type.collection_path())
}

fn parse_data_element(xml: &str, name: &str) -> Result<DdicTypeInfo, DdicTypeError> {
    let document = parse_adt_document(xml)?;
    let root = document.root_element();
    let element = find_child(root, "dataElement");

    let data_element = element.map(|element| DataElementInfo {
        type_source: DataElementTypeSource::parse(
            child_text(element, "typeKind").as_deref(),
            child_text(element, "typeName").as_deref(),
        ),
        short_label: child_text(element, "shortFieldLabel"),
        medium_label: child_text(element, "mediumFieldLabel"),
        long_label: child_text(element, "longFieldLabel"),
        heading_label: child_text(element, "headingFieldLabel"),
        search_help: child_text(element, "searchHelp"),
        search_help_parameter: child_text(element, "searchHelpParameter"),
        set_get_parameter: child_text(element, "setGetParameter"),
        change_document: child_flag(element, "changeDocument"),
    });

    Ok(DdicTypeInfo {
        name: name.to_owned(),
        kind: MetadataAdtObjectType::DataElement.as_str(),
        uri: ddic_object_uri(MetadataAdtObjectType::DataElement, name),
        description: find_non_empty_attribute(root, "description"),
        package: package_name(root),
        effective_type: EffectiveType {
            data_type: element.and_then(|element| child_text(element, "dataType")),
            length: element.and_then(|element| child_number(element, "dataTypeLength")),
            decimals: element.and_then(|element| child_number(element, "dataTypeDecimals")),
        },
        data_element,
        domain: None,
    })
}

fn parse_domain_object(xml: &str, name: &str) -> Result<DdicTypeInfo, DdicTypeError> {
    let domain = parse_domain(xml, name)?;
    Ok(DdicTypeInfo {
        name: domain.name.clone(),
        kind: MetadataAdtObjectType::Domain.as_str(),
        uri: domain.uri.clone(),
        description: domain.description.clone(),
        package: domain.package.clone(),
        effective_type: EffectiveType {
            data_type: domain.data_type.clone(),
            length: domain.length,
            decimals: domain.decimals,
        },
        data_element: None,
        domain: Some(domain),
    })
}

fn parse_domain(xml: &str, name: &str) -> Result<DomainInfo, DdicTypeError> {
    let document = parse_adt_document(xml)?;
    let root = document.root_element();
    let content = find_child(root, "content");
    let type_information = content.and_then(|content| find_child(content, "typeInformation"));
    let output = content.and_then(|content| find_child(content, "outputInformation"));
    let values = content.and_then(|content| find_child(content, "valueInformation"));

    Ok(DomainInfo {
        name: name.to_owned(),
        uri: ddic_object_uri(MetadataAdtObjectType::Domain, name),
        description: find_non_empty_attribute(root, "description"),
        package: package_name(root),
        data_type: type_information.and_then(|node| child_text(node, "datatype")),
        length: type_information.and_then(|node| child_number(node, "length")),
        decimals: type_information.and_then(|node| child_number(node, "decimals")),
        output_length: output.and_then(|node| child_number(node, "length")),
        conversion_exit: output.and_then(|node| child_text(node, "conversionExit")),
        lowercase: output.is_some_and(|node| child_flag(node, "lowercase")),
        sign_exists: output.is_some_and(|node| child_flag(node, "signExists")),
        value_table: values
            .and_then(|node| find_child(node, "valueTableRef"))
            .and_then(object_ref),
        fixed_values: values
            .and_then(|node| find_child(node, "fixValues"))
            .map(fixed_values)
            .unwrap_or_default(),
    })
}

fn fixed_values(node: roxmltree::Node) -> Vec<DomainFixedValue> {
    node.children()
        .filter(|child| child.is_element() && child.tag_name().name() == "fixValue")
        .filter_map(|child| {
            Some(DomainFixedValue {
                position: child_number(child, "position"),
                low: child_text(child, "low")?,
                high: child_text(child, "high"),
                text: child_text(child, "text"),
            })
        })
        .collect()
}

fn object_ref(node: roxmltree::Node) -> Option<DdicObjectRef> {
    Some(DdicObjectRef {
        name: find_non_empty_attribute(node, "name")?,
        uri: find_non_empty_attribute(node, "uri"),
    })
}

fn package_name(root: roxmltree::Node) -> Option<String> {
    find_non_empty_attribute(find_child(root, "packageRef")?, "name")
}

/// Text content of a child element, treating blank and self-closed as absent.
/// SAP writes `<dtel:searchHelp/>` rather than omitting the element.
fn child_text(node: roxmltree::Node, name: &str) -> Option<String> {
    let text = find_child(node, name)?.text()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// A zero-padded DDIC number such as `000016`.
fn child_number(node: roxmltree::Node, name: &str) -> Option<u32> {
    child_text(node, name)?.parse().ok()
}

fn child_flag(node: roxmltree::Node, name: &str) -> bool {
    child_text(node, name).is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOMAIN_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<doma:domain adtcore:name="ZSAMPLE_STATUS_DOM" adtcore:type="DOMA/DD" adtcore:description="Status"
    xmlns:doma="http://www.sap.com/dictionary/domain" xmlns:adtcore="http://www.sap.com/adt/core">
  <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zpkg" adtcore:type="DEVC/K" adtcore:name="ZPKG"/>
  <doma:content>
    <doma:typeInformation><doma:datatype>NUMC</doma:datatype><doma:length>000002</doma:length><doma:decimals>000000</doma:decimals></doma:typeInformation>
    <doma:outputInformation><doma:length>000002</doma:length><doma:style>00</doma:style><doma:conversionExit/><doma:signExists>false</doma:signExists><doma:lowercase>true</doma:lowercase><doma:ampmFormat>false</doma:ampmFormat></doma:outputInformation>
    <doma:valueInformation>
      <doma:valueTableRef adtcore:uri="/sap/bc/adt/ddic/tables/zsample_values" adtcore:type="TABL/DT" adtcore:name="ZSAMPLE_VALUES"/>
      <doma:appendExists>false</doma:appendExists>
      <doma:fixValues>
        <doma:fixValue><doma:position>0001</doma:position><doma:low>01</doma:low><doma:high/><doma:text>Optional</doma:text></doma:fixValue>
        <doma:fixValue><doma:position>0002</doma:position><doma:low>02</doma:low><doma:high>09</doma:high><doma:text>Mandatory</doma:text></doma:fixValue>
      </doma:fixValues>
    </doma:valueInformation>
  </doma:content>
</doma:domain>"#;

    fn data_element_xml(type_kind: &str, type_name: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<blue:wbobj adtcore:name="ZSAMPLE_FIELD" adtcore:type="DTEL/DE" adtcore:description="A sample field"
    xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:adtcore="http://www.sap.com/adt/core">
  <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zpkg" adtcore:type="DEVC/K" adtcore:name="ZPKG"/>
  <dtel:dataElement xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements">
    <dtel:typeKind>{type_kind}</dtel:typeKind>
    <dtel:typeName>{type_name}</dtel:typeName>
    <dtel:dataType>NUMC</dtel:dataType>
    <dtel:dataTypeLength>000002</dtel:dataTypeLength>
    <dtel:dataTypeDecimals>000000</dtel:dataTypeDecimals>
    <dtel:shortFieldLabel>Status</dtel:shortFieldLabel>
    <dtel:mediumFieldLabel>Status type</dtel:mediumFieldLabel>
    <dtel:longFieldLabel>Sample status label</dtel:longFieldLabel>
    <dtel:headingFieldLabel>Sample status</dtel:headingFieldLabel>
    <dtel:searchHelp/>
    <dtel:setGetParameter/>
    <dtel:changeDocument>true</dtel:changeDocument>
  </dtel:dataElement>
</blue:wbobj>"#
        )
    }

    #[test]
    fn reads_a_domain_typed_data_element() {
        let info = parse_data_element(
            &data_element_xml("domain", "ZSAMPLE_STATUS_DOM"),
            "ZSAMPLE_FIELD",
        )
        .expect("parses");
        let element = info.data_element.expect("has data element detail");

        assert_eq!(info.kind, "DTEL");
        assert_eq!(info.uri, "/sap/bc/adt/ddic/dataelements/zsample_field");
        assert_eq!(info.description.as_deref(), Some("A sample field"));
        assert_eq!(info.package.as_deref(), Some("ZPKG"));
        assert_eq!(info.effective_type.data_type.as_deref(), Some("NUMC"));
        assert_eq!(info.effective_type.length, Some(2));
        assert_eq!(info.effective_type.decimals, Some(0));
        assert_eq!(
            element.type_source,
            DataElementTypeSource::Domain("ZSAMPLE_STATUS_DOM".to_owned())
        );
        assert_eq!(
            element.type_source.domain_name(),
            Some("ZSAMPLE_STATUS_DOM")
        );
        assert_eq!(element.long_label.as_deref(), Some("Sample status label"));
        assert!(element.change_document);
        // Self-closed elements mean "no value", not an empty string.
        assert_eq!(element.search_help, None);
        assert_eq!(element.set_get_parameter, None);
    }

    #[test]
    fn a_predefined_type_names_no_domain() {
        let info = parse_data_element(&data_element_xml("predefinedAbapType", ""), "ZSAMPLE_FIELD")
            .expect("parses");
        let element = info.data_element.expect("has data element detail");

        assert_eq!(
            element.type_source,
            DataElementTypeSource::PredefinedAbapType
        );
        assert_eq!(element.type_source.domain_name(), None);
        assert_eq!(info.effective_type.data_type.as_deref(), Some("NUMC"));
    }

    #[test]
    fn an_unrecognized_type_kind_is_preserved_rather_than_guessed() {
        let info = parse_data_element(
            &data_element_xml("referenceType", "IF_SAMPLE"),
            "ZSAMPLE_FIELD",
        )
        .expect("parses");
        let element = info.data_element.expect("has data element detail");

        assert_eq!(
            element.type_source,
            DataElementTypeSource::Other("referenceType".to_owned())
        );
        // An unknown kind must not send us looking for a domain that is not one.
        assert_eq!(element.type_source.domain_name(), None);
    }

    #[test]
    fn reads_a_domain_with_fixed_values_and_a_value_table() {
        let domain = parse_domain(DOMAIN_XML, "ZSAMPLE_STATUS_DOM").expect("parses");

        assert_eq!(domain.uri, "/sap/bc/adt/ddic/domains/zsample_status_dom");
        assert_eq!(domain.data_type.as_deref(), Some("NUMC"));
        assert_eq!(domain.length, Some(2));
        assert_eq!(domain.output_length, Some(2));
        assert_eq!(domain.conversion_exit, None);
        assert!(domain.lowercase);
        assert!(!domain.sign_exists);
        assert_eq!(
            domain.value_table,
            Some(DdicObjectRef {
                name: "ZSAMPLE_VALUES".to_owned(),
                uri: Some("/sap/bc/adt/ddic/tables/zsample_values".to_owned()),
            })
        );
        assert_eq!(domain.fixed_values.len(), 2);
        assert_eq!(domain.fixed_values[0].position, Some(1));
        assert_eq!(domain.fixed_values[0].low, "01");
        assert_eq!(domain.fixed_values[0].high, None);
        assert_eq!(domain.fixed_values[0].text.as_deref(), Some("Optional"));
        // An interval keeps its upper bound.
        assert_eq!(domain.fixed_values[1].high.as_deref(), Some("09"));
    }

    #[test]
    fn a_domain_read_directly_reports_its_own_type_as_effective() {
        let info = parse_domain_object(DOMAIN_XML, "ZSAMPLE_STATUS_DOM").expect("parses");

        assert_eq!(info.kind, "DOMA");
        assert_eq!(info.data_element, None);
        assert_eq!(info.effective_type.data_type.as_deref(), Some("NUMC"));
        assert_eq!(
            info.domain.expect("has domain detail").fixed_values.len(),
            2
        );
    }

    #[test]
    fn registered_namespaces_stay_one_path_segment() {
        assert_eq!(
            ddic_object_uri(MetadataAdtObjectType::Domain, "/ACME/SAMPLE_TEXT"),
            "/sap/bc/adt/ddic/domains/%2facme%2fsample_text"
        );
    }

    #[test]
    fn a_domain_without_fixed_values_reports_an_empty_list() {
        let xml = r#"<doma:domain xmlns:doma="urn:d" xmlns:adtcore="urn:a"><doma:content>
            <doma:typeInformation><doma:datatype>STRING</doma:datatype><doma:length>000000</doma:length></doma:typeInformation>
            <doma:valueInformation><doma:valueTableRef/><doma:fixValues/></doma:valueInformation>
        </doma:content></doma:domain>"#;
        let domain = parse_domain(xml, "ZEMPTY").expect("parses");

        assert!(domain.fixed_values.is_empty());
        assert_eq!(domain.value_table, None);
    }

    #[test]
    fn malformed_xml_is_a_parse_error() {
        let error = parse_domain("<not-closed", "ZX").unwrap_err();
        assert_eq!(error.code(), "adt_response_parse_error");
    }
}
