//! The fields, associations and parameters of a CDS entity.
//!
//! A view entity has no `DD03L` rows, so the structure reader cannot list it.
//! ADT's element info (the read behind Eclipse's element-info popup) describes
//! the active entity instead: each field's key flag, type and reference, each
//! association's target, cardinality and kind, and each parameter.
//!
//! An entity that does not exist and one that was never activated get the same
//! answer, an empty `elementInfo`, so the DDL source's own document is read to
//! tell the two apart.

use serde::Serialize;
use thiserror::Error;

use super::{
    client::{SapClient, SapClientError},
    editable_source::{EditableAdtObjectType, editable_source_identity},
    find_attribute_value, find_non_empty_attribute,
};
use crate::reportable_error::{ReportableError, sap_http_status};

const ELEMENT_INFO_PATH: &str = "/sap/bc/adt/ddic/ddl/elementinfo";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CdsEntityInfo {
    /// As the entity defines it, which may be mixed case.
    pub name: String,
    pub kind: &'static str,
    /// SAP's word for the entity: `view`, `projectionView`, and others not yet
    /// seen are passed through.
    pub entity_type: String,
    pub root: bool,
    pub uri: Option<String>,
    /// Element info describes the active entity only.
    pub version: &'static str,
    pub description: Option<String>,
    pub package: Option<String>,
    pub field_count: usize,
    pub key_field_count: usize,
    pub fields: Vec<CdsField>,
    pub associations: Vec<CdsAssociation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<CdsParameter>,
    /// Present only when annotations were asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<CdsAnnotation>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CdsField {
    pub name: String,
    pub is_key: bool,
    /// The built-in type, upper case: `CHAR`, `CURR`, `DEC`.
    pub data_type: Option<String>,
    pub length: Option<u32>,
    pub decimals: Option<u32>,
    pub data_element: Option<String>,
    /// The currency or unit field an amount or quantity refers to.
    pub reference_field: Option<String>,
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<CdsAnnotation>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CdsAssociation {
    pub name: String,
    pub target: Option<String>,
    /// `min..max`, as written in CDS.
    pub cardinality: Option<String>,
    /// `association`, `composition` or `to_parent`; an unrecognized kind is
    /// reported as `kind <number>` rather than guessed at.
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CdsParameter {
    pub name: String,
    pub data_type: Option<String>,
    pub length: Option<u32>,
    pub decimals: Option<u32>,
    pub data_element: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CdsAnnotation {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Error)]
pub enum CdsEntityError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error("SAP returned malformed element info: {0}")]
    Parse(#[from] roxmltree::Error),
    #[error("invalid CDS entity name '{0}'")]
    InvalidName(String),
    #[error("'{0}' is not a CDS entity")]
    NotFound(String),
    #[error("CDS entity '{0}' has never been activated, so it has no field list yet")]
    NotActive(String),
    #[error("'{name}' is a {adt_type}, not a CDS entity")]
    NotCds { name: String, adt_type: String },
    #[error("a CDS entity's field list describes its active version only")]
    InactiveNotServed,
}

impl CdsEntityError {
    #[must_use]
    pub const fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }
}

impl ReportableError for CdsEntityError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(_) => "cds_entity_response_invalid",
            Self::InvalidName(_) => "invalid_object_name",
            Self::NotFound(_) => "cds_entity_not_found",
            Self::NotActive(_) => "cds_entity_not_active",
            Self::NotCds { .. } => "not_a_cds_entity",
            Self::InactiveNotServed => "cds_inactive_version_not_served",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(match self {
            Self::Sap(error) => Some(error),
            _ => None,
        })
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Parse(_) => Some("The element info response was not valid ADT XML.".to_owned()),
            Self::InvalidName(_) => {
                Some("Use a DDIC name: letters, digits, underscore and slash only.".to_owned())
            }
            Self::NotFound(_) => Some(
                "Search for the name to see what it actually is: `fractal object search <NAME>`."
                    .to_owned(),
            ),
            Self::NotActive(name) => Some(format!(
                "Activate it first: `fractal edit activate --type DDLS --name {name}`. To see what is staged, read its source with `fractal edit read --type DDLS --name {name}`."
            )),
            Self::NotCds { .. } => Some(
                "Run `fractal object show` without --type to read it as what it is.".to_owned(),
            ),
            Self::InactiveNotServed => Some(
                "Leave out --version inactive, or activate the entity first and read it then."
                    .to_owned(),
            ),
        }
    }
}

/// Reads a CDS entity's fields, associations and parameters.
///
/// # Errors
///
/// Returns [`CdsEntityError::NotFound`] when there is no such DDL source,
/// [`CdsEntityError::NotActive`] when there is one but it was never activated,
/// and [`CdsEntityError::NotCds`] when the name is some other DDIC object.
pub async fn get_cds_entity(
    sap: &SapClient,
    name: &str,
    with_annotations: bool,
) -> Result<CdsEntityInfo, CdsEntityError> {
    let identity = editable_source_identity(EditableAdtObjectType::DdlSource, name)
        .map_err(|_| CdsEntityError::InvalidName(name.to_owned()))?;
    let response = sap
        .get_text_with_query(
            ELEMENT_INFO_PATH,
            &[
                ("path", identity.name.as_str()),
                ("getTargetForAssociation", "false"),
                ("getExtensionViews", "true"),
                ("getSecondaryObjects", "true"),
            ],
        )
        .await?;
    if let Some(info) = parse_element_info(&identity.name, &response, with_annotations)? {
        return Ok(info);
    }
    match sap.get_text(&identity.object_uri).await {
        Ok(_) => Err(CdsEntityError::NotActive(identity.name)),
        Err(error) if error.is_not_found() => Err(CdsEntityError::NotFound(identity.name)),
        Err(error) => Err(error.into()),
    }
}

/// `None` for the empty answer that means "no such active entity".
fn parse_element_info(
    requested: &str,
    response: &str,
    with_annotations: bool,
) -> Result<Option<CdsEntityInfo>, CdsEntityError> {
    let document = roxmltree::Document::parse(response)?;
    let root = document.root_element();
    let Some(name) = find_non_empty_attribute(root, "name") else {
        return Ok(None);
    };
    let adt_type = find_non_empty_attribute(root, "type").unwrap_or_default();
    if !adt_type.starts_with("DDLS") {
        return Err(CdsEntityError::NotCds {
            name: requested.to_owned(),
            adt_type: adt_type.split('/').next().unwrap_or_default().to_owned(),
        });
    }
    let properties = Properties::of(root);
    let mut fields = Vec::new();
    let mut associations = Vec::new();
    let mut parameters = Vec::new();
    let mut package = None;
    for element in children(root, "elementInfo") {
        let element_name = find_non_empty_attribute(element, "name").unwrap_or_default();
        let element_properties = Properties::of(element);
        match find_attribute_value(element, "type").unwrap_or_default() {
            "STOB/DOF" => fields.push(CdsField {
                name: element_name,
                is_key: element_properties.get("ddicIsKey") == Some("true"),
                data_type: element_properties.upper("ddicDataType"),
                length: element_properties.number("ddicLength"),
                decimals: element_properties.number("ddicDecimals"),
                data_element: element_properties.upper("ddicDataElement"),
                reference_field: element_properties.upper("ddicReferenceField"),
                description: short_text(element),
                annotations: with_annotations.then(|| element_properties.annotations()),
            }),
            "STOB/DOA" => associations.push(CdsAssociation {
                name: element_name,
                target: element_properties.text("targetEntity"),
                cardinality: cardinality(&element_properties),
                kind: association_kind(element_properties.get("associationKind")),
            }),
            "STOB/DOP" => parameters.push(CdsParameter {
                name: element_name,
                data_type: element_properties.upper("ddicDataType"),
                length: element_properties.number("ddicLength"),
                decimals: element_properties.number("ddicDecimals"),
                data_element: element_properties.upper("ddicDataElement"),
            }),
            "DEVC/K" => package = Some(element_name),
            _ => {}
        }
    }
    Ok(Some(CdsEntityInfo {
        name,
        kind: "DDLS",
        entity_type: properties.text("entityType").unwrap_or_default(),
        root: properties.get("isRootNode") == Some("true"),
        uri: find_non_empty_attribute(root, "uri"),
        version: "active",
        description: short_text(root),
        package,
        field_count: fields.len(),
        key_field_count: fields.iter().filter(|field| field.is_key).count(),
        fields,
        associations,
        parameters,
        annotations: with_annotations.then(|| properties.annotations()),
    }))
}

/// Refuses the inactive layer before any request: element info has none.
///
/// # Errors
///
/// Returns [`CdsEntityError::InactiveNotServed`] for an inactive request.
pub const fn require_active(version: super::adt_version::AdtVersion) -> Result<(), CdsEntityError> {
    match version {
        super::adt_version::AdtVersion::Active => Ok(()),
        super::adt_version::AdtVersion::Inactive => Err(CdsEntityError::InactiveNotServed),
    }
}

fn association_kind(kind: Option<&str>) -> String {
    match kind.map(str::trim) {
        Some("0") => "association".to_owned(),
        Some("1") => "composition".to_owned(),
        Some("6") => "to_parent".to_owned(),
        Some(other) if !other.is_empty() => format!("kind {other}"),
        _ => "association".to_owned(),
    }
}

fn cardinality(properties: &Properties) -> Option<String> {
    match (
        properties.text("cardinalityMin"),
        properties.text("cardinalityMax"),
    ) {
        (Some(min), Some(max)) => Some(format!("{min}..{max}")),
        _ => None,
    }
}

fn children<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &'a str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'input>> {
    node.children()
        .filter(move |child| child.is_element() && child.tag_name().name() == name)
}

fn short_text(element: roxmltree::Node<'_, '_>) -> Option<String> {
    children(element, "documentation")
        .find(|documentation| find_attribute_value(*documentation, "rel") == Some("shorttext"))
        .and_then(|documentation| documentation.text())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// An element's `properties` block: `entry` elements keyed by attribute.
struct Properties(Vec<(String, String)>);

impl Properties {
    fn of(element: roxmltree::Node<'_, '_>) -> Self {
        Self(
            children(element, "properties")
                .flat_map(|properties| children(properties, "entry"))
                .filter_map(|entry| {
                    let key = find_attribute_value(entry, "key")?;
                    Some((key.to_owned(), entry.text().unwrap_or_default().to_owned()))
                })
                .collect(),
        )
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    fn text(&self, key: &str) -> Option<String> {
        self.get(key)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }

    fn upper(&self, key: &str) -> Option<String> {
        self.text(key).map(|value| value.to_ascii_uppercase())
    }

    fn number(&self, key: &str) -> Option<u32> {
        self.get(key)?.trim().parse().ok()
    }

    /// `annotationKey.N` paired with `annotationValue.N`, in SAP's order.
    fn annotations(&self) -> Vec<CdsAnnotation> {
        self.0
            .iter()
            .filter_map(|(key, name)| {
                let index = key.strip_prefix("annotationKey.")?;
                Some(CdsAnnotation {
                    name: name.clone(),
                    value: self
                        .get(&format!("annotationValue.{index}"))
                        .unwrap_or_default()
                        .to_owned(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live answer for a child view entity, renamed.
    const CHILD: &str = r#"<?xml version="1.0" encoding="utf-8"?><abapsource:elementInfo adtcore:uri="/sap/bc/adt/ddic/ddl/sources/zsample_r_item" adtcore:type="DDLS/STOB" adtcore:name="ZSAMPLE_R_ITEM" xmlns:abapsource="http://www.sap.com/adt/abapsource" xmlns:adtcore="http://www.sap.com/adt/core"><abapsource:properties><abapsource:entry abapsource:key="isRootNode">false</abapsource:entry><abapsource:entry abapsource:key="entityType">view</abapsource:entry><abapsource:entry abapsource:key="clientSessionVarUsed">true</abapsource:entry><abapsource:entry abapsource:key="clientDependent">true</abapsource:entry><abapsource:entry abapsource:key="isBufferable">false</abapsource:entry><abapsource:entry abapsource:key="isBuffered">false</abapsource:entry><abapsource:entry abapsource:key="annotationKey.0">ACCESSCONTROL.AUTHORIZATIONCHECK</abapsource:entry><abapsource:entry abapsource:key="annotationValue.0">#NOT_REQUIRED</abapsource:entry><abapsource:entry abapsource:key="annotationKey.1">ENDUSERTEXT.LABEL</abapsource:entry><abapsource:entry abapsource:key="annotationValue.1">Sample item</abapsource:entry></abapsource:properties><abapsource:documentation abapsource:rel="shorttext" abapsource:type="text/plain">Sample item</abapsource:documentation><abapsource:elementInfo adtcore:type="STOB/DOF" adtcore:name="HeadId"><abapsource:properties><abapsource:entry abapsource:key="ddicIsKey">true</abapsource:entry><abapsource:entry abapsource:key="ddicDataType">char</abapsource:entry><abapsource:entry abapsource:key="ddicLength">000010</abapsource:entry></abapsource:properties></abapsource:elementInfo><abapsource:elementInfo adtcore:type="STOB/DOF" adtcore:name="ItemNo"><abapsource:properties><abapsource:entry abapsource:key="ddicIsKey">true</abapsource:entry><abapsource:entry abapsource:key="ddicDataType">numc</abapsource:entry><abapsource:entry abapsource:key="ddicLength">000004</abapsource:entry></abapsource:properties></abapsource:elementInfo><abapsource:elementInfo adtcore:type="STOB/DOF" adtcore:name="Amount"><abapsource:properties><abapsource:entry abapsource:key="ddicIsKey">false</abapsource:entry><abapsource:entry abapsource:key="ddicDataType">curr</abapsource:entry><abapsource:entry abapsource:key="ddicLength">000015</abapsource:entry><abapsource:entry abapsource:key="ddicDecimals">000002</abapsource:entry><abapsource:entry abapsource:key="ddicReferenceField">CURRENCY</abapsource:entry><abapsource:entry abapsource:key="ddicReferenceTable">ZSAMPLE_R_ITEM</abapsource:entry><abapsource:entry abapsource:key="annotationKey.0">SEMANTICS.AMOUNT.CURRENCYCODE</abapsource:entry><abapsource:entry abapsource:key="annotationValue.0">'Currency'</abapsource:entry></abapsource:properties></abapsource:elementInfo><abapsource:elementInfo adtcore:type="STOB/DOF" adtcore:name="Currency"><abapsource:properties><abapsource:entry abapsource:key="ddicIsKey">false</abapsource:entry><abapsource:entry abapsource:key="ddicDataType">cuky</abapsource:entry><abapsource:entry abapsource:key="ddicLength">000005</abapsource:entry></abapsource:properties></abapsource:elementInfo><abapsource:elementInfo adtcore:type="STOB/DOF" adtcore:name="CreatedAt"><abapsource:properties><abapsource:entry abapsource:key="ddicIsKey">false</abapsource:entry><abapsource:entry abapsource:key="ddicDataElement">timestampl</abapsource:entry><abapsource:entry abapsource:key="ddicDataType">dec</abapsource:entry><abapsource:entry abapsource:key="ddicLength">000021</abapsource:entry><abapsource:entry abapsource:key="ddicDecimals">000007</abapsource:entry></abapsource:properties><abapsource:documentation abapsource:rel="shorttext" abapsource:type="text/plain">UTC Time Stamp in Long Form (YYYYMMDDhhmmssmmmuuun)</abapsource:documentation></abapsource:elementInfo><abapsource:elementInfo adtcore:uri="/sap/bc/adt/packages/%24tmp" adtcore:type="DEVC/K" adtcore:name="$TMP"><abapsource:properties><abapsource:entry abapsource:key="parentName">$TMP</abapsource:entry></abapsource:properties></abapsource:elementInfo><abapsource:elementInfo adtcore:type="STOB/DOA" adtcore:name="_Head"><abapsource:properties><abapsource:entry abapsource:key="targetEntity">ZSAMPLE_R_HEAD</abapsource:entry><abapsource:entry abapsource:key="targetEntityType">Entity</abapsource:entry><abapsource:entry abapsource:key="targetEntityTypeKey">DDLS/DF</abapsource:entry><abapsource:entry abapsource:key="cardinality"/><abapsource:entry abapsource:key="cardinalityMin">1 </abapsource:entry><abapsource:entry abapsource:key="cardinalityMax">1 </abapsource:entry><abapsource:entry abapsource:key="associationKind">6</abapsource:entry></abapsource:properties></abapsource:elementInfo></abapsource:elementInfo>"#;

    /// A live answer for a projection with a parameter-free composition, and a
    /// parameter element taken from a parameterised view, renamed.
    const PROJECTION: &str = r#"<?xml version="1.0" encoding="utf-8"?><abapsource:elementInfo adtcore:uri="/sap/bc/adt/ddic/ddl/sources/zsample_c_head" adtcore:type="DDLS/STOB" adtcore:name="ZSAMPLE_C_HEAD" xmlns:abapsource="http://www.sap.com/adt/abapsource" xmlns:adtcore="http://www.sap.com/adt/core"><abapsource:properties><abapsource:entry abapsource:key="isRootNode">true</abapsource:entry><abapsource:entry abapsource:key="entityType">projectionView</abapsource:entry></abapsource:properties><abapsource:elementInfo adtcore:type="STOB/DOF" adtcore:name="HeadId"><abapsource:properties><abapsource:entry abapsource:key="ddicIsKey">true</abapsource:entry><abapsource:entry abapsource:key="ddicDataType">char</abapsource:entry><abapsource:entry abapsource:key="ddicLength">000010</abapsource:entry></abapsource:properties></abapsource:elementInfo><abapsource:elementInfo adtcore:type="STOB/DOA" adtcore:name="_Item"><abapsource:properties><abapsource:entry abapsource:key="targetEntity">ZSAMPLE_C_ITEM</abapsource:entry><abapsource:entry abapsource:key="cardinality"/><abapsource:entry abapsource:key="cardinalityMin">0 </abapsource:entry><abapsource:entry abapsource:key="cardinalityMax">*</abapsource:entry><abapsource:entry abapsource:key="associationKind">1</abapsource:entry></abapsource:properties></abapsource:elementInfo><abapsource:elementInfo adtcore:type="STOB/DOP" adtcore:name="p_head"><abapsource:properties><abapsource:entry abapsource:key="ddicDataType">char</abapsource:entry><abapsource:entry abapsource:key="ddicLength">000010</abapsource:entry></abapsource:properties></abapsource:elementInfo></abapsource:elementInfo>"#;

    const EMPTY: &str = r#"<?xml version="1.0" encoding="utf-8"?><abapsource:elementInfo xmlns:abapsource="http://www.sap.com/adt/abapsource"><abapsource:properties/></abapsource:elementInfo>"#;

    #[test]
    fn reads_fields_keys_types_and_references() {
        let info = parse_element_info("ZSAMPLE_R_ITEM", CHILD, false)
            .unwrap()
            .unwrap();
        assert_eq!(info.name, "ZSAMPLE_R_ITEM");
        assert_eq!(info.entity_type, "view");
        assert!(!info.root);
        assert_eq!(info.package.as_deref(), Some("$TMP"));
        assert_eq!(info.description.as_deref(), Some("Sample item"));
        assert_eq!((info.field_count, info.key_field_count), (5, 2));
        let names: Vec<&str> = info
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["HeadId", "ItemNo", "Amount", "Currency", "CreatedAt"]
        );

        let amount = &info.fields[2];
        assert_eq!(amount.data_type.as_deref(), Some("CURR"));
        assert_eq!((amount.length, amount.decimals), (Some(15), Some(2)));
        assert_eq!(amount.reference_field.as_deref(), Some("CURRENCY"));
        assert_eq!(amount.annotations, None, "left out unless asked for");

        let created = &info.fields[4];
        assert_eq!(created.data_element.as_deref(), Some("TIMESTAMPL"));
        assert!(
            created
                .description
                .as_deref()
                .unwrap()
                .starts_with("UTC Time Stamp")
        );
    }

    #[test]
    fn names_each_association_kind_and_its_cardinality() {
        let child = parse_element_info("ZSAMPLE_R_ITEM", CHILD, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            child.associations,
            [CdsAssociation {
                name: "_Head".to_owned(),
                target: Some("ZSAMPLE_R_HEAD".to_owned()),
                cardinality: Some("1..1".to_owned()),
                kind: "to_parent".to_owned(),
            }]
        );
        let projection = parse_element_info("ZSAMPLE_C_HEAD", PROJECTION, false)
            .unwrap()
            .unwrap();
        assert_eq!(projection.entity_type, "projectionView");
        assert!(projection.root);
        assert_eq!(projection.associations[0].kind, "composition");
        assert_eq!(
            projection.associations[0].cardinality.as_deref(),
            Some("0..*")
        );
        assert_eq!(association_kind(Some("0")), "association");
        assert_eq!(association_kind(Some("9")), "kind 9");
    }

    #[test]
    fn reads_parameters() {
        let projection = parse_element_info("ZSAMPLE_C_HEAD", PROJECTION, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            projection.parameters,
            [CdsParameter {
                name: "p_head".to_owned(),
                data_type: Some("CHAR".to_owned()),
                length: Some(10),
                decimals: None,
                data_element: None,
            }]
        );
    }

    #[test]
    fn annotations_are_paired_in_order_when_asked_for() {
        let info = parse_element_info("ZSAMPLE_R_ITEM", CHILD, true)
            .unwrap()
            .unwrap();
        assert_eq!(
            info.annotations.unwrap(),
            [
                CdsAnnotation {
                    name: "ACCESSCONTROL.AUTHORIZATIONCHECK".to_owned(),
                    value: "#NOT_REQUIRED".to_owned(),
                },
                CdsAnnotation {
                    name: "ENDUSERTEXT.LABEL".to_owned(),
                    value: "Sample item".to_owned(),
                },
            ]
        );
        assert_eq!(
            info.fields[2].annotations.as_deref(),
            Some(
                &[CdsAnnotation {
                    name: "SEMANTICS.AMOUNT.CURRENCYCODE".to_owned(),
                    value: "'Currency'".to_owned(),
                }][..]
            )
        );
        assert_eq!(info.fields[0].annotations.as_deref(), Some(&[][..]));
    }

    #[test]
    fn the_empty_answer_means_no_active_entity() {
        assert_eq!(
            parse_element_info("ZSAMPLE_NONE", EMPTY, false).unwrap(),
            None
        );
    }

    #[test]
    fn a_table_is_not_read_as_a_cds_entity() {
        let table = r#"<abapsource:elementInfo adtcore:uri="/sap/bc/adt/ddic/tables/zsample" adtcore:type="TABL/DT" adtcore:name="zsample" xmlns:abapsource="http://www.sap.com/adt/abapsource" xmlns:adtcore="http://www.sap.com/adt/core"><abapsource:properties/></abapsource:elementInfo>"#;
        let error = parse_element_info("ZSAMPLE", table, false).unwrap_err();
        assert_eq!(error.code(), "not_a_cds_entity");
        assert_eq!(error.to_string(), "'ZSAMPLE' is a TABL, not a CDS entity");
    }

    #[test]
    fn only_the_active_layer_is_served() {
        use crate::sap::adt_version::AdtVersion;
        assert!(require_active(AdtVersion::Active).is_ok());
        assert_eq!(
            require_active(AdtVersion::Inactive).unwrap_err().code(),
            "cds_inactive_version_not_served"
        );
    }
}
