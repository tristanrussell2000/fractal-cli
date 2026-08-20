use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderValue};
use roxmltree::Document;
use thiserror::Error;

use super::client::{SapClient, SapError};
use super::{find_child, non_empty_attribute};
use crate::{config::Profile, pattern::glob_matches};

const SEARCH_PATH: &str = "/sap/bc/adt/repository/informationsystem/search";
const USAGES_PATH: &str = "/sap/bc/adt/repository/informationsystem/usageReferences";
const HARD_SEARCH_MAX: usize = 500;
const SOURCE_SUFFIX: &str = "/source/main";
const USAGES_CONTENT_TYPE: &str =
    "application/vnd.sap.adt.repository.usagereferences.request.v1+xml";

#[derive(Debug, Error)]
pub enum AdtError {
    #[error(transparent)]
    Sap(#[from] SapError),
    #[error("invalid object search query: {0}")]
    InvalidQuery(String),
    #[error("could not parse ADT XML response: {0}")]
    Parse(String),
    #[error("invalid ADT object URI: {0}")]
    InvalidUri(String),
    #[error("the URI already includes a source suffix: {0}")]
    DoubledSourceSuffix(String),
    #[error("{kind} objects do not have an ABAP source view")]
    NoSourceForKind { kind: String, uri: String },
    #[error("no description found for object URI: {0}")]
    NoDescription(String),
    #[error("could not preserve valid UTF-8 while paging source: {0}")]
    SourceEncoding(String),
    #[error("could not aggregate ADT search responses: {0}")]
    SearchAggregation(String),
}

impl AdtError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::InvalidQuery(_) => "invalid_search_query",
            Self::Parse(_) => "adt_response_parse_error",
            Self::InvalidUri(_) => "invalid_adt_uri",
            Self::DoubledSourceSuffix(_) => "doubled_source_suffix",
            Self::NoSourceForKind { .. } => "no_source_for_kind",
            Self::NoDescription(_) => "no_description",
            Self::SourceEncoding(_) => "source_encoding_error",
            Self::SearchAggregation(_) => "search_aggregation_error",
        }
    }

    #[must_use]
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => Some(error.hint().to_owned()),
            Self::InvalidQuery(_) => Some("Provide a non-empty object search query.".to_owned()),
            Self::Parse(_) => {
                Some("The SAP ADT response did not match the expected search format.".to_owned())
            }
            Self::InvalidUri(_) => Some(
                "Use an object URI under /sap/bc/adt/. Discover it with `fractal object search`."
                    .to_owned(),
            ),
            Self::DoubledSourceSuffix(_) => {
                Some("Pass the object URI without /source/main; Fractal appends it.".to_owned())
            }
            Self::NoSourceForKind { .. } => {
                Some("Use `fractal object xml` to retrieve metadata for this object.".to_owned())
            }
            Self::NoDescription(_) => Some(
                "This URI doesn't expose a description (shadow or fragment URIs are common causes). Try `fractal object xml` for full metadata, or strip any #fragment from the URI and retry against the primary object."
                    .to_owned(),
            ),
            Self::SourceEncoding(_) => Some(
                "The source response could not be converted into a safe UTF-8 page; retry without paging or report the object URI."
                    .to_owned(),
            ),
            Self::SearchAggregation(_) => Some(
                "Retry the search; if it persists, inspect the underlying SAP request failures."
                    .to_owned(),
            ),
        }
    }
}

#[derive(Debug, Error)]
#[error("unknown repository kind '{0}'")]
pub struct RepositoryKindParseError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdtObjectType {
    ClasOc,
    IntfOi,
    TablDt,
    TablDs,
    TtypTt,
    ViewDv,
    DtelDe,
    DomaDd,
    DdlsDf,
    BdefBdo,
    SrvdSrv,
    SrvbSvb,
    MsagN,
    FugrF,
    ProgP,
    EnhoXhh,
    EnhsXsb,
    EnhsXsd,
    EnhsXb,
    Unknown(String),
}

impl AdtObjectType {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "CLAS/OC" => Self::ClasOc,
            "INTF/OI" => Self::IntfOi,
            "TABL/DT" => Self::TablDt,
            "TABL/DS" => Self::TablDs,
            "TTYP/TT" => Self::TtypTt,
            "VIEW/DV" => Self::ViewDv,
            "DTEL/DE" => Self::DtelDe,
            "DOMA/DD" => Self::DomaDd,
            "DDLS/DF" => Self::DdlsDf,
            "BDEF/BDO" => Self::BdefBdo,
            "SRVD/SRV" => Self::SrvdSrv,
            "SRVB/SVB" => Self::SrvbSvb,
            "MSAG/N" => Self::MsagN,
            "FUGR/F" => Self::FugrF,
            "PROG/P" => Self::ProgP,
            "ENHO/XHH" => Self::EnhoXhh,
            "ENHS/XSB" => Self::EnhsXsb,
            "ENHS/XSD" => Self::EnhsXsd,
            "ENHS/XB" => Self::EnhsXb,
            _ => Self::Unknown(value.to_owned()),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::ClasOc => "CLAS/OC",
            Self::IntfOi => "INTF/OI",
            Self::TablDt => "TABL/DT",
            Self::TablDs => "TABL/DS",
            Self::TtypTt => "TTYP/TT",
            Self::ViewDv => "VIEW/DV",
            Self::DtelDe => "DTEL/DE",
            Self::DomaDd => "DOMA/DD",
            Self::DdlsDf => "DDLS/DF",
            Self::BdefBdo => "BDEF/BDO",
            Self::SrvdSrv => "SRVD/SRV",
            Self::SrvbSvb => "SRVB/SVB",
            Self::MsagN => "MSAG/N",
            Self::FugrF => "FUGR/F",
            Self::ProgP => "PROG/P",
            Self::EnhoXhh => "ENHO/XHH",
            Self::EnhsXsb => "ENHS/XSB",
            Self::EnhsXsd => "ENHS/XSD",
            Self::EnhsXb => "ENHS/XB",
            Self::Unknown(value) => value,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> RepositoryKind {
        match self {
            Self::ClasOc => RepositoryKind::Clas,
            Self::IntfOi => RepositoryKind::Intf,
            Self::TablDt => RepositoryKind::Tabl,
            Self::TablDs => RepositoryKind::Stru,
            Self::TtypTt => RepositoryKind::Ttyp,
            Self::ViewDv => RepositoryKind::View,
            Self::DtelDe => RepositoryKind::Dtel,
            Self::DomaDd => RepositoryKind::Doma,
            Self::DdlsDf => RepositoryKind::Ddls,
            Self::BdefBdo => RepositoryKind::Bdef,
            Self::SrvdSrv => RepositoryKind::Srvd,
            Self::SrvbSvb => RepositoryKind::Srvb,
            Self::MsagN => RepositoryKind::Msag,
            Self::FugrF => RepositoryKind::Fugr,
            Self::ProgP => RepositoryKind::Prog,
            Self::EnhoXhh => RepositoryKind::Enho,
            Self::EnhsXsb | Self::EnhsXsd | Self::EnhsXb => RepositoryKind::Enhs,
            Self::Unknown(_) => RepositoryKind::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepositoryKind {
    Clas,
    Intf,
    Tabl,
    Stru,
    Ttyp,
    View,
    Dtel,
    Doma,
    Ddls,
    Bdef,
    Srvd,
    Srvb,
    Msag,
    Fugr,
    Prog,
    Enho,
    Enhs,
    Other,
}

impl RepositoryKind {
    /// Parses a logical repository kind such as `CLAS` or `PROG`.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryKindParseError`] when the value is not a supported kind.
    pub fn parse(value: &str) -> Result<Self, RepositoryKindParseError> {
        match value.to_ascii_uppercase().as_str() {
            "CLAS" => Ok(Self::Clas),
            "INTF" => Ok(Self::Intf),
            "TABL" => Ok(Self::Tabl),
            "STRU" => Ok(Self::Stru),
            "TTYP" => Ok(Self::Ttyp),
            "VIEW" => Ok(Self::View),
            "DTEL" => Ok(Self::Dtel),
            "DOMA" => Ok(Self::Doma),
            "DDLS" => Ok(Self::Ddls),
            "BDEF" => Ok(Self::Bdef),
            "SRVD" => Ok(Self::Srvd),
            "SRVB" => Ok(Self::Srvb),
            "MSAG" => Ok(Self::Msag),
            "FUGR" => Ok(Self::Fugr),
            "PROG" => Ok(Self::Prog),
            "ENHO" => Ok(Self::Enho),
            "ENHS" => Ok(Self::Enhs),
            "OTHER" => Ok(Self::Other),
            _ => Err(RepositoryKindParseError(value.to_owned())),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clas => "CLAS",
            Self::Intf => "INTF",
            Self::Tabl => "TABL",
            Self::Stru => "STRU",
            Self::Ttyp => "TTYP",
            Self::View => "VIEW",
            Self::Dtel => "DTEL",
            Self::Doma => "DOMA",
            Self::Ddls => "DDLS",
            Self::Bdef => "BDEF",
            Self::Srvd => "SRVD",
            Self::Srvb => "SRVB",
            Self::Msag => "MSAG",
            Self::Fugr => "FUGR",
            Self::Prog => "PROG",
            Self::Enho => "ENHO",
            Self::Enhs => "ENHS",
            Self::Other => "OTHER",
        }
    }

    /// Every known kind, in the same order as [`Self::as_str`].
    pub const ALL: [Self; 18] = [
        Self::Clas,
        Self::Intf,
        Self::Tabl,
        Self::Stru,
        Self::Ttyp,
        Self::View,
        Self::Dtel,
        Self::Doma,
        Self::Ddls,
        Self::Bdef,
        Self::Srvd,
        Self::Srvb,
        Self::Msag,
        Self::Fugr,
        Self::Prog,
        Self::Enho,
        Self::Enhs,
        Self::Other,
    ];

    /// A short, human-readable description of the kind for reference/lookup use.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Clas => "Class — an ABAP object-oriented class",
            Self::Intf => "Interface — an ABAP object-oriented interface",
            Self::Tabl => "Database table — a DDIC transparent table",
            Self::Stru => "Structure — a DDIC structure with no database table behind it",
            Self::Ttyp => "Table type — a DDIC type for internal tables",
            Self::View => "View — a classic DDIC database view",
            Self::Dtel => "Data element — a DDIC field type carrying semantic meaning and labels",
            Self::Doma => "Domain — a DDIC value range and technical type for data elements",
            Self::Ddls => "CDS view — a Core Data Services view definition (DDL source)",
            Self::Bdef => {
                "Behavior definition — a RAP (RESTful ABAP Programming) behavior definition"
            }
            Self::Srvd => "Service definition — a RAP service definition exposing CDS views",
            Self::Srvb => {
                "Service binding — a RAP service binding (e.g. OData) for a service definition"
            }
            Self::Msag => "Message class — a container of ABAP messages",
            Self::Fugr => "Function group — a container of function modules",
            Self::Prog => "Program — a classic ABAP report or executable program",
            Self::Enho => {
                "Enhancement implementation — an implementation of an enhancement spot or BAdI"
            }
            Self::Enhs => {
                "Enhancement spot — a defined extension point (BAdI definition, source plug-in)"
            }
            Self::Other => {
                "Any object type not covered by the kinds above — check the raw object_type field for the exact SAP ADT type code"
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ObjectSearchHit {
    pub uri: Option<String>,
    pub name: String,
    pub object_type: AdtObjectType,
    pub description: Option<String>,
    pub package: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ObjectSearchOptions {
    pub package_patterns: Option<Vec<String>>,
    pub kind: Option<RepositoryKind>,
    pub offset: usize,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ObjectSearchResult {
    pub total: usize,
    pub hits: Vec<ObjectSearchHit>,
    pub sap_search_cap: usize,
    pub possibly_truncated_by_sap_cap: bool,
}

#[derive(Debug, Clone)]
pub struct ObjectInfoResult {
    pub uri: String,
    pub description: String,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct ByteRangeOptions {
    pub offset: usize,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ByteRangeResult {
    pub start_byte: usize,
    pub end_byte: usize,
    pub total_bytes: usize,
    pub truncated: bool,
    pub next_offset: Option<usize>,
    pub content: String,
}

/// Fetches an ADT object's complete source and returns a locally paged byte range.
///
/// The HTTP response is fetched in full; `offset` and `limit` are applied locally.
/// Byte boundaries are adjusted so returned text remains valid UTF-8.
///
/// # Errors
///
/// Returns [`AdtError`] for invalid or unsupported object URIs, SAP request
/// failures, or malformed source responses.
pub async fn get_source(
    sap: &SapClient,
    uri: &str,
    options: ByteRangeOptions,
) -> Result<ByteRangeResult, AdtError> {
    validate_source_uri(uri)?;
    let kind = no_source_kind(uri);
    if let Some(kind) = kind {
        return Err(AdtError::NoSourceForKind {
            kind: kind.to_owned(),
            uri: uri.to_owned(),
        });
    }

    let source_uri = format!("{}{}", uri.trim_end_matches('/'), SOURCE_SUFFIX);
    let source = sap.get_text_with_query(&source_uri, &[]).await?;
    page_text(&source, options)
}

/// Fetches raw ADT metadata XML for an object URI.
///
/// # Errors
///
/// Returns [`AdtError::InvalidUri`] for a non-ADT URI or the underlying SAP
/// error when the metadata request fails.
pub async fn get_xml(
    sap: &mut SapClient,
    uri: &str,
    options: ByteRangeOptions,
) -> Result<ByteRangeResult, AdtError> {
    if !uri.starts_with("/sap/bc/adt/") {
        return Err(AdtError::InvalidUri(uri.to_owned()));
    }
    let xml = sap.get_text(uri).await?;
    page_text(&xml, options)
}

/// Fetches an ADT object's metadata XML and extracts its short description.
///
/// This requests the same URI as [`get_xml`]; unlike `get_xml`, the response is
/// parsed and reduced to the first `description` attribute found anywhere in
/// the document, matching `SapFractal`'s `get_object_info` behavior.
///
/// # Errors
///
/// Returns [`AdtError::InvalidUri`] for a non-ADT URI, the underlying SAP error
/// when the metadata request fails, [`AdtError::Parse`] when the response is
/// not valid XML, or [`AdtError::NoDescription`] when no `description`
/// attribute is present anywhere in the document.
pub async fn get_object_info(sap: &mut SapClient, uri: &str) -> Result<ObjectInfoResult, AdtError> {
    if !uri.starts_with("/sap/bc/adt/") {
        return Err(AdtError::InvalidUri(uri.to_owned()));
    }
    let xml = sap.get_text(uri).await?;
    parse_object_info(&xml, uri)
}

fn parse_object_info(xml: &str, uri: &str) -> Result<ObjectInfoResult, AdtError> {
    let document = Document::parse(xml).map_err(|error| AdtError::Parse(error.to_string()))?;
    let description = find_description(document.root_element())
        .ok_or_else(|| AdtError::NoDescription(uri.to_owned()))?;
    Ok(ObjectInfoResult {
        uri: uri.to_owned(),
        description,
    })
}

/// Depth-first search for the first `description` attribute in the document,
/// checking each element's own attributes before descending into its children.
fn find_description(node: roxmltree::Node) -> Option<String> {
    node.attributes()
        .find(|attr| attr.name() == "description")
        .map(|attr| attr.value().to_owned())
        .or_else(|| {
            node.children()
                .filter(roxmltree::Node::is_element)
                .find_map(find_description)
        })
}

/// Fetches where-used ("usage references") for an ADT object URI.
///
/// Returns every referencing row SAP reports, including non-result hierarchy
/// context (containing object, method, package); see [`UsageReference`] for
/// how to tell those apart from direct hits. Self-references are stripped.
///
/// # Errors
///
/// Returns [`AdtError::InvalidUri`] for a non-ADT URI, the underlying SAP
/// error when the request fails, or [`AdtError::Parse`] when the response is
/// not valid XML.
pub async fn get_object_usages(
    sap: &mut SapClient,
    uri: &str,
) -> Result<Vec<UsageReference>, AdtError> {
    if !uri.starts_with("/sap/bc/adt/") {
        return Err(AdtError::InvalidUri(uri.to_owned()));
    }
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

fn parse_usage_references(xml: &str, target_uri: &str) -> Result<Vec<UsageReference>, AdtError> {
    let document = Document::parse(xml).map_err(|error| AdtError::Parse(error.to_string()))?;
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

/// Searches SAP's ADT repository and aggregates plain and namespace-scoped results.
///
/// # Errors
///
/// Returns [`AdtError`] when the query is invalid, SAP requests fail, or a
/// search response cannot be parsed.
pub async fn search_objects(
    sap: &mut SapClient,
    profile: &Profile,
    query: &str,
    options: ObjectSearchOptions,
) -> Result<ObjectSearchResult, AdtError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(AdtError::InvalidQuery(
            "search query is required".to_owned(),
        ));
    }

    let patterns = options
        .package_patterns
        .filter(|patterns| !patterns.is_empty())
        .unwrap_or_else(|| profile.customer_namespaces.clone());
    let queries = build_search_queries(query, &patterns);
    let responses = fetch_search_responses(sap, &queries).await;
    let aggregated = aggregate_search_results(responses, &patterns, options.kind)?;
    let (total, hits) = page_search_results(aggregated.hits, options.offset, options.limit);

    Ok(ObjectSearchResult {
        total,
        hits,
        sap_search_cap: 500,
        possibly_truncated_by_sap_cap: aggregated.possibly_truncated_by_sap_cap,
    })
}

#[derive(Debug)]
struct AggregatedSearch {
    hits: Vec<ObjectSearchHit>,
    possibly_truncated_by_sap_cap: bool,
}

fn build_search_queries(query: &str, patterns: &[String]) -> Vec<String> {
    let plain = if query.ends_with('*') {
        query.to_owned()
    } else {
        format!("{query}*")
    };
    let mut queries = vec![plain.clone()];

    for pattern in patterns {
        if pattern.is_empty() || pattern == "*" {
            continue;
        }
        let scope = pattern.strip_suffix('*').unwrap_or(pattern);
        let base = if plain.starts_with('*') {
            plain.clone()
        } else {
            format!("*{plain}")
        };
        queries.push(format!("{scope}{base}"));
    }
    queries
}

async fn fetch_search_responses(
    sap: &SapClient,
    queries: &[String],
) -> Vec<Result<String, SapError>> {
    futures::future::join_all(queries.iter().map(|search_query| async {
        let query_params = [
            ("operation", "quickSearch"),
            ("query", search_query.as_str()),
            ("maxResults", "500"),
        ];
        sap.get_text_with_query(SEARCH_PATH, &query_params).await
    }))
    .await
}

fn aggregate_search_results(
    responses: Vec<Result<String, SapError>>,
    patterns: &[String],
    kind: Option<RepositoryKind>,
) -> Result<AggregatedSearch, AdtError> {
    let mut by_uri = BTreeMap::new();
    let mut orphans = Vec::new();
    let mut first_error = None;
    let mut successful_queries = 0;
    let mut possibly_truncated_by_sap_cap = false;

    for response in responses {
        match response {
            Ok(xml) => {
                successful_queries += 1;
                let parsed_hits = parse_object_references(&xml)?;
                possibly_truncated_by_sap_cap |= parsed_hits.len() >= 500;
                for hit in parsed_hits {
                    if hit.object_type.as_str() == "STOB/DO"
                        || !matches_scope(&hit, patterns)
                        || kind.is_some_and(|expected| hit.object_type.kind() != expected)
                    {
                        continue;
                    }
                    if let Some(uri) = &hit.uri {
                        by_uri.entry(uri.clone()).or_insert(hit);
                    } else {
                        orphans.push(hit);
                    }
                }
            }
            Err(error) => first_error = first_error.or(Some(error)),
        }
    }

    if successful_queries == 0 {
        return Err(first_error.map_or_else(
            || {
                AdtError::SearchAggregation(
                    "all generated searches failed without returning an error".to_owned(),
                )
            },
            AdtError::Sap,
        ));
    }

    let mut hits: Vec<_> = by_uri.into_values().collect();
    hits.extend(orphans);
    hits.sort_by_key(|hit| hit.name.to_ascii_uppercase());
    Ok(AggregatedSearch {
        hits,
        possibly_truncated_by_sap_cap,
    })
}

fn page_search_results(
    hits: Vec<ObjectSearchHit>,
    offset: usize,
    limit: Option<usize>,
) -> (usize, Vec<ObjectSearchHit>) {
    let total = hits.len();
    let hits = match limit.map(|limit| limit.clamp(1, HARD_SEARCH_MAX)) {
        Some(limit) => hits.into_iter().skip(offset).take(limit).collect(),
        None => hits,
    };
    (total, hits)
}

fn page_text(text: &str, options: ByteRangeOptions) -> Result<ByteRangeResult, AdtError> {
    let bytes = text.as_bytes();
    let total_bytes = bytes.len();
    let start_byte = utf8_safe_start(bytes, options.offset.min(total_bytes));
    let requested_end = options.limit.map_or(total_bytes, |limit| {
        start_byte.saturating_add(limit).min(total_bytes)
    });
    let end_byte = utf8_safe_end(bytes, requested_end).max(start_byte);
    let content = std::str::from_utf8(&bytes[start_byte..end_byte])
        .map_err(|error| AdtError::SourceEncoding(error.to_string()))?
        .to_owned();
    let truncated = end_byte < total_bytes;

    Ok(ByteRangeResult {
        start_byte,
        end_byte,
        total_bytes,
        truncated,
        next_offset: truncated.then_some(end_byte),
        content,
    })
}

fn validate_source_uri(uri: &str) -> Result<(), AdtError> {
    if !uri.starts_with("/sap/bc/adt/") {
        return Err(AdtError::InvalidUri(uri.to_owned()));
    }
    if uri.ends_with(SOURCE_SUFFIX) {
        return Err(AdtError::DoubledSourceSuffix(uri.to_owned()));
    }
    Ok(())
}

fn no_source_kind(uri: &str) -> Option<&'static str> {
    let uri = uri.to_ascii_lowercase();
    if uri.contains("/ddic/dataelements/") {
        Some("DTEL")
    } else if uri.contains("/ddic/domains/") {
        Some("DOMA")
    } else if uri.contains("/ddic/tabletypes/") {
        Some("TTYP")
    } else if uri.contains("/messageclass/") || uri.contains("/messageclasses/") {
        Some("MSAG")
    } else {
        None
    }
}

#[inline]
const fn is_utf8_continuation_byte(byte: u8) -> bool {
    (byte & 0b1100_0000) == 0b1000_0000
}

fn utf8_safe_start(bytes: &[u8], requested_start: usize) -> usize {
    let mut start = requested_start.min(bytes.len());
    while start < bytes.len() && is_utf8_continuation_byte(bytes[start]) {
        start += 1;
    }
    start
}

fn utf8_safe_end(bytes: &[u8], requested_end: usize) -> usize {
    let mut end = requested_end.min(bytes.len());
    while end > 0 && end < bytes.len() && is_utf8_continuation_byte(bytes[end]) {
        end -= 1;
    }
    end
}

fn parse_object_references(xml: &str) -> Result<Vec<ObjectSearchHit>, AdtError> {
    let document = Document::parse(xml).map_err(|error| AdtError::Parse(error.to_string()))?;

    Ok(document
        .descendants()
        .filter(|node| node.tag_name().name() == "objectReference")
        .filter_map(|node| {
            let name = node.attribute("name")?.trim();
            let object_type = node.attribute("type")?.trim();
            if name.is_empty() || object_type.is_empty() {
                return None;
            }
            Some(ObjectSearchHit {
                uri: non_empty_attribute(node.attribute("uri")),
                name: name.to_owned(),
                object_type: AdtObjectType::parse(object_type),
                description: non_empty_attribute(node.attribute("description")),
                package: node
                    .attribute("packageName")
                    .or_else(|| node.attribute("packageRef"))
                    .and_then(|value| non_empty_attribute(Some(value))),
            })
        })
        .collect())
}

fn matches_scope(hit: &ObjectSearchHit, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    hit.package
        .as_deref()
        .or(Some(hit.name.as_str()))
        .is_some_and(|value| patterns.iter().any(|pattern| glob_matches(pattern, value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_case_insensitive_globs() {
        assert!(glob_matches("Z*", "zpackage"));
        assert!(glob_matches("ZFOO_*", "ZFOO_BAR"));
        assert!(!glob_matches("ZFOO_*", "YFOO_BAR"));
    }

    #[test]
    fn parses_repository_kinds() {
        assert_eq!(RepositoryKind::parse("clas").unwrap(), RepositoryKind::Clas);
        assert_eq!(
            RepositoryKind::parse("OTHER").unwrap(),
            RepositoryKind::Other
        );
        assert!(RepositoryKind::parse("invalid").is_err());
    }

    #[test]
    fn classifies_invariant_failures_as_actionable_errors() {
        let encoding = AdtError::SourceEncoding("invalid boundary".to_owned());
        assert_eq!(encoding.code(), "source_encoding_error");
        assert!(encoding.hint().is_some());

        let aggregation = AdtError::SearchAggregation("no underlying error".to_owned());
        assert_eq!(aggregation.code(), "search_aggregation_error");
        assert!(aggregation.hint().is_some());
    }

    #[test]
    fn parses_known_and_unknown_object_types() {
        assert_eq!(AdtObjectType::parse("CLAS/OC").as_str(), "CLAS/OC");
        assert_eq!(AdtObjectType::parse("TTYP/DA").as_str(), "TTYP/DA");
        assert_eq!(
            AdtObjectType::parse("TTYP/DA").kind(),
            RepositoryKind::Other
        );
    }

    #[test]
    fn maps_known_and_unknown_object_types() {
        let cases = [
            ("CLAS/OC", RepositoryKind::Clas),
            ("INTF/OI", RepositoryKind::Intf),
            ("TABL/DT", RepositoryKind::Tabl),
            ("TABL/DS", RepositoryKind::Stru),
            ("DDLS/DF", RepositoryKind::Ddls),
            ("ENHS/XSD", RepositoryKind::Enhs),
            ("UNKNOWN/X", RepositoryKind::Other),
        ];
        for (object_type, expected) in cases {
            assert_eq!(AdtObjectType::parse(object_type).kind(), expected);
        }
    }

    #[test]
    fn builds_plain_and_namespace_scoped_queries() {
        assert_eq!(
            build_search_queries("VERSION", &["Z*".to_owned(), "Y*".to_owned()]),
            vec!["VERSION*", "Z*VERSION*", "Y*VERSION*"]
        );
        assert_eq!(
            build_search_queries("VERSION*", &["ZAPP".to_owned()]),
            vec!["VERSION*", "ZAPP*VERSION*"]
        );
        assert_eq!(
            build_search_queries("*WORKFLOW", &["Z*".to_owned()]),
            vec!["*WORKFLOW*", "Z*WORKFLOW*"]
        );
        assert_eq!(
            build_search_queries("VERSION", &["*".to_owned()]),
            vec!["VERSION*"]
        );
    }

    fn hit(name: &str) -> ObjectSearchHit {
        ObjectSearchHit {
            uri: Some(format!("/objects/{name}")),
            name: name.to_owned(),
            object_type: AdtObjectType::parse("CLAS/OC"),
            description: None,
            package: Some("ZAPP".to_owned()),
        }
    }

    #[test]
    fn pages_results_with_offsets_and_limits() {
        let hits = vec![hit("A"), hit("B"), hit("C"), hit("D")];
        let (total, page) = page_search_results(hits.clone(), 1, Some(2));
        assert_eq!(total, 4);
        assert_eq!(
            page.iter().map(|hit| hit.name.as_str()).collect::<Vec<_>>(),
            ["B", "C"]
        );

        let (_, page) = page_search_results(hits.clone(), 99, Some(2));
        assert!(page.is_empty());
        let (_, page) = page_search_results(hits.clone(), 0, Some(999));
        assert_eq!(page.len(), 4);
        let (_, page) = page_search_results(hits, 2, None);
        assert_eq!(
            page.iter().map(|hit| hit.name.as_str()).collect::<Vec<_>>(),
            ["A", "B", "C", "D"]
        );
    }

    #[test]
    fn aggregates_filters_deduplicates_and_sorts_hits() {
        let first = r#"<r:objectReferences xmlns:r="urn:test">
            <r:objectReference name="ZB" type="CLAS/OC" packageName="ZAPP" uri="/b"/>
            <r:objectReference name="ZA" type="CLAS/OC" packageName="ZAPP" uri="/a"/>
            <r:objectReference name="STANDARD" type="CLAS/OC" packageName="SAPP" uri="/standard"/>
            <r:objectReference name="ZTABLE" type="TABL/DT" packageName="ZAPP" uri="/table"/>
            <r:objectReference name="ZSHADOW" type="STOB/DO" packageName="ZAPP" uri="/shadow"/>
            <r:objectReference name="ZORPHAN" type="CLAS/OC" uri=""/>
        </r:objectReferences>"#;
        let second = r#"<r:objectReferences xmlns:r="urn:test">
            <r:objectReference name="ZB" type="CLAS/OC" packageName="ZAPP" uri="/b"/>
        </r:objectReferences>"#;

        let aggregate = aggregate_search_results(
            vec![Ok(first.to_owned()), Ok(second.to_owned())],
            &["Z*".to_owned()],
            Some(RepositoryKind::Clas),
        )
        .unwrap();
        let names: Vec<_> = aggregate.hits.iter().map(|hit| hit.name.as_str()).collect();
        assert_eq!(names, ["ZA", "ZB", "ZORPHAN"]);
        assert!(!aggregate.possibly_truncated_by_sap_cap);
    }

    #[test]
    fn aggregation_keeps_partial_success_and_reports_total_failure() {
        let xml = r#"<objectReferences><objectReference name="ZOK" type="CLAS/OC" packageName="ZAPP" uri="/ok"/></objectReferences>"#;
        let network_error = SapError::Network {
            url: "http://sap".to_owned(),
            message: "offline".to_owned(),
        };
        let partial = aggregate_search_results(
            vec![Ok(xml.to_owned()), Err(network_error)],
            &["Z*".to_owned()],
            None,
        )
        .unwrap();
        assert_eq!(partial.hits.len(), 1);

        let error = aggregate_search_results(Vec::new(), &["Z*".to_owned()], None).unwrap_err();
        assert_eq!(error.code(), "search_aggregation_error");
    }

    #[test]
    fn detects_a_response_at_the_sap_cap() {
        let references: String = (0..500)
            .map(|index| format!(r#"<objectReference name="Z{index}" type="CLAS/OC" packageName="ZAPP" uri="/{index}"/>"#))
            .collect();
        let xml = format!(r#"<objectReferences>{references}</objectReferences>"#);
        let aggregate = aggregate_search_results(vec![Ok(xml)], &["Z*".to_owned()], None).unwrap();
        assert!(aggregate.possibly_truncated_by_sap_cap);
    }

    #[test]
    fn finds_description_on_the_root_element() {
        let xml = r#"<class:abapClass xmlns:class="urn:test" description="Root class"/>"#;
        let info = parse_object_info(xml, "/sap/bc/adt/oo/classes/zcl_example").unwrap();
        assert_eq!(info.description, "Root class");
        assert_eq!(info.uri, "/sap/bc/adt/oo/classes/zcl_example");
    }

    #[test]
    fn finds_description_nested_under_a_child_element() {
        let xml = r#"<class:abapClass xmlns:class="urn:test">
            <class:include>
                <class:section description="Nested description"/>
            </class:include>
        </class:abapClass>"#;
        let info = parse_object_info(xml, "/uri").unwrap();
        assert_eq!(info.description, "Nested description");
    }

    #[test]
    fn matches_description_attributes_regardless_of_namespace_prefix() {
        let xml = r#"<class:abapClass xmlns:class="urn:test" xmlns:adtcore="urn:adt" adtcore:description="Namespaced description"/>"#;
        let info = parse_object_info(xml, "/uri").unwrap();
        assert_eq!(info.description, "Namespaced description");
    }

    #[test]
    fn returns_a_hinted_error_when_no_description_is_present() {
        let xml = r#"<class:abapClass xmlns:class="urn:test"><class:include/></class:abapClass>"#;
        let error = parse_object_info(xml, "/uri").unwrap_err();
        assert_eq!(error.code(), "no_description");
        assert!(error.hint().is_some());
    }

    #[test]
    fn returns_a_parse_error_for_malformed_object_info_xml() {
        let error = parse_object_info("<not-closed", "/uri").unwrap_err();
        assert_eq!(error.code(), "adt_response_parse_error");
    }

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
