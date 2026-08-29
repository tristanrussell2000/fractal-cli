//! Repository object search across the plain and customer-namespace scopes.

use std::collections::BTreeMap;

use thiserror::Error;

use super::{
    adt_response::{AdtResponseParseError, parse_adt_document},
    client::{SapClient, SapError},
    non_empty_attribute,
    repository_kind::{AdtObjectType, RepositoryKind},
};
use crate::{config::Profile, pattern::glob_matches};

const SEARCH_PATH: &str = "/sap/bc/adt/repository/informationsystem/search";
const HARD_SEARCH_MAX: usize = 500;

/// A failure while searching for repository objects.
#[derive(Debug, Error)]
pub enum ObjectSearchError {
    #[error(transparent)]
    Sap(#[from] SapError),
    #[error("invalid object search query: {0}")]
    InvalidQuery(String),
    #[error(transparent)]
    Parse(#[from] AdtResponseParseError),
    #[error("could not aggregate ADT search responses: {0}")]
    Aggregation(String),
}

impl ObjectSearchError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::InvalidQuery(_) => "invalid_search_query",
            Self::Parse(error) => error.code(),
            Self::Aggregation(_) => "search_aggregation_error",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::Sap(error) => error.hint(),
            Self::InvalidQuery(_) => "Provide a non-empty object search query.".to_owned(),
            Self::Parse(error) => error.hint(),
            Self::Aggregation(_) => {
                "Retry the search; if it persists, inspect the underlying SAP request failures."
                    .to_owned()
            }
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
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
            _ => None,
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

/// Searches SAP's ADT repository and aggregates plain and namespace-scoped results.
///
/// # Errors
///
/// Returns [`ObjectSearchError`] when the query is invalid, SAP requests fail, or a
/// search response cannot be parsed.
pub async fn search_objects(
    sap: &mut SapClient,
    profile: &Profile,
    query: &str,
    options: ObjectSearchOptions,
) -> Result<ObjectSearchResult, ObjectSearchError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(ObjectSearchError::InvalidQuery(
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
) -> Result<AggregatedSearch, ObjectSearchError> {
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
                ObjectSearchError::Aggregation(
                    "all generated searches failed without returning an error".to_owned(),
                )
            },
            ObjectSearchError::Sap,
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

fn parse_object_references(xml: &str) -> Result<Vec<ObjectSearchHit>, ObjectSearchError> {
    let document = parse_adt_document(xml)?;

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
    fn classifies_an_aggregation_invariant_failure_as_an_actionable_error() {
        let aggregation = ObjectSearchError::Aggregation("no underlying error".to_owned());

        assert_eq!(aggregation.code(), "search_aggregation_error");
        assert!(!aggregation.hint().is_empty());
    }

    #[test]
    fn matches_case_insensitive_globs() {
        assert!(glob_matches("Z*", "zpackage"));
        assert!(glob_matches("ZFOO_*", "ZFOO_BAR"));
        assert!(!glob_matches("ZFOO_*", "YFOO_BAR"));
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
}
