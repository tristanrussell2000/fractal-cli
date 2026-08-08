use std::collections::BTreeMap;

use roxmltree::Document;
use thiserror::Error;

use super::client::{SapClient, SapError};
use crate::config::Profile;

const SEARCH_PATH: &str = "/sap/bc/adt/repository/informationsystem/search";
const HARD_SEARCH_MAX: usize = 500;

#[derive(Debug, Error)]
pub enum AdtError {
    #[error(transparent)]
    Sap(#[from] SapError),
    #[error("invalid object search query: {0}")]
    InvalidQuery(String),
    #[error("could not parse ADT search response: {0}")]
    Parse(String),
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

    let mut by_uri = BTreeMap::new();
    let mut orphans = Vec::new();
    let mut first_error = None;
    let mut successful_queries = 0;

    for search_query in queries {
        let query_params = [
            ("operation", "quickSearch"),
            ("query", search_query.as_str()),
            ("maxResults", "500"),
        ];
        match sap.get_text_with_query(SEARCH_PATH, &query_params).await {
            Ok(xml) => {
                successful_queries += 1;
                for hit in parse_object_references(&xml)? {
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

    Ok(ObjectSearchResult { total, hits })
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
                kind: kind_from_object_type(object_type),
                description: non_empty_attribute(node.attribute("description")),
                package: node
                    .attribute("packageName")
                    .or_else(|| node.attribute("packageRef"))
                    .and_then(|value| non_empty_attribute(Some(value))),
            })
        })
        .collect())
}

fn kind_from_object_type(object_type: &str) -> RepositoryKind {
    match object_type {
        "CLAS/OC" => RepositoryKind::Clas,
        "INTF/OI" => RepositoryKind::Intf,
        "TABL/DT" => RepositoryKind::Tabl,
        "TABL/DS" => RepositoryKind::Stru,
        "TTYP/TT" => RepositoryKind::Ttyp,
        "VIEW/DV" => RepositoryKind::View,
        "DTEL/DE" => RepositoryKind::Dtel,
        "DOMA/DD" => RepositoryKind::Doma,
        "DDLS/DF" => RepositoryKind::Ddls,
        "BDEF/BDO" => RepositoryKind::Bdef,
        "SRVD/SRV" => RepositoryKind::Srvd,
        "SRVB/SVB" => RepositoryKind::Srvb,
        "MSAG/N" => RepositoryKind::Msag,
        "FUGR/F" => RepositoryKind::Fugr,
        "PROG/P" => RepositoryKind::Prog,
        "ENHO/XHH" => RepositoryKind::Enho,
        "ENHS/XSB" | "ENHS/XSD" | "ENHS/XB" => RepositoryKind::Enhs,
        _ => RepositoryKind::Other,
    }
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
    use super::{RepositoryKind, glob_matches, kind_from_object_type};

    #[test]
    fn matches_case_insensitive_globs() {
        assert!(glob_matches("Z*", "zpackage"));
        assert!(glob_matches("ZFOO_*", "ZFOO_BAR"));
        assert!(!glob_matches("ZFOO_*", "YFOO_BAR"));
    }

    #[test]
    fn maps_known_and_unknown_object_types() {
        assert_eq!(kind_from_object_type("CLAS/OC"), RepositoryKind::Clas);
        assert_eq!(kind_from_object_type("ENHS/XSD"), RepositoryKind::Enhs);
        assert_eq!(kind_from_object_type("UNKNOWN/X"), RepositoryKind::Other);
    }
}
