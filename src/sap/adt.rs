use std::collections::BTreeMap;

use roxmltree::Document;
use thiserror::Error;

use super::client::{SapClient, SapError};
use crate::config::Profile;

const SEARCH_PATH: &str = "/sap/bc/adt/repository/informationsystem/search";
const HARD_SEARCH_MAX: usize = 500;
const SOURCE_SUFFIX: &str = "/source/main";

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
    sap: &mut SapClient,
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
    let source = sap.get_text(&source_uri).await?;
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
        sap.get_text_with_query_read_only(SEARCH_PATH, &query_params)
            .await
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

fn non_empty_attribute(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_owned())
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

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.to_ascii_uppercase();
    let value = value.to_ascii_uppercase();
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut p = 0;
    let mut v = 0;
    let mut star = None;
    let mut retry = 0;

    while v < value.len() {
        if p < pattern.len() && pattern[p] == value[v] {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = v;
        } else if let Some(star_position) = star {
            p = star_position + 1;
            retry += 1;
            v = retry;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
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
}
