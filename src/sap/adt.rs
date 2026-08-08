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
    #[error("could not parse ADT search response: {0}")]
    Parse(String),
    #[error("invalid ADT object URI: {0}")]
    InvalidUri(String),
    #[error("the URI already includes a source suffix: {0}")]
    DoubledSourceSuffix(String),
    #[error("{kind} objects do not have an ABAP source view")]
    NoSourceForKind { kind: String, uri: String },
}

impl AdtError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::InvalidQuery(_) => "invalid_search_query",
            Self::Parse(_) => "adt_response_parse_error",
            Self::InvalidUri(_) => "invalid_adt_uri",
            Self::DoubledSourceSuffix(_) => "doubled_source_suffix",
            Self::NoSourceForKind { .. } => "no_source_for_kind",
        }
    }

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
        }
    }
}

#[derive(Debug, Error)]
#[error("unknown repository kind '{0}'")]
pub struct RepositoryKindParseError(pub String);

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

    pub fn from_object_type(object_type: &str) -> Self {
        match object_type {
            "CLAS/OC" => Self::Clas,
            "INTF/OI" => Self::Intf,
            "TABL/DT" => Self::Tabl,
            "TABL/DS" => Self::Stru,
            "TTYP/TT" => Self::Ttyp,
            "VIEW/DV" => Self::View,
            "DTEL/DE" => Self::Dtel,
            "DOMA/DD" => Self::Doma,
            "DDLS/DF" => Self::Ddls,
            "BDEF/BDO" => Self::Bdef,
            "SRVD/SRV" => Self::Srvd,
            "SRVB/SVB" => Self::Srvb,
            "MSAG/N" => Self::Msag,
            "FUGR/F" => Self::Fugr,
            "PROG/P" => Self::Prog,
            "ENHO/XHH" => Self::Enho,
            "ENHS/XSB" | "ENHS/XSD" | "ENHS/XB" => Self::Enhs,
            _ => Self::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
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
    pub object_type: String,
    pub kind: RepositoryKind,
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

#[derive(Debug, Clone, Default)]
pub struct SourceOptions {
    pub offset: usize,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SourceResult {
    pub start_byte: usize,
    pub end_byte: usize,
    pub total_bytes: usize,
    pub truncated: bool,
    pub next_offset: Option<usize>,
    pub source: String,
}

pub async fn get_source(
    sap: &mut SapClient,
    uri: &str,
    options: SourceOptions,
) -> Result<SourceResult, AdtError> {
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
    let bytes = source.as_bytes();
    let total_bytes = bytes.len();
    let start_byte = utf8_safe_start(bytes, options.offset.min(total_bytes));
    let requested_end = options
        .limit
        .map(|limit| start_byte.saturating_add(limit).min(total_bytes))
        .unwrap_or(total_bytes);
    let end_byte = utf8_safe_end(bytes, requested_end).max(start_byte);
    let source = std::str::from_utf8(&bytes[start_byte..end_byte])
        .expect("UTF-8-safe source range must be valid")
        .to_owned();
    let truncated = end_byte < total_bytes;

    Ok(SourceResult {
        start_byte,
        end_byte,
        total_bytes,
        truncated,
        next_offset: truncated.then_some(end_byte),
        source,
    })
}

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
    let plain = if query.ends_with('*') {
        query.to_owned()
    } else {
        format!("{query}*")
    };

    let mut queries = vec![plain.clone()];
    for pattern in &patterns {
        if pattern.is_empty() || pattern == "*" {
            continue;
        }
        let scope = if pattern.ends_with('*') {
            pattern.clone()
        } else {
            format!("{pattern}*")
        };
        let base = if plain.starts_with('*') {
            plain.clone()
        } else {
            format!("*{plain}")
        };
        let combined = if scope.ends_with('*') && base.starts_with('*') {
            format!("{}{}", scope.trim_end_matches('*'), base)
        } else {
            format!("{scope}{base}")
        };
        queries.push(combined);
    }

    let query_results = futures::future::join_all(queries.iter().map(|search_query| async {
        let query_params = [
            ("operation", "quickSearch"),
            ("query", search_query.as_str()),
            ("maxResults", "500"),
        ];
        sap.get_text_with_query_read_only(SEARCH_PATH, &query_params)
            .await
    }))
    .await;

    let mut by_uri = BTreeMap::new();
    let mut orphans = Vec::new();
    let mut first_error = None;
    let mut successful_queries = 0;
    let mut possibly_truncated_by_sap_cap = false;

    for result in query_results {
        match result {
            Ok(xml) => {
                successful_queries += 1;
                let parsed_hits = parse_object_references(&xml)?;
                if parsed_hits.len() >= 500 {
                    possibly_truncated_by_sap_cap = true;
                }
                for hit in parsed_hits {
                    if hit.object_type == "STOB/DO" {
                        continue;
                    }
                    if !matches_scope(&hit, &patterns) {
                        continue;
                    }
                    if options.kind.is_some_and(|kind| hit.kind != kind) {
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
        return Err(AdtError::Sap(
            first_error.expect("at least one search query was generated"),
        ));
    }

    let mut all: Vec<_> = by_uri.into_values().collect();
    all.extend(orphans);
    all.sort_by_key(|hit| hit.name.to_ascii_uppercase());

    let total = all.len();
    let limit = options.limit.map(|limit| limit.clamp(1, HARD_SEARCH_MAX));
    let hits = match limit {
        Some(limit) => all.into_iter().skip(options.offset).take(limit).collect(),
        None => all,
    };

    Ok(ObjectSearchResult {
        total,
        hits,
        sap_search_cap: 500,
        possibly_truncated_by_sap_cap,
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
fn is_utf8_continuation_byte(byte: u8) -> bool {
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
                object_type: object_type.to_owned(),
                kind: RepositoryKind::from_object_type(object_type),
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
    use super::{RepositoryKind, glob_matches};

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
    fn maps_known_and_unknown_object_types() {
        assert_eq!(
            RepositoryKind::from_object_type("CLAS/OC"),
            RepositoryKind::Clas
        );
        assert_eq!(
            RepositoryKind::from_object_type("ENHS/XSD"),
            RepositoryKind::Enhs
        );
        assert_eq!(
            RepositoryKind::from_object_type("UNKNOWN/X"),
            RepositoryKind::Other
        );
    }
}
