//! An object's version history, and one version's content.
//!
//! SAP writes a version when a transport carrying the object is released or
//! imported. Besides those numbered versions, every list has `00000`, the
//! active version, and `99999` when something is staged. An object still in
//! development therefore lists only those two.
//!
//! Where the list lives differs by type (a class has one per include, a
//! program `source/main/versions`, a CDS view `versions`), so it is found
//! through the object document's `…/relations/versions` link. Each entry's
//! content must be asked for in the type the entry declares; anything else is
//! a 406.

use reqwest::header::{ACCEPT, HeaderMap, HeaderValue};
use serde::Serialize;
use similar::TextDiff;
use thiserror::Error;

use super::{
    class_include::ClassInclude,
    client::{SapClient, SapClientError},
    find_attribute_value, find_child_text, find_children, find_non_empty_attribute,
};
use crate::reportable_error::{ReportableError, sap_http_status};

const VERSIONS_RELATION: &str = "/relations/versions";
const REQUEST_RELATION: &str = "/relations/transport/request";
pub const ACTIVE: &str = "00000";
pub const INACTIVE: &str = "99999";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Version {
    pub id: String,
    /// `active`, `inactive`, or `history` for a numbered version.
    pub kind: &'static str,
    pub date: Option<String>,
    pub author: Option<String>,
    pub request: Option<String>,
    pub description: Option<String>,
    #[serde(skip)]
    content_uri: String,
    #[serde(skip)]
    content_type: String,
}

#[derive(Debug, Error)]
pub enum VersionsError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error("SAP returned a malformed version list: {0}")]
    Parse(#[from] roxmltree::Error),
    #[error("{0} links no version list")]
    NoVersionList(String),
    #[error("SAP lists versions of {0} but does not serve their content")]
    ContentNotServed(String),
    #[error("{object} has no version {version}")]
    NoSuchVersion {
        object: String,
        version: String,
        available: Vec<String>,
    },
}

impl ReportableError for VersionsError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(_) => "versions_response_invalid",
            Self::NoVersionList(_) => "versions_not_available",
            Self::ContentNotServed(_) => "version_content_not_served",
            Self::NoSuchVersion { .. } => "version_not_found",
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
            Self::Parse(_) => Some("SAP's version list was not a valid Atom feed.".to_owned()),
            Self::NoVersionList(_) => Some(
                "This kind of object keeps no version history in ADT; for a class, check --include."
                    .to_owned(),
            ),
            Self::ContentNotServed(_) => Some(
                "Only the active version can be read, with `fractal object xml`.".to_owned(),
            ),
            Self::NoSuchVersion { available, .. } => Some(format!(
                "It has {}. `active` and `inactive` also work.",
                available.join(", ")
            )),
        }
    }
}

/// The versions of an object, newest first; for a class, of one include.
///
/// # Errors
///
/// Returns [`VersionsError::NoVersionList`] when the object links none.
pub async fn list_versions(
    sap: &SapClient,
    object_uri: &str,
    include: Option<ClassInclude>,
) -> Result<Vec<Version>, VersionsError> {
    let document = sap.get_text(object_uri).await?;
    let feed_uri = versions_uri(object_uri, &document, include)?
        .ok_or_else(|| VersionsError::NoVersionList(object_uri.to_owned()))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/atom+xml;type=feed"),
    );
    let feed = sap
        .get_bytes_with_query_and_headers(&feed_uri, &[], headers)
        .await?;
    parse_versions(&String::from_utf8_lossy(&feed))
}

/// `version` among `versions`, with `active` and `inactive` as aliases.
///
/// # Errors
///
/// Returns [`VersionsError::NoSuchVersion`] naming the ones there are.
pub fn find_version<'a>(
    versions: &'a [Version],
    object: &str,
    version: &str,
) -> Result<&'a Version, VersionsError> {
    let wanted = match version.trim().to_ascii_lowercase().as_str() {
        "active" => ACTIVE.to_owned(),
        "inactive" => INACTIVE.to_owned(),
        number => format!("{number:0>5}"),
    };
    versions
        .iter()
        .find(|candidate| candidate.id == wanted)
        .ok_or_else(|| VersionsError::NoSuchVersion {
            object: object.to_owned(),
            version: version.to_owned(),
            available: versions.iter().map(|version| version.id.clone()).collect(),
        })
}

/// One version's content.
///
/// # Errors
///
/// Returns [`VersionsError`] when the content cannot be read.
pub async fn read_version(sap: &SapClient, version: &Version) -> Result<String, VersionsError> {
    let mut headers = HeaderMap::new();
    if let Ok(accept) = HeaderValue::from_str(&version.content_type) {
        headers.insert(ACCEPT, accept);
    }
    // A data element's versions are listed, but reading one is a 403
    // ("Request version for DTEL is not supported").
    let bytes = match sap
        .get_bytes_with_query_and_headers(&version.content_uri, &[], headers)
        .await
    {
        Err(SapClientError::Http { status, .. }) if status.as_u16() == 403 => {
            let object = version
                .content_uri
                .split("/versions/")
                .next()
                .unwrap_or(&version.content_uri);
            return Err(VersionsError::ContentNotServed(object.to_owned()));
        }
        result => result?,
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// A unified diff of two texts, line endings ignored. Empty when they match.
///
/// SAP stores source without a final newline; both sides get one so the diff
/// does not end in "No newline at end of file".
#[must_use]
pub fn unified_diff(old_label: &str, new_label: &str, old: &str, new: &str) -> String {
    let normalized = |text: &str| {
        let mut text = text.replace("\r\n", "\n");
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text
    };
    let old = normalized(old);
    let new = normalized(new);
    if old == new {
        return String::new();
    }
    TextDiff::from_lines(&old, &new)
        .unified_diff()
        .context_radius(3)
        .header(old_label, new_label)
        .to_string()
}

/// The version list's URI from the object's own document. A class links one
/// per include; the others link one.
fn versions_uri(
    object_uri: &str,
    document: &str,
    include: Option<ClassInclude>,
) -> Result<Option<String>, VersionsError> {
    let document = roxmltree::Document::parse(document)?;
    let links: Vec<String> = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "link")
        .filter(|link| {
            find_attribute_value(*link, "rel").is_some_and(|rel| rel.ends_with(VERSIONS_RELATION))
        })
        .filter_map(|link| find_non_empty_attribute(link, "href"))
        .collect();
    // A class's main stream is the default; another object has one list.
    let segment = format!(
        "includes/{}/versions",
        include.unwrap_or(ClassInclude::Main).as_str()
    );
    let chosen = links
        .iter()
        .find(|href| href.ends_with(&segment))
        .cloned()
        .or_else(|| {
            include
                .is_none()
                .then(|| links.into_iter().next())
                .flatten()
        });
    Ok(chosen.map(|href| {
        if href.starts_with('/') {
            href
        } else {
            format!("{}/{href}", object_uri.trim_end_matches('/'))
        }
    }))
}

fn parse_versions(feed: &str) -> Result<Vec<Version>, VersionsError> {
    let document = roxmltree::Document::parse(feed)?;
    Ok(find_children(document.root_element(), "entry")
        .filter_map(|entry| {
            let id = find_child_text(entry, "id")?;
            let content = find_children(entry, "content").next()?;
            let request = find_children(entry, "link")
                .find(|link| {
                    find_attribute_value(*link, "rel")
                        .is_some_and(|rel| rel.ends_with(REQUEST_RELATION))
                })
                .and_then(|link| find_non_empty_attribute(link, "name"));
            Some(Version {
                kind: match id.as_str() {
                    ACTIVE => "active",
                    INACTIVE => "inactive",
                    _ => "history",
                },
                date: find_child_text(entry, "updated"),
                author: find_children(entry, "author")
                    .next()
                    .and_then(|author| find_child_text(author, "name")),
                description: request
                    .as_ref()
                    .and_then(|_| find_child_text(entry, "title")),
                request,
                content_uri: find_non_empty_attribute(content, "src")?,
                content_type: find_non_empty_attribute(content, "type")
                    .unwrap_or_else(|| "text/plain".to_owned()),
                id,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HISTORY: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/versions/history.xml"
    ));
    const STAGED: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/versions/staged.xml"
    ));

    #[test]
    fn a_history_lists_each_version_with_its_request() {
        let versions = parse_versions(HISTORY).unwrap();
        let ids: Vec<&str> = versions.iter().map(|version| version.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "00008", "00007", "00006", "00000", "00005", "00004", "00003", "00002", "00001"
            ],
            "newest first, with the active version where its date puts it"
        );
        let newest = &versions[0];
        assert_eq!(newest.kind, "history");
        assert_eq!(newest.request.as_deref(), Some("A4HK900101"));
        assert_eq!(newest.description.as_deref(), Some("Sample release 1"));
        assert_eq!(newest.author.as_deref(), Some("DEVUSER1"));
        assert_eq!(newest.date.as_deref(), Some("2026-11-07T01:29:17Z"));
        assert_eq!(versions[3].kind, "active");
        assert!(
            newest
                .content_uri
                .ends_with("/versions/20261107012917/00008/content")
        );
        assert_eq!(newest.content_type, "text/plain");
    }

    #[test]
    fn an_object_in_development_has_only_active_and_staged() {
        let versions = parse_versions(STAGED).unwrap();
        let kinds: Vec<(&str, &str)> = versions
            .iter()
            .map(|version| (version.id.as_str(), version.kind))
            .collect();
        assert_eq!(kinds, [("99999", "inactive"), ("00000", "active")]);
        assert_eq!(versions[0].request.as_deref(), Some("A4HK900123"));
    }

    #[test]
    fn versions_are_found_by_number_or_by_name() {
        let versions = parse_versions(HISTORY).unwrap();
        assert_eq!(
            find_version(&versions, "ZSAMPLE_VERS", "5").unwrap().id,
            "00005"
        );
        assert_eq!(
            find_version(&versions, "ZSAMPLE_VERS", "active")
                .unwrap()
                .id,
            "00000"
        );
        let missing = find_version(&versions, "ZSAMPLE_VERS", "inactive").unwrap_err();
        assert_eq!(missing.code(), "version_not_found");
        assert!(missing.hint().unwrap().contains("00008"));
    }

    #[test]
    fn a_class_list_is_chosen_by_include_and_resolved_against_the_object() {
        let class = r#"<class:abapClass xmlns:class="http://www.sap.com/adt/oo/classes" xmlns:atom="http://www.w3.org/2005/Atom"><class:include><atom:link href="includes/definitions/versions" rel="http://www.sap.com/adt/relations/versions"/></class:include><class:include><atom:link href="includes/main/versions" rel="http://www.sap.com/adt/relations/versions"/></class:include><class:include><atom:link href="includes/implementations/versions" rel="http://www.sap.com/adt/relations/versions"/></class:include></class:abapClass>"#;
        let uri = "/sap/bc/adt/oo/classes/zsample_vers";
        assert_eq!(
            versions_uri(uri, class, None).unwrap().as_deref(),
            Some("/sap/bc/adt/oo/classes/zsample_vers/includes/main/versions")
        );
        assert_eq!(
            versions_uri(uri, class, Some(ClassInclude::Implementations))
                .unwrap()
                .as_deref(),
            Some("/sap/bc/adt/oo/classes/zsample_vers/includes/implementations/versions")
        );
        let view = r#"<ddl:ddlSource xmlns:ddl="http://www.sap.com/adt/ddic/ddlsources" xmlns:atom="http://www.w3.org/2005/Atom"><atom:link href="versions" rel="http://www.sap.com/adt/relations/versions"/></ddl:ddlSource>"#;
        assert_eq!(
            versions_uri("/sap/bc/adt/ddic/ddl/sources/zsample_v", view, None)
                .unwrap()
                .as_deref(),
            Some("/sap/bc/adt/ddic/ddl/sources/zsample_v/versions")
        );
        assert_eq!(versions_uri(uri, "<x/>", None).unwrap(), None);
    }

    #[test]
    fn a_diff_ignores_line_endings() {
        let old = "METHOD step.\r\n  result = 2.\r\nENDMETHOD.\r\n";
        let new = "METHOD step.\n  result = 3.\nENDMETHOD.\n";
        let diff = unified_diff("00000", "99999", old, new);
        assert!(diff.starts_with("--- 00000\n+++ 99999\n"), "{diff}");
        assert!(diff.contains("-  result = 2.\n+  result = 3.\n"), "{diff}");
        assert!(
            !diff.contains("-METHOD step."),
            "unchanged lines are not reported: {diff}"
        );
        assert_eq!(unified_diff("a", "b", "x\r\n", "x\n"), "");
        let unterminated = unified_diff("a", "b", "x\ny", "x\nz");
        assert!(!unterminated.contains("No newline"), "{unterminated}");
    }
}
